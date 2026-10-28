//! Mail synchronization for on-premises Microsoft Exchange (2013, 2016,
//! 2019, Subscription Edition) through Exchange Web Services.
//!
//! * Folders come from the folder tree below the IPM subtree, with roles for
//!   the well-known folders; Outlook categories become colored labels (the
//!   master category list plus every category found on a message).
//! * Messages are tracked per folder with `SyncFolderItems` and its stored
//!   sync state. A folder's first sync lists it newest first (`FindItem`), a
//!   few pages per cycle, so the inbox fills quickly while the enumeration
//!   that establishes the sync state runs alongside; later rounds report only
//!   changes. Headers come from batched `GetItem` requests. Item ids change
//!   when a message moves: moves made here record the new id, moves made
//!   elsewhere hand the local copy over by `Message-ID`.
//! * Bodies are downloaded lazily as MIME and parsed exactly like IMAP
//!   messages, so HTML, inline images and attachments behave the same.
//! * Streaming notifications (Exchange 2010 SP1 and later) trigger syncs of
//!   the folders they name. Polling stays the safety net: the inbox, sent
//!   items and drafts every 45 seconds, every folder every 5 minutes (less
//!   often while notifications flow).
//! * Local changes are replayed from the pending-operation queue; messages
//!   are sent as MIME with `SendAndSaveCopy`, which files the sent copy.

mod account;
pub(crate) mod calendar;
mod mapping;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use envelope_ews::{
    self as ews, DistinguishedFolder, EwsClient, EwsError, FindMessagesOptions, FlagStatus,
    FolderId, GetMessagesOptions, ItemChange, ItemId, MessageUpdate, NotificationEventType,
    PropertyPath, ResponseCode, StreamingMessage, SyncOptions,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub(crate) use account::{ExchangeHint, exchange_hint};

use super::graph::{category_name, category_upsert, save_source, source_lock, source_path};
use super::imap::folder_priority;
use crate::db::mail::{self, AccountRow, FolderRow, FolderSyncState, MessageMeta};
use crate::db::misc::{self, OpItem, PendingOp};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mime;
use crate::model::*;

/// Changes per `SyncFolderItems` round (the most Exchange returns).
const SYNC_PAGE_SIZE: u32 = ews::MAX_SYNC_CHANGES;
/// `SyncFolderItems` rounds per folder and cycle.
const SYNC_PAGES_PER_CYCLE: usize = 4;
/// Messages per page of the newest-first listing of a new folder.
const LIST_PAGE_SIZE: u32 = 100;
/// Listing pages per folder and cycle.
const LIST_PAGES_PER_CYCLE: usize = 5;
/// Newest inbox messages stored each cycle while the inbox's first sync
/// still runs.
const PROBE_SIZE: u32 = 25;
/// How often the inbox, sent items and drafts are checked for changes.
const POLL_INTERVAL: Duration = Duration::from_secs(45);
/// How often every folder is checked.
const FULL_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// The same while streaming notifications report changes as they happen.
const PUSH_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const PUSH_FULL_SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// A wake-up (manual refresh, local change) checks every folder at most this
/// often.
const MIN_FULL_SYNC_GAP: Duration = Duration::from_secs(60);
/// Wake-ups in quick succession poll the frequent folders at most this often.
const MIN_POLL_GAP: Duration = Duration::from_secs(5);
/// How often the folder tree and categories are refreshed.
const FOLDER_LIST_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Calendar syncs triggered by notifications are at least this far apart.
const CALENDAR_PUSH_GAP: Duration = Duration::from_secs(30);
/// Concurrent MIME downloads (each fetches up to ten messages).
const DOWNLOADS_IN_FLIGHT: usize = 2;
/// Minutes a notification stream stays open before it is renewed.
const STREAM_MINUTES: u32 = 30;
/// After rejected credentials, wake-ups retry the sign-in at most this
/// often (directory lockout policies count failed attempts).
const AUTH_RETRY_GAP: Duration = Duration::from_secs(5 * 60);

// ---------------------------------------------------------------------------
// Client and errors

/// An EWS client for one account's mailbox, plus what is needed to turn its
/// failures into engine errors.
#[derive(Clone)]
pub(crate) struct EwsMail {
    client: EwsClient,
    throttled_until: Arc<parking_lot::Mutex<Option<Instant>>>,
}

/// Sign-in scheme actually used for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Scheme {
    Ntlm,
    Basic,
}

/// Everything a client is built from; a change builds a new client.
#[derive(Clone, PartialEq, Eq)]
struct ClientKey {
    url: String,
    username: String,
    password: String,
    scheme: Scheme,
    accept_invalid_certs: bool,
    version: Option<String>,
    anchor: String,
}

/// Clients by account. Clones share connections, which matters for NTLM:
/// every new connection costs a handshake.
fn clients() -> &'static parking_lot::Mutex<HashMap<i64, (ClientKey, EwsMail)>> {
    static CLIENTS: OnceLock<parking_lot::Mutex<HashMap<i64, (ClientKey, EwsMail)>>> =
        OnceLock::new();
    CLIENTS.get_or_init(Default::default)
}

/// Accounts (and their servers) with automatic sign-in whose server turned
/// out not to offer NTLM: they use Basic while the app runs.
fn basic_only() -> &'static parking_lot::Mutex<HashSet<(i64, String)>> {
    static BASIC: OnceLock<parking_lot::Mutex<HashSet<(i64, String)>>> = OnceLock::new();
    BASIC.get_or_init(Default::default)
}

fn basic_key(account: &AccountRow) -> (i64, String) {
    let url = account
        .config
        .exchange
        .as_ref()
        .map(|c| c.ews_url.clone())
        .unwrap_or_default();
    (account.id, url)
}

fn build_client(key: &ClientKey) -> Result<EwsClient> {
    let auth = match key.scheme {
        Scheme::Ntlm => ews::Auth::ntlm(key.username.clone(), key.password.clone()),
        Scheme::Basic => ews::Auth::basic(key.username.clone(), key.password.clone()),
    };
    let mut builder = EwsClient::builder(key.url.clone(), auth)
        .accept_invalid_certs(key.accept_invalid_certs)
        .anchor_mailbox(key.anchor.clone());
    if let Some(version) = key.version.as_deref().and_then(ews::ExchangeVersion::parse) {
        builder = builder.server_version(version);
    }
    builder.build().map_err(ews_error)
}

impl EwsMail {
    /// The underlying EWS client.
    pub(crate) fn client(&self) -> &EwsClient {
        &self.client
    }

    fn new(client: EwsClient) -> EwsMail {
        EwsMail {
            client,
            throttled_until: Arc::default(),
        }
    }

    /// The client for an account, signing in with its stored password.
    pub(crate) async fn for_account(engine: &Engine, account: &AccountRow) -> Result<EwsMail> {
        let config =
            account.config.exchange.clone().ok_or_else(|| {
                Error::Invalid("This Exchange account has no server address".into())
            })?;
        let password = engine
            .secrets
            .get(&crate::secrets::password_key(account.id))
            .await?
            .ok_or_else(|| {
                Error::Auth("The password of this account is missing; sign in again".into())
            })?;
        let scheme = match config.auth {
            ExchangeAuth::Ntlm => Scheme::Ntlm,
            ExchangeAuth::Basic => Scheme::Basic,
            ExchangeAuth::Auto if basic_only().lock().contains(&basic_key(account)) => {
                Scheme::Basic
            }
            ExchangeAuth::Auto => Scheme::Ntlm,
        };
        let key = ClientKey {
            url: config.ews_url.clone(),
            username: Some(config.username.trim())
                .filter(|u| !u.is_empty())
                .unwrap_or(&account.email)
                .to_string(),
            password,
            scheme,
            accept_invalid_certs: account.config.accept_invalid_certs,
            version: config.version.clone(),
            anchor: account.email.clone(),
        };
        if let Some((cached, mail)) = clients().lock().get(&account.id)
            && *cached == key
        {
            return Ok(mail.clone());
        }
        let mail = EwsMail::new(build_client(&key)?);
        clients().lock().insert(account.id, (key, mail.clone()));
        Ok(mail)
    }

    /// Converts a failure, remembering how long the server asked us to back
    /// off.
    fn err(&self, e: EwsError) -> Error {
        if let Some(after) = e.retry_after() {
            *self.throttled_until.lock() = Some(Instant::now() + after);
        }
        ews_error(e)
    }

    /// Remaining time the server asked us to stay away.
    fn throttle_delay(&self) -> Duration {
        let until = *self.throttled_until.lock();
        until
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    }
}

/// Whether a rejected sign-in means the server does not speak NTLM at all
/// (rather than refusing the credentials).
pub(crate) fn ntlm_not_offered(e: &EwsError) -> bool {
    match e {
        EwsError::Unauthorized {
            detail: Some(detail),
        } => {
            let detail = detail.to_ascii_lowercase();
            detail.contains("does not offer ntlm")
                || detail.contains("did not send an ntlm challenge")
        }
        _ => false,
    }
}

/// The text of every error in a chain.
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

