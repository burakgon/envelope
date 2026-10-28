//! IMAP synchronization (generic servers and Gmail).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use envelope_imap::{Auth, BodyPart, Client, FetchItem, IdleEnd, QresyncParams};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::mail::{self, AccountRow, FolderRow, FolderSyncState, FolderUpsert, MessageMeta};
use crate::db::misc::{self, OpItem, PendingOp};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mime;
use crate::model::*;

/// Headers + metadata requested for every message.
fn header_items(gmail: bool) -> String {
    let mut items = format!(
        "UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS ({})]",
        mime::SYNC_HEADER_FIELDS
    );
    if gmail {
        items.push_str(" X-GM-MSGID X-GM-THRID X-GM-LABELS");
    }
    items
}

fn flag_items(gmail: bool) -> &'static str {
    if gmail {
        "UID FLAGS X-GM-LABELS"
    } else {
        "UID FLAGS"
    }
}

const HEADER_CHUNK: usize = 250;
const BACKFILL_PER_CYCLE: usize = 3000;
const FULL_BODY_LIMIT: u32 = 256 * 1024;
const IDLE_MAX: Duration = Duration::from_secs(25 * 60);
const FULL_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn security(s: Security) -> envelope_imap::Security {
    match s {
        Security::Tls => envelope_imap::Security::Tls,
        Security::StartTls => envelope_imap::Security::StartTls,
        Security::None => envelope_imap::Security::None,
    }
}

/// Connects and logs in with explicit credentials.
pub async fn connect_with(
    server: &ServerConfig,
    config: &AccountConfig,
    auth: &Auth,
) -> Result<Client> {
    let mut cfg =
        envelope_imap::Config::new(server.host.clone(), server.port, security(server.security));
    cfg.accept_invalid_certs = config.accept_invalid_certs;
    let mut client = Client::connect(&cfg).await?;
    client.authenticate(auth).await?;
    let _ = client.enable(&["QRESYNC", "UTF8=ACCEPT"]).await;
    let _ = client.id("Envelope", env!("CARGO_PKG_VERSION")).await;
    Ok(client)
}

/// IMAP credentials for an account (OAuth tokens are refreshed as needed).
pub(crate) async fn imap_auth(
    engine: &Engine,
    account: &AccountRow,
    force_refresh: bool,
) -> Result<Auth> {
    let server_user = account
        .config
        .imap
        .as_ref()
        .map(|s| s.username.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| account.email.clone());
    match account.config.auth {
        AuthMethod::Password => {
            let password = engine
                .secrets
                .get(&crate::secrets::password_key(account.id))
                .await?
                .ok_or_else(|| Error::Auth("password missing, please sign in again".into()))?;
            Ok(Auth::Password {
                user: server_user,
                password,
            })
        }
        AuthMethod::OAuthGoogle | AuthMethod::OAuthMicrosoft => Ok(Auth::XOAuth2 {
            user: account.email.clone(),
            token: engine
                .tokens
                .access_token(account.id, force_refresh)
                .await?,
        }),
    }
}

/// Connects an account, retrying once with a refreshed OAuth token.
pub(crate) async fn connect(engine: &Engine, account: &AccountRow) -> Result<Client> {
    let server = account
        .config
        .imap
        .clone()
        .ok_or_else(|| Error::Invalid("no IMAP server configured".into()))?;
    let auth = imap_auth(engine, account, false).await?;
    match connect_with(&server, &account.config, &auth).await {
        Err(e) if e.is_auth() && account.config.auth != AuthMethod::Password => {
            let auth = imap_auth(engine, account, true).await?;
            connect_with(&server, &account.config, &auth).await
        }
        other => other,
    }
}

/// Runs the sync loop for an IMAP account until stopped.
pub async fn run(engine: Engine, account_id: i64, wake: Arc<Notify>, stop: CancellationToken) {
    let mut backoff = Duration::from_secs(2);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let account = match engine.account_row(account_id) {
            Ok(a) => a,
            Err(_) => return,
        };
        engine.set_sync_state(account_id, SyncState::Connecting);
        let result = async {
            let client = connect(&engine, &account).await?;
            let mut session = Session::new(engine.clone(), account, client);
            session.run(&wake, &stop).await
        }
        .await;
        match result {
            Ok(()) => return,
            Err(e) if e.is_auth() => {
                tracing::warn!(account_id, "authentication failed: {e}");
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
                tracing::warn!(account_id, "sync error: {e}");
                let state = if e.is_transient() {
                    SyncState::Offline
                } else {
                    SyncState::Error {
                        message: e.to_string(),
                    }
                };
                engine.set_sync_state(account_id, state);
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = wake.notified() => { backoff = Duration::from_secs(2); }
                    _ = tokio::time::sleep(backoff) => {
                        backoff = (backoff * 2).min(Duration::from_secs(300));
                    }
                }
            }
        }
    }
}

struct Session {
    engine: Engine,
    account: AccountRow,
    client: Client,
    gmail: bool,
    folders: Vec<FolderRow>,
    /// Gmail label name → folder id.
    labels: HashMap<String, i64>,
    last_full: Option<Instant>,
    last_folder_list: Option<Instant>,
    initial_done: bool,
}

impl Session {
    fn new(engine: Engine, account: AccountRow, client: Client) -> Session {
        let gmail = client.is_gmail() || account.kind == AccountKind::Gmail;
        Session {
            engine,
            account,
            client,
            gmail,
            folders: Vec::new(),
            labels: HashMap::new(),
            last_full: None,
            last_folder_list: None,
            initial_done: false,
        }
    }

    fn id(&self) -> i64 {
        self.account.id
    }

