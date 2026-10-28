//! Microsoft Graph mail synchronization (Outlook.com and Microsoft 365).
//!
//! * Folders come from the mail folder tree, with roles for the well-known
//!   folders; Outlook categories become colored labels.
//! * Messages are tracked per folder with delta queries. A folder's first
//!   round lists it newest first, a few pages per cycle, so the inbox fills
//!   quickly while older history keeps arriving in the background; later
//!   rounds return only changes. Item ids are immutable, so a message keeps
//!   its id (and its local row) when it moves between folders.
//! * Bodies are downloaded lazily as MIME (`$value`) and parsed exactly like
//!   IMAP messages, so HTML, inline images and attachments behave the same.
//! * Graph offers a desktop client no push channel: the inbox, sent items and
//!   drafts are polled often, every other folder every few minutes.
//! * Local changes are replayed from the pending-operation queue, batched
//!   through `$batch`; messages are sent as MIME through `sendMail`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use envelope_graph::{
    BatchRequest, DeltaCursor, FlagStatus, GraphClient, GraphError, GraphRequest,
    InferenceClassification, MailFolder, Message, MessageDeltaOptions, MessageListOptions,
    MessageUpdate, OutlookCategory, Recipient,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::imap::folder_priority;
use super::{AccountTokens, TokenFailure};
use crate::db::mail::{self, AccountRow, FolderRow, FolderSyncState, FolderUpsert, MessageMeta};
use crate::db::misc::{self, OpItem, PendingOp};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mime;
use crate::model::*;

/// Messages per delta page.
const PAGE_SIZE: u32 = 200;
/// Delta pages fetched per folder and cycle before other work gets a turn
/// (about as many messages as one IMAP backfill step).
const PAGES_PER_CYCLE: usize = 15;
/// How often the inbox, sent items and drafts are checked for changes.
const POLL_INTERVAL: Duration = Duration::from_secs(45);
/// How often every folder is checked.
const FULL_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// A wake-up (manual refresh, local change) checks every folder at most this
/// often.
const MIN_FULL_SYNC_GAP: Duration = Duration::from_secs(60);
/// Wake-ups in quick succession poll the frequent folders at most this often.
const MIN_POLL_GAP: Duration = Duration::from_secs(5);
/// How often the folder tree and categories are refreshed.
const FOLDER_LIST_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Concurrent downloads (Outlook serves four requests per mailbox at a time).
const DOWNLOADS_IN_FLIGHT: usize = 3;
/// Newest inbox messages stored while the inbox's first listing still runs.
const PROBE_SIZE: u32 = 25;
/// Remote id prefix of the label folders that stand for Outlook categories.
pub(crate) const CATEGORY_PREFIX: &str = "category:";

// ---------------------------------------------------------------------------
// Client and errors

/// A Graph client for one account's mailbox, plus what is needed to turn its
/// failures into engine errors.
#[derive(Clone)]
pub(crate) struct GraphMail {
    client: GraphClient,
    tokens: Option<Arc<AccountTokens>>,
    throttled_until: Arc<parking_lot::Mutex<Option<Instant>>>,
}

impl GraphMail {
    /// A client for an account, authenticating with its stored OAuth tokens.
    pub(crate) fn for_account(engine: &Engine, account_id: i64) -> Result<GraphMail> {
        let tokens = AccountTokens::new(engine, account_id);
        let source: Arc<dyn envelope_graph::TokenSource> = tokens.clone();
        let client = GraphClient::builder(source)
            .http_client(shared_http())
            .build()
            .map_err(|e| Error::Other(e.to_string()))?;
        Ok(GraphMail {
            client,
            tokens: Some(tokens),
            throttled_until: Arc::default(),
        })
    }

    #[cfg(test)]
    fn with_client(client: GraphClient) -> GraphMail {
        GraphMail {
            client,
            tokens: None,
            throttled_until: Arc::default(),
        }
    }

    /// Converts a Graph failure, remembering how long the server asked us to
    /// back off.
    fn err(&self, e: GraphError) -> Error {
        if let Some(after) = e.retry_after() {
            *self.throttled_until.lock() = Some(Instant::now() + after);
        }
        graph_error(e, self.tokens.as_ref().and_then(|t| t.last_failure()))
    }

    /// Remaining time the server asked us to stay away.
    fn throttle_delay(&self) -> Duration {
        let until = *self.throttled_until.lock();
        until
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    }
}

