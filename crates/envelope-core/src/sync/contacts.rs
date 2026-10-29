//! Contact synchronization: CardDAV (ctag, `sync-collection` or an etag diff,
//! then `addressbook-multiget`), Google People (sync tokens) and Microsoft
//! Graph (per-folder delta), uploading local changes first, plus a worker
//! that fetches remote photos and makes list thumbnails.
//!
//! Local edits never wait for the network: they are stored with a
//! `local_state` and pushed here. Uploads are conditional (etags) where the
//! server supports it; when the server copy changed meanwhile, the user's
//! changes are merged field by field into the new server copy.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use envelope_dav::{AddressBook, DavClient, DavError, DavObject, NewAddressBook, Precondition};
use envelope_ical::Contact;
use envelope_ical::vcard::{contact_to_vcard, parse_vcard_with_kind, upsert_contact};
use futures::StreamExt as _;

use super::calendar::{dav_client_at, google_client, graph_client};
use crate::contacts::{data_url, merge3, parse_data_url, sniff_image_mime, thumbnail};
use crate::db::contacts::{
    self as cdb, BookRow, BookSyncState, BookUpsert, ContactRow, Photo, PhotoChange, Pushed,
    SyncedContact,
};
use crate::db::mail::{self, AccountRow};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{AccountKind, EngineEvent};

/// How often address books are checked for changes on the server.
const PULL_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// `remote_id` of the single Google address book.
const GOOGLE_BOOK: &str = "people/me/connections";
/// `remote_id` of the default Contacts folder of a Microsoft account.
const GRAPH_DEFAULT_BOOK: &str = "contacts";
/// vCards fetched per `addressbook-multiget` (and stored per transaction).
const MULTIGET_CHUNK: usize = 100;
/// A change the server keeps refusing is given up after this many attempts.
const MAX_ATTEMPTS: i64 = 5;
/// Largest photo downloaded.
const MAX_PHOTO_BYTES: usize = 8 << 20;

/// Wakes the contacts sync after local changes or account changes.
pub(crate) fn wake(engine: &Engine) {
    engine.contacts_wake.notify_one();
}

/// Whether an account has address books to sync.
pub(crate) fn has_contacts(account: &AccountRow) -> bool {
    match account.kind {
        AccountKind::Gmail | AccountKind::Microsoft => true,
        AccountKind::Imap => account.config.carddav_url.is_some(),
        AccountKind::Exchange => true,
    }
}

/// Background loop: uploads local changes and pulls server changes for every
/// account with contacts, and keeps the photo worker going.
pub(crate) fn spawn_scheduler(engine: Engine) {
    tokio::spawn(async move {
        // Give mail a head start when the app launches.
        tokio::select! {
            _ = engine.shutdown.cancelled() => return,
            _ = engine.contacts_wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
        }
        spawn_photo_worker(engine.clone());
        let mut next_pull = Instant::now();
        loop {
            let pull = Instant::now() >= next_pull;
            run_cycle(&engine, pull).await;
            if pull {
                next_pull = Instant::now() + PULL_INTERVAL;
            }
            let retry = engine
                .db
                .read(cdb::next_push_at)
                .ok()
                .flatten()
                .map(|at| Duration::from_millis((at - now_ms()).clamp(1_000, i64::MAX) as u64));
            let wait = next_pull
                .saturating_duration_since(Instant::now())
                .min(retry.unwrap_or(PULL_INTERVAL));
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = engine.contacts_wake.notified() => {
                    // Let a burst of edits settle into one round.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    next_pull = Instant::now();
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }
    });
}

async fn run_cycle(engine: &Engine, pull: bool) {
    let accounts = engine.db.read(mail::list_accounts).unwrap_or_default();
    let mut changed = Vec::new();
    for account in accounts
        .into_iter()
        .filter(|a| a.enabled && has_contacts(a))
    {
        match sync_account(engine, &account, pull).await {
            Ok(ids) => changed.extend(ids),
            Err(e) if e.is_transient() => {
                tracing::debug!(account = account.id, "contacts sync postponed: {e}")
            }
            Err(e) => tracing::warn!(account = account.id, "contacts sync failed: {e}"),
        }
    }
    emit_changed(engine, changed);
    engine.photos_wake.notify_one();
}

fn emit_changed(engine: &Engine, mut book_ids: Vec<i64>) {
    if book_ids.is_empty() {
        return;
    }
    book_ids.sort_unstable();
    book_ids.dedup();
    engine.emit(EngineEvent::ContactsChanged { book_ids });
}

/// Synchronizes the address books of one account; returns the ids of books
/// whose contacts (or which themselves) changed.
pub(crate) async fn sync_account(
    engine: &Engine,
    account: &AccountRow,
    pull: bool,
) -> Result<Vec<i64>> {
    match account.kind {
        AccountKind::Imap => {
            let url = account
                .config
                .carddav_url
                .clone()
                .ok_or_else(|| Error::Invalid("no CardDAV server configured".into()))?;
            let client = dav_client_at(engine, account, &url).await?;
            carddav_sync(engine, account, &client, &url, pull).await
        }
        AccountKind::Gmail => {
            let client = google_client(engine, account.id)?;
            let writable = engine
                .tokens
                .contacts_writable(account.id)
                .await
                .unwrap_or(false);
            google_sync(engine, account, &client, writable, pull).await
        }
        AccountKind::Microsoft => {
            let client = graph_client(engine, account.id)?;
            let writable = engine
                .tokens
                .contacts_writable(account.id)
                .await
                .unwrap_or(false);
            graph_sync(engine, account, &client, writable, pull).await
        }
        AccountKind::Exchange => ews_sync(engine, account, pull).await,
    }
}

/// What address books of an account are called: the server's name, or the
/// account's label when the server's is missing or generic.
fn book_name(server: Option<&str>, account: &AccountRow) -> String {
    let generic = [
        "",
        "contacts",
        "default",
        "addressbook",
        "address book",
        "card",
    ];
    match server.map(str::trim) {
        Some(name) if !generic.contains(&name.to_lowercase().as_str()) => name.to_owned(),
        _ => account
            .label
            .clone()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| account.email.clone()),
    }
}

// ---------------------------------------------------------------------------
// Upload outcomes

/// What an upload achieved.
enum Done {
    Saved(Box<Pushed>),
    Deleted,
}

/// Why an upload failed, which decides what happens to the change (also
/// used by the task sync).
#[derive(Debug)]
pub(crate) enum Failure {
    /// Try again later (network, server trouble, rate limits).
    Transient(String),
    /// The credentials were rejected.
    Auth(String),
    /// Not allowed: a read-only address book or grant.
    Forbidden(String),
    /// The server refuses this change.
    Rejected(String),
}

impl From<DavError> for Failure {
    fn from(e: DavError) -> Self {
        let message = e.to_string();
        if e.is_transient() {
            Failure::Transient(message)
        } else {
            match e {
                DavError::Unauthorized | DavError::TokenSource(_) => Failure::Auth(message),
                DavError::Forbidden { .. } => Failure::Forbidden(message),
                _ => Failure::Rejected(message),
            }
        }
    }
}