/// A description of a TLS certificate problem, if that is what went wrong.
pub(crate) fn certificate_problem(e: &EwsError) -> Option<String> {
    let EwsError::Transport(inner) = e else {
        return None;
    };
    let chain = error_chain(inner);
    let lower = chain.to_ascii_lowercase();
    let hit = [
        "certificate",
        "unknownissuer",
        "notvalidforname",
        "invalid peer",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    hit.then(|| {
        chain
            .rsplit(": ")
            .find(|part| part.to_ascii_lowercase().contains("certificate"))
            .unwrap_or(&chain)
            .trim()
            .to_string()
    })
}

/// Maps an EWS failure onto engine errors: rejected sign-ins become
/// [`Error::Auth`] (the UI asks for the password again), unreachable or busy
/// servers [`Error::Network`] (offline, retried), everything else a readable
/// error.
pub(crate) fn ews_error(e: EwsError) -> Error {
    if e.is_auth_error() {
        return Error::Auth(match &e {
            EwsError::Unauthorized {
                detail: Some(detail),
            } if ntlm_not_offered(&e) => {
                format!("Exchange did not accept the sign-in method ({detail})")
            }
            _ => "Exchange rejected the user name or password".into(),
        });
    }
    if let Some(problem) = certificate_problem(&e) {
        return Error::Other(format!(
            "The Exchange server's security certificate is not trusted ({problem})"
        ));
    }
    if e.is_transient() {
        return Error::Network(e.to_string());
    }
    if e.is_not_found() {
        return Error::NotFound(e.to_string());
    }
    match e.response_code() {
        Some(ResponseCode::ErrorMessageSizeExceeded) => {
            return Error::Invalid("The message is too large for your Exchange server".into());
        }
        Some(ResponseCode::ErrorQuotaExceeded) => {
            return Error::Other("Your Exchange mailbox is full".into());
        }
        Some(ResponseCode::ErrorSendAsDenied) => {
            return Error::Invalid("Exchange does not allow sending from this address".into());
        }
        _ => {}
    }
    match e {
        EwsError::Forbidden { .. } => Error::Other(
            "Exchange refused access to Exchange Web Services (EWS may be turned off for this account)"
                .into(),
        ),
        EwsError::InvalidInput(message) => Error::Invalid(message),
        other => Error::Other(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Folder bookkeeping

/// Item sync bookkeeping of a folder, kept in [`FolderSyncState::cursor`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ItemState {
    /// `SyncFolderItems` state; `None` starts a new enumeration.
    sync_state: Option<String>,
    /// The enumeration reached the end: later rounds report changes.
    current: bool,
    /// Offset of the newest-first listing that fills a new folder; `None`
    /// once it finished (or was not needed).
    listing: Option<u32>,
    /// The sync state was lost: once the new enumeration completes, local
    /// copies the server no longer has are removed.
    prune: bool,
}

impl ItemState {
    fn fresh() -> ItemState {
        ItemState {
            listing: Some(0),
            ..ItemState::default()
        }
    }

    fn load(state: &FolderSyncState) -> ItemState {
        state
            .cursor
            .as_deref()
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or_else(ItemState::fresh)
    }

    fn store(&self, state: &mut FolderSyncState) {
        state.cursor = serde_json::to_string(self).ok();
    }
}

/// `PR_FLAG_STATUS`: the follow-up flag on every server version.
const PR_FLAG_STATUS: u16 = 0x1090;

/// Properties requested with every `SyncFolderItems` change. With `rich`,
/// the read state, flag and categories come along, so known messages need
/// no further request.
fn sync_properties(rich: bool) -> Vec<PropertyPath> {
    let mut properties = vec![
        PropertyPath::field("item:ItemClass"),
        PropertyPath::field("item:DateTimeReceived"),
    ];
    if rich {
        properties.push(PropertyPath::field("message:IsRead"));
        properties.push(PropertyPath::field("item:Categories"));
        properties.push(PropertyPath::extended_tag(PR_FLAG_STATUS, "Integer"));
    }
    properties
}

/// A change to apply to the local copy of a folder.
#[derive(Debug)]
enum Incoming {
    /// A message with its headers (new, or changed in a way that needs them).
    Full {
        meta: Box<MessageMeta>,
        categories: Vec<String>,
    },
    /// A known message whose read state, flag or categories changed.
    State {
        remote_id: String,
        seen: Option<bool>,
        flagged: Option<bool>,
        categories: Option<Vec<String>>,
    },
    /// The message left the folder.
    Gone(String),
}

impl Incoming {
    fn full(m: &ews::Message) -> Incoming {
        Incoming::Full {
            meta: Box::new(mapping::message_meta(m)),
            categories: m.categories.clone(),
        }
    }
}

/// Messages with queued local changes the server has not seen yet.
#[derive(Debug, Default)]
struct PendingChanges {
    /// Moved or deleted locally: their folder is ours to decide.
    moving: HashSet<i64>,
    /// Categories changed locally.
    labeling: HashSet<i64>,
}

fn pending_changes(c: &rusqlite::Connection, account_id: i64) -> Result<PendingChanges> {
    let mut out = PendingChanges::default();
    for op in misc::queued_ops(c, account_id)? {
        match op {
            PendingOp::Move { items, .. } | PendingOp::Expunge { items } => {
                out.moving.extend(items.iter().map(|i| i.message_id));
            }
            PendingOp::Labels { message_ids, .. } => out.labeling.extend(message_ids),
            _ => {}
        }
    }
    Ok(out)
}

/// A message that left every folder (a move elsewhere reported by its old
/// folder first) with this `Message-ID`: the copy a new id belongs to.
fn orphan_by_message_id(
    tx: &rusqlite::Transaction<'_>,
    account_id: i64,
    message_id: &str,
) -> Result<Option<(i64, i64, i64)>> {
    use rusqlite::OptionalExtension;
    Ok(tx
        .query_row(
            "SELECT m.id, m.thread_id, m.flags FROM messages m
             WHERE m.account_id = ? AND m.message_id = ?
               AND NOT EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                               WHERE mf.message_id = m.id AND f.kind != 'label')
             ORDER BY m.id DESC LIMIT 1",
            rusqlite::params![account_id, message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

/// Adds label folders for categories found on messages but missing from the
/// master category list.
fn add_category_labels(
    tx: &rusqlite::Transaction<'_>,
    account_id: i64,
    names: &[String],
) -> Result<()> {
    for name in names {
        let label = category_upsert(name, None);
        tx.execute(
            "INSERT INTO folders (account_id, remote_id, name, path, role, kind, delimiter, selectable, sort_order, color)
             VALUES (?, ?, ?, ?, NULL, ?, NULL, 0, ?, NULL)
             ON CONFLICT (account_id, remote_id) DO NOTHING",
            rusqlite::params![
                account_id,
                label.remote_id,
                label.name,
                label.path,
                label.kind,
                label.sort_order
            ],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming notifications

/// What the notification stream reported, for the sync session.
#[derive(Default)]
struct PushState {
    /// Folder ids named in notifications since the session last looked.
    folders: parking_lot::Mutex<HashSet<String>>,
    /// A subscription is active and its stream connected.
    alive: AtomicBool,
    /// Changes may have been missed (a new subscription): look everywhere.
    resync: AtomicBool,
    /// Signals new notifications.
    notify: Notify,
}

const PUSH_EVENTS: &[NotificationEventType] = &[
    NotificationEventType::NewMail,
    NotificationEventType::Created,
    NotificationEventType::Deleted,
    NotificationEventType::Modified,
    NotificationEventType::Moved,
    NotificationEventType::Copied,
];

/// Keeps a streaming subscription for every folder of the mailbox open and
/// reports the folders it names. Gives up quietly on servers without
/// streaming notifications; polling covers them.
async fn listen(client: EwsClient, push: Arc<PushState>, stop: CancellationToken) {
    let mut failures: u32 = 0;
    let pause = |failures: u32| {
        Duration::from_secs(5 * 2u64.pow(failures.min(7))).min(Duration::from_secs(600))
    };
    'subscribe: loop {
        if stop.is_cancelled() {
            return;
        }
        let subscription = match client.subscribe_streaming_all_folders(PUSH_EVENTS).await {
            Ok(id) => id,
            Err(EwsError::InvalidInput(reason)) => {
                tracing::debug!("no streaming notifications: {reason}");
                return;
            }
            Err(e) => {
                push.alive.store(false, Ordering::SeqCst);
                failures += 1;
                if e.is_auth_error() {
                    // The sync loop reports the sign-in problem.
                    return;
                }
                tracing::debug!("cannot subscribe to notifications: {e}");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(pause(failures)) => continue,
                }
            }
        };
        // Changes made before the subscription existed may have been missed.
        push.resync.store(true, Ordering::SeqCst);
        push.notify.notify_one();
        loop {
            let opened = Instant::now();
            let mut stream = match client
                .get_streaming_events(std::slice::from_ref(&subscription), STREAM_MINUTES)
                .await
            {
                Ok(stream) => stream,
                Err(e) => {
                    push.alive.store(false, Ordering::SeqCst);
                    failures += 1;
                    tracing::debug!("notification stream failed: {e}");
                    let gone = e
                        .response_code()
                        .is_some_and(ResponseCode::is_subscription_gone);
                    tokio::select! {
                        _ = stop.cancelled() => return,
                        _ = tokio::time::sleep(pause(failures)) => {}
                    }
                    if gone || !e.is_transient() {
                        continue 'subscribe;
                    }
                    continue;
                }
            };
            let resubscribe = loop {
                let next = tokio::select! {
                    _ = stop.cancelled() => return,
                    next = stream.next() => next,
                };
                // Notifications flow once the server's messages arrive (a
                // proxy that buffers the stream never gets this far).
                if matches!(
                    next,
                    Some(Ok(
                        StreamingMessage::KeepAlive | StreamingMessage::Notifications(_)
                    ))
                ) {
                    push.alive.store(true, Ordering::SeqCst);
                    failures = 0;
                }
                match next {
                    None | Some(Ok(StreamingMessage::Closed)) => break false,
                    Some(Ok(StreamingMessage::KeepAlive)) => {}
                    Some(Ok(StreamingMessage::Notifications(batch))) => {
                        let mut folders = push.folders.lock();
                        for event in batch.iter().flat_map(|n| &n.events) {
                            folders
                                .extend(event.affected_folders().into_iter().map(|f| f.id.clone()));
                        }
                        drop(folders);
                        push.notify.notify_one();
                    }
                    Some(Ok(StreamingMessage::SubscriptionErrors(errors))) => {
                        tracing::debug!("notification subscription ended: {errors:?}");
                        break true;
                    }
                    Some(Err(e)) => {
                        tracing::debug!("notification stream broke: {e}");
                        failures += 1;
                        break false;
                    }
                }
            };
            drop(stream);
            push.alive.store(false, Ordering::SeqCst);
            if resubscribe {
                continue 'subscribe;
            }
            // A stream that ends right away should not be reopened in a loop.
            if opened.elapsed() < Duration::from_secs(10) {
                failures += 1;
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(pause(failures)) => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The sync worker

/// Runs the sync loop of an Exchange account until stopped.
pub async fn run(engine: Engine, account_id: i64, wake: Arc<Notify>, stop: CancellationToken) {
    let mut backoff = Duration::from_secs(2);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let Ok(account) = engine.account_row(account_id) else {
            return;
        };
        engine.set_sync_state(account_id, SyncState::Connecting);
        let ews = match EwsMail::for_account(&engine, &account).await {
            Ok(ews) => ews,
            Err(e) => {
                let state = if e.is_auth() {
                    SyncState::AuthError {
                        message: e.to_string(),
                    }
                } else {
                    SyncState::Error {
                        message: e.to_string(),
                    }
                };
                engine.set_sync_state(account_id, state);
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30 * 60)) => {}
                }
                continue;
            }
        };
        let key = basic_key(&account);
        match run_once(&engine, account, ews.clone(), &wake, &stop).await {
            Ok(()) => return,
            Err(Failure::NtlmNotOffered) => {
                // Automatic sign-in: the server only takes Basic.
                tracing::info!(account_id, "Exchange does not offer NTLM, using Basic");
                basic_only().lock().insert(key);
            }
            Err(Failure::Auth(e)) => {
                tracing::warn!(account_id, "Exchange sign-in failed: {e}");
                engine.set_sync_state(
                    account_id,
                    SyncState::AuthError {
                        message: e.to_string(),
                    },
                );
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = async {
                        tokio::time::sleep(AUTH_RETRY_GAP).await;
                        wake.notified().await;
                    } => {}
                    _ = tokio::time::sleep(Duration::from_secs(30 * 60)) => {}
                }
            }
            Err(Failure::Other(e, long_run)) => {
                if long_run {
                    backoff = Duration::from_secs(2);
                }
                tracing::warn!(account_id, "Exchange sync error: {e}");
                let state = if e.is_transient() {
                    SyncState::Offline
                } else {
                    SyncState::Error {
                        message: e.to_string(),
                    }
                };
                engine.set_sync_state(account_id, state);
                let delay = backoff.max(ews.throttle_delay());
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = wake.notified() => { backoff = Duration::from_secs(2); }
                    _ = tokio::time::sleep(delay) => {
                        backoff = (backoff * 2).min(Duration::from_secs(300));
                    }
                }
            }
        }
    }
}

/// How a session ended.
enum Failure {
    /// Automatic sign-in met a server without NTLM: retry with Basic.
    NtlmNotOffered,
    Auth(Error),
    /// Any other failure, and whether the session ran for a while.
    Other(Error, bool),
}

/// One session: the notification listener and the sync loop, until an
/// error or stop.
async fn run_once(
    engine: &Engine,
    account: AccountRow,
    ews: EwsMail,
    wake: &Notify,
    stop: &CancellationToken,
) -> std::result::Result<(), Failure> {
    let auto = account
        .config
        .exchange
        .as_ref()
        .is_some_and(|c| c.auth == ExchangeAuth::Auto)
        && !basic_only().lock().contains(&basic_key(&account));
    let started = Instant::now();
    let push = Arc::new(PushState::default());
    let listener_stop = stop.child_token();
    let listener = tokio::spawn(listen(
        ews.client.clone(),
        push.clone(),
        listener_stop.clone(),
    ));
    let mut session = Session::new(engine.clone(), account, ews, stop.clone());
    session.push = Some(push);
    let result = session.run(wake).await;
    listener_stop.cancel();
    let _ = listener.await;
    match result {
        Ok(()) => Ok(()),
        Err(error) if auto && ntlm_unavailable(&error) => Err(Failure::NtlmNotOffered),
        Err(error) if error.is_auth() => Err(Failure::Auth(error)),
        Err(error) => Err(Failure::Other(
            error,
            started.elapsed() > Duration::from_secs(120),
        )),
    }
}

/// Whether a sign-in failure (as mapped by [`ews_error`]) means the server
/// does not speak NTLM.
fn ntlm_unavailable(error: &Error) -> bool {
    match error {
        Error::Auth(message) => {
            let message = message.to_ascii_lowercase();
            message.contains("does not offer ntlm")
                || message.contains("did not send an ntlm challenge")
        }
        _ => false,
    }
}

struct Session {
    engine: Engine,
    account: AccountRow,
    ews: EwsMail,
    stop: CancellationToken,
    push: Option<Arc<PushState>>,
    folders: Vec<FolderRow>,
    /// Lower-case category name → label folder id.
    categories: HashMap<String, i64>,
    /// Remote ids of the mailbox's calendar folders.
    calendar_folders: HashSet<String>,
    last_full: Option<Instant>,
    last_poll: Option<Instant>,
    last_folder_list: Option<Instant>,
    last_calendar_push: Option<Instant>,
    /// The first pass over every folder finished.
    initial_done: bool,
    /// Folders whose listing or enumeration continues in the next cycle.
    backfill: BTreeSet<i64>,
    /// Creating a missing archive folder was attempted.
    archive_checked: bool,
    /// `SyncFolderItems` returns read state, flag and categories (turned off
    /// should a server reject those properties).
    rich_sync: bool,
}

impl Session {
    fn new(engine: Engine, account: AccountRow, ews: EwsMail, stop: CancellationToken) -> Self {
        Session {
            engine,
            account,
            ews,
            stop,
            push: None,
            folders: Vec::new(),
            categories: HashMap::new(),
            calendar_folders: HashSet::new(),
            last_full: None,
            last_poll: None,
            last_folder_list: None,
            last_calendar_push: None,
            initial_done: false,
            backfill: BTreeSet::new(),
            archive_checked: false,
            rich_sync: true,
        }
    }

    fn id(&self) -> i64 {
        self.account.id
    }

    fn err(&self, e: EwsError) -> Error {
        self.ews.err(e)
    }

    fn push_alive(&self) -> bool {
        self.push
            .as_ref()
            .is_some_and(|p| p.alive.load(Ordering::SeqCst))
    }

    fn intervals(&self) -> (Duration, Duration) {
        if self.push_alive() {
            (PUSH_POLL_INTERVAL, PUSH_FULL_SYNC_INTERVAL)
        } else {
            (POLL_INTERVAL, FULL_SYNC_INTERVAL)
        }
    }

    async fn run(&mut self, wake: &Notify) -> Result<()> {
        self.reload_folders()?;
        loop {
            if self.stop.is_cancelled() {
                return Ok(());
            }
            self.process_ops().await?;
            // First: a new subscription may ask for a full sync.
            let pushed = self.take_pushed_folders();
            let (_, full_interval) = self.intervals();
            let full_due = self.last_full.is_none_or(|t| t.elapsed() >= full_interval);
            let poll_due = self.last_poll.is_none_or(|t| t.elapsed() >= MIN_POLL_GAP)
                || !self.backfill.is_empty();
            let mut more = false;
            if full_due {
                if self.folders.is_empty()
                    || self
                        .last_folder_list
                        .is_none_or(|t| t.elapsed() >= FOLDER_LIST_INTERVAL)
                {
                    self.sync_folder_list().await?;
                }
                let (backlog, clean) = self.sync_folders(None).await?;
                more |= backlog;
                self.last_full = Some(Instant::now());
                self.last_poll = self.last_full;
                self.initial_done = true;
                if clean && !self.stop.is_cancelled() {
                    self.tidy().await?;
                }
            } else if poll_due || !pushed.is_empty() {
                let mut quick = self.quick_folders();
                quick.extend(pushed);
                more |= self.sync_folders(Some(&quick)).await?.0;
                self.last_poll = Some(Instant::now());
            }
            more |= self.prefetch_bodies(if more { 30 } else { 100 }).await?;
            self.process_ops().await?;
            self.engine.set_sync_state(self.id(), SyncState::Idle);
            if more {
                // Keep downloading, but stay responsive to local changes.
                tokio::select! {
                    _ = self.stop.cancelled() => {}
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
                continue;
            }
            self.wait_for_changes(wake).await?;
        }
    }

    /// Local folders named by notifications since the last look; a calendar
    /// among them refreshes this account's calendars.
    fn take_pushed_folders(&mut self) -> BTreeSet<i64> {
        let Some(push) = self.push.clone() else {
            return BTreeSet::new();
        };
        if push.resync.swap(false, Ordering::SeqCst)
            && self
                .last_full
                .is_none_or(|t| t.elapsed() >= MIN_FULL_SYNC_GAP)
        {
            self.last_full = None;
        }
        let remote: HashSet<String> = std::mem::take(&mut *push.folders.lock());
        if remote.is_empty() {
            return BTreeSet::new();
        }
        if remote.iter().any(|r| self.calendar_folders.contains(r))
            && self
                .last_calendar_push
                .is_none_or(|t| t.elapsed() >= CALENDAR_PUSH_GAP)
        {
            self.last_calendar_push = Some(Instant::now());
            let engine = self.engine.clone();
            let account = self.account.clone();
            let ews = self.ews.clone();
            tokio::spawn(async move {
                match calendar::sync_with(&engine, &account, &ews).await {
                    Ok(changed) if !changed.is_empty() => {
                        engine.emit(EngineEvent::CalendarChanged {
                            calendar_ids: changed,
                        });
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!("calendar refresh after a notification failed: {e}"),
                }
            });
        }
        self.folders
            .iter()
            .filter(|f| f.kind != "label" && remote.contains(&f.remote_id))
            .map(|f| f.id)
            .collect()
    }

    /// Sleeps until the next poll, a notification, a due queued operation, a
    /// wake-up or stop.
    async fn wait_for_changes(&mut self, wake: &Notify) -> Result<()> {
        let since = |t: Option<Instant>| t.map(|t| t.elapsed()).unwrap_or_default();
        let (poll, full) = self.intervals();
        let mut wait = poll
            .saturating_sub(since(self.last_poll))
            .min(full.saturating_sub(since(self.last_full)));
        let account_id = self.id();
        if let Some(next) = self.engine.db.read(|c| misc::next_op_time(c, account_id))? {
            wait = wait.min(Duration::from_millis((next - now_ms()).max(0) as u64));
        }
        let push = self.push.clone();
        let pushed = async {
            match &push {
                Some(push) => push.notify.notified().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = self.stop.cancelled() => {}
            _ = wake.notified() => {
                // A manual refresh or local change: look everywhere, but not
                // on every single click.
                if self.last_full.is_none_or(|t| t.elapsed() >= MIN_FULL_SYNC_GAP) {
                    self.last_full = None;
                }
            }
            _ = pushed => {}
            _ = tokio::time::sleep(wait.max(Duration::from_millis(500))) => {}
        }
        Ok(())
    }

    // ----- folders -----------------------------------------------------------

    /// The master category list; `None` when it cannot be read.
    async fn master_categories(&self) -> Result<Option<Vec<(String, Option<String>)>>> {
        match self
            .ews
            .client
            .execute_raw(mapping::CATEGORY_LIST_REQUEST, true)
            .await
        {
            Ok(response) => Ok(Some(mapping::category_list(&response).unwrap_or_default())),
            Err(e) => {
                let e = self.err(e);
                if e.is_transient() || e.is_auth() {
                    return Err(e);
                }
                tracing::debug!(
                    account_id = self.id(),
                    "cannot read Outlook categories: {e}"
                );
                Ok(None)
            }
        }
    }

    async fn sync_folder_list(&mut self) -> Result<()> {
        let mut tree = self
            .ews
            .client
            .folder_tree()
            .await
            .map_err(|e| self.err(e))?;
        let categories = self.master_categories().await?.unwrap_or_default();
        let mut upserts = mapping::folder_upserts(&tree, &categories);
        if !self.archive_checked
            && !tree.is_empty()
            && !upserts.iter().any(|u| u.role == Some(FolderRole::Archive))
        {
            // Archiving needs a destination; create the conventional folder.
            self.archive_checked = true;
            match self
                .ews
                .client
                .create_folder(
                    DistinguishedFolder::MsgFolderRoot,
                    "Archive",
                    ews::FOLDER_CLASS_MAIL,
                )
                .await
            {
                Ok(id) => {
                    let root = tree
                        .iter()
                        .find(|f| f.role == Some(ews::FolderRole::Inbox))
                        .and_then(|f| f.parent_id.clone());
                    tree.push(ews::Folder {
                        id,
                        parent_id: root,
                        display_name: "Archive".into(),
                        folder_class: Some(ews::FOLDER_CLASS_MAIL.into()),
                        kind: ews::FolderKind::Folder,
                        total_count: Some(0),
                        unread_count: Some(0),
                        child_folder_count: Some(0),
                        is_hidden: false,
                        distinguished_id: None,
                        role: Some(ews::FolderRole::Archive),
                    });
                    upserts = mapping::folder_upserts(&tree, &categories);
                }
                Err(e) => tracing::warn!(account_id = self.id(), "cannot create Archive: {e}"),
            }
        }
        // Categories seen on messages stay labels even when the master list
        // does not name them.
        for f in self.folders.iter().filter(|f| f.kind == "label") {
            if let Some(name) = category_name(f)
                && !upserts.iter().any(|u| u.remote_id == f.remote_id)
            {
                upserts.push(category_upsert(name, None));
            }
        }
        self.calendar_folders = tree
            .iter()
            .filter(|f| f.is_calendar())
            .map(|f| f.id.id.clone())
            .collect();
        let account_id = self.id();
        self.engine
            .db
            .write_async(move |tx| mail::sync_folders(tx, account_id, &upserts).map(|_| ()))
            .await?;
        self.reload_folders()?;
        self.last_folder_list = Some(Instant::now());
        self.engine.emit(EngineEvent::FoldersChanged { account_id });
        Ok(())
    }

    fn reload_folders(&mut self) -> Result<()> {
        let account_id = self.id();
        self.folders = self
            .engine
            .db
            .read(|c| mail::list_folder_rows(c, account_id))?;
        self.categories = self
            .folders
            .iter()
            .filter(|f| f.kind == "label")
            .filter_map(|f| Some((category_name(f)?.to_lowercase(), f.id)))
            .collect();
        Ok(())
    }

    /// Label folders for a message's categories, creating labels for
    /// categories not seen before.
    fn category_folders(&self, names: &[String]) -> Vec<i64> {
        let mut ids: Vec<i64> = names
            .iter()
            .filter_map(|n| self.categories.get(&n.trim().to_lowercase()).copied())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Makes sure every category of `incoming` has a label folder.
    fn ensure_categories(&mut self, incoming: &[Incoming]) -> Result<()> {
        let mut missing: Vec<String> = Vec::new();
        for item in incoming {
            let names = match item {
                Incoming::Full { categories, .. } => categories.as_slice(),
                Incoming::State {
                    categories: Some(categories),
                    ..
                } => categories.as_slice(),
                _ => &[],
            };
            for name in names {
                let name = name.trim();
                if !name.is_empty()
                    && !self.categories.contains_key(&name.to_lowercase())
                    && !missing.iter().any(|m| m.eq_ignore_ascii_case(name))
                {
                    missing.push(name.to_string());
                }
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let account_id = self.id();
        self.engine
            .db
            .write(|tx| add_category_labels(tx, account_id, &missing))?;
        self.reload_folders()?;
        self.engine.emit(EngineEvent::FoldersChanged { account_id });
        Ok(())
    }

    /// Folders polled often: inbox, sent items, drafts and folders whose
    /// first listing is still running.
    fn quick_folders(&self) -> BTreeSet<i64> {
        let mut ids: BTreeSet<i64> = self
            .folders
            .iter()
            .filter(|f| {
                matches!(
                    f.role,
                    Some(FolderRole::Inbox | FolderRole::Sent | FolderRole::Drafts)
                )
            })
            .map(|f| f.id)
            .collect();
        ids.extend(self.backfill.iter().copied());
        ids
    }

    /// Synchronizes every folder (or the given ones) in priority order.
    /// Returns whether work continues in the next cycle and whether every
    /// folder could be read.
    async fn sync_folders(&mut self, only: Option<&BTreeSet<i64>>) -> Result<(bool, bool)> {
        let mut list: Vec<FolderRow> = self
            .folders
            .iter()
            .filter(|f| f.kind != "label" && f.selectable)
            .filter(|f| only.is_none_or(|ids| ids.contains(&f.id)))
            .cloned()
            .collect();
        list.sort_by_key(|f| folder_priority(f.role));
        let total = list.len().max(1);
        let mut more = false;
        let mut clean = true;
        for (i, folder) in list.iter().enumerate() {
            if self.stop.is_cancelled() {
                return Ok((more, false));
            }
            if !self.initial_done {
                self.engine.set_sync_state(
                    self.id(),
                    SyncState::Syncing {
                        folder: Some(folder.name.clone()),
                        progress: Some(i as f32 / total as f32),
                    },
                );
            }
            match self.sync_folder(folder).await {
                Ok(true) => {
                    self.backfill.insert(folder.id);
                    more = true;
                }
                Ok(false) => {
                    self.backfill.remove(&folder.id);
                }
                Err(Error::NotFound(text)) => {
                    // The folder vanished; the next folder listing fixes it.
                    tracing::debug!("skipping folder {}: {text}", folder.name);
                    self.backfill.remove(&folder.id);
                    self.last_folder_list = None;
                    clean = false;
                }
                Err(e) => return Err(e),
            }
            // Keep local changes flowing during long synchronizations.
            self.process_ops().await?;
        }
        Ok((more, clean))
    }

    /// Runs (part of) the listing and the item sync of one folder. Returns
    /// true when work continues in the next cycle.
    async fn sync_folder(&mut self, folder: &FolderRow) -> Result<bool> {
        // Re-read: an earlier step may have changed the state.
        let folder = self.engine.db.read(|c| mail::get_folder(c, folder.id))?;
        let mut state = folder.sync_state.clone();
        let mut items = ItemState::load(&state);
        if items.current {
            items.listing = None;
        }
        let inbox = folder.role == Some(FolderRole::Inbox);
        if inbox && !items.current && items.listing.is_none_or(|offset| offset > 0) {
            // A long first sync of the inbox: show new arrivals meanwhile.
            self.probe(&folder, &state).await?;
        }
        if items.listing.is_some() {
            self.list_folder(&folder, &mut items, &mut state).await?;
        }
        // While the listing runs, it gets most of the work.
        let budget = if items.listing.is_some() {
            1
        } else {
            SYNC_PAGES_PER_CYCLE
        };
        let mut restarted = false;
        let mut pages = 0;
        while pages < budget {
            if self.stop.is_cancelled() {
                return Ok(true);
            }
            let options = SyncOptions {
                max_changes: SYNC_PAGE_SIZE,
                properties: sync_properties(self.rich_sync),
                ignore: Vec::new(),
            };
            let page = match self
                .ews
                .client
                .sync_folder_items(
                    FolderId::new(folder.remote_id.clone()),
                    items.sync_state.as_deref(),
                    &options,
                )
                .await
            {
                Ok(page) => page,
                Err(e) if e.requires_resync() && items.sync_state.is_some() && !restarted => {
                    tracing::info!(
                        account_id = self.id(),
                        "sync state of {} expired, enumerating the folder again",
                        folder.name
                    );
                    restarted = true;
                    items = ItemState {
                        prune: true,
                        ..ItemState::default()
                    };
                    continue;
                }
                Err(e)
                    if self.rich_sync
                        && matches!(
                            e.response_code(),
                            Some(
                                ResponseCode::ErrorInvalidPropertyRequest
                                    | ResponseCode::ErrorSchemaValidation
                            )
                        ) =>
                {
                    tracing::info!(
                        account_id = self.id(),
                        "the server refuses read state and flags in item sync: {e}"
                    );
                    self.rich_sync = false;
                    continue;
                }
                Err(e) => return Err(self.err(e)),
            };
            pages += 1;
            // New-mail notifications only for changes, not for enumerations.
            let notify = inbox && items.current;
            let incoming = self.resolve_changes(page.changes).await?;
            items.sync_state = Some(page.sync_state);
            let done = page.includes_last_item_in_range;
            if done && !items.current {
                items.current = true;
                items.listing = None;
            }
            state.complete |= items.current;
            state.last_sync = Some(now_ms());
            items.store(&mut state);
            self.store(&folder, incoming, &state, notify).await?;
            if done {
                if items.prune {
                    self.prune_folder(&folder).await?;
                    items.prune = false;
                    items.store(&mut state);
                    let folder_id = folder.id;
                    let saved = state.clone();
                    self.engine
                        .db
                        .write(|tx| mail::set_folder_sync_state(tx, folder_id, &saved))?;
                }
                return Ok(items.listing.is_some());
            }
        }
        Ok(true)
    }

    /// Stores the newest messages of a folder outside the listing and the
    /// enumeration.
    async fn probe(&mut self, folder: &FolderRow, state: &FolderSyncState) -> Result<()> {
        let options = FindMessagesOptions {
            offset: 0,
            page_size: PROBE_SIZE,
            newest_first: true,
            received_since: None,
            properties: Some(vec![PropertyPath::field("item:ItemClass")]),
        };
        let page = self
            .ews
            .client
            .find_messages(FolderId::new(folder.remote_id.clone()), &options)
            .await
            .map_err(|e| self.err(e))?;
        let ids: Vec<String> = page
            .items
            .iter()
            .filter(|m| mapping::is_mail_kind(m.kind))
            .map(|m| m.id.id.clone())
            .collect();
        let unknown = self.unknown(&ids)?;
        if unknown.is_empty() {
            return Ok(());
        }
        let incoming: Vec<Incoming> = self
            .fetch_headers(&unknown)
            .await?
            .iter()
            .map(Incoming::full)
            .collect();
        self.store(folder, incoming, state, false).await
    }

    /// Lists a new folder newest first, a few pages per cycle.
    async fn list_folder(
        &mut self,
        folder: &FolderRow,
        items: &mut ItemState,
        state: &mut FolderSyncState,
    ) -> Result<()> {
        for _ in 0..LIST_PAGES_PER_CYCLE {
            let Some(offset) = items.listing else {
                return Ok(());
            };
            if self.stop.is_cancelled() {
                return Ok(());
            }
            let options = FindMessagesOptions {
                offset,
                page_size: LIST_PAGE_SIZE,
                newest_first: true,
                received_since: None,
                properties: Some(vec![PropertyPath::field("item:ItemClass")]),
            };
            let page = self
                .ews
                .client
                .find_messages(FolderId::new(folder.remote_id.clone()), &options)
                .await
                .map_err(|e| self.err(e))?;
            let returned = page.items.len() as u32;
            let ids: Vec<String> = page
                .items
                .iter()
                .filter(|m| mapping::is_mail_kind(m.kind))
                .map(|m| m.id.id.clone())
                .collect();
            let unknown = self.unknown(&ids)?;
            let incoming: Vec<Incoming> = self
                .fetch_headers(&unknown)
                .await?
                .iter()
                .map(Incoming::full)
                .collect();
            items.listing = if page.includes_last_item_in_range || returned == 0 {
                None
            } else {
                Some(
                    page.next_offset
                        .unwrap_or(offset + returned)
                        .max(offset + 1),
                )
            };
            if items.listing.is_none() {
                state.complete = true;
            }
            state.last_sync = Some(now_ms());
            items.store(state);
            self.store(folder, incoming, state, false).await?;
        }
        Ok(())
    }

    /// Remote ids of `ids` not stored locally.
    fn unknown(&self, ids: &[String]) -> Result<Vec<String>> {
        let account_id = self.id();
        self.engine.db.read(|c| {
            let mut out = Vec::new();
            for id in ids {
                if mail::message_by_remote(c, account_id, id)?.is_none() {
                    out.push(id.clone());
                }
            }
            Ok(out)
        })
    }

    /// Headers of messages by id; messages that vanished meanwhile, and items
    /// that are not mail, are skipped.
    async fn fetch_headers(&self, ids: &[String]) -> Result<Vec<ews::Message>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let item_ids: Vec<ItemId> = ids.iter().map(|id| ItemId::new(id.clone())).collect();
        let results = self
            .ews
            .client
            .get_messages(&item_ids, &GetMessagesOptions::default())
            .await
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for (id, result) in ids.iter().zip(results) {
            match result {
                Ok(m) if mapping::is_mail_kind(m.kind) => out.push(m),
                Ok(_) => {}
                Err(e) if e.code.is_not_found() => {}
                Err(e) if e.code.is_transient() => return Err(self.err(EwsError::from(e))),
                Err(e) => tracing::debug!("cannot read item {id}: {e}"),
            }
        }
        Ok(out)
    }

    /// Turns a page of reported changes into local changes, fetching headers
    /// where they are needed: for messages new to us, drafts (edited
    /// elsewhere) and, when the server does not report state with the
    /// changes, every changed message.
    async fn resolve_changes(&self, changes: Vec<ItemChange>) -> Result<Vec<Incoming>> {
        let account_id = self.id();
        let reported: Vec<String> = changes
            .iter()
            .filter_map(|c| match c {
                ItemChange::Created(m) | ItemChange::Updated(m)
                    if mapping::is_mail_kind(m.kind) =>
                {
                    Some(m.id.id.clone())
                }
                _ => None,
            })
            .collect();
        let known: HashMap<String, i64> = self.engine.db.read(|c| {
            let mut out = HashMap::new();
            for id in &reported {
                if let Some((_, _, flags)) = mail::message_by_remote(c, account_id, id)? {
                    out.insert(id.clone(), flags);
                }
            }
            Ok(out)
        })?;
        let needs_headers = |id: &str| match known.get(id) {
            Some(flags) => !self.rich_sync || flags & Flags::DRAFT != 0,
            None => true,
        };
        let wanted: Vec<String> = reported
            .iter()
            .filter(|id| needs_headers(id))
            .cloned()
            .collect();
        let mut fetched: HashMap<String, ews::Message> = self
            .fetch_headers(&wanted)
            .await?
            .into_iter()
            .map(|m| (m.id.id.clone(), m))
            .collect();
        let mut out = Vec::new();
        for change in changes {
            match change {
                ItemChange::Created(m) | ItemChange::Updated(m) => {
                    if !mapping::is_mail_kind(m.kind) {
                        continue;
                    }
                    if needs_headers(&m.id.id) {
                        if let Some(full) = fetched.remove(&m.id.id) {
                            out.push(Incoming::full(&full));
                        }
                    } else {
                        out.push(Incoming::State {
                            remote_id: m.id.id.clone(),
                            seen: m.is_read,
                            flagged: Some(m.is_flagged()),
                            categories: Some(m.categories.clone()),
                        });
                    }
                }
                ItemChange::Deleted { id } => out.push(Incoming::Gone(id.id)),
                ItemChange::ReadFlagChanged { id, is_read } => out.push(Incoming::State {
                    remote_id: id.id,
                    seen: Some(is_read),
                    flagged: None,
                    categories: None,
                }),
            }
        }
        Ok(out)
    }

    /// Stores changes to one folder together with its new sync state.
    async fn store(
        &mut self,
        folder: &FolderRow,
        incoming: Vec<Incoming>,
        state: &FolderSyncState,
        notify: bool,
    ) -> Result<()> {
        self.ensure_categories(&incoming)?;
        let account_id = self.id();
        let folder_id = folder.id;
        let resolved: Vec<(Incoming, Option<Vec<i64>>)> = incoming
            .into_iter()
            .map(|item| {
                let labels = match &item {
                    Incoming::Full { categories, .. } => Some(self.category_folders(categories)),
                    Incoming::State {
                        categories: Some(categories),
                        ..
                    } => Some(self.category_folders(categories)),
                    _ => None,
                };
                (item, labels)
            })
            .collect();
        let state = state.clone();
        let my_email = self.account.email.to_lowercase();
        let (threads, folders, mut fresh_unread, redrafted, arrivals) = self
            .engine
            .db
            .write_async(move |tx| {
                let pending = pending_changes(tx, account_id)?;
                let mut threads = BTreeSet::new();
                let mut fresh_unread = Vec::new();
                let mut redrafted = Vec::new();
                let mut arrivals = Vec::new();
                for (item, labels) in resolved {
                    match item {
                        Incoming::Full { meta, .. } => {
                            let mut meta = *meta;
                            let remote = meta.remote_id.clone().unwrap_or_default();
                            let mut existing = mail::message_by_remote(tx, account_id, &remote)?;
                            if existing.is_none()
                                && let Some(mid) = meta.message_id.as_deref()
                                && let Some(orphan) = orphan_by_message_id(tx, account_id, mid)?
                            {
                                // Moved elsewhere: the copy gets its new id.
                                mail::set_remote_id(tx, orphan.0, &remote)?;
                                existing = Some(orphan);
                            }
                            let stored = match existing {
                                Some((id, thread_id, old_flags)) => {
                                    // Exchange does not report replied/forwarded: keep ours.
                                    meta.flags |= old_flags & (Flags::ANSWERED | Flags::FORWARDED);
                                    mail::set_flags_and_keywords(
                                        tx,
                                        id,
                                        meta.flags,
                                        &meta.keywords,
                                    )?;
                                    if !pending.moving.contains(&id) {
                                        mail::set_only_folder(tx, id, folder_id)?;
                                    }
                                    if meta.flags & Flags::DRAFT != 0 {
                                        // Drafts change in place when edited elsewhere.
                                        mail::refresh_draft(tx, id, &meta)?;
                                        redrafted.push(id);
                                    }
                                    mail::Stored {
                                        message_id: id,
                                        thread_id,
                                        is_new: false,
                                    }
                                }
                                None => {
                                    mail::upsert_message(tx, account_id, folder_id, None, &meta)?
                                }
                            };
                            if let Some(labels) = labels
                                && !pending.labeling.contains(&stored.message_id)
                            {
                                mail::set_label_folders(tx, stored.message_id, &labels)?;
                            }
                            if stored.is_new {
                                let from_me = meta
                                    .from
                                    .as_ref()
                                    .is_some_and(|f| f.email.eq_ignore_ascii_case(&my_email));
                                if from_me {
                                    let recipients: Vec<Address> =
                                        meta.to.iter().chain(&meta.cc).cloned().collect();
                                    misc::record_addresses(tx, &recipients, true, meta.date)?;
                                } else if let Some(f) = &meta.from {
                                    misc::record_addresses(
                                        tx,
                                        std::slice::from_ref(f),
                                        false,
                                        meta.date,
                                    )?;
                                }
                                if notify
                                    && meta.flags & (Flags::SEEN | Flags::DRAFT) == 0
                                    && !from_me
                                {
                                    fresh_unread.push(stored.thread_id);
                                }
                                if notify && meta.flags & Flags::DRAFT == 0 && !from_me {
                                    arrivals.push(crate::rules::Incoming {
                                        account_id,
                                        thread_id: stored.thread_id,
                                        from: meta.from.clone(),
                                        to: meta.to.clone(),
                                        cc: meta.cc.clone(),
                                        subject: meta.subject.clone(),
                                    });
                                }
                            }
                            threads.insert(stored.thread_id);
                        }
                        Incoming::State {
                            remote_id,
                            seen,
                            flagged,
                            ..
                        } => {
                            let Some((id, thread_id, old)) =
                                mail::message_by_remote(tx, account_id, &remote_id)?
                            else {
                                continue;
                            };
                            let mut flags = old;
                            for (value, bit) in [(seen, Flags::SEEN), (flagged, Flags::FLAGGED)] {
                                match value {
                                    Some(true) => flags |= bit,
                                    Some(false) => flags &= !bit,
                                    None => {}
                                }
                            }
                            if flags != old {
                                mail::set_message_flags(tx, id, flags)?;
                            }
                            if !pending.moving.contains(&id) {
                                mail::set_only_folder(tx, id, folder_id)?;
                            }
                            if let Some(labels) = labels
                                && !pending.labeling.contains(&id)
                            {
                                mail::set_label_folders(tx, id, &labels)?;
                            }
                            threads.insert(thread_id);
                        }
                        Incoming::Gone(remote) => {
                            if let Some((id, _, _)) =
                                mail::message_by_remote(tx, account_id, &remote)?
                                && let Some(thread) = mail::detach_from_folder(tx, folder_id, id)?
                            {
                                threads.insert(thread);
                            }
                        }
                    }
                }
                mail::set_folder_sync_state(tx, folder_id, &state)?;
                let mut folders = mail::refresh_threads(tx, &threads)?;
                folders.insert(folder_id);
                mail::refresh_folder_counts(tx, &folders)?;
                Ok((threads, folders, fresh_unread, redrafted, arrivals))
            })
            .await?;
        // The user's rules run on new inbox mail before anyone is notified.
        let quiet = crate::rules::apply(&self.engine, &arrivals);
        fresh_unread.retain(|t| !quiet.contains(t));
        for id in redrafted {
            let _ = tokio::fs::remove_file(source_path(&self.engine, account_id, id)).await;
        }
        if !threads.is_empty() {
            self.engine.emit(EngineEvent::MailChanged {
                account_id,
                folder_ids: folders.into_iter().collect(),
                thread_ids: threads.into_iter().collect(),
            });
        }
        if !fresh_unread.is_empty() {
            // Notify only for conversations that are still in the inbox.
            let summaries = self.engine.db.read(|c| {
                let mut list = mail::thread_summaries(c, &fresh_unread)?;
                list.retain(|t| t.folder_ids.contains(&folder_id));
                Ok(list)
            })?;
            if !summaries.is_empty() {
                self.engine.emit(EngineEvent::NewMail {
                    account_id,
                    threads: summaries,
                });
            }
        }
        Ok(())
    }

    /// After an enumeration that replaced a lost sync state: removes local
    /// copies the server no longer has in the folder.
    async fn prune_folder(&self, folder: &FolderRow) -> Result<()> {
        let mut on_server: HashSet<String> = HashSet::new();
        let mut offset = 0u32;
        loop {
            let options = FindMessagesOptions {
                offset,
                page_size: ews::MAX_FIND_PAGE_SIZE,
                newest_first: true,
                received_since: None,
                properties: Some(Vec::new()),
            };
            let page = self
                .ews
                .client
                .find_messages(FolderId::new(folder.remote_id.clone()), &options)
                .await
                .map_err(|e| self.err(e))?;
            let returned = page.items.len() as u32;
            on_server.extend(page.items.into_iter().map(|m| m.id.id));
            match page.next_offset {
                Some(next) if returned > 0 && next > offset => offset = next,
                _ => break,
            }
        }
        let account_id = self.id();
        let folder_id = folder.id;
        let (threads, folders) = self
            .engine
            .db
            .write_async(move |tx| {
                let pending = pending_changes(tx, account_id)?;
                let mut threads = BTreeSet::new();
                for (id, thread, remote) in mail::folder_remote_ids(tx, folder_id)? {
                    if !on_server.contains(&remote)
                        && !pending.moving.contains(&id)
                        && mail::detach_from_folder(tx, folder_id, id)?.is_some()
                    {
                        threads.insert(thread);
                    }
                }
                let mut folders = mail::refresh_threads(tx, &threads)?;
                folders.insert(folder_id);
                mail::refresh_folder_counts(tx, &folders)?;
                Ok((threads, folders))
            })
            .await?;
        if !threads.is_empty() {
            self.engine.emit(EngineEvent::MailChanged {
                account_id,
                folder_ids: folders.into_iter().collect(),
                thread_ids: threads.into_iter().collect(),
            });
        }
        Ok(())
    }

    /// After every folder was read: deletes messages that left all of them
    /// (deleted, or moved somewhere we do not synchronize) and hides labels
    /// nothing carries.
    async fn tidy(&self) -> Result<()> {
        let account_id = self.id();
        let (orphans, folders, labels_changed) = self
            .engine
            .db
            .write_async(move |tx| {
                let orphans = mail::delete_orphan_messages(tx, account_id)?;
                let threads: BTreeSet<i64> = orphans.iter().map(|(_, t)| *t).collect();
                let folders = mail::refresh_threads(tx, &threads)?;
                mail::refresh_folder_counts(tx, &folders)?;
                let labels_changed = mail::hide_unused_labels(tx, account_id)?;
                Ok((orphans, folders, labels_changed))
            })
            .await?;
        for (id, _) in &orphans {
            let _ = tokio::fs::remove_file(source_path(&self.engine, account_id, *id)).await;
        }
        if !orphans.is_empty() {
            let mut threads: Vec<i64> = orphans.iter().map(|(_, t)| *t).collect();
            threads.sort_unstable();
            threads.dedup();
            self.engine.emit(EngineEvent::MailChanged {
                account_id,
                folder_ids: folders.into_iter().collect(),
                thread_ids: threads,
            });
        }
        if labels_changed {
            self.engine.emit(EngineEvent::FoldersChanged { account_id });
        }
        Ok(())
    }

    // ----- bodies ------------------------------------------------------------

    /// Downloads bodies of recent messages without attachments (messages with
    /// attachments are fetched when opened). Returns true if more are waiting.
    async fn prefetch_bodies(&mut self, limit: i64) -> Result<bool> {
        let days = self.account.config.body_sync_days.unwrap_or(365);
        let since = now_ms() - i64::from(days) * 86_400_000;
        let account_id = self.id();
        let mut ids = self
            .engine
            .db
            .read(|c| mail::messages_without_body(c, account_id, Some(since), true, limit + 1))?;
        if ids.is_empty() {
            return Ok(false);
        }
        let more = ids.len() as i64 > limit;
        ids.truncate(limit as usize);
        let threads = download_bodies(&self.engine, &self.ews, account_id, &ids, false).await?;
        if !threads.is_empty() {
            let set = threads.clone();
            let folders = self
                .engine
                .db
                .write_async(move |tx| mail::refresh_threads(tx, &set))
                .await?;
            self.engine.emit(EngineEvent::MailChanged {
                account_id,
                folder_ids: folders.into_iter().collect(),
                thread_ids: threads.into_iter().collect(),
            });
        }
        Ok(more)
    }

    // ----- pending operations ------------------------------------------------

    async fn process_ops(&mut self) -> Result<()> {
        let account_id = self.id();
        loop {
            let ops = self.engine.db.read(|c| misc::due_ops(c, account_id))?;
            if ops.is_empty() {
                return Ok(());
            }
            for op in ops {
                match self.apply_op(&op.op).await {
                    Ok(()) => {
                        self.engine.db.write(|tx| misc::complete_op(tx, op.id))?;
                    }
                    Err(e) if e.is_transient() || e.is_auth() => return Err(e),
                    Err(e) => {
                        tracing::warn!(
                            account_id,
                            "operation failed ({} attempts): {e}",
                            op.attempts + 1
                        );
                        if op.attempts >= 4 {
                            self.engine.db.write(|tx| misc::complete_op(tx, op.id))?;
                            // Give up: let the server's state replace our
                            // optimistic change.
                            self.restore_from_server(&op.op).await?;
                        } else {
                            let delay = 5_000 * (1 << op.attempts.min(6));
                            self.engine
                                .db
                                .write(|tx| misc::fail_op(tx, op.id, &e.to_string(), delay))?;
                        }
                    }
                }
            }
        }
    }

    async fn apply_op(&mut self, op: &PendingOp) -> Result<()> {
        match op {
            PendingOp::Flags {
                message_ids,
                add,
                remove,
            } => self.apply_flags(message_ids, add, remove).await,
            PendingOp::Labels {
                message_ids,
                add,
                remove,
            } => self.apply_categories(message_ids, add, remove).await,
            PendingOp::Move { items, dest_folder } => self.apply_move(items, dest_folder).await,
            PendingOp::Expunge { items } => self.apply_delete(items).await,
            PendingOp::SaveDraft { draft_id } => self.save_draft(*draft_id).await,
            PendingOp::DeleteDraft { remote_id, .. } => match remote_id {
                Some(id) => self.delete_remote_draft(id).await,
                None => Ok(()),
            },
            PendingOp::AppendSent { outbox_id } => {
                // `SendAndSaveCopy` already filed the message in Sent Items.
                let id = *outbox_id;
                self.engine
                    .db
                    .write(|tx| misc::outbox_delete(tx, id).map(|_| ()))
            }
        }
    }

    /// Fails with the first error other than "the item no longer exists";
    /// returns the new ids items reported.
    fn check<T>(
        &self,
        results: Vec<std::result::Result<T, ews::ResponseError>>,
    ) -> Result<Vec<Option<T>>> {
        let mut out = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(value) => out.push(Some(value)),
                Err(e) if e.code.is_not_found() => out.push(None),
                Err(e) => return Err(self.err(EwsError::from(e))),
            }
        }
        Ok(out)
    }

    /// Current remote ids of messages (ids change when messages move).
    fn current_remote_ids(&self, message_ids: &[i64]) -> Result<Vec<(i64, String)>> {
        self.engine.db.read(|c| mail::remote_ids(c, message_ids))
    }

    async fn apply_flags(
        &self,
        message_ids: &[i64],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        let targets = self.current_remote_ids(message_ids)?;
        if targets.is_empty() {
            return Ok(());
        }
        let mut update = MessageUpdate::default();
        let mut verb = None;
        for flag in add {
            match flag.to_ascii_lowercase().as_str() {
                "\\seen" => update.is_read = Some(true),
                "\\flagged" => update.flag = Some(FlagStatus::Flagged),
                "\\answered" => verb = Some(false),
                "$forwarded" | "forwarded" => verb = Some(true),
                _ => {}
            }
        }
        for flag in remove {
            match flag.to_ascii_lowercase().as_str() {
                "\\seen" => update.is_read = Some(false),
                "\\flagged" => update.flag = Some(FlagStatus::NotFlagged),
                _ => {}
            }
        }
        if update != MessageUpdate::default() {
            let updates: Vec<(ItemId, MessageUpdate)> = targets
                .iter()
                .map(|(_, remote)| (ItemId::new(remote.clone()), update.clone()))
                .collect();
            let results = self
                .ews
                .client
                .update_messages(&updates)
                .await
                .map_err(|e| self.err(e))?;
            self.check(results)?;
        }
        if let Some(forwarded) = verb {
            self.mark_last_verb(&targets, forwarded).await;
        }
        Ok(())
    }

    /// Records a reply or forward the way Outlook does. Best effort:
    /// failures are only logged.
    async fn mark_last_verb(&self, targets: &[(i64, String)], forwarded: bool) {
        let ids: Vec<String> = targets.iter().map(|(_, r)| r.clone()).collect();
        let request = mapping::last_verb_request(&ids, forwarded, chrono::Utc::now());
        match self.ews.client.execute_raw(&request, false).await {
            Ok(response) => {
                if let Some(code) = mapping::raw_error(&response) {
                    tracing::debug!("cannot mark messages as replied/forwarded: {code}");
                }
            }
            Err(e) => tracing::debug!("cannot mark messages as replied/forwarded: {e}"),
        }
    }

    async fn apply_categories(
        &self,
        message_ids: &[i64],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        let targets = self.current_remote_ids(message_ids)?;
        if targets.is_empty() {
            return Ok(());
        }
        // An update replaces the whole list: start from the server's.
        let ids: Vec<ItemId> = targets
            .iter()
            .map(|(_, r)| ItemId::new(r.clone()))
            .collect();
        let current = self
            .ews
            .client
            .get_messages(&ids, &GetMessagesOptions::default())
            .await
            .map_err(|e| self.err(e))?;
        let mut updates = Vec::new();
        for (id, message) in ids.into_iter().zip(self.check(current)?) {
            let Some(message) = message else { continue };
            let mut categories = message.categories;
            categories.retain(|c| !remove.iter().any(|r| r.eq_ignore_ascii_case(c)));
            for a in add {
                if !categories.iter().any(|c| c.eq_ignore_ascii_case(a)) {
                    categories.push(a.clone());
                }
            }
            updates.push((
                id,
                MessageUpdate {
                    categories: Some(categories),
                    ..MessageUpdate::default()
                },
            ));
        }
        if updates.is_empty() {
            return Ok(());
        }
        let results = self
            .ews
            .client
            .update_messages(&updates)
            .await
            .map_err(|e| self.err(e))?;
        self.check(results).map(drop)
    }

    async fn apply_move(&self, items: &[OpItem], dest: &str) -> Result<()> {
        let message_ids: Vec<i64> = items.iter().map(|i| i.message_id).collect();
        let current: HashMap<i64, String> =
            self.current_remote_ids(&message_ids)?.into_iter().collect();
        let mut moving: Vec<(i64, String)> = Vec::new();
        for item in items.iter().filter(|i| i.folder != dest) {
            let remote = current
                .get(&item.message_id)
                .cloned()
                .or_else(|| item.remote_id.clone());
            if let Some(remote) = remote
                && !moving.iter().any(|(_, r)| *r == remote)
            {
                moving.push((item.message_id, remote));
            }
        }
        if moving.is_empty() {
            return Ok(());
        }
        let ids: Vec<ItemId> = moving.iter().map(|(_, r)| ItemId::new(r.clone())).collect();
        let results = self
            .ews
            .client
            .move_items(&ids, FolderId::new(dest.to_string()))
            .await
            .map_err(|e| self.err(e))?;
        let mut renamed = Vec::new();
        let mut failure = None;
        for ((message_id, old), result) in moving.into_iter().zip(results) {
            match result {
                // A moved item gets a new id.
                Ok(Some(new)) if new.id != old => renamed.push((message_id, new.id)),
                Ok(_) => {}
                Err(e) if e.code.is_not_found() => {}
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        if !renamed.is_empty() {
            self.engine.db.write(|tx| {
                for (id, remote) in &renamed {
                    mail::set_remote_id(tx, *id, remote)?;
                }
                Ok(())
            })?;
        }
        match failure {
            Some(e) => Err(self.err(EwsError::from(e))),
            None => Ok(()),
        }
    }

    /// Deletes messages for good (from Deleted Items or Junk). Exchange keeps
    /// them in Recoverable Items for its retention period.
    async fn apply_delete(&self, items: &[OpItem]) -> Result<()> {
        let message_ids: Vec<i64> = items.iter().map(|i| i.message_id).collect();
        let current: HashMap<i64, String> =
            self.current_remote_ids(&message_ids)?.into_iter().collect();
        let mut remotes: Vec<String> = items
            .iter()
            .filter_map(|i| {
                current
                    .get(&i.message_id)
                    .cloned()
                    .or_else(|| i.remote_id.clone())
            })
            .collect();
        remotes.sort_unstable();
        remotes.dedup();
        if remotes.is_empty() {
            return Ok(());
        }
        let ids: Vec<ItemId> = remotes.into_iter().map(ItemId::new).collect();
        let results = self
            .ews
            .client
            .delete_items(&ids, ews::DeleteType::SoftDelete)
            .await
            .map_err(|e| self.err(e))?;
        self.check(results).map(drop)
    }

    /// Uploads a draft, replacing its previous server copy.
    async fn save_draft(&self, draft_id: i64) -> Result<()> {
        let Ok(draft) = self.engine.db.read(|c| misc::draft_get(c, draft_id)) else {
            return Ok(()); // already sent or discarded
        };
        let rendered = crate::sync::render_draft(&self.engine, &draft.data, true).await?;
        // Keep Bcc recipients on the server copy too.
        let mime = super::graph::with_bcc_header(&rendered.mime, &rendered.recipients);
        let created = self
            .ews
            .client
            .save_draft_mime(&mime)
            .await
            .map_err(|e| self.err(e))?;
        let kept = self.engine.db.write(|tx| {
            // The draft may have been sent or discarded during the upload.
            if misc::draft_get(tx, draft_id).is_err() {
                return Ok(false);
            }
            misc::draft_set_remote(tx, draft_id, None, Some(&created.id))?;
            Ok(true)
        })?;
        let stale = if kept {
            draft.remote_id
        } else {
            Some(created.id)
        };
        if let Some(old) = stale
            && let Err(e) = self.delete_remote_draft(&old).await
        {
            tracing::debug!("deleting the old draft failed, retrying later: {e}");
            let account_id = self.id();
            self.engine.db.write(|tx| {
                let op = PendingOp::DeleteDraft {
                    remote_uid: None,
                    remote_id: Some(old.clone()),
                };
                misc::queue_op(tx, account_id, &op, 30_000).map(|_| ())
            })?;
        }
        Ok(())
    }

    async fn delete_remote_draft(&self, remote_id: &str) -> Result<()> {
        let results = self
            .ews
            .client
            .delete_items(
                &[ItemId::new(remote_id.to_string())],
                ews::DeleteType::SoftDelete,
            )
            .await
            .map_err(|e| self.err(e))?;
        self.check(results).map(drop)
    }

    /// Re-reads the messages of an operation that had to be dropped, so the
    /// server's state replaces our optimistic local change.
    async fn restore_from_server(&mut self, op: &PendingOp) -> Result<()> {
        self.last_full = None;
        let remotes: Vec<String> = match op {
            PendingOp::Flags { message_ids, .. } | PendingOp::Labels { message_ids, .. } => self
                .current_remote_ids(message_ids)?
                .into_iter()
                .map(|(_, r)| r)
                .collect(),
            PendingOp::Move { items, .. } | PendingOp::Expunge { items } => {
                let ids: Vec<i64> = items.iter().map(|i| i.message_id).collect();
                let current: HashMap<i64, String> =
                    self.current_remote_ids(&ids)?.into_iter().collect();
                items
                    .iter()
                    .filter_map(|i| {
                        current
                            .get(&i.message_id)
                            .cloned()
                            .or_else(|| i.remote_id.clone())
                    })
                    .collect()
            }
            _ => return Ok(()),
        };
        for message in self.fetch_headers(&remotes).await? {
            let folder = message
                .parent_folder_id
                .as_ref()
                .and_then(|p| self.folders.iter().find(|f| f.remote_id == p.id))
                .cloned();
            if let Some(folder) = folder {
                let folder = self.engine.db.read(|c| mail::get_folder(c, folder.id))?;
                let state = folder.sync_state.clone();
                self.store(&folder, vec![Incoming::full(&message)], &state, false)
                    .await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bodies, attachments and sending (shared by the worker and user actions)

/// Downloads the MIME source of messages and stores their bodies exactly like
/// IMAP full-message fetches. With `keep_source`, the source of messages with
/// attachments is cached for the attachment downloads that usually follow.
/// Returns the touched threads.
async fn download_bodies(
    engine: &Engine,
    ews: &EwsMail,
    account_id: i64,
    message_ids: &[i64],
    keep_source: bool,
) -> Result<BTreeSet<i64>> {
    use futures::StreamExt;
    let targets = engine.db.read(|c| mail::remote_ids(c, message_ids))?;
    let options = GetMessagesOptions {
        include_mime_content: true,
        ..GetMessagesOptions::default()
    };
    let chunks: Vec<Vec<(i64, String)>> = targets
        .chunks(ews::GET_ITEM_MIME_BATCH_SIZE)
        .map(<[_]>::to_vec)
        .collect();
    let fetched: Vec<(Vec<(i64, String)>, std::result::Result<Vec<_>, EwsError>)> =
        futures::stream::iter(chunks)
            .map(|chunk| {
                let options = &options;
                async move {
                    let ids: Vec<ItemId> =
                        chunk.iter().map(|(_, r)| ItemId::new(r.clone())).collect();
                    let result = ews.client.get_messages(&ids, options).await;
                    (chunk, result)
                }
            })
            .buffer_unordered(DOWNLOADS_IN_FLIGHT)
            .collect()
            .await;
    let mut sources = Vec::new();
    let mut failed = Vec::new();
    let mut error = None;
    for (chunk, result) in fetched {
        match result {
            Ok(results) => {
                for ((id, _), result) in chunk.into_iter().zip(results) {
                    match result {
                        Ok(m) => match m.mime_content {
                            Some(raw) if !raw.is_empty() => sources.push((id, raw)),
                            _ => failed.push(id),
                        },
                        Err(e) if e.code.is_transient() => {
                            error.get_or_insert(ews.err(EwsError::from(e)));
                        }
                        Err(e) => {
                            tracing::debug!(message_id = id, "cannot download message: {e}");
                            failed.push(id);
                        }
                    }
                }
            }
            Err(e) => {
                let e = ews.err(e);
                if e.is_transient() || e.is_auth() {
                    error.get_or_insert(e);
                } else {
                    tracing::debug!("cannot download messages: {e}");
                    failed.extend(chunk.into_iter().map(|(id, _)| id));
                }
            }
        }
    }
    let (threads, keep) = engine
        .db
        .write_async(move |tx| {
            let mut threads = BTreeSet::new();
            let mut keep = Vec::new();
            for (id, raw) in sources {
                match mime::parse_full(&raw) {
                    Some((meta, body)) => {
                        mail::store_body(
                            tx,
                            id,
                            body.html.as_deref(),
                            body.text.as_deref(),
                            &body.snippet,
                            Some(&body.attachments),
                        )?;
                        mail::update_from_source(tx, id, &meta, raw.len() as i64)?;
                        if keep_source && !body.attachments.is_empty() {
                            keep.push((id, raw));
                        }
                    }
                    None => mail::mark_body_failed(tx, id)?,
                }
                let thread: Option<i64> = rusqlite::OptionalExtension::optional(tx.query_row(
                    "SELECT thread_id FROM messages WHERE id = ?",
                    [id],
                    |r| r.get(0),
                ))?;
                threads.extend(thread);
            }
            for id in failed {
                mail::mark_body_failed(tx, id)?;
            }
            Ok((threads, keep))
        })
        .await?;
    for (id, raw) in keep {
        save_source(&source_path(engine, account_id, id), &raw).await;
    }
    match error {
        Some(e) => Err(e),
        None => Ok(threads),
    }
}

/// The MIME source of a message, from the cache or the server.
async fn source(
    engine: &Engine,
    ews: &EwsMail,
    account_id: i64,
    message_id: i64,
    keep: bool,
) -> Result<Vec<u8>> {
    let path = source_path(engine, account_id, message_id);
    let lock = source_lock(&path);
    let _guard = lock.lock().await;
    if let Ok(raw) = tokio::fs::read(&path).await {
        return Ok(raw);
    }
    let (_, remote) = engine
        .db
        .read(|c| mail::remote_ids(c, &[message_id]))?
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound("message is not on the server yet".into()))?;
    let raw = ews
        .client
        .get_mime_content(&ItemId::new(remote))
        .await
        .map_err(|e| ews.err(e))?;
    if keep {
        save_source(&path, &raw).await;
    }
    Ok(raw)
}

/// Downloads message bodies for the reader.
pub(crate) async fn fetch_bodies(
    engine: &Engine,
    account: &AccountRow,
    message_ids: &[i64],
) -> Result<()> {
    let ews = EwsMail::for_account(engine, account).await?;
    download_bodies(engine, &ews, account.id, message_ids, true)
        .await
        .map(drop)
}

/// The decoded content of an attachment (`part_id` as stored from the
/// message source).
pub(crate) async fn attachment_data(
    engine: &Engine,
    account: &AccountRow,
    message_id: i64,
    part_id: &str,
) -> Result<Vec<u8>> {
    let ews = EwsMail::for_account(engine, account).await?;
    attachment_with(engine, &ews, account.id, message_id, part_id).await
}

async fn attachment_with(
    engine: &Engine,
    ews: &EwsMail,
    account_id: i64,
    message_id: i64,
    part_id: &str,
) -> Result<Vec<u8>> {
    let raw = source(engine, ews, account_id, message_id, true).await?;
    mime::extract_part(&raw, part_id).ok_or_else(|| Error::NotFound("attachment".into()))
}

/// The raw RFC 5322 source of a message (view source / export).
pub(crate) async fn message_source(
    engine: &Engine,
    account: &AccountRow,
    message_id: i64,
) -> Result<Vec<u8>> {
    let ews = EwsMail::for_account(engine, account).await?;
    source(engine, &ews, account.id, message_id, false).await
}

/// Sends a rendered message; Exchange files a copy in Sent Items.
pub(crate) async fn send(
    engine: &Engine,
    account: &AccountRow,
    mime: &[u8],
    recipients: &[String],
) -> Result<()> {
    let ews = EwsMail::for_account(engine, account).await?;
    send_with(&ews, mime, recipients).await
}

async fn send_with(ews: &EwsMail, mime: &[u8], recipients: &[String]) -> Result<()> {
    // Exchange takes the recipients from the headers; Bcc recipients only
    // exist in our envelope, so they travel in a `Bcc:` header Exchange
    // strips before delivery.
    let mime = super::graph::with_bcc_header(mime, recipients);
    ews.client.send_mime(&mime).await.map_err(|e| ews.err(e))
}