/// One HTTP connection pool for every Graph mail client.
fn shared_http() -> reqwest::Client {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

/// Maps a Graph failure onto engine errors: rejected sign-ins become
/// [`Error::Auth`] (the UI asks to sign in again), unreachable or overloaded
/// servers [`Error::Network`] (offline, retried), everything else a readable
/// error. `token_failure` tells why no access token could be obtained when
/// Graph was never reached.
fn graph_error(e: GraphError, token_failure: Option<TokenFailure>) -> Error {
    match e {
        GraphError::Auth(message) => match token_failure {
            Some(TokenFailure::Network) => Error::Network(message),
            Some(TokenFailure::Other) => Error::Other(message),
            Some(TokenFailure::Auth) | None => Error::Auth(message),
        },
        GraphError::Transport(e) => Error::Network(e.to_string()),
        GraphError::Api(api) => {
            let text = match api.message.trim() {
                "" => format!("Microsoft Graph error {} ({})", api.status, api.code),
                message => message.to_string(),
            };
            let unsupported = [
                "MailboxNotEnabledForRESTAPI",
                "MailboxNotSupportedForRESTAPI",
            ]
            .iter()
            .any(|c| api.code.eq_ignore_ascii_case(c));
            if unsupported {
                return Error::Other(
                    "This mailbox cannot be reached through Microsoft Graph \
                     (it may be hosted on an on-premises Exchange server)"
                        .into(),
                );
            }
            match api.status {
                401 => Error::Auth(format!(
                    "Microsoft rejected the sign-in, please sign in again ({text})"
                )),
                0 | 429 | 500..=599 => Error::Network(text),
                404 => Error::NotFound(text),
                413 => Error::Invalid(
                    "The message is too large: Microsoft 365 accepts about 3 MB \
                     (attachments included) from Envelope"
                        .into(),
                ),
                _ => Error::Other(text),
            }
        }
        other => Error::Other(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Mapping Graph data

/// Delta bookkeeping of a folder, kept in [`FolderSyncState::cursor`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DeltaState {
    /// Where the current round stands; `None` starts a full listing.
    delta: Option<DeltaCursor>,
    /// The current round lists the whole folder (first sync, or after an
    /// expired delta link) rather than reporting changes.
    full: bool,
    /// An expired delta link forced a full round: once it completes, local
    /// copies the server no longer has are removed.
    prune: bool,
}

impl DeltaState {
    fn load(state: &FolderSyncState) -> DeltaState {
        state
            .cursor
            .as_deref()
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or_default()
    }

    fn store(&self, state: &mut FolderSyncState) {
        state.cursor = serde_json::to_string(self).ok();
    }
}

fn address(r: &Recipient) -> Option<Address> {
    let email = r.address()?.trim();
    if email.is_empty() {
        return None;
    }
    Some(Address::new(r.name().map(|n| n.trim().to_string()), email))
}

fn addresses(list: &[Recipient]) -> Vec<Address> {
    list.iter().filter_map(address).collect()
}

fn clean_message_id(id: &str) -> Option<String> {
    let id = id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// What a listed message tells about itself. Threading and mailing-list
/// headers are filled in from the MIME source when the body is downloaded;
/// conversations are grouped by Graph's conversation id meanwhile.
fn message_meta(m: &Message) -> MessageMeta {
    let mut flags = 0;
    if m.is_read {
        flags |= Flags::SEEN;
    }
    if m.is_flagged() {
        flags |= Flags::FLAGGED;
    }
    if m.is_draft {
        flags |= Flags::DRAFT;
    }
    let mut keywords = Vec::new();
    if m.inference_classification == Some(InferenceClassification::Other) {
        keywords.push(mail::OTHER_INBOX_KEYWORD.to_string());
    }
    MessageMeta {
        remote_id: Some(m.id.clone()),
        remote_thread_id: m.conversation_id.clone().filter(|c| !c.is_empty()),
        message_id: m.internet_message_id.as_deref().and_then(clean_message_id),
        subject: m.subject.as_deref().unwrap_or("").trim().to_string(),
        from: m
            .from
            .as_ref()
            .and_then(address)
            .or_else(|| m.sender.as_ref().and_then(address)),
        to: addresses(&m.to_recipients),
        cc: addresses(&m.cc_recipients),
        bcc: addresses(&m.bcc_recipients),
        reply_to: addresses(&m.reply_to),
        date: m
            .received_date_time
            .or(m.sent_date_time)
            .map(|d| d.timestamp_millis())
            .unwrap_or_else(now_ms),
        flags,
        keywords,
        has_attachments: m.has_attachments,
        snippet: m
            .body_preview
            .as_deref()
            .map(mime::make_snippet)
            .filter(|s| !s.is_empty()),
        ..Default::default()
    }
}

/// Our role for a well-known Graph folder. The outbox is not synchronized:
/// messages only pass through it while Exchange sends them.
fn folder_role(role: envelope_graph::FolderRole) -> Option<FolderRole> {
    use envelope_graph::FolderRole as Graph;
    match role {
        Graph::Inbox => Some(FolderRole::Inbox),
        Graph::Drafts => Some(FolderRole::Drafts),
        Graph::SentItems => Some(FolderRole::Sent),
        Graph::DeletedItems => Some(FolderRole::Trash),
        Graph::JunkEmail => Some(FolderRole::Junk),
        Graph::Archive => Some(FolderRole::Archive),
        Graph::Outbox => None,
    }
}

/// Folder rows for the mailbox's folder tree (parents first, as
/// [`GraphClient::mail_folder_tree`] returns it) and its master categories.
fn folder_upserts(tree: &[MailFolder], categories: &[OutlookCategory]) -> Vec<FolderUpsert> {
    let synced: Vec<&MailFolder> = tree
        .iter()
        .filter(|f| !f.is_hidden && f.role != Some(envelope_graph::FolderRole::Outbox))
        .collect();
    let known: HashSet<&str> = synced.iter().map(|f| f.id.as_str()).collect();
    let mut paths: HashMap<&str, String> = HashMap::new();
    for f in &synced {
        let name = f.display_name.trim();
        let path = match f.parent_id.as_deref().and_then(|p| paths.get(p)) {
            Some(parent) => format!("{parent}/{name}"),
            None => name.to_string(),
        };
        paths.insert(f.id.as_str(), path);
    }
    let mut roles: Vec<Option<FolderRole>> = synced
        .iter()
        .map(|f| f.role.and_then(folder_role))
        .collect();
    if !roles.contains(&Some(FolderRole::Archive)) {
        // Mailboxes without the well-known archive folder: archive into a
        // top-level folder called "Archive", like IMAP accounts do.
        let top_level_archive = synced.iter().position(|f| {
            f.display_name.trim().eq_ignore_ascii_case("archive")
                && f.parent_id.as_deref().is_none_or(|p| !known.contains(p))
        });
        if let Some(i) = top_level_archive {
            roles[i] = Some(FolderRole::Archive);
        }
    }
    let mut out: Vec<FolderUpsert> = synced
        .iter()
        .zip(roles)
        .map(|(f, role)| FolderUpsert {
            remote_id: f.id.clone(),
            name: f.display_name.trim().to_string(),
            path: paths.get(f.id.as_str()).cloned().unwrap_or_default(),
            parent_remote_id: f.parent_id.clone().filter(|p| known.contains(p.as_str())),
            role,
            kind: "folder".into(),
            delimiter: Some("/".into()),
            selectable: true,
            sort_order: folder_priority(role) as i64,
            color: None,
        })
        .collect();
    let mut seen = HashSet::new();
    for c in categories {
        let name = c.display_name.trim();
        if name.is_empty() || !seen.insert(name.to_lowercase()) {
            continue;
        }
        out.push(category_upsert(name, c.color.hex().map(str::to_string)));
    }
    out
}

/// A label folder for an Outlook category. Categories are applied rather than
/// moved into, so they are not offered as move targets.
fn category_upsert(name: &str, color: Option<String>) -> FolderUpsert {
    FolderUpsert {
        remote_id: format!("{CATEGORY_PREFIX}{name}"),
        name: name.to_string(),
        path: name.to_string(),
        parent_remote_id: None,
        role: None,
        kind: "label".into(),
        delimiter: None,
        selectable: false,
        sort_order: 20,
        color,
    }
}

/// The Outlook category a label folder stands for.
pub(crate) fn category_name(folder: &FolderRow) -> Option<&str> {
    folder.remote_id.strip_prefix(CATEGORY_PREFIX)
}

/// Graph takes the recipients of a MIME submission from its headers, but Bcc
/// recipients only exist in our envelope: name them in a `Bcc:` header,
/// which Exchange delivers to and strips before delivery.
fn with_bcc_header<'a>(mime: &'a [u8], recipients: &[String]) -> Cow<'a, [u8]> {
    let end = header_end(mime);
    let meta = mime::meta_from_headers(&mime[..end]);
    let listed: HashSet<String> = meta
        .to
        .iter()
        .chain(&meta.cc)
        .chain(&meta.bcc)
        .map(|a| a.email.trim().to_lowercase())
        .collect();
    let mut hidden: Vec<&str> = Vec::new();
    for r in recipients.iter().map(|r| r.trim()) {
        if !r.is_empty()
            && !listed.contains(&r.to_lowercase())
            && !hidden.iter().any(|h| h.eq_ignore_ascii_case(r))
        {
            hidden.push(r);
        }
    }
    if hidden.is_empty() {
        return Cow::Borrowed(mime);
    }
    let mut out = format!("Bcc: {}\r\n", hidden.join(",\r\n ")).into_bytes();
    out.extend_from_slice(mime);
    Cow::Owned(out)
}

/// Length of the header block of a message, including the blank line.
fn header_end(mime: &[u8]) -> usize {
    mime.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| mime.windows(2).position(|w| w == b"\n\n").map(|p| p + 2))
        .unwrap_or(mime.len())
}

// ---------------------------------------------------------------------------
// The sync worker

/// Runs the sync loop of a Microsoft account until stopped.
pub async fn run(engine: Engine, account_id: i64, wake: Arc<Notify>, stop: CancellationToken) {
    match GraphMail::for_account(&engine, account_id) {
        Ok(graph) => run_with(engine, account_id, graph, wake, stop).await,
        Err(e) => engine.set_sync_state(
            account_id,
            SyncState::Error {
                message: e.to_string(),
            },
        ),
    }
}

async fn run_with(
    engine: Engine,
    account_id: i64,
    graph: GraphMail,
    wake: Arc<Notify>,
    stop: CancellationToken,
) {
    let mut backoff = Duration::from_secs(2);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let Ok(account) = engine.account_row(account_id) else {
            return;
        };
        engine.set_sync_state(account_id, SyncState::Connecting);
        let started = Instant::now();
        let mut session = Session::new(engine.clone(), account, graph.clone(), stop.clone());
        let result = session.run(&wake).await;
        if started.elapsed() > Duration::from_secs(120) {
            backoff = Duration::from_secs(2);
        }
        match result {
            Ok(()) => return,
            Err(e) if e.is_auth() => {
                tracing::warn!(account_id, "Microsoft sign-in failed: {e}");
                engine.set_sync_state(
                    account_id,
                    SyncState::AuthError {
                        message: e.to_string(),
                    },
                );
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30 * 60)) => {}
                }
            }
            Err(e) => {
                tracing::warn!(account_id, "Microsoft sync error: {e}");
                let state = if e.is_transient() {
                    SyncState::Offline
                } else {
                    SyncState::Error {
                        message: e.to_string(),
                    }
                };
                engine.set_sync_state(account_id, state);
                let delay = backoff.max(graph.throttle_delay());
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

struct Session {
    engine: Engine,
    account: AccountRow,
    graph: GraphMail,
    stop: CancellationToken,
    folders: Vec<FolderRow>,
    /// Lower-case category name → label folder id.
    categories: HashMap<String, i64>,
    last_full: Option<Instant>,
    last_poll: Option<Instant>,
    last_folder_list: Option<Instant>,
    /// The first pass over every folder finished.
    initial_done: bool,
    /// Folders whose listing continues in the next cycle.
    backfill: BTreeSet<i64>,
    /// Creating a missing archive folder was attempted.
    archive_checked: bool,
}

impl Session {
    fn new(engine: Engine, account: AccountRow, graph: GraphMail, stop: CancellationToken) -> Self {
        Session {
            engine,
            account,
            graph,
            stop,
            folders: Vec::new(),
            categories: HashMap::new(),
            last_full: None,
            last_poll: None,
            last_folder_list: None,
            initial_done: false,
            backfill: BTreeSet::new(),
            archive_checked: false,
        }
    }

    fn id(&self) -> i64 {
        self.account.id
    }

    fn err(&self, e: GraphError) -> Error {
        self.graph.err(e)
    }

    async fn run(&mut self, wake: &Notify) -> Result<()> {
        self.reload_folders()?;
        loop {
            if self.stop.is_cancelled() {
                return Ok(());
            }
            self.process_ops().await?;
            let full_due = self
                .last_full
                .is_none_or(|t| t.elapsed() >= FULL_SYNC_INTERVAL);
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
            } else if poll_due {
                let quick = self.quick_folders();
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

    /// Sleeps until the next poll, a due queued operation, a wake-up or stop.
    async fn wait_for_changes(&mut self, wake: &Notify) -> Result<()> {
        let since = |t: Option<Instant>| t.map(|t| t.elapsed()).unwrap_or_default();
        let mut wait = POLL_INTERVAL
            .saturating_sub(since(self.last_poll))
            .min(FULL_SYNC_INTERVAL.saturating_sub(since(self.last_full)));
        let account_id = self.id();
        if let Some(next) = self.engine.db.read(|c| misc::next_op_time(c, account_id))? {
            wait = wait.min(Duration::from_millis((next - now_ms()).max(0) as u64));
        }
        tokio::select! {
            _ = self.stop.cancelled() => {}
            _ = wake.notified() => {
                // A manual refresh or local change: look everywhere, but not
                // on every single click.
                if self.last_full.is_none_or(|t| t.elapsed() >= MIN_FULL_SYNC_GAP) {
                    self.last_full = None;
                }
            }
            _ = tokio::time::sleep(wait.max(Duration::from_millis(500))) => {}
        }
        Ok(())
    }

    // ----- folders -----------------------------------------------------------

    async fn sync_folder_list(&mut self) -> Result<()> {
        let mut tree = self
            .graph
            .client
            .mail_folder_tree()
            .await
            .map_err(|e| self.err(e))?;
        let categories = match self.graph.client.master_categories().await {
            Ok(list) => Some(list),
            Err(e) => {
                let e = self.err(e);
                if e.is_transient() || e.is_auth() {
                    return Err(e);
                }
                tracing::warn!(
                    account_id = self.id(),
                    "cannot read Outlook categories: {e}"
                );
                None
            }
        };
        let mut upserts = folder_upserts(&tree, categories.as_deref().unwrap_or_default());
        if !self.archive_checked
            && !tree.is_empty()
            && !upserts.iter().any(|u| u.role == Some(FolderRole::Archive))
        {
            // Archiving needs a destination; create the conventional folder.
            self.archive_checked = true;
            match self.graph.client.create_mail_folder(None, "Archive").await {
                Ok(folder) => {
                    tree.push(folder);
                    upserts = folder_upserts(&tree, categories.as_deref().unwrap_or_default());
                }
                Err(e) => tracing::warn!(account_id = self.id(), "cannot create Archive: {e}"),
            }
        }
        if categories.is_none() {
            // Keep the labels we know rather than dropping them.
            for f in self.folders.iter().filter(|f| f.kind == "label") {
                if let Some(name) = category_name(f) {
                    upserts.push(category_upsert(name, None));
                }
            }
        }
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

    /// Label folders for a message's categories (unknown ones are skipped).
    fn category_folders(&self, names: &[String]) -> Vec<i64> {
        let mut ids: Vec<i64> = names
            .iter()
            .filter_map(|n| self.categories.get(&n.trim().to_lowercase()).copied())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
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
    /// Returns whether listings continue in the next cycle and whether every
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

    /// Runs (part of) a delta round for one folder. Returns true when the
    /// round continues in the next cycle.
    async fn sync_folder(&mut self, folder: &FolderRow) -> Result<bool> {
        // Re-read: an earlier step may have changed the state.
        let folder = self.engine.db.read(|c| mail::get_folder(c, folder.id))?;
        let mut state = folder.sync_state.clone();
        let mut delta = DeltaState::load(&state);
        let inbox = folder.role == Some(FolderRole::Inbox);
        if inbox && delta.full && delta.delta.is_some() {
            // A long first listing of the inbox: show new arrivals meanwhile.
            self.probe(&folder).await?;
        }
        let options = MessageDeltaOptions {
            page_size: Some(PAGE_SIZE),
            ..MessageDeltaOptions::default()
        };
        let mut restarted = false;
        for _ in 0..PAGES_PER_CYCLE {
            if self.stop.is_cancelled() {
                return Ok(true);
            }
            if delta.delta.is_none() {
                delta.full = true;
            }
            let page = match self
                .graph
                .client
                .message_delta(&folder.remote_id, delta.delta.as_ref(), &options)
                .await
            {
                Ok(page) => page,
                Err(e) if e.is_resync_required() && delta.delta.is_some() && !restarted => {
                    tracing::info!(
                        account_id = self.id(),
                        "delta link of {} expired, listing the folder again",
                        folder.name
                    );
                    restarted = true;
                    delta = DeltaState {
                        delta: None,
                        full: true,
                        prune: true,
                    };
                    continue;
                }
                Err(e) => return Err(self.err(e)),
            };
            let done = page.next.is_done();
            // New-mail notifications only for changes, not for listings.
            let notify = inbox && !delta.full;
            delta.delta = Some(page.next);
            if done {
                delta.full = false;
                state.complete = true;
            }
            state.last_sync = Some(now_ms());
            delta.store(&mut state);
            self.store_page(&folder, page.items, page.removed_ids, &state, notify)
                .await?;
            if done {
                if delta.prune {
                    self.prune_folder(&folder).await?;
                    delta.prune = false;
                    delta.store(&mut state);
                    let folder_id = folder.id;
                    self.engine
                        .db
                        .write(|tx| mail::set_folder_sync_state(tx, folder_id, &state))?;
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Stores the newest messages of a folder outside the delta round.
    async fn probe(&self, folder: &FolderRow) -> Result<()> {
        let options = MessageListOptions {
            top: PROBE_SIZE,
            ..MessageListOptions::default()
        };
        let page = self
            .graph
            .client
            .list_messages(&folder.remote_id, &options)
            .await
            .map_err(|e| self.err(e))?;
        self.store_page(folder, page.items, Vec::new(), &folder.sync_state, false)
            .await
    }

    /// Stores one page of messages (created/changed and removed ones) together
    /// with the folder's new sync state.
    async fn store_page(
        &self,
        folder: &FolderRow,
        items: Vec<Message>,
        removed: Vec<String>,
        state: &FolderSyncState,
        notify: bool,
    ) -> Result<()> {
        let account_id = self.id();
        let folder_id = folder.id;
        let incoming: Vec<(MessageMeta, Vec<i64>)> = items
            .iter()
            .map(|m| (message_meta(m), self.category_folders(&m.categories)))
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
                for (mut meta, labels) in incoming {
                    let remote = meta.remote_id.clone().unwrap_or_default();
                    let stored = match mail::message_by_remote(tx, account_id, &remote)? {
                        Some((id, thread_id, old_flags)) => {
                            // Graph does not report replied/forwarded: keep ours.
                            meta.flags |= old_flags & (Flags::ANSWERED | Flags::FORWARDED);
                            mail::set_flags_and_keywords(tx, id, meta.flags, &meta.keywords)?;
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
                        None => mail::upsert_message(tx, account_id, folder_id, None, &meta)?,
                    };
                    if !pending.labeling.contains(&stored.message_id) {
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
                            misc::record_addresses(tx, std::slice::from_ref(f), false, meta.date)?;
                        }
                        if notify && meta.flags & (Flags::SEEN | Flags::DRAFT) == 0 && !from_me {
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
                for remote in &removed {
                    if let Some((id, _, _)) = mail::message_by_remote(tx, account_id, remote)?
                        && let Some(thread) = mail::detach_from_folder(tx, folder_id, id)?
                    {
                        threads.insert(thread);
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

    /// After a full listing that replaced an expired delta link: removes local
    /// copies the server no longer has in the folder.
    async fn prune_folder(&self, folder: &FolderRow) -> Result<()> {
        let options = MessageListOptions {
            top: 1000,
            select: vec!["id".into()],
            order_by: None,
            ..MessageListOptions::default()
        };
        let client = &self.graph.client;
        let mut page = client
            .list_messages(&folder.remote_id, &options)
            .await
            .map_err(|e| self.err(e))?;
        let mut on_server: HashSet<String> = HashSet::new();
        loop {
            on_server.extend(page.items.into_iter().map(|m| m.id));
            match page.next_link {
                Some(link) => {
                    page = client
                        .list_messages_next(&link)
                        .await
                        .map_err(|e| self.err(e))?;
                }
                None => break,
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

    // ----- bodies ---------------------------------------------------------------

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
        let threads = download_bodies(&self.engine, &self.graph, account_id, &ids, false).await?;
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

    // ----- pending operations --------------------------------------------------

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
                // `sendMail` already filed the message in Sent Items.
                let id = *outbox_id;
                self.engine
                    .db
                    .write(|tx| misc::outbox_delete(tx, id).map(|_| ()))
            }
        }
    }

    /// Fails with the first error other than "the message no longer exists".
    fn check<T>(&self, results: Vec<std::result::Result<T, GraphError>>) -> Result<()> {
        for result in results {
            if let Err(e) = result
                && !e.is_not_found()
            {
                return Err(self.err(e));
            }
        }
        Ok(())
    }

    async fn apply_flags(
        &self,
        message_ids: &[i64],
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        let targets = self.engine.db.read(|c| mail::remote_ids(c, message_ids))?;
        if targets.is_empty() {
            return Ok(());
        }
        let mut update = MessageUpdate::new();
        let mut verb = None;
        for flag in add {
            match flag.to_ascii_lowercase().as_str() {
                "\\seen" => update = update.read(true),
                "\\flagged" => update = update.flag(FlagStatus::Flagged),
                "\\answered" => verb = Some(false),
                "$forwarded" | "forwarded" => verb = Some(true),
                _ => {}
            }
        }
        for flag in remove {
            match flag.to_ascii_lowercase().as_str() {
                "\\seen" => update = update.read(false),
                "\\flagged" => update = update.flag(FlagStatus::NotFlagged),
                _ => {}
            }
        }
        if !update.is_empty() {
            let updates: Vec<(String, MessageUpdate)> = targets
                .iter()
                .map(|(_, remote)| (remote.clone(), update.clone()))
                .collect();
            let results = self
                .graph
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

    /// Records a reply or forward the way Outlook does (last verb executed,
    /// its time and the matching icon), so other Outlook clients show it.
    /// Best effort: failures are only logged.
    async fn mark_last_verb(&self, targets: &[(i64, String)], forwarded: bool) {
        let (verb, icon) = if forwarded { (104, 262) } else { (102, 261) };
        let body = json!({ "singleValueExtendedProperties": [
            { "id": "Integer 0x1081", "value": verb.to_string() },
            { "id": "Integer 0x1080", "value": icon.to_string() },
            { "id": "SystemTime 0x1082", "value": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true) },
        ] });
        let requests: Vec<BatchRequest> = targets
            .iter()
            .enumerate()
            .filter_map(|(i, (_, remote))| {
                let request = GraphRequest::patch(["me", "messages", remote.as_str()])
                    .json(&body)
                    .ok()?;
                Some(BatchRequest::new(i.to_string(), request))
            })
            .collect();
        match self.graph.client.batch(requests).await {
            Ok(responses) => {
                for r in responses.iter().filter(|r| !r.is_success()) {
                    tracing::debug!("cannot mark message as replied/forwarded: {:?}", r.error());
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
        let targets = self.engine.db.read(|c| mail::remote_ids(c, message_ids))?;
        if targets.is_empty() {
            return Ok(());
        }
        // An update replaces the whole list: start from the server's.
        let requests: Vec<BatchRequest> = targets
            .iter()
            .enumerate()
            .map(|(i, (_, remote))| {
                BatchRequest::new(
                    i.to_string(),
                    GraphRequest::get(["me", "messages", remote.as_str()])
                        .query("$select", "categories"),
                )
            })
            .collect();
        let responses = self
            .graph
            .client
            .batch(requests)
            .await
            .map_err(|e| self.err(e))?;
        let mut updates = Vec::new();
        for (response, (_, remote)) in responses.into_iter().zip(&targets) {
            let message: Message = match response.json() {
                Ok(m) => m,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(self.err(e)),
            };
            let mut categories = message.categories;
            categories.retain(|c| !remove.iter().any(|r| r.eq_ignore_ascii_case(c)));
            for a in add {
                if !categories.iter().any(|c| c.eq_ignore_ascii_case(a)) {
                    categories.push(a.clone());
                }
            }
            updates.push((remote.clone(), MessageUpdate::new().categories(categories)));
        }
        if updates.is_empty() {
            return Ok(());
        }
        let results = self
            .graph
            .client
            .update_messages(&updates)
            .await
            .map_err(|e| self.err(e))?;
        self.check(results)
    }

    async fn apply_move(&self, items: &[OpItem], dest: &str) -> Result<()> {
        let mut moving: Vec<(i64, String)> = Vec::new();
        for item in items.iter().filter(|i| i.folder != dest) {
            if let Some(remote) = &item.remote_id
                && !moving.iter().any(|(_, r)| r == remote)
            {
                moving.push((item.message_id, remote.clone()));
            }
        }
        if moving.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = moving.iter().map(|(_, r)| r.clone()).collect();
        let results = self
            .graph
            .client
            .move_messages(&ids, dest)
            .await
            .map_err(|e| self.err(e))?;
        let mut renamed = Vec::new();
        let mut failure = None;
        for ((message_id, old), result) in moving.into_iter().zip(results) {
            match result {
                // Without immutable ids a moved message gets a new id.
                Ok(new) if new != old => renamed.push((message_id, new)),
                Ok(_) => {}
                Err(e) if e.is_not_found() => {}
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
            Some(e) => Err(self.err(e)),
            None => Ok(()),
        }
    }

    /// Deletes messages for good (from Deleted Items or Junk). Exchange keeps
    /// them in Recoverable Items for its retention period.
    async fn apply_delete(&self, items: &[OpItem]) -> Result<()> {
        let mut remotes: Vec<&str> = items
            .iter()
            .filter_map(|i| i.remote_id.as_deref())
            .collect();
        remotes.sort_unstable();
        remotes.dedup();
        if remotes.is_empty() {
            return Ok(());
        }
        let requests = remotes
            .iter()
            .enumerate()
            .map(|(i, remote)| {
                BatchRequest::new(
                    i.to_string(),
                    GraphRequest::delete(["me", "messages", *remote]),
                )
            })
            .collect();
        let responses = self
            .graph
            .client
            .batch(requests)
            .await
            .map_err(|e| self.err(e))?;
        self.check(responses.into_iter().map(|r| r.into_result()).collect())
    }

    /// Uploads a draft, replacing its previous server copy.
    async fn save_draft(&self, draft_id: i64) -> Result<()> {
        let Ok(draft) = self.engine.db.read(|c| misc::draft_get(c, draft_id)) else {
            return Ok(()); // already sent or discarded
        };
        let rendered = crate::sync::render_draft(&self.engine, &draft.data, true).await?;
        // Keep Bcc recipients on the server copy too.
        let mime = with_bcc_header(&rendered.mime, &rendered.recipients);
        let created = self
            .graph
            .client
            .create_draft_mime(&mime, None)
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
        match self.graph.client.delete_draft(remote_id).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_not_found() => Ok(()),
            Err(e) => Err(self.err(e)),
        }
    }

    /// Re-reads the messages of an operation that had to be dropped, so the
    /// server's state replaces our optimistic local change.
    async fn restore_from_server(&mut self, op: &PendingOp) -> Result<()> {
        self.last_full = None;
        let remotes: Vec<String> = match op {
            PendingOp::Flags { message_ids, .. } | PendingOp::Labels { message_ids, .. } => self
                .engine
                .db
                .read(|c| mail::remote_ids(c, message_ids))?
                .into_iter()
                .map(|(_, r)| r)
                .collect(),
            PendingOp::Move { items, .. } | PendingOp::Expunge { items } => {
                items.iter().filter_map(|i| i.remote_id.clone()).collect()
            }
            _ => return Ok(()),
        };
        for remote in remotes {
            let message = match self.graph.client.get_message(&remote).await {
                Ok(m) => m,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(self.err(e)),
            };
            let folder = message
                .parent_folder_id
                .as_deref()
                .and_then(|p| self.folders.iter().find(|f| f.remote_id == p))
                .cloned();
            if let Some(folder) = folder {
                let folder = self.engine.db.read(|c| mail::get_folder(c, folder.id))?;
                self.store_page(
                    &folder,
                    vec![message],
                    Vec::new(),
                    &folder.sync_state,
                    false,
                )
                .await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bodies, attachments and sending (shared by the worker and user actions)

/// Where the MIME source of a message is cached for attachment downloads.
fn source_path(engine: &Engine, account_id: i64, message_id: i64) -> PathBuf {
    engine
        .paths
        .cache
        .join(format!("account-{account_id}"))
        .join("source")
        .join(format!("{message_id}.eml"))
}

/// Serializes downloads of the same message source (a message with several
/// inline images asks for all of them at once).
fn source_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = parking_lot::Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

async fn save_source(path: &Path, raw: &[u8]) {
    let Some(dir) = path.parent() else { return };
    let tmp = dir.join(format!(".{}.part", uuid::Uuid::new_v4().simple()));
    let result = async {
        tokio::fs::create_dir_all(dir).await?;
        tokio::fs::write(&tmp, raw).await?;
        tokio::fs::rename(&tmp, path).await
    }
    .await;
    if let Err(e) = result {
        tracing::debug!("cannot cache message source: {e}");
        let _ = tokio::fs::remove_file(&tmp).await;
    }
}

/// Downloads the MIME source of messages and stores their bodies exactly like
/// IMAP full-message fetches. With `keep_source`, the source of messages with
/// attachments is cached for the attachment downloads that usually follow.
/// Returns the touched threads.
async fn download_bodies(
    engine: &Engine,
    graph: &GraphMail,
    account_id: i64,
    message_ids: &[i64],
    keep_source: bool,
) -> Result<BTreeSet<i64>> {
    let targets = engine.db.read(|c| mail::remote_ids(c, message_ids))?;
    let fetched: Vec<(i64, std::result::Result<bytes::Bytes, GraphError>)> =
        futures::stream::iter(targets)
            .map(|(id, remote)| async move { (id, graph.client.get_message_mime(&remote).await) })
            .buffer_unordered(DOWNLOADS_IN_FLIGHT)
            .collect()
            .await;
    let mut sources = Vec::new();
    let mut failed = Vec::new();
    let mut error = None;
    for (id, result) in fetched {
        match result {
            Ok(raw) => sources.push((id, raw.to_vec())),
            Err(e) => {
                let e = graph.err(e);
                if e.is_transient() || e.is_auth() {
                    error.get_or_insert(e);
                } else {
                    tracing::debug!(message_id = id, "cannot download message: {e}");
                    failed.push(id);
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
    graph: &GraphMail,
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
    let raw = graph
        .client
        .get_message_mime(&remote)
        .await
        .map_err(|e| graph.err(e))?
        .to_vec();
    if keep {
        save_source(&path, &raw).await;
    }
    Ok(raw)
}

/// Downloads message bodies for the reader.
pub(crate) async fn fetch_bodies(
    engine: &Engine,
    account_id: i64,
    message_ids: &[i64],
) -> Result<()> {
    let graph = GraphMail::for_account(engine, account_id)?;
    download_bodies(engine, &graph, account_id, message_ids, true)
        .await
        .map(drop)
}

/// The decoded content of an attachment (`part_id` as stored from the
/// message source).
pub(crate) async fn attachment_data(
    engine: &Engine,
    account_id: i64,
    message_id: i64,
    part_id: &str,
) -> Result<Vec<u8>> {
    let graph = GraphMail::for_account(engine, account_id)?;
    attachment_with(engine, &graph, account_id, message_id, part_id).await
}

async fn attachment_with(
    engine: &Engine,
    graph: &GraphMail,
    account_id: i64,
    message_id: i64,
    part_id: &str,
) -> Result<Vec<u8>> {
    let raw = source(engine, graph, account_id, message_id, true).await?;
    mime::extract_part(&raw, part_id).ok_or_else(|| Error::NotFound("attachment".into()))
}

/// The raw RFC 5322 source of a message (view source / export).
pub(crate) async fn message_source(
    engine: &Engine,
    account_id: i64,
    message_id: i64,
) -> Result<Vec<u8>> {
    let graph = GraphMail::for_account(engine, account_id)?;
    source(engine, &graph, account_id, message_id, false).await
}

/// Sends a rendered message; Exchange files a copy in Sent Items.
pub(crate) async fn send(
    engine: &Engine,
    account_id: i64,
    mime: &[u8],
    recipients: &[String],
) -> Result<()> {
    let graph = GraphMail::for_account(engine, account_id)?;
    send_with(&graph, mime, recipients).await
}

async fn send_with(graph: &GraphMail, mime: &[u8], recipients: &[String]) -> Result<()> {
    let mime = with_bcc_header(mime, recipients);
    graph
        .client
        .send_mime(&mime)
        .await
        .map_err(|e| graph.err(e))
}

#[cfg(test)]
mod tests;