    async fn run(&mut self, wake: &Notify, stop: &CancellationToken) -> Result<()> {
        loop {
            if stop.is_cancelled() {
                let _ = self.client.logout().await;
                return Ok(());
            }
            self.process_ops().await?;
            let full_due = self
                .last_full
                .is_none_or(|t| t.elapsed() >= FULL_SYNC_INTERVAL);
            let mut more = false;
            if full_due {
                if self
                    .last_folder_list
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(15 * 60))
                {
                    self.sync_folder_list().await?;
                }
                more |= self.sync_all().await?;
                self.last_full = Some(Instant::now());
            } else if let Some(folder) = self.idle_folder() {
                more |= self.sync_folder(&folder, BACKFILL_PER_CYCLE).await?;
            }
            more |= self.prefetch_bodies(if more { 50 } else { 150 }).await?;
            self.process_ops().await?;
            if !self.initial_done && !more {
                self.initial_done = true;
            }
            self.engine.set_sync_state(self.id(), SyncState::Idle);

            if more {
                // Keep downloading history, but stay responsive to local changes.
                tokio::select! {
                    _ = stop.cancelled() => continue,
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(50)) => { self.last_full = None; }
                }
                continue;
            }
            self.wait_for_changes(wake, stop).await?;
        }
    }

    /// Folder watched with IDLE: INBOX (or All Mail on Gmail).
    fn idle_folder(&self) -> Option<FolderRow> {
        let role = if self.gmail {
            FolderRole::All
        } else {
            FolderRole::Inbox
        };
        self.folders.iter().find(|f| f.role == Some(role)).cloned()
    }

    async fn wait_for_changes(&mut self, wake: &Notify, stop: &CancellationToken) -> Result<()> {
        let remaining = FULL_SYNC_INTERVAL
            .saturating_sub(self.last_full.map(|t| t.elapsed()).unwrap_or_default());
        // Changes queued with an undo window (or a retry delay) must go out when
        // they come due, even if nothing else wakes us.
        let account_id = self.id();
        let op_due = self
            .engine
            .db
            .read(|c| misc::next_op_time(c, account_id))?
            .map(|at| Duration::from_millis((at - now_ms()).max(0) as u64 + 50));
        if op_due.is_some_and(|d| d <= Duration::from_millis(50)) {
            return Ok(());
        }
        if self.client.has_cap("IDLE")
            && let Some(folder) = self.idle_folder()
        {
            if self.client.selected() != Some(folder.remote_id.as_str()) {
                self.client.select(&folder.remote_id, None).await?;
            }
            let idle_max = remaining.max(Duration::from_secs(30)).min(IDLE_MAX);
            let max = op_due.map_or(idle_max, |d| d.min(idle_max));
            let for_ops = max < idle_max;
            let interrupt = async {
                tokio::select! {
                    _ = wake.notified() => {}
                    _ = stop.cancelled() => {}
                }
            };
            let (end, _) = self.client.idle(max, interrupt).await?;
            match end {
                IdleEnd::Changed => {
                    // Give the server a moment to settle bursts of updates.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                IdleEnd::Timeout if for_ops => {}
                IdleEnd::Timeout => self.last_full = None,
                IdleEnd::Interrupted => {
                    if !stop.is_cancelled() {
                        self.last_full = None;
                    }
                }
            }
        } else {
            let poll = remaining.max(Duration::from_secs(60));
            tokio::select! {
                _ = stop.cancelled() => {}
                _ = wake.notified() => { self.last_full = None; }
                _ = tokio::time::sleep(op_due.map_or(poll, |d| d.min(poll))) => {
                    if op_due.is_none_or(|d| d >= poll) {
                        self.last_full = None;
                    }
                }
            }
            // Keep the connection alive.
            self.client.noop().await?;
        }
        Ok(())
    }

    // ----- folders -----------------------------------------------------------

    async fn sync_folder_list(&mut self) -> Result<()> {
        let (entries, _) = self.client.list(false).await?;
        let mut entries = entries;
        let has_archive = entries.iter().any(|e| {
            e.special_use() == Some("\\Archive") || e.name.eq_ignore_ascii_case("archive")
        });
        if !self.gmail && !has_archive && !entries.is_empty() {
            // Archiving needs a destination; create the conventional folder.
            let delim = entries
                .iter()
                .find_map(|e| e.delimiter.clone())
                .unwrap_or_else(|| "/".into());
            let inbox_parent = entries
                .iter()
                .any(|e| e.name.starts_with(&format!("INBOX{delim}")))
                && !entries.iter().any(|e| {
                    e.name.eq_ignore_ascii_case("Sent")
                        || e.special_use() == Some("\\Sent") && !e.name.starts_with("INBOX")
                });
            let name = if inbox_parent {
                format!("INBOX{delim}Archive")
            } else {
                "Archive".to_string()
            };
            if self.client.create(&name).await.is_ok() {
                let _ = self.client.subscribe(&name).await;
                entries = self.client.list(false).await?.0;
            }
        }
        let upserts = folder_upserts(&entries, self.gmail);
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
        self.folders = self
            .engine
            .db
            .read(|c| mail::list_folder_rows(c, self.account.id))?;
        self.labels = self
            .folders
            .iter()
            .map(|f| (f.remote_id.clone(), f.id))
            .collect();
        Ok(())
    }

    fn role_folder(&self, role: FolderRole) -> Option<&FolderRow> {
        self.folders.iter().find(|f| f.role == Some(role))
    }

    /// Folders that hold real message copies (Gmail: All Mail, Trash, Spam).
    fn synced_folders(&self) -> Vec<FolderRow> {
        let mut list: Vec<FolderRow> = self
            .folders
            .iter()
            .filter(|f| f.selectable)
            .filter(|f| {
                !self.gmail
                    || matches!(
                        f.role,
                        Some(FolderRole::All | FolderRole::Trash | FolderRole::Junk)
                    )
            })
            .cloned()
            .collect();
        list.sort_by_key(|f| folder_priority(f.role));
        list
    }

    async fn sync_all(&mut self) -> Result<bool> {
        let folders = self.synced_folders();
        let total = folders.len().max(1);
        let mut more = false;
        for (i, folder) in folders.iter().enumerate() {
            if !self.initial_done {
                self.engine.set_sync_state(
                    self.id(),
                    SyncState::Syncing {
                        folder: Some(folder.name.clone()),
                        progress: Some(i as f32 / total as f32),
                    },
                );
            }
            match self.sync_folder(folder, BACKFILL_PER_CYCLE).await {
                Ok(m) => more |= m,
                Err(Error::Imap(envelope_imap::Error::No { text, .. })) => {
                    // Folder vanished or is inaccessible; the next LIST fixes it.
                    tracing::debug!("skipping folder {}: {text}", folder.remote_id);
                    self.last_folder_list = None;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(more)
    }

    /// Synchronizes one folder; returns true when older history remains.
    async fn sync_folder(&mut self, folder: &FolderRow, budget: usize) -> Result<bool> {
        // Re-read state: an earlier step in this cycle may have changed it.
        let folder = self.engine.db.read(|c| mail::get_folder(c, folder.id))?;
        let mut state = folder.sync_state.clone();
        let qresync = match (
            self.client.is_enabled("QRESYNC"),
            state.uid_validity,
            state.highest_modseq,
        ) {
            (true, Some(v), Some(m)) if m > 0 => Some(QresyncParams {
                uid_validity: v,
                modseq: m,
                known_uids: None,
            }),
            _ => None,
        };
        let sel = self
            .client
            .select(&folder.remote_id, qresync.as_ref())
            .await?;
        let uid_validity = sel.uid_validity.unwrap_or(0);
        if state.uid_validity.is_some_and(|v| v != uid_validity) {
            tracing::info!("UIDVALIDITY changed for {}, resyncing", folder.remote_id);
            self.wipe_folder(folder.id).await?;
            state = FolderSyncState::default();
        }
        let mut touched: BTreeSet<i64> = BTreeSet::new();
        let fresh = state.uid_validity.is_none();

        // Flag changes and expunges reported by QRESYNC during SELECT.
        if qresync.is_some() && !fresh {
            touched.extend(self.apply_vanished(&folder, &sel.vanished).await?);
            touched.extend(self.apply_flags(&folder, &sel.changed).await?);
        } else if !fresh {
            let modseq_changed = match (state.highest_modseq, sel.highest_modseq) {
                (Some(old), Some(new)) => new != old,
                _ => true,
            };
            if modseq_changed {
                let changes = match (state.highest_modseq, sel.highest_modseq.is_some()) {
                    (Some(old), true) => {
                        self.client
                            .uid_fetch_changed("1:*", flag_items(self.gmail), old)
                            .await?
                            .0
                    }
                    _ => {
                        // No CONDSTORE: refresh flags of the most recent messages.
                        let from = sel.uid_next.unwrap_or(1).saturating_sub(2000).max(1);
                        self.client
                            .uid_fetch(&format!("{from}:*"), flag_items(self.gmail))
                            .await?
                    }
                };
                touched.extend(self.apply_flags(&folder, &changes).await?);
            }
        }

        // New messages since last time.
        let mut local: HashMap<u32, i64> = self
            .engine
            .db
            .read(|c| mail::folder_uids(c, folder.id))?
            .into_iter()
            .map(|(uid, id, _)| (uid, id))
            .collect();
        if let (Some(prev_next), Some(next)) = (state.uid_next, sel.uid_next)
            && next > prev_next
            && state.complete
        {
            let items = self
                .client
                .uid_fetch(&format!("{prev_next}:*"), &header_items(self.gmail))
                .await?;
            let items: Vec<FetchItem> = items
                .into_iter()
                .filter(|i| i.uid.is_some_and(|u| u >= prev_next))
                .collect();
            for i in &items {
                if let Some(u) = i.uid {
                    local.insert(u, 0);
                }
            }
            touched.extend(self.store_headers(&folder, items, !fresh).await?);
        }

        // Full reconciliation when history is incomplete or counts disagree.
        let mut more = false;
        let count_mismatch = local.len() as u32 != sel.exists;
        if !state.complete || count_mismatch {
            let server: Vec<u32> = if sel.exists == 0 {
                Vec::new()
            } else {
                self.client.uid_search("ALL").await?
            };
            let server_set: HashSet<u32> = server.iter().copied().collect();
            let gone: Vec<u32> = local
                .keys()
                .copied()
                .filter(|u| !server_set.contains(u))
                .collect();
            if !gone.is_empty() {
                touched.extend(self.apply_vanished(&folder, &gone).await?);
            }
            let mut missing: Vec<u32> = server
                .into_iter()
                .filter(|u| !local.contains_key(u))
                .collect();
            missing.sort_unstable_by(|a, b| b.cmp(a));
            let take = missing.len().min(budget);
            for chunk in missing[..take].chunks(HEADER_CHUNK) {
                let set = envelope_imap::uid_set_string(chunk);
                let items = self
                    .client
                    .uid_fetch(&set, &header_items(self.gmail))
                    .await?;
                let changed = self
                    .store_headers(&folder, items, !fresh && state.complete)
                    .await?;
                touched.extend(changed);
            }
            more = missing.len() > take;
            state.complete = !more;
            state.oldest_synced_uid = missing[..take].last().copied().or(state.oldest_synced_uid);
        }

        state.uid_validity = Some(uid_validity);
        state.uid_next = sel.uid_next;
        state.highest_modseq = sel.highest_modseq;
        state.message_count = Some(sel.exists);
        state.last_sync = Some(now_ms());
        let folder_id = folder.id;
        let touched_vec: Vec<i64> = touched.iter().copied().collect();
        let changed_folders = self
            .engine
            .db
            .write_async(move |tx| {
                mail::set_folder_sync_state(tx, folder_id, &state)?;
                let mut folders = mail::refresh_threads(tx, &touched)?;
                folders.insert(folder_id);
                mail::refresh_folder_counts(tx, &folders)?;
                Ok(folders)
            })
            .await?;
        if !touched_vec.is_empty() {
            self.engine.emit(EngineEvent::MailChanged {
                account_id: self.id(),
                folder_ids: changed_folders.into_iter().collect(),
                thread_ids: touched_vec,
            });
        }
        Ok(more)
    }

    /// Removes local copies of UIDs that no longer exist on the server.
    async fn apply_vanished(&self, folder: &FolderRow, uids: &[u32]) -> Result<BTreeSet<i64>> {
        if uids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let folder_id = folder.id;
        let uids = uids.to_vec();
        self.engine
            .db
            .write_async(move |tx| {
                let mut threads = BTreeSet::new();
                for uid in uids {
                    let id: Option<i64> = rusqlite::OptionalExtension::optional(tx.query_row(
                        "SELECT message_id FROM message_folders WHERE folder_id = ? AND uid = ?",
                        rusqlite::params![folder_id, uid],
                        |r| r.get(0),
                    ))?;
                    if let Some(id) = id
                        && let Some(t) = mail::remove_from_folder(tx, folder_id, id)?
                    {
                        threads.insert(t);
                    }
                }
                Ok(threads)
            })
            .await
    }

    /// Applies flag (and Gmail label) updates.
    async fn apply_flags(&self, folder: &FolderRow, items: &[FetchItem]) -> Result<BTreeSet<i64>> {
        let updates: Vec<(u32, i64, Vec<String>, Option<Vec<i64>>)> = items
            .iter()
            .filter_map(|i| {
                let uid = i.uid?;
                let flags = i.flags.as_ref()?;
                let (bits, keywords) = mime::flags_from_imap(flags);
                let labels = i.gm_labels.as_ref().map(|l| self.gmail_label_folders(l));
                Some((uid, bits, keywords, labels))
            })
            .collect();
        if updates.is_empty() {
            return Ok(BTreeSet::new());
        }
        let folder_id = folder.id;
        self.engine
            .db
            .write_async(move |tx| {
                let mut threads = BTreeSet::new();
                for (uid, bits, _keywords, labels) in updates {
                    let row: Option<(i64, i64, i64)> = rusqlite::OptionalExtension::optional(tx.query_row(
                        "SELECT m.id, m.thread_id, m.flags FROM message_folders mf JOIN messages m ON m.id = mf.message_id
                         WHERE mf.folder_id = ? AND mf.uid = ?",
                        rusqlite::params![folder_id, uid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    ))?;
                    let Some((id, thread, old)) = row else { continue };
                    if old != bits {
                        mail::set_message_flags(tx, id, bits)?;
                        threads.insert(thread);
                    }
                    if let Some(labels) = labels {
                        mail::set_message_labels(tx, id, &labels)?;
                        threads.insert(thread);
                    }
                }
                Ok(threads)
            })
            .await
    }

    fn gmail_label_folders(&self, labels: &[String]) -> Vec<i64> {
        labels
            .iter()
            .filter_map(|l| {
                let role = match l.to_ascii_lowercase().as_str() {
                    "\\inbox" | "inbox" => Some(FolderRole::Inbox),
                    "\\sent" => Some(FolderRole::Sent),
                    "\\draft" | "\\drafts" => Some(FolderRole::Drafts),
                    "\\starred" => Some(FolderRole::Starred),
                    "\\important" => Some(FolderRole::Important),
                    "\\trash" | "\\spam" | "\\junk" | "\\all" => return None,
                    _ => None,
                };
                match role {
                    Some(r) => self.role_folder(r).map(|f| f.id),
                    None => self.labels.get(l).copied(),
                }
            })
            .collect()
    }

    /// Stores freshly fetched headers. Returns touched thread ids.
    async fn store_headers(
        &self,
        folder: &FolderRow,
        items: Vec<FetchItem>,
        notify_new: bool,
    ) -> Result<BTreeSet<i64>> {
        if items.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut prepared: Vec<(
            u32,
            MessageMeta,
            Vec<mail::AttachmentMeta>,
            Option<Vec<i64>>,
        )> = Vec::with_capacity(items.len());
        for item in items {
            let Some(uid) = item.uid else { continue };
            let header = item
                .header
                .clone()
                .or_else(|| item.sections.first().map(|(_, d)| d.clone()))
                .unwrap_or_default();
            let mut meta = mime::meta_from_headers(&header);
            if meta.date == 0 {
                meta.date = item
                    .internal_date
                    .map(|d| d.timestamp_millis())
                    .unwrap_or_else(now_ms);
            }
            let (bits, keywords) = mime::flags_from_imap(item.flags.as_deref().unwrap_or(&[]));
            meta.flags = bits;
            meta.keywords = keywords;
            meta.size = item.size.unwrap_or(0) as i64;
            let mut attachments = Vec::new();
            if let Some(bs) = &item.bodystructure {
                let plan = mime::body_plan(bs);
                if let Some(cal) = plan.calendar {
                    meta.calendar_method = Some(
                        cal.param("method")
                            .unwrap_or("REQUEST")
                            .to_ascii_uppercase(),
                    );
                }
                attachments = plan.attachments;
                meta.has_attachments = attachments.iter().any(|a| !a.inline);
                meta.structure = serde_json::to_string(bs).ok();
            }
            if self.gmail {
                meta.remote_id = item.gm_msgid.map(|m| m.to_string());
                meta.remote_thread_id = item.gm_thrid.map(|t| t.to_string());
            }
            let labels = item.gm_labels.as_ref().map(|l| self.gmail_label_folders(l));
            prepared.push((uid, meta, attachments, labels));
        }
        let account_id = self.id();
        let folder_id = folder.id;
        let is_inbox_like = matches!(folder.role, Some(FolderRole::Inbox | FolderRole::All));
        let is_inbox = folder.role == Some(FolderRole::Inbox);
        let is_junk = folder.role == Some(FolderRole::Junk);
        let folder_remote = folder.remote_id.clone();
        let my_email = self.account.email.to_lowercase();
        let (threads, mut fresh_unread, arrivals) = self
            .engine
            .db
            .write_async(move |tx| {
                // Messages with a queued move or delete are leaving this folder:
                // seeing them here again before the server catches up must not
                // bring them back as new mail.
                let leaving: HashSet<u32> = misc::queued_ops(tx, account_id)?
                    .into_iter()
                    .flat_map(|op| match op {
                        PendingOp::Move { items, .. } | PendingOp::Expunge { items } => items,
                        _ => Vec::new(),
                    })
                    .filter(|i| i.folder == folder_remote)
                    .filter_map(|i| i.uid)
                    .collect();
                let mut threads = BTreeSet::new();
                let mut fresh_unread = Vec::new();
                let mut arrivals = Vec::new();
                let mine = crate::crypto::my_emails(tx)?;
                for (uid, meta, attachments, labels) in prepared {
                    if leaving.contains(&uid) {
                        continue;
                    }
                    let stored = mail::upsert_message(tx, account_id, folder_id, Some(uid), &meta)?;
                    if stored.is_new {
                        if !attachments.is_empty() {
                            mail::replace_attachments(tx, stored.message_id, &attachments)?;
                        }
                        if !is_junk {
                            crate::crypto::on_headers(tx, &meta, &mine)?;
                        }
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
                        if meta.flags & Flags::SEEN == 0 && !from_me {
                            fresh_unread.push(stored.thread_id);
                        }
                        if notify_new && is_inbox && !from_me && meta.flags & Flags::DRAFT == 0 {
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
                    if let Some(labels) = labels {
                        mail::set_message_labels(tx, stored.message_id, &labels)?;
                    }
                    threads.insert(stored.thread_id);
                }
                Ok((threads, fresh_unread, arrivals))
            })
            .await?;
        // The user's rules run on new inbox mail before anyone is notified.
        let quiet = crate::rules::apply(&self.engine, &arrivals);
        fresh_unread.retain(|t| !quiet.contains(t));
        if notify_new && is_inbox_like && !fresh_unread.is_empty() {
            let ids: Vec<i64> = fresh_unread;
            let engine = self.engine.clone();
            let inbox = self.role_folder(FolderRole::Inbox).map(|f| f.id);
            // Notify only for threads that actually landed in the inbox. The
            // threads are refreshed first so that they name their senders.
            let summaries = engine.db.write(move |tx| {
                mail::refresh_threads(tx, &ids.iter().copied().collect())?;
                let mut list = mail::thread_summaries(tx, &ids)?;
                if let Some(inbox) = inbox {
                    list.retain(|t| t.folder_ids.contains(&inbox) || t.folder_ids.is_empty());
                }
                Ok(list)
            })?;
            if !summaries.is_empty() {
                engine.emit(EngineEvent::NewMail {
                    account_id,
                    threads: summaries,
                });
            }
        }
        Ok(threads)
    }

    async fn wipe_folder(&self, folder_id: i64) -> Result<()> {
        self.engine
            .db
            .write_async(move |tx| {
                let ids: Vec<i64> = {
                    let mut st =
                        tx.prepare("SELECT message_id FROM message_folders WHERE folder_id = ?")?;
                    st.query_map([folder_id], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?
                };
                let mut threads = BTreeSet::new();
                for id in ids {
                    if let Some(t) = mail::remove_from_folder(tx, folder_id, id)? {
                        threads.insert(t);
                    }
                }
                mail::refresh_threads(tx, &threads)?;
                Ok(())
            })
            .await
    }

    // ----- bodies ---------------------------------------------------------------

    /// Downloads bodies of recent messages. Returns true if more are waiting.
    async fn prefetch_bodies(&mut self, limit: i64) -> Result<bool> {
        let days = self.account.config.body_sync_days.unwrap_or(365);
        let since = now_ms() - (days as i64) * 86_400_000;
        let account_id = self.id();
        let ids = self
            .engine
            .db
            .read(|c| mail::messages_without_body(c, account_id, Some(since), false, limit + 1))?;
        if ids.is_empty() {
            return Ok(false);
        }
        let more = ids.len() as i64 > limit;
        let ids: Vec<i64> = ids.into_iter().take(limit as usize).collect();
        let fetched = fetch_bodies(&self.engine, &mut self.client, &ids).await?;
        if fetched > 0 {
            let threads: Vec<i64> = self.engine.db.read(|c| {
                let placeholders = vec!["?"; ids.len()].join(",");
                let mut st = c.prepare(&format!(
                    "SELECT DISTINCT thread_id FROM messages WHERE id IN ({placeholders})"
                ))?;
                let rows = st.query_map(rusqlite::params_from_iter(ids.iter()), |r| r.get(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<i64>>>()?)
            })?;
            let set: BTreeSet<i64> = threads.iter().copied().collect();
            let folders = self
                .engine
                .db
                .write_async(move |tx| mail::refresh_threads(tx, &set))
                .await?;
            self.engine.emit(EngineEvent::MailChanged {
                account_id,
                folder_ids: folders.into_iter().collect(),
                thread_ids: threads,
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
                    Err(e) if e.is_transient() => return Err(e),
                    Err(e) => {
                        tracing::warn!("operation failed ({} attempts): {e}", op.attempts + 1);
                        if op.attempts >= 4 {
                            self.engine.db.write(|tx| misc::complete_op(tx, op.id))?;
                            // Re-read server state to undo our optimistic change.
                            self.last_full = None;
                        } else {
                            let delay = 5_000 * (1 << op.attempts.min(6));
                            self.engine
                                .db
                                .write(|tx| misc::fail_op(tx, op.id, &e.to_string(), delay))?;
                        }
                    }
                }
            }
            // Ops that are delayed (undo window) stay queued; stop when nothing is due.
            let due = self.engine.db.read(|c| misc::due_ops(c, account_id))?;
            if due.is_empty() {
                return Ok(());
            }
        }
    }

    async fn select(&mut self, folder: &str) -> Result<()> {
        if self.client.selected() != Some(folder) {
            self.client.select(folder, None).await?;
        }
        Ok(())
    }

    /// Server locations (folder, uid) of messages, grouped by folder.
    fn group_by_folder(&self, message_ids: &[i64]) -> Result<HashMap<String, Vec<u32>>> {
        let locations = self.engine.db.read(|c| mail::locations(c, message_ids))?;
        let mut groups: HashMap<String, Vec<u32>> = HashMap::new();
        for l in locations {
            if let Some(uid) = l.uid {
                groups.entry(l.folder_remote).or_default().push(uid);
            }
        }
        Ok(groups)
    }

    async fn apply_op(&mut self, op: &PendingOp) -> Result<()> {
        match op {
            PendingOp::Flags {
                message_ids,
                add,
                remove,
            } => {
                for (folder, uids) in self.group_by_folder(message_ids)? {
                    self.select(&folder).await?;
                    let add: Vec<&str> = add.iter().map(String::as_str).collect();
                    let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
                    if !add.is_empty() {
                        self.client.uid_store(&uids, "+FLAGS.SILENT", &add).await?;
                    }
                    if !remove.is_empty() {
                        self.client
                            .uid_store(&uids, "-FLAGS.SILENT", &remove)
                            .await?;
                    }
                }
                Ok(())
            }
            PendingOp::Labels {
                message_ids,
                add,
                remove,
            } => {
                for (folder, uids) in self.group_by_folder(message_ids)? {
                    self.select(&folder).await?;
                    let add: Vec<&str> = add.iter().map(String::as_str).collect();
                    let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
                    self.client.gmail_store_labels(&uids, true, &add).await?;
                    self.client
                        .gmail_store_labels(&uids, false, &remove)
                        .await?;
                }
                Ok(())
            }
            PendingOp::Move { items, dest_folder } => {
                let mut by_folder: HashMap<String, Vec<&OpItem>> = HashMap::new();
                for item in items {
                    if item.uid.is_some() && item.folder != *dest_folder {
                        by_folder.entry(item.folder.clone()).or_default().push(item);
                    }
                }
                for (folder, items) in by_folder {
                    self.select(&folder).await?;
                    let uids: Vec<u32> = items.iter().filter_map(|i| i.uid).collect();
                    let pairs = self.client.uid_move(&uids, dest_folder).await?;
                    // Record new UIDs so the next sync does not download them again.
                    if !pairs.is_empty() {
                        let dest = dest_folder.clone();
                        let account_id = self.id();
                        let map: Vec<(i64, u32)> = pairs
                            .iter()
                            .filter_map(|(src, dst)| {
                                items
                                    .iter()
                                    .find(|i| i.uid == Some(*src))
                                    .map(|i| (i.message_id, *dst))
                            })
                            .collect();
                        let moved = map.clone();
                        self.engine.db.write(|tx| {
                            if let Some(f) = mail::folder_by_remote(tx, account_id, &dest)? {
                                for (mid, uid) in map {
                                    tx.execute(
                                        "UPDATE message_folders SET uid = ? WHERE folder_id = ? AND message_id = ? AND uid IS NULL",
                                        rusqlite::params![uid, f.id, mid],
                                    )?;
                                }
                            }
                            Ok(())
                        })?;
                        // Read/star changes made while the move was queued could not reach
                        // the old location; carry them over to the new one.
                        let (seen, flagged) = self.engine.db.read(|c| {
                            let mut seen = Vec::new();
                            let mut flagged = Vec::new();
                            for (mid, uid) in &moved {
                                let flags: i64 =
                                    rusqlite::OptionalExtension::optional(c.query_row(
                                        "SELECT flags FROM messages WHERE id = ?",
                                        [mid],
                                        |r| r.get(0),
                                    ))?
                                    .unwrap_or(0);
                                if flags & Flags::SEEN != 0 {
                                    seen.push(*uid);
                                }
                                if flags & Flags::FLAGGED != 0 {
                                    flagged.push(*uid);
                                }
                            }
                            Ok((seen, flagged))
                        })?;
                        if !seen.is_empty() || !flagged.is_empty() {
                            self.select(dest_folder).await?;
                            if !seen.is_empty() {
                                self.client
                                    .uid_store(&seen, "+FLAGS.SILENT", &["\\Seen"])
                                    .await?;
                            }
                            if !flagged.is_empty() {
                                self.client
                                    .uid_store(&flagged, "+FLAGS.SILENT", &["\\Flagged"])
                                    .await?;
                            }
                        }
                    }
                }
                Ok(())
            }
            PendingOp::Expunge { items } => {
                let mut by_folder: HashMap<String, Vec<u32>> = HashMap::new();
                for item in items {
                    if let Some(uid) = item.uid {
                        by_folder.entry(item.folder.clone()).or_default().push(uid);
                    }
                }
                for (folder, uids) in by_folder {
                    self.select(&folder).await?;
                    self.client
                        .uid_store(&uids, "+FLAGS.SILENT", &["\\Deleted"])
                        .await?;
                    self.client.uid_expunge(&uids).await?;
                }
                Ok(())
            }
            PendingOp::SaveDraft { draft_id } => self.save_draft(*draft_id).await,
            PendingOp::DeleteDraft { remote_uid, .. } => {
                if let (Some(uid), Some(drafts)) =
                    (remote_uid, self.role_folder(FolderRole::Drafts).cloned())
                {
                    self.select(&drafts.remote_id).await?;
                    self.client
                        .uid_store(&[*uid], "+FLAGS.SILENT", &["\\Deleted"])
                        .await?;
                    self.client.uid_expunge(&[*uid]).await?;
                }
                Ok(())
            }
            PendingOp::AppendSent { outbox_id } => {
                let id = *outbox_id;
                let Some(row) = self.engine.db.read(|c| misc::outbox_get(c, id))? else {
                    return Ok(());
                };
                if let Some(sent) = self.role_folder(FolderRole::Sent).cloned() {
                    self.client
                        .append(&sent.remote_id, &["\\Seen"], None, &row.mime)
                        .await?;
                }
                self.engine
                    .db
                    .write(|tx| misc::outbox_delete(tx, id).map(|_| ()))?;
                Ok(())
            }
        }
    }

    async fn save_draft(&mut self, draft_id: i64) -> Result<()> {
        let Ok(draft) = self.engine.db.read(|c| misc::draft_get(c, draft_id)) else {
            return Ok(()); // already sent or discarded
        };
        let Some(drafts) = self.role_folder(FolderRole::Drafts).cloned() else {
            return Ok(());
        };
        let rendered = crate::sync::render_draft(&self.engine, &draft.data, true).await?;
        let appended = self
            .client
            .append(
                &drafts.remote_id,
                &["\\Draft", "\\Seen"],
                None,
                &rendered.mime,
            )
            .await?;
        if let Some(old) = draft.remote_uid {
            self.select(&drafts.remote_id).await?;
            self.client
                .uid_store(&[old], "+FLAGS.SILENT", &["\\Deleted"])
                .await?;
            self.client.uid_expunge(&[old]).await?;
        }
        let uid = appended.map(|(_, uid)| uid);
        self.engine
            .db
            .write(|tx| misc::draft_set_remote(tx, draft_id, uid, None))?;
        Ok(())
    }
}

pub(crate) fn folder_priority(role: Option<FolderRole>) -> u8 {
    match role {
        Some(FolderRole::Inbox) => 0,
        Some(FolderRole::All) => 1,
        Some(FolderRole::Sent) => 2,
        Some(FolderRole::Drafts) => 3,
        Some(FolderRole::Archive) => 4,
        None => 5,
        Some(FolderRole::Starred | FolderRole::Important) => 6,
        Some(FolderRole::Junk) => 7,
        Some(FolderRole::Trash) => 8,
    }
}

fn role_by_name(name: &str) -> Option<FolderRole> {
    let n = name.to_lowercase();
    let last = n.rsplit(['/', '.']).next().unwrap_or(&n).trim().to_string();
    const SENT: &[&str] = &[
        "sent",
        "sent items",
        "sent messages",
        "sent mail",
        "gönderilmiş öğeler",
        "gönderilenler",
        "gesendet",
        "gesendete elemente",
        "envoyés",
        "éléments envoyés",
        "enviados",
        "elementos enviados",
        "inviata",
        "posta inviata",
        "verzonden",
    ];
    const DRAFTS: &[&str] = &[
        "drafts",
        "draft",
        "taslaklar",
        "entwürfe",
        "brouillons",
        "borradores",
        "bozze",
        "concepten",
    ];
    const TRASH: &[&str] = &[
        "trash",
        "deleted",
        "deleted items",
        "deleted messages",
        "bin",
        "çöp",
        "çöp kutusu",
        "silinmiş öğeler",
        "papierkorb",
        "gelöschte elemente",
        "corbeille",
        "papelera",
        "cestino",
        "prullenbak",
    ];
    const JUNK: &[&str] = &[
        "junk",
        "spam",
        "junk e-mail",
        "junk email",
        "bulk mail",
        "önemsiz",
        "önemsiz e-posta",
        "spam klasörü",
        "courrier indésirable",
        "correo no deseado",
        "posta indesiderata",
    ];
    const ARCHIVE: &[&str] = &[
        "archive", "archives", "arşiv", "archiv", "archivo", "archivio", "archief",
    ];
    if n == "inbox" {
        Some(FolderRole::Inbox)
    } else if SENT.contains(&last.as_str()) {
        Some(FolderRole::Sent)
    } else if DRAFTS.contains(&last.as_str()) {
        Some(FolderRole::Drafts)
    } else if TRASH.contains(&last.as_str()) {
        Some(FolderRole::Trash)
    } else if JUNK.contains(&last.as_str()) {
        Some(FolderRole::Junk)
    } else if ARCHIVE.contains(&last.as_str()) {
        Some(FolderRole::Archive)
    } else {
        None
    }
}

fn folder_upserts(entries: &[envelope_imap::ListEntry], gmail: bool) -> Vec<FolderUpsert> {
    let mut used_roles: HashSet<FolderRole> = HashSet::new();
    let mut out = Vec::new();
    // Special-use attributes take precedence over name heuristics.
    let mut sorted: Vec<&envelope_imap::ListEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| (e.special_use().is_none(), e.name.len()));
    for e in sorted {
        let role = if e.name.eq_ignore_ascii_case("INBOX") {
            Some(FolderRole::Inbox)
        } else {
            match e.special_use() {
                Some("\\Sent") => Some(FolderRole::Sent),
                Some("\\Drafts") => Some(FolderRole::Drafts),
                Some("\\Trash") => Some(FolderRole::Trash),
                Some("\\Junk") => Some(FolderRole::Junk),
                Some("\\Archive") => Some(FolderRole::Archive),
                Some("\\All") => Some(FolderRole::All),
                Some("\\Flagged") if gmail => Some(FolderRole::Starred),
                _ if gmail && e.has_attr("\\Important") => Some(FolderRole::Important),
                _ if gmail => None,
                _ => role_by_name(&e.name),
            }
        };
        let role = role.filter(|r| used_roles.insert(*r));
        let delim = e.delimiter.clone();
        let (parent, name) = match &delim {
            Some(d) if !d.is_empty() && e.name.contains(d.as_str()) => {
                let (p, n) = e.name.rsplit_once(d.as_str()).unwrap_or(("", &e.name));
                (Some(p.to_string()), n.to_string())
            }
            _ => (None, e.name.clone()),
        };
        // Gmail's "[Gmail]" container is not a real folder.
        let parent = parent
            .filter(|p| !(gmail && p.starts_with("[Gmail]") || p.starts_with("[Google Mail]")));
        let selectable = e.is_selectable();
        if gmail && !selectable && (e.name == "[Gmail]" || e.name == "[Google Mail]") {
            continue;
        }
        let kind = if gmail && role.is_none() {
            "label"
        } else {
            "folder"
        };
        let display_name = match role {
            Some(FolderRole::Inbox) => "Inbox".to_string(),
            _ => name,
        };
        out.push(FolderUpsert {
            remote_id: e.name.clone(),
            name: display_name,
            path: e.name.clone(),
            parent_remote_id: parent,
            role,
            kind: kind.into(),
            delimiter: delim,
            selectable,
            sort_order: folder_priority(role) as i64,
            color: None,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Body downloads (shared by the sync worker and interactive fetches)

struct BodyTarget {
    message_id: i64,
    folder: String,
    uid: u32,
    size: u32,
    structure: Option<BodyPart>,
}

/// Fetches and stores the bodies of the given messages. Returns how many succeeded.
pub(crate) async fn fetch_bodies(
    engine: &Engine,
    client: &mut Client,
    message_ids: &[i64],
) -> Result<usize> {
    let locations = engine.db.read(|c| mail::locations(c, message_ids))?;
    let mut targets: Vec<BodyTarget> = Vec::new();
    let mut seen = HashSet::new();
    // Prefer non-trash locations.
    let mut locations = locations;
    locations.sort_by_key(|l| matches!(l.role, Some(FolderRole::Trash | FolderRole::Junk)));
    for l in locations {
        let Some(uid) = l.uid else { continue };
        if !seen.insert(l.message_id) {
            continue;
        }
        let (structure, size) = engine
            .db
            .read(|c| mail::message_structure(c, l.message_id))?;
        targets.push(BodyTarget {
            message_id: l.message_id,
            folder: l.folder_remote,
            uid,
            size: size as u32,
            structure: structure.and_then(|s| serde_json::from_str(&s).ok()),
        });
    }
    let mut by_folder: HashMap<String, Vec<BodyTarget>> = HashMap::new();
    for t in targets {
        by_folder.entry(t.folder.clone()).or_default().push(t);
    }
    let mut done = 0;
    for (folder, targets) in by_folder {
        if client.selected() != Some(folder.as_str()) {
            client.select(&folder, None).await?;
        }
        let (small, large): (Vec<&BodyTarget>, Vec<&BodyTarget>) = targets.iter().partition(|t| {
            t.size <= FULL_BODY_LIMIT || t.structure.as_ref().is_none_or(mime::has_openpgp)
        });
        // Small messages: whole message in batches.
        for chunk in small.chunks(20) {
            let uids: Vec<u32> = chunk.iter().map(|t| t.uid).collect();
            let items = client
                .uid_fetch(&envelope_imap::uid_set_string(&uids), "UID BODY.PEEK[]")
                .await?;
            let mut results = Vec::new();
            for t in chunk {
                let raw = items
                    .iter()
                    .find(|i| i.uid == Some(t.uid))
                    .and_then(|i| i.body.clone());
                results.push((t.message_id, t.structure.is_some(), raw));
            }
            done += store_full_bodies(engine, results).await?;
        }
        // Large messages: only the displayable parts.
        for t in large {
            let Some(structure) = &t.structure else {
                continue;
            };
            let plan = mime::body_plan(structure);
            let mut sections = Vec::new();
            for p in [plan.html, plan.text, plan.calendar].into_iter().flatten() {
                sections.push(format!("BODY.PEEK[{}]", p.part_id));
            }
            if sections.is_empty() {
                engine
                    .db
                    .write(|tx| mail::store_body(tx, t.message_id, None, Some(""), "", None))?;
                done += 1;
                continue;
            }
            let items = client
                .uid_fetch(&t.uid.to_string(), &format!("UID {}", sections.join(" ")))
                .await?;
            let Some(item) = items.into_iter().find(|i| i.uid == Some(t.uid)) else {
                continue;
            };
            let html = plan.html.and_then(|p| {
                item.section(&p.part_id)
                    .map(|raw| mime::decode_text_part(raw, p))
            });
            let mut text = plan.text.and_then(|p| {
                item.section(&p.part_id)
                    .map(|raw| mime::decode_text_part(raw, p))
            });
            if text.is_none()
                && let Some(h) = &html
            {
                text = Some(mime::html_to_text(h));
            }
            let snippet = mime::make_snippet(text.as_deref().unwrap_or(""));
            let id = t.message_id;
            engine.db.write(|tx| {
                mail::store_body(tx, id, html.as_deref(), text.as_deref(), &snippet, None)
            })?;
            done += 1;
        }
    }
    Ok(done)
}

async fn store_full_bodies(
    engine: &Engine,
    results: Vec<(i64, bool, Option<Vec<u8>>)>,
) -> Result<usize> {
    let (n, encrypted) = engine
        .db
        .write_async(move |tx| {
            let mut n = 0;
            let mut encrypted = Vec::new();
            for (id, has_structure, raw) in results {
                let parsed = raw
                    .as_deref()
                    .and_then(|r| mime::parse_full(r).map(|p| (r, p)));
                match parsed {
                    Some((raw, (meta, mut body))) => {
                        let pgp = crate::crypto::on_source(tx, id, raw, &meta, &mut body)?;
                        if pgp.is_some_and(|k| k.is_encrypted()) {
                            encrypted.push(id);
                        }
                        // Attachments with IMAP part ids came from BODYSTRUCTURE
                        // already (an encrypted message's are only known
                        // after decryption).
                        let atts = if pgp.is_some_and(|k| k.is_encrypted()) {
                            Some(&[][..])
                        } else if has_structure {
                            None
                        } else {
                            Some(body.attachments.as_slice())
                        };
                        mail::store_body(
                            tx,
                            id,
                            body.html.as_deref(),
                            body.text.as_deref(),
                            &body.snippet,
                            atts,
                        )?;
                        n += 1;
                    }
                    None => mail::mark_body_failed(tx, id)?,
                }
            }
            Ok((n, encrypted))
        })
        .await?;
    engine.decrypt_later(encrypted);
    Ok(n)
}

/// Downloads an attachment part.
pub(crate) async fn fetch_part(
    client: &mut Client,
    folder: &str,
    uid: u32,
    part_id: &str,
    encoding: &str,
) -> Result<Vec<u8>> {
    if client.selected() != Some(folder) {
        client.select(folder, None).await?;
    }
    if let Some(index) = part_id.strip_prefix("mp:") {
        let items = client
            .uid_fetch(&uid.to_string(), "UID BODY.PEEK[]")
            .await?;
        let raw = items
            .into_iter()
            .find_map(|i| i.body)
            .ok_or_else(|| Error::NotFound("message".into()))?;
        return mime::extract_part(&raw, &format!("mp:{index}"))
            .ok_or_else(|| Error::NotFound("attachment".into()));
    }
    let items = client
        .uid_fetch(&uid.to_string(), &format!("UID BODY.PEEK[{part_id}]"))
        .await?;
    let raw = items
        .into_iter()
        .find_map(|i| i.section(part_id).map(<[u8]>::to_vec))
        .ok_or_else(|| Error::NotFound("attachment".into()))?;
    Ok(mime::decode_binary(&raw, encoding))
}

/// Downloads the raw message (view source / export).
pub(crate) async fn fetch_raw(client: &mut Client, folder: &str, uid: u32) -> Result<Vec<u8>> {
    if client.selected() != Some(folder) {
        client.select(folder, None).await?;
    }
    let items = client
        .uid_fetch(&uid.to_string(), "UID BODY.PEEK[]")
        .await?;
    items
        .into_iter()
        .find_map(|i| i.body)
        .ok_or_else(|| Error::NotFound("message".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_roles() {
        assert_eq!(role_by_name("INBOX"), Some(FolderRole::Inbox));
        assert_eq!(role_by_name("Gönderilmiş Öğeler"), Some(FolderRole::Sent));
        assert_eq!(role_by_name("INBOX.Sent Items"), Some(FolderRole::Sent));
        assert_eq!(role_by_name("Papierkorb"), Some(FolderRole::Trash));
        assert_eq!(role_by_name("Projects"), None);
    }
}