impl From<envelope_google::Error> for Failure {
    fn from(e: envelope_google::Error) -> Self {
        let message = e.to_string();
        let transient = matches!(e, envelope_google::Error::Http(_))
            || e.api_error()
                .is_some_and(|a| a.is_retryable() || a.status >= 500);
        if transient {
            Failure::Transient(message)
        } else if e.is_unauthorized() {
            Failure::Auth(message)
        } else if e.is_forbidden() {
            Failure::Forbidden(message)
        } else {
            Failure::Rejected(message)
        }
    }
}

impl From<envelope_graph::GraphError> for Failure {
    fn from(e: envelope_graph::GraphError) -> Self {
        let message = e.to_string();
        if e.is_transient() {
            Failure::Transient(message)
        } else if e.is_unauthorized() || matches!(e, envelope_graph::GraphError::Auth(_)) {
            Failure::Auth(message)
        } else if e.status() == Some(403) {
            Failure::Forbidden(message)
        } else {
            Failure::Rejected(message)
        }
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        if e.is_transient() {
            Failure::Transient(e.to_string())
        } else if e.is_auth() {
            Failure::Auth(e.to_string())
        } else {
            Failure::Rejected(e.to_string())
        }
    }
}

pub(crate) fn dav_err(e: DavError) -> Error {
    if e.is_transient() {
        Error::Network(e.to_string())
    } else if matches!(e, DavError::Unauthorized | DavError::TokenSource(_)) {
        Error::Auth(e.to_string())
    } else {
        Error::Other(e.to_string())
    }
}

fn google_err(e: envelope_google::Error) -> Error {
    match Failure::from(e) {
        Failure::Transient(m) => Error::Network(m),
        Failure::Auth(m) => Error::Auth(m),
        Failure::Forbidden(m) | Failure::Rejected(m) => Error::Other(m),
    }
}

fn graph_err(e: envelope_graph::GraphError) -> Error {
    match Failure::from(e) {
        Failure::Transient(m) => Error::Network(m),
        Failure::Auth(m) => Error::Auth(m),
        Failure::Forbidden(m) | Failure::Rejected(m) => Error::Other(m),
    }
}

/// Delay before retrying a failed upload.
pub(crate) fn backoff(attempts: i64, transient: bool) -> i64 {
    let cap = if transient { 15 * 60_000 } else { 60 * 60_000 };
    (30_000_i64 << attempts.clamp(0, 10)).min(cap)
}

/// Records the outcome of one upload. Returns whether the contact changed
/// locally; fails (stopping the account's round) when retrying later makes
/// more sense than going on.
fn settle(
    engine: &Engine,
    book: &BookRow,
    row: &ContactRow,
    outcome: std::result::Result<Done, Failure>,
) -> Result<bool> {
    match outcome {
        Ok(Done::Saved(pushed)) => {
            engine
                .db
                .write(|tx| cdb::finish_push(tx, row.id, row.version, &pushed))?;
            Ok(true)
        }
        Ok(Done::Deleted) => {
            engine.db.write(|tx| cdb::delete_row(tx, row.id))?;
            Ok(true)
        }
        Err(Failure::Transient(message)) => {
            let retry = now_ms() + backoff(row.push_attempts, true);
            engine
                .db
                .write(|tx| cdb::push_failed(tx, row.id, &message, retry))?;
            Err(Error::Network(message))
        }
        Err(Failure::Auth(message)) => Err(Error::Auth(message)),
        Err(Failure::Forbidden(message)) => {
            tracing::warn!(
                contact = row.id,
                "the server does not allow this change: {message}"
            );
            engine.db.write(|tx| {
                tx.execute(
                    "UPDATE address_books SET read_only = 1 WHERE id = ?",
                    [book.id],
                )?;
                cdb::discard_local_change(tx, row.id, book.id)
            })?;
            Ok(true)
        }
        Err(Failure::Rejected(message)) if row.push_attempts + 1 >= MAX_ATTEMPTS => {
            tracing::warn!(
                contact = row.id,
                "giving up on a change the server refuses: {message}"
            );
            engine
                .db
                .write(|tx| cdb::discard_local_change(tx, row.id, book.id))?;
            Ok(true)
        }
        Err(Failure::Rejected(message)) => {
            tracing::warn!(contact = row.id, "the server refused a change: {message}");
            let retry = now_ms() + backoff(row.push_attempts, false);
            engine
                .db
                .write(|tx| cdb::push_failed(tx, row.id, &message, retry))?;
            Ok(false)
        }
    }
}

/// Uploads the pending changes of a book with `push_one`.
async fn push_book<F, Fut>(engine: &Engine, book: &BookRow, mut push_one: F) -> Result<bool>
where
    F: FnMut(ContactRow, Option<Photo>) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<Done, Failure>>,
{
    if book.read_only {
        return Ok(false);
    }
    let book_id = book.id;
    let rows = engine
        .db
        .read_async(move |c| cdb::dirty_rows(c, book_id))
        .await?;
    let mut changed = false;
    for row in rows {
        let photo = if row.photo_dirty {
            let id = row.id;
            engine.db.read(|c| cdb::contact_photo(c, id))?
        } else {
            None
        };
        let outcome = push_one(row.clone(), photo).await;
        changed |= settle(engine, book, &row, outcome)?;
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// CardDAV

async fn carddav_sync(
    engine: &Engine,
    account: &AccountRow,
    client: &DavClient,
    url: &str,
    pull: bool,
) -> Result<Vec<i64>> {
    let mut changed = Vec::new();
    let account_id = account.id;
    for book in engine.db.read(|c| cdb::account_books(c, account_id))? {
        let pushed = push_book(engine, &book, |row, photo| {
            let book = book.clone();
            async move { carddav_push(client, &book, &row, photo).await }
        })
        .await?;
        if pushed {
            changed.push(book.id);
        }
    }
    if !pull {
        return Ok(changed);
    }

    let home = carddav_home(engine, account, client, url).await?;
    let mut remote = client.list_addressbooks(&home).await.map_err(dav_err)?;
    if remote.is_empty() && account.state.get("carddavBookCreated").is_none() {
        // Some servers (Radicale) start without any address book.
        match client
            .create_addressbook(&home, &NewAddressBook::new("Contacts"))
            .await
        {
            Ok(href) => tracing::info!(account = account_id, "created address book {href}"),
            Err(e) => tracing::debug!(
                account = account_id,
                "could not create an address book: {e}"
            ),
        }
        engine.db.write(|tx| {
            cdb::set_account_state_value(tx, account_id, "carddavBookCreated", &true.into())
        })?;
        remote = client.list_addressbooks(&home).await.map_err(dav_err)?;
    }
    let upserts: Vec<BookUpsert> = remote
        .iter()
        .enumerate()
        .map(|(i, b)| BookUpsert {
            remote_id: b.href.clone(),
            name: book_name(b.display_name.as_deref(), account),
            read_only: b.read_only,
            is_default: i == 0,
            sort_order: i as i64,
        })
        .collect();
    changed.extend(
        engine
            .db
            .write(|tx| cdb::sync_books(tx, account_id, "carddav", &upserts))?,
    );
    for book in engine.db.read(|c| cdb::account_books(c, account_id))? {
        let Some(dav) = remote.iter().find(|b| b.href == book.remote_id) else {
            continue;
        };
        match carddav_pull(engine, client, &book, dav).await {
            Ok(true) => changed.push(book.id),
            Ok(false) => {}
            Err(e) if e.is_transient() || e.is_auth() => return Err(e),
            Err(e) => tracing::warn!("address book {} sync failed: {e}", book.name),
        }
    }
    Ok(changed)
}

/// The account's address book home, discovered once (again when the
/// CardDAV address changes) and remembered.
async fn carddav_home(
    engine: &Engine,
    account: &AccountRow,
    client: &DavClient,
    url: &str,
) -> Result<String> {
    let known = account.state.get("carddavUrl").and_then(|v| v.as_str()) == Some(url);
    if let Some(home) = account
        .state
        .get("carddavHome")
        .and_then(|v| v.as_str())
        .filter(|_| known)
    {
        return Ok(home.to_owned());
    }
    let found = client
        .discover_from(url, envelope_dav::ServiceType::CardDav)
        .await
        .map_err(dav_err)?;
    let home = found
        .addressbook_home_set
        .first()
        .cloned()
        .ok_or_else(|| Error::Other("the server has no address books".into()))?;
    let account_id = account.id;
    engine.db.write(|tx| {
        cdb::set_account_state_value(tx, account_id, "carddavHome", &home.clone().into())?;
        cdb::set_account_state_value(tx, account_id, "carddavUrl", &url.into())
    })?;
    Ok(home)
}

/// Pulls the changes of one address book. Returns whether anything changed.
async fn carddav_pull(
    engine: &Engine,
    client: &DavClient,
    book: &BookRow,
    dav: &AddressBook,
) -> Result<bool> {
    let state = &book.sync_state;
    if state.last_sync.is_some() && dav.ctag.is_some() && state.ctag == dav.ctag {
        return Ok(false);
    }
    let book_id = book.id;
    let local = engine
        .db
        .read_async(move |c| cdb::remote_index(c, book_id))
        .await?;
    // Either the changes since the last sync token, or the full listing.
    let mut token = state.sync_token.clone();
    let (refs, removed, full, sync_token) = loop {
        if !dav.supports_sync {
            let refs = client.list_etags(&book.remote_id).await.map_err(dav_err)?;
            break (refs, Vec::new(), true, None);
        }
        match client
            .sync_collection(&book.remote_id, token.as_deref())
            .await
        {
            // A truncated listing is not complete enough to infer removals.
            Ok(delta) => {
                break (
                    delta.changed,
                    delta.removed,
                    token.is_none() && !delta.truncated,
                    delta.sync_token,
                );
            }
            Err(e) if e.requires_full_resync() && token.is_some() => token = None,
            Err(e) => return Err(dav_err(e)),
        }
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut wanted: Vec<String> = Vec::new();
    for r in &refs {
        if !seen.insert(r.href.clone()) {
            continue;
        }
        let fetch = match local.get(&r.href) {
            None => true,
            // Contacts with local changes are reconciled when pushed.
            Some((_, state)) if *state != cdb::SYNCED => false,
            Some((etag, _)) => etag.is_none() || r.etag.is_none() || *etag != r.etag,
        };
        if fetch {
            wanted.push(r.href.clone());
        }
    }
    let mut any = false;
    for chunk in wanted.chunks(MULTIGET_CHUNK) {
        let result = client
            .addressbook_multiget(&book.remote_id, chunk)
            .await
            .map_err(dav_err)?;
        any |= !result.objects.is_empty() || !result.missing.is_empty();
        store_vcards(engine, book_id, result.objects, result.missing).await?;
    }
    let mut gone: Vec<String> = removed;
    if full {
        gone.extend(
            local
                .iter()
                .filter(|(href, (_, state))| *state != cdb::CREATED && !seen.contains(*href))
                .map(|(href, _)| href.clone()),
        );
    }
    if !gone.is_empty() {
        let deleted = engine
            .db
            .write_async(move |tx| {
                let mut n = 0;
                for href in &gone {
                    n += cdb::delete_synced(tx, book_id, href)? as usize;
                }
                Ok(n)
            })
            .await?;
        any |= deleted > 0;
    }
    let new_state = BookSyncState {
        ctag: dav.ctag.clone(),
        sync_token: sync_token.or_else(|| dav.sync_token.clone()),
        last_sync: Some(now_ms()),
        ..BookSyncState::default()
    };
    engine
        .db
        .write(|tx| cdb::set_book_sync_state(tx, book_id, &new_state))?;
    Ok(any)
}

/// What sync stores for a vCard from the server (`None`: unreadable).
fn synced_vcard(href: String, etag: Option<String>, data: String) -> Option<SyncedContact> {
    let (mut contact, kind) = match parse_vcard_with_kind(&data) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!("unreadable vCard {href}: {e}");
            return None;
        }
    };
    let photo = match contact.photo.take() {
        Some(p) if p.starts_with("data:") => parse_data_url(&p)
            .map(PhotoChange::Data)
            .unwrap_or(PhotoChange::Clear),
        Some(p) if p.starts_with("http://") || p.starts_with("https://") => PhotoChange::Remote(p),
        _ => PhotoChange::Clear,
    };
    Some(SyncedContact {
        remote_id: href,
        etag,
        uid: contact.uid.clone(),
        contact,
        base: Some(data),
        photo,
        is_group: kind.as_deref() == Some("group"),
    })
}

async fn store_vcards(
    engine: &Engine,
    book_id: i64,
    objects: Vec<DavObject>,
    missing: Vec<String>,
) -> Result<()> {
    engine
        .db
        .write_async(move |tx| {
            for object in objects {
                if let Some(item) = synced_vcard(object.href, object.etag, object.data) {
                    cdb::upsert_synced(tx, book_id, &item)?;
                }
            }
            for href in &missing {
                cdb::delete_synced(tx, book_id, href)?;
            }
            Ok(())
        })
        .await
}

/// The contact to upload: the stored one with its photo as the server
/// should get it (the edited photo, or the server's own when unchanged).
fn outgoing(row: &ContactRow, base: Option<&Contact>, photo: Option<&Photo>) -> Contact {
    let mut contact = row.contact.clone();
    contact.photo = if row.photo_dirty {
        photo.map(data_url)
    } else {
        base.and_then(|b| b.photo.clone())
    };
    if contact.uid.is_empty() {
        contact.uid = base.map(|b| b.uid.clone()).unwrap_or_default();
    }
    contact
}

fn pushed_vcard(href: &str, etag: Option<String>, card: String, sent: &Contact) -> Pushed {
    // Reading back the card just sent gives the same normalized form a later
    // sync would.
    let contact = parse_vcard_with_kind(&card)
        .map(|(c, _)| c)
        .unwrap_or_else(|_| sent.clone());
    Pushed {
        remote_id: href.to_owned(),
        etag,
        uid: contact.uid.clone(),
        contact,
        base: Some(card),
        photo_url: None,
    }
}

/// Uploads one change to a CardDAV server.
async fn carddav_push(
    client: &DavClient,
    book: &BookRow,
    row: &ContactRow,
    photo: Option<Photo>,
) -> std::result::Result<Done, Failure> {
    if row.local_state == cdb::DELETED {
        if let Some(href) = &row.remote_id {
            match client.delete(href, None).await {
                Ok(()) | Err(DavError::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        return Ok(Done::Deleted);
    }
    let href = match &row.remote_id {
        Some(href) => href.clone(),
        None => {
            envelope_dav::object_href(&book.remote_id, &row.uid, envelope_dav::ObjectKind::Contact)?
        }
    };
    let mut base_raw = row.base.clone().unwrap_or_default();
    let mut base = parse_vcard_with_kind(&base_raw).ok().map(|(c, _)| c);
    let mut local = outgoing(row, base.as_ref(), photo.as_ref());
    let mut etag = row.etag.clone();

    if row.local_state == cdb::CREATED || base_raw.trim().is_empty() {
        let card = contact_to_vcard(&local);
        match client.put(&href, &card, Precondition::IfNoneMatch).await {
            Ok(etag) => {
                return Ok(Done::Saved(Box::new(pushed_vcard(
                    &href, etag, card, &local,
                ))));
            }
            Err(DavError::PreconditionFailed { .. }) => {
                // Already there: an earlier upload whose answer was lost, or
                // a clash with another contact.
                let existing = client.get(&href).await?;
                let theirs = parse_vcard_with_kind(&existing.data)
                    .map(|(c, _)| c)
                    .unwrap_or_default();
                if theirs.uid != local.uid {
                    return Err(Failure::Rejected(format!(
                        "{href} already holds another contact"
                    )));
                }
                base = Some(theirs);
                base_raw = existing.data;
                etag = existing.etag;
            }
            Err(e) => return Err(e.into()),
        }
    }

    for _ in 0..3 {
        let card =
            upsert_contact(&base_raw, &local).map_err(|e| Failure::Rejected(e.to_string()))?;
        let condition = etag.clone().map(Precondition::IfMatch).unwrap_or_default();
        match client.put(&href, &card, condition).await {
            Ok(etag) => {
                return Ok(Done::Saved(Box::new(pushed_vcard(
                    &href, etag, card, &local,
                ))));
            }
            Err(DavError::PreconditionFailed { .. } | DavError::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
        // Changed (or deleted) on the server since we last saw it.
        match client.get(&href).await {
            Ok(server) => {
                tracing::info!(%href, "merging a contact that also changed on the server");
                let theirs = parse_vcard_with_kind(&server.data)
                    .map(|(c, _)| c)
                    .unwrap_or_default();
                let ours_base = base.clone().unwrap_or_else(|| theirs.clone());
                local = merge3(&ours_base, &local, &theirs);
                base = Some(theirs);
                base_raw = server.data;
                etag = server.etag;
            }
            Err(DavError::NotFound { .. }) => {
                // Deleted there while edited here: the edit wins.
                let card = upsert_contact(&base_raw, &local)
                    .map_err(|e| Failure::Rejected(e.to_string()))?;
                return match client.put(&href, &card, Precondition::IfNoneMatch).await {
                    Ok(etag) => Ok(Done::Saved(Box::new(pushed_vcard(
                        &href, etag, card, &local,
                    )))),
                    Err(e) => Err(e.into()),
                };
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(Failure::Transient(format!(
        "{href} keeps changing on the server"
    )))
}

// ---------------------------------------------------------------------------
// Google

async fn google_sync(
    engine: &Engine,
    account: &AccountRow,
    client: &envelope_google::GoogleClient,
    writable: bool,
    pull: bool,
) -> Result<Vec<i64>> {
    let account_id = account.id;
    let upsert = BookUpsert {
        remote_id: GOOGLE_BOOK.to_owned(),
        name: book_name(None, account),
        read_only: !writable,
        is_default: true,
        sort_order: 0,
    };
    let mut changed = engine
        .db
        .write(|tx| cdb::sync_books(tx, account_id, "google", std::slice::from_ref(&upsert)))?;
    let Some(book) = engine
        .db
        .read(|c| cdb::account_books(c, account_id))?
        .into_iter()
        .next()
    else {
        return Ok(changed);
    };
    if push_book(engine, &book, |row, photo| async move {
        google_push(client, &row, photo).await
    })
    .await?
    {
        changed.push(book.id);
    }
    // Pushing may have reset the sync state (a refused change is undone by
    // syncing from scratch).
    let book = engine.db.read(|c| cdb::get_book(c, book.id))?;
    if pull && google_pull(engine, client, &book).await? {
        changed.push(book.id);
    }
    Ok(changed)
}

fn synced_google(contact: envelope_google::GoogleContact) -> SyncedContact {
    let envelope_google::GoogleContact {
        mut contact,
        etag,
        person,
    } = contact;
    let photo = match contact.photo.take() {
        Some(url) => PhotoChange::Remote(url),
        None => PhotoChange::Clear,
    };
    SyncedContact {
        remote_id: contact.uid.clone(),
        etag,
        uid: contact.uid.clone(),
        contact,
        base: Some(person.to_string()),
        photo,
        is_group: false,
    }
}

async fn google_pull(
    engine: &Engine,
    client: &envelope_google::GoogleClient,
    book: &BookRow,
) -> Result<bool> {
    let book_id = book.id;
    let mut token = book.sync_state.sync_token.clone();
    let mut any = false;
    loop {
        let full = token.is_none();
        let mut seen: HashSet<String> = HashSet::new();
        let mut next_token = None;
        let mut expired = false;
        let mut sync =
            client.contacts_sync(envelope_google::ContactSource::Connections, token.clone());
        loop {
            match sync.next_page().await {
                Ok(Some(page)) => {
                    any |= !page.contacts.is_empty() || !page.deleted.is_empty();
                    if full {
                        seen.extend(page.contacts.iter().map(|c| c.contact.uid.clone()));
                    }
                    if page.next_sync_token.is_some() {
                        next_token = page.next_sync_token.clone();
                    }
                    let items: Vec<SyncedContact> =
                        page.contacts.into_iter().map(synced_google).collect();
                    let deleted = page.deleted;
                    engine
                        .db
                        .write_async(move |tx| {
                            for item in &items {
                                cdb::upsert_synced(tx, book_id, item)?;
                            }
                            for id in &deleted {
                                cdb::delete_synced(tx, book_id, id)?;
                            }
                            Ok(())
                        })
                        .await?;
                }
                Ok(None) => break,
                Err(envelope_google::Error::SyncTokenExpired) if !full => {
                    expired = true;
                    break;
                }
                Err(e) => return Err(google_err(e)),
            }
        }
        if expired {
            token = None;
            continue;
        }
        if full {
            let swept = engine
                .db
                .write_async(move |tx| cdb::sweep(tx, book_id, &seen))
                .await?;
            any |= swept > 0;
        }
        let state = BookSyncState {
            sync_token: next_token,
            last_sync: Some(now_ms()),
            ..BookSyncState::default()
        };
        engine
            .db
            .write(|tx| cdb::set_book_sync_state(tx, book_id, &state))?;
        return Ok(any);
    }
}

fn pushed_google(saved: envelope_google::GoogleContact) -> Pushed {
    Pushed {
        remote_id: saved.contact.uid.clone(),
        etag: saved.etag,
        uid: saved.contact.uid.clone(),
        photo_url: saved.contact.photo.clone(),
        base: Some(saved.person.to_string()),
        contact: saved.contact,
    }
}

/// The Google contact a local change started from.
fn google_base(row: &ContactRow) -> Option<envelope_google::GoogleContact> {
    let person: serde_json::Value = serde_json::from_str(row.base.as_deref()?).ok()?;
    envelope_google::GoogleContact::from_person(person).ok()
}

/// Uploads one change to Google.
async fn google_push(
    client: &envelope_google::GoogleClient,
    row: &ContactRow,
    photo: Option<Photo>,
) -> std::result::Result<Done, Failure> {
    let resource = row.remote_id.clone().filter(|r| !r.is_empty());
    if row.local_state == cdb::DELETED {
        if let Some(resource) = &resource {
            match client.delete_contact(resource).await {
                Ok(()) => {}
                Err(e) if e.is_not_found() || e.is_gone() => {}
                Err(e) => return Err(e.into()),
            }
        }
        return Ok(Done::Deleted);
    }
    let base = google_base(row).filter(|_| resource.is_some() && row.local_state != cdb::CREATED);
    let mut saved = match base {
        None => client.create_contact(&row.contact).await?,
        Some(base) => match client.update_contact(&base, &row.contact).await {
            Ok(saved) => saved,
            Err(e) if e.is_stale_etag() => {
                let fresh = client.get_contact(&base.contact.uid).await?;
                let merged = merge3(&base.contact, &row.contact, &fresh.contact);
                client.update_contact(&fresh, &merged).await?
            }
            Err(e) if e.is_not_found() || e.is_gone() => {
                client.create_contact(&row.contact).await?
            }
            Err(e) => return Err(e.into()),
        },
    };
    if row.photo_dirty {
        let uid = saved.contact.uid.clone();
        let result = match &photo {
            Some(photo) => client
                .update_contact_photo(&uid, &photo.bytes)
                .await
                .map(Some),
            None if saved.contact.photo.is_some() => {
                client.delete_contact_photo(&uid).await.map(Some)
            }
            None => Ok(None),
        };
        match result.map_err(Failure::from) {
            Ok(Some(updated)) => saved = updated,
            Ok(None) => {}
            Err(failure @ (Failure::Transient(_) | Failure::Auth(_))) => return Err(failure),
            // The other changes are saved; a photo Google refuses is not
            // worth holding them back.
            Err(failure) => tracing::warn!(contact = row.id, "photo not saved: {failure:?}"),
        }
    }
    Ok(Done::Saved(Box::new(pushed_google(saved))))
}

// ---------------------------------------------------------------------------
// Microsoft Graph

async fn graph_sync(
    engine: &Engine,
    account: &AccountRow,
    client: &envelope_graph::GraphClient,
    writable: bool,
    pull: bool,
) -> Result<Vec<i64>> {
    let account_id = account.id;
    let mut changed = Vec::new();
    for book in engine.db.read(|c| cdb::account_books(c, account_id))? {
        let pushed = push_book(engine, &book, |row, photo| {
            let book = book.clone();
            async move { graph_push(client, &book, &row, photo).await }
        })
        .await?;
        if pushed {
            changed.push(book.id);
        }
    }
    if !pull {
        return Ok(changed);
    }
    let folders = client.contact_folders().await.map_err(graph_err)?;
    let mut upserts = vec![BookUpsert {
        remote_id: GRAPH_DEFAULT_BOOK.to_owned(),
        name: book_name(None, account),
        read_only: !writable,
        is_default: true,
        sort_order: 0,
    }];
    upserts.extend(folders.iter().enumerate().map(|(i, f)| BookUpsert {
        remote_id: f.id.clone(),
        name: if f.display_name.trim().is_empty() {
            "Contacts".to_owned()
        } else {
            f.display_name.clone()
        },
        read_only: !writable,
        is_default: false,
        sort_order: i as i64 + 1,
    }));
    changed.extend(
        engine
            .db
            .write(|tx| cdb::sync_books(tx, account_id, "graph", &upserts))?,
    );
    let default_folder = folders.iter().find_map(|f| f.parent_folder_id.clone());
    for book in engine.db.read(|c| cdb::account_books(c, account_id))? {
        match graph_pull(engine, client, &book, default_folder.as_deref()).await {
            Ok(true) => changed.push(book.id),
            Ok(false) => {}
            Err(e) if e.is_transient() || e.is_auth() => return Err(e),
            Err(e) => tracing::warn!("contact folder {} sync failed: {e}", book.name),
        }
    }
    Ok(changed)
}

/// The Graph folder behind a book.
async fn graph_folder(
    client: &envelope_graph::GraphClient,
    book: &BookRow,
    default_folder: Option<&str>,
) -> Result<Option<String>> {
    if book.remote_id != GRAPH_DEFAULT_BOOK {
        return Ok(Some(book.remote_id.clone()));
    }
    if let Some(id) = &book.sync_state.folder_id {
        return Ok(Some(id.clone()));
    }
    if let Some(id) = default_folder {
        return Ok(Some(id.to_owned()));
    }
    client.default_contact_folder_id().await.map_err(graph_err)
}

async fn graph_pull(
    engine: &Engine,
    client: &envelope_graph::GraphClient,
    book: &BookRow,
    default_folder: Option<&str>,
) -> Result<bool> {
    let Some(folder) = graph_folder(client, book, default_folder).await? else {
        // An empty default folder: nothing to sync, and no id to sync it by.
        return Ok(false);
    };
    let book_id = book.id;
    let mut state = book.sync_state.clone();
    state.folder_id = (book.remote_id == GRAPH_DEFAULT_BOOK).then(|| folder.clone());
    let mut cursor: Option<envelope_graph::DeltaCursor> = state
        .cursor
        .clone()
        .and_then(|v| serde_json::from_value(v).ok());
    // Removals are only inferred from a complete listing done in one go.
    let mut full = cursor.is_none();
    let mut seen: HashSet<String> = HashSet::new();
    let mut any = false;
    let marker = now_ms();
    loop {
        let page = match client
            .contacts_delta(&folder, cursor.as_ref(), Some(200))
            .await
        {
            Ok(page) => page,
            Err(e) if e.is_resync_required() && cursor.is_some() => {
                cursor = None;
                full = true;
                seen.clear();
                continue;
            }
            Err(e) => return Err(graph_err(e)),
        };
        any |= !page.items.is_empty() || !page.removed_ids.is_empty();
        if full {
            seen.extend(page.items.iter().map(|c| c.uid.clone()));
        }
        let items: Vec<SyncedContact> = page
            .items
            .into_iter()
            .map(|contact| SyncedContact {
                remote_id: contact.uid.clone(),
                etag: None,
                uid: contact.uid.clone(),
                base: serde_json::to_string(&contact).ok(),
                photo: PhotoChange::Remote(format!("graph:{marker}")),
                contact,
                is_group: false,
            })
            .collect();
        let removed = page.removed_ids;
        cursor = Some(page.next.clone());
        state.cursor = serde_json::to_value(&page.next).ok();
        let saved_state = state.clone();
        engine
            .db
            .write_async(move |tx| {
                for item in &items {
                    cdb::upsert_synced(tx, book_id, item)?;
                }
                for id in &removed {
                    cdb::delete_synced(tx, book_id, id)?;
                }
                cdb::set_book_sync_state(tx, book_id, &saved_state)
            })
            .await?;
        if page.next.is_done() {
            break;
        }
    }
    if full {
        let swept = engine
            .db
            .write_async(move |tx| cdb::sweep(tx, book_id, &seen))
            .await?;
        any |= swept > 0;
    }
    state.last_sync = Some(now_ms());
    engine
        .db
        .write(|tx| cdb::set_book_sync_state(tx, book_id, &state))?;
    Ok(any)
}

/// Uploads one change to Microsoft Graph.
async fn graph_push(
    client: &envelope_graph::GraphClient,
    book: &BookRow,
    row: &ContactRow,
    photo: Option<Photo>,
) -> std::result::Result<Done, Failure> {
    let id = row.remote_id.clone().filter(|r| !r.is_empty());
    if row.local_state == cdb::DELETED {
        if let Some(id) = &id {
            match client.delete_contact(id).await {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        return Ok(Done::Deleted);
    }
    let folder = (book.remote_id != GRAPH_DEFAULT_BOOK).then_some(book.remote_id.as_str());
    let base: Option<Contact> = row
        .base
        .as_deref()
        .and_then(|b| serde_json::from_str(b).ok());
    let saved = match (&id, base) {
        (Some(id), Some(base)) if row.local_state != cdb::CREATED => {
            match client.update_contact(id, &base, &row.contact).await {
                Ok(saved) => saved,
                Err(e) if e.is_not_found() => client.create_contact(folder, &row.contact).await?,
                Err(e) => return Err(e.into()),
            }
        }
        _ => client.create_contact(folder, &row.contact).await?,
    };
    let has_photo = if row.photo_dirty {
        let result = match &photo {
            Some(photo) => {
                client
                    .set_contact_photo(&saved.uid, photo.bytes.clone(), &photo.mime)
                    .await
            }
            None => client.delete_contact_photo(&saved.uid).await,
        };
        match result.map_err(Failure::from) {
            Ok(()) => {}
            Err(failure @ (Failure::Transient(_) | Failure::Auth(_))) => return Err(failure),
            // The other changes are saved; a photo Outlook refuses is not
            // worth holding them back.
            Err(failure) => tracing::warn!(contact = row.id, "photo not saved: {failure:?}"),
        }
        photo.is_some()
    } else {
        row.has_photo
    };
    Ok(Done::Saved(Box::new(Pushed {
        remote_id: saved.uid.clone(),
        etag: None,
        uid: saved.uid.clone(),
        base: serde_json::to_string(&saved).ok(),
        // The photo here is the one on the server: nothing to fetch.
        photo_url: has_photo.then(|| format!("graph:{}", now_ms())),
        contact: saved,
    })))
}

// ---------------------------------------------------------------------------
// Photos

/// Fetches remote photos and makes list thumbnails in the background.
fn spawn_photo_worker(engine: Engine) {
    tokio::spawn(async move {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
            .build();
        let http = match http {
            Ok(http) => http,
            Err(e) => {
                tracing::warn!("contact photos disabled: {e}");
                return;
            }
        };
        loop {
            fetch_photos(&engine, &http, |account| {
                graph_client(&engine, account).ok()
            })
            .await;
            make_thumbnails(&engine).await;
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = engine.photos_wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(30 * 60)) => {}
            }
        }
    });
}

/// Emits `ContactsChanged` at most every few seconds while work goes on.
struct Throttle {
    books: HashSet<i64>,
    last: Instant,
}

impl Throttle {
    fn new() -> Self {
        Throttle {
            books: HashSet::new(),
            last: Instant::now(),
        }
    }

    fn add(&mut self, engine: &Engine, book: i64) {
        self.books.insert(book);
        if self.last.elapsed() > Duration::from_secs(3) {
            self.flush(engine);
        }
    }

    fn flush(&mut self, engine: &Engine) {
        emit_changed(engine, self.books.drain().collect());
        self.last = Instant::now();
    }
}

/// A photo URL with the size Envelope shows (Google serves any size).
fn sized_photo_url(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    if !lower.contains("googleusercontent.com") {
        return url.to_owned();
    }
    if let Some(pos) = url.rfind("=s")
        && url[pos + 2..].bytes().all(|b| b.is_ascii_digit())
        && pos + 2 < url.len()
    {
        return format!("{}=s256", &url[..pos]);
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}sz=256")
}

/// Downloads one photo (`Ok(None)`: there is none).
async fn download_photo(
    http: &reqwest::Client,
    url: &str,
) -> std::result::Result<Option<Photo>, String> {
    let response = http
        .get(sized_photo_url(url))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403 | 404 | 410) {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let header_mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_PHOTO_BYTES {
        return Ok(None);
    }
    let mime = sniff_image_mime(&bytes)
        .map(str::to_owned)
        .or(header_mime.filter(|m| m.starts_with("image/")));
    Ok(mime.map(|mime| Photo {
        bytes: bytes.to_vec(),
        mime,
    }))
}

async fn fetch_photos(
    engine: &Engine,
    http: &reqwest::Client,
    graph_client: impl Fn(i64) -> Option<envelope_graph::GraphClient>,
) {
    let Ok(pending) = engine
        .db
        .read_async(|c| cdb::photos_to_fetch(c, 2_000))
        .await
    else {
        return;
    };
    if pending.is_empty() {
        return;
    }
    let books: HashMap<i64, BookRow> = engine
        .db
        .read(cdb::list_books)
        .unwrap_or_default()
        .into_iter()
        .map(|b| (b.id, b))
        .collect();
    let mut graph: HashMap<i64, Option<envelope_graph::GraphClient>> = HashMap::new();
    for (_, book_id, _, url) in &pending {
        if url.starts_with("graph:")
            && let Some(account) = books.get(book_id).and_then(|b| b.account_id)
        {
            graph
                .entry(account)
                .or_insert_with(|| graph_client(account));
        }
    }
    let graph = &graph;
    let books = &books;
    let mut results = futures::stream::iter(pending)
        .map(|(id, book_id, remote_id, url)| async move {
            let result = if url.starts_with("graph:") {
                let client = books
                    .get(&book_id)
                    .and_then(|b| b.account_id)
                    .and_then(|a| graph.get(&a).cloned().flatten());
                match client {
                    Some(client) if !remote_id.is_empty() => {
                        match client.contact_photo(&remote_id).await {
                            Ok(found) => Ok(found.map(|(bytes, mime)| Photo {
                                mime: sniff_image_mime(&bytes).map(str::to_owned).unwrap_or(mime),
                                bytes: bytes.to_vec(),
                            })),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    _ => Ok(None),
                }
            } else if url.starts_with("http://") || url.starts_with("https://") {
                download_photo(http, &url).await
            } else {
                Ok(None)
            };
            (id, book_id, url, result)
        })
        .buffer_unordered(4);
    let mut throttle = Throttle::new();
    let mut failures = 0;
    while let Some((id, book_id, url, result)) = results.next().await {
        match result {
            Ok(photo) => {
                let stored = engine
                    .db
                    .write(|tx| cdb::store_fetched_photo(tx, id, &url, photo.as_ref()));
                if matches!(stored, Ok(true)) {
                    throttle.add(engine, book_id);
                }
            }
            Err(e) => {
                failures += 1;
                tracing::debug!(contact = id, "photo download failed: {e}");
            }
        }
        if engine.shutdown.is_cancelled() {
            break;
        }
    }
    if failures > 0 {
        tracing::info!("{failures} contact photos could not be downloaded; will retry later");
    }
    throttle.flush(engine);
}

async fn make_thumbnails(engine: &Engine) {
    let mut throttle = Throttle::new();
    loop {
        let Ok(batch) = engine
            .db
            .read_async(|c| cdb::photos_without_thumbs(c, 50))
            .await
        else {
            break;
        };
        if batch.is_empty() {
            break;
        }
        let made = tokio::task::spawn_blocking(move || {
            batch
                .into_iter()
                .map(|(id, book, hash, bytes)| (id, book, hash, thumbnail(&bytes)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if made.is_empty() {
            break;
        }
        let stored = engine.db.write(|tx| {
            for (id, _, hash, thumb) in &made {
                // An empty thumbnail marks photos that cannot be decoded.
                cdb::set_thumb(tx, *id, hash, thumb.as_deref().unwrap_or(""))?;
            }
            Ok(())
        });
        if stored.is_err() {
            break;
        }
        for (_, book, ..) in &made {
            throttle.add(engine, *book);
        }
        if engine.shutdown.is_cancelled() {
            break;
        }
    }
    throttle.flush(engine);
}

#[cfg(test)]
mod tests;

// ----- Exchange (EWS) ------------------------------------------------------------

/// The Contacts folder of an Exchange mailbox.
const EWS_BOOK: &str = "ews:contacts";

async fn ews_sync(engine: &Engine, account: &AccountRow, pull: bool) -> Result<Vec<i64>> {
    let account_id = account.id;
    let upsert = BookUpsert {
        remote_id: EWS_BOOK.to_owned(),
        name: book_name(None, account),
        read_only: false,
        is_default: true,
        sort_order: 0,
    };
    let mut changed = engine
        .db
        .write(|tx| cdb::sync_books(tx, account_id, "ews", std::slice::from_ref(&upsert)))?;
    let Some(book) = engine
        .db
        .read(|c| cdb::account_books(c, account_id))?
        .into_iter()
        .next()
    else {
        return Ok(changed);
    };
    let mail = super::ews::EwsMail::for_account(engine, account).await?;
    let pushed = push_book(engine, &book, |row, _photo| {
        let client = mail.client().clone();
        async move { ews_push(&client, &row).await }
    })
    .await?;
    if pushed && !changed.contains(&book.id) {
        changed.push(book.id);
    }
    if !pull {
        return Ok(changed);
    }
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0;
    loop {
        let page = mail
            .client()
            .find_contacts(envelope_ews::DistinguishedFolder::Contacts, offset, 500)
            .await
            .map_err(super::ews::ews_error)?;
        let count = page.items.len() as u32;
        for c in page.items {
            if c.is_group {
                continue;
            }
            let Some(id) = c.id.clone() else {
                continue;
            };
            seen.insert(id.id.clone());
            items.push(SyncedContact {
                remote_id: id.id.clone(),
                etag: id.change_key.clone(),
                uid: id.id.clone(),
                contact: ews_contact(&c, &id.id),
                base: None,
                photo: PhotoChange::Keep,
                is_group: false,
            });
        }
        match page.next_offset {
            Some(next) if !page.includes_last_item_in_range && count > 0 => offset = next,
            _ => break,
        }
    }
    let book_id = book.id;
    let listed = !seen.is_empty();
    let swept = engine
        .db
        .write_async(move |tx| {
            for item in &items {
                cdb::upsert_synced(tx, book_id, item)?;
            }
            cdb::sweep(tx, book_id, &seen)
        })
        .await?;
    let mut state = book.sync_state.clone();
    state.last_sync = Some(now_ms());
    engine
        .db
        .write(|tx| cdb::set_book_sync_state(tx, book_id, &state))?;
    // Unchanged contacts are rewritten in place; report the book as changed
    // when anything could have moved.
    if swept > 0 || listed {
        changed.push(book_id);
    }
    Ok(changed)
}

/// An Exchange contact in the shared contact model.
fn ews_contact(c: &envelope_ews::Contact, id: &str) -> Contact {
    use envelope_ical::ContactField;
    let name = c
        .display_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            let parts: Vec<&str> = [c.given_name.as_deref(), c.surname.as_deref()]
                .into_iter()
                .flatten()
                .filter(|p| !p.trim().is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        })
        .or_else(|| c.file_as.clone())
        .or_else(|| c.email_addresses.first().map(|e| e.address.clone()))
        .unwrap_or_default();
    let phone_kind = |key: &str| -> Option<String> {
        let k = key.to_ascii_lowercase();
        Some(
            if k.contains("mobile") || k.contains("car") {
                "cell"
            } else if k.contains("fax") {
                "fax"
            } else if k.contains("home") {
                "home"
            } else if k.contains("business") || k.contains("company") || k.contains("assistant") {
                "work"
            } else {
                "other"
            }
            .to_owned(),
        )
    };
    Contact {
        uid: id.to_owned(),
        name,
        given_name: c.given_name.clone(),
        family_name: c.surname.clone(),
        nickname: c.nickname.clone(),
        emails: c
            .email_addresses
            .iter()
            .enumerate()
            .map(|(i, e)| ContactField {
                value: e.address.clone(),
                kind: None,
                preferred: i == 0,
            })
            .collect(),
        phones: c
            .phone_numbers
            .iter()
            .map(|p| ContactField {
                value: p.number.clone(),
                kind: phone_kind(&p.key),
                preferred: false,
            })
            .collect(),
        organization: c.company_name.clone().filter(|s| !s.trim().is_empty()),
        title: c.job_title.clone().filter(|s| !s.trim().is_empty()),
        note: None,
        birthday: c.birthday.map(|b| b.date_naive()),
        photo: None,
        addresses: Vec::new(),
        urls: Vec::new(),
    }
}

impl From<envelope_ews::EwsError> for Failure {
    fn from(e: envelope_ews::EwsError) -> Self {
        Failure::from(super::ews::ews_error(e))
    }
}

/// Phone keys of an Exchange contact for each kind of number, in the order
/// they are used.
fn ews_phone_keys(kind: Option<&str>) -> &'static [&'static str] {
    const OTHER: &[&str] = &[
        "OtherTelephone",
        "PrimaryPhone",
        "Callback",
        "Radio",
        "Isdn",
        "Telex",
        "TtyTddPhone",
    ];
    match kind.map(str::to_ascii_lowercase).as_deref() {
        Some("cell" | "mobile" | "iphone") => &["MobilePhone", "CarPhone"],
        Some("work") => &[
            "BusinessPhone",
            "BusinessPhone2",
            "CompanyMainPhone",
            "AssistantPhone",
        ],
        Some("home") => &["HomePhone", "HomePhone2"],
        Some("fax") => &["BusinessFax", "HomeFax", "OtherFax"],
        Some("pager") => &["Pager"],
        _ => OTHER,
    }
}

fn digits(number: &str) -> String {
    number.chars().filter(char::is_ascii_digit).collect()
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// The text fields of a contact as Exchange names them.
fn ews_texts(contact: &Contact) -> Vec<(envelope_ews::ContactText, Option<String>)> {
    use envelope_ews::ContactText as T;
    vec![
        (
            T::DisplayName,
            Some(contact.name.trim().to_owned()).filter(|n| !n.is_empty()),
        ),
        (T::GivenName, non_empty(&contact.given_name)),
        (T::Surname, non_empty(&contact.family_name)),
        (T::Nickname, non_empty(&contact.nickname)),
        (T::CompanyName, non_empty(&contact.organization)),
        (T::JobTitle, non_empty(&contact.title)),
    ]
}

/// Places phone numbers on Exchange keys. `taken` are keys already in use;
/// numbers without a free key are left out (Exchange has a fixed set).
fn place_phones<'a>(
    phones: impl Iterator<Item = &'a envelope_ical::ContactField>,
    taken: &mut Vec<String>,
) -> Vec<(String, String)> {
    let mut placed = Vec::new();
    for phone in phones {
        let key = ews_phone_keys(phone.kind.as_deref())
            .iter()
            .chain(ews_phone_keys(None))
            .find(|k| !taken.iter().any(|t| t == *k));
        if let Some(key) = key {
            taken.push((*key).to_owned());
            placed.push(((*key).to_owned(), phone.value.trim().to_owned()));
        }
    }
    placed
}

/// A new Exchange contact from the shared model.
fn ews_new(contact: &Contact) -> envelope_ews::NewContact {
    let mut taken = Vec::new();
    envelope_ews::NewContact {
        text: ews_texts(contact)
            .into_iter()
            .filter_map(|(field, value)| Some((field, value?)))
            .collect(),
        emails: contact
            .emails
            .iter()
            .filter(|e| !e.value.trim().is_empty())
            .take(3)
            .enumerate()
            .map(|(i, e)| (format!("EmailAddress{}", i + 1), e.value.trim().to_owned()))
            .collect(),
        phones: place_phones(
            contact.phones.iter().filter(|p| !p.value.trim().is_empty()),
            &mut taken,
        ),
        birthday: contact.birthday,
    }
}

/// The changes that turn the contact on the server into `desired`, leaving
/// what Envelope does not show (other phone keys, addresses, notes) alone.
fn ews_changes(
    server: &envelope_ews::Contact,
    desired: &Contact,
) -> Vec<envelope_ews::ContactChange> {
    use envelope_ews::{ContactChange as C, ContactText as T};
    let mut changes = Vec::new();
    let current = |field: T| -> Option<String> {
        let value = match field {
            T::DisplayName => &server.display_name,
            T::GivenName => &server.given_name,
            T::Surname => &server.surname,
            T::Nickname => &server.nickname,
            T::CompanyName => &server.company_name,
            T::JobTitle => &server.job_title,
        };
        non_empty(value)
    };
    for (field, value) in ews_texts(desired) {
        if current(field) != value {
            changes.push(C::Text(field, value));
        }
    }

    let wanted: Vec<String> = desired
        .emails
        .iter()
        .map(|e| e.value.trim().to_owned())
        .filter(|e| !e.is_empty())
        .take(3)
        .collect();
    for i in 0..3 {
        let key = format!("EmailAddress{}", i + 1);
        let have = server
            .email_addresses
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.address.clone());
        let want = wanted.get(i).cloned();
        if !have
            .as_deref()
            .map(str::to_lowercase)
            .eq(&want.as_deref().map(str::to_lowercase))
        {
            changes.push(C::Email(key, want));
        }
    }

    // Numbers already on the server stay on their keys; the rest of the
    // server's numbers go, and new ones take free keys.
    let mut kept: Vec<String> = Vec::new();
    let mut new = Vec::new();
    for phone in desired.phones.iter().filter(|p| !p.value.trim().is_empty()) {
        match server
            .phone_numbers
            .iter()
            .find(|s| !kept.contains(&s.key) && digits(&s.number) == digits(&phone.value))
        {
            Some(s) => kept.push(s.key.clone()),
            None => new.push(phone),
        }
    }
    let dropped: Vec<String> = server
        .phone_numbers
        .iter()
        .filter(|s| !kept.contains(&s.key))
        .map(|s| s.key.clone())
        .collect();
    let mut taken = kept.clone();
    let placed = place_phones(new.into_iter(), &mut taken);
    for key in &dropped {
        if !placed.iter().any(|(k, _)| k == key) {
            changes.push(C::Phone(key.clone(), None));
        }
    }
    for (key, number) in placed {
        changes.push(C::Phone(key, Some(number)));
    }

    if server.birthday.map(|b| b.date_naive()) != desired.birthday {
        changes.push(C::Birthday(desired.birthday));
    }
    changes
}

async fn ews_push(
    client: &envelope_ews::EwsClient,
    row: &ContactRow,
) -> std::result::Result<Done, Failure> {
    let id = row
        .remote_id
        .clone()
        .filter(|r| !r.is_empty())
        .map(|id| envelope_ews::ItemId {
            id,
            change_key: None,
        });
    if row.local_state == cdb::DELETED {
        if let Some(id) = &id
            && let Err(e) = client.delete_contact(id).await
            && !matches!(&e, envelope_ews::EwsError::Response(r) if r.code == envelope_ews::ResponseCode::ErrorItemNotFound)
        {
            return Err(e.into());
        }
        return Ok(Done::Deleted);
    }
    let server = match &id {
        Some(id) if row.local_state != cdb::CREATED => client
            .get_contacts(std::slice::from_ref(id))
            .await?
            .into_iter()
            .next()
            .and_then(|r| r.ok()),
        _ => None,
    };
    let saved = match (&id, server) {
        (Some(id), Some(server)) => {
            client
                .update_contact(id, &ews_changes(&server, &row.contact))
                .await?
        }
        // New, or gone from the server: create it (again).
        _ => {
            client
                .create_contact(
                    envelope_ews::DistinguishedFolder::Contacts,
                    &ews_new(&row.contact),
                )
                .await?
        }
    };
    let mut contact = row.contact.clone();
    contact.uid = saved.id.clone();
    Ok(Done::Saved(Box::new(Pushed {
        remote_id: saved.id.clone(),
        etag: saved.change_key.clone(),
        uid: saved.id,
        base: None,
        photo_url: None,
        contact,
    })))
}
