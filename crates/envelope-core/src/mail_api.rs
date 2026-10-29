//! Mail operations exposed to the UI. Changes are applied locally first
//! (instant UI) and queued for the server.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::db::mail::{self, FolderRow};
use crate::db::misc::{self, OpItem, PendingOp};
use crate::db::now_ms;
use crate::engine::{Engine, ImapSlot};
use crate::error::{Error, Result};
use crate::model::*;

/// Delay before moves hit the server, so they can be undone cheaply.
const UNDO_WINDOW_MS: i64 = 6_000;

/// Information needed to revert an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoToken {
    pub account_ops: Vec<(i64, i64)>,
    /// message id → previous (folder id, uid) locations
    pub locations: Vec<(i64, Vec<(i64, Option<u32>)>)>,
    pub flags: Vec<(i64, i64)>,
    pub snoozed: Vec<(i64, Option<i64>)>,
}

/// What "Mark all as read" changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkedRead {
    /// Conversations marked read.
    pub threads: i64,
    /// Messages that were unread: marking them unread again undoes it.
    pub message_ids: Vec<i64>,
}

/// A change to the follow-up reminders of conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FollowUpChange {
    /// No more reminders ("Done").
    Done,
    /// Remind again at this time (unix ms) unless someone replies first.
    RemindAt { at: i64 },
    /// Back in Follow up, as when the reminder came due (undo).
    Due,
}

impl Engine {
    // ----- listing -------------------------------------------------------------

    pub fn folders(&self) -> Result<Vec<Folder>> {
        self.db.read(mail::list_folders)
    }

    pub fn threads(&self, q: &ThreadQuery) -> Result<ThreadPage> {
        let search = q.search.as_deref().and_then(crate::search::compile);
        self.db.read(|c| {
            let sql = mail::list_sql(c, &q.mailbox, &q.filter, search)?;
            mail::query_threads(c, &sql, q.offset.max(0), q.limit.clamp(1, 500))
        })
    }

    pub fn thread(&self, id: i64) -> Result<ThreadDetail> {
        self.db.read(|c| mail::get_thread(c, id))
    }

    /// Downloads missing bodies (if any) and returns the messages.
    pub async fn load_bodies(&self, message_ids: Vec<i64>) -> Result<Vec<MessageView>> {
        let missing: Vec<(i64, i64)> = self.db.read(|c| {
            let mut out = Vec::new();
            for id in &message_ids {
                let row: Option<(i64, i64)> = rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT account_id, body_state FROM messages WHERE id = ?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ))?;
                if let Some((account, state)) = row
                    && state != 1
                {
                    out.push((account, *id));
                }
            }
            Ok(out)
        })?;
        let mut by_account: HashMap<i64, Vec<i64>> = HashMap::new();
        for (a, m) in missing {
            by_account.entry(a).or_default().push(m);
        }
        for (account_id, ids) in by_account {
            let account = self.account_row(account_id)?;
            match account.kind {
                AccountKind::Imap | AccountKind::Gmail => {
                    self.with_imap(account_id, async |engine, client| {
                        crate::sync::imap::fetch_bodies(engine, client, &ids).await
                    })
                    .await?;
                }
                AccountKind::Microsoft => {
                    crate::sync::graph::fetch_bodies(self, account_id, &ids).await?;
                }
                AccountKind::Exchange => {
                    crate::sync::ews::fetch_bodies(self, &account, &ids).await?;
                }
            }
            let threads: BTreeSet<i64> = self.db.read(|c| {
                let mut set = BTreeSet::new();
                for id in &message_ids {
                    if let Ok(t) =
                        c.query_row("SELECT thread_id FROM messages WHERE id = ?", [id], |r| {
                            r.get(0)
                        })
                    {
                        set.insert(t);
                    }
                }
                Ok(set)
            })?;
            self.db.write(|tx| mail::refresh_threads(tx, &threads))?;
        }
        self.db.read(|c| {
            message_ids
                .iter()
                .map(|id| mail::get_message(c, *id))
                .collect()
        })
    }

    /// Runs `f` with this account's interactive IMAP connection (connecting lazily).
    pub(crate) async fn with_imap<T>(
        &self,
        account_id: i64,
        f: impl AsyncFnOnce(&Engine, &mut envelope_imap::Client) -> Result<T>,
    ) -> Result<T> {
        let slot: ImapSlot = self
            .interactive
            .lock()
            .entry(account_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(None)))
            .clone();
        let mut guard = slot.lock().await;
        if guard.is_none() {
            let account = self.account_row(account_id)?;
            *guard = Some(crate::sync::imap::connect(self, &account).await?);
        }
        let client = guard.as_mut().expect("connected above");
        match f(self, client).await {
            Ok(v) => Ok(v),
            Err(e) => {
                if e.is_transient() {
                    // Stale connection: reconnect once and retry.
                    *guard = None;
                    drop(guard);
                    return Err(e);
                }
                Err(e)
            }
        }
    }

    // ----- flags ---------------------------------------------------------------

    /// Marks threads (or single messages) read/unread/starred.
    pub fn set_flags(
        &self,
        thread_ids: &[i64],
        message_ids: &[i64],
        change: &FlagChange,
    ) -> Result<()> {
        let mut per_account: HashMap<i64, (Vec<i64>, Vec<i64>, Vec<i64>, Vec<i64>)> =
            HashMap::new();
        let threads: BTreeSet<i64> = self.db.write(|tx| {
            let mut targets: Vec<(i64, i64, i64, i64, bool)> = Vec::new(); // (msg, account, thread, flags, is_last)
            let mut load = |sql: &str, id: i64| -> Result<()> {
                let mut st = tx.prepare_cached(sql)?;
                let rows: Vec<(i64, i64, i64, i64)> = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                let n = rows.len();
                for (i, (m, a, t, f)) in rows.into_iter().enumerate() {
                    targets.push((m, a, t, f, i + 1 == n));
                }
                Ok(())
            };
            for t in thread_ids {
                load(
                    "SELECT id, account_id, thread_id, flags FROM messages WHERE thread_id = ? AND (flags & 8) = 0 ORDER BY date",
                    *t,
                )?;
            }
            for m in message_ids {
                load("SELECT id, account_id, thread_id, flags FROM messages WHERE id = ?", *m)?;
            }
            let mut touched = BTreeSet::new();
            for (msg, account, thread, flags, is_last) in targets {
                let mut new = flags;
                let entry = per_account.entry(account).or_default();
                if let Some(seen) = change.seen {
                    // Marking a conversation unread only flips its newest message.
                    if seen || is_last || !message_ids.is_empty() {
                        if seen && flags & Flags::SEEN == 0 {
                            new |= Flags::SEEN;
                            entry.0.push(msg);
                        } else if !seen && flags & Flags::SEEN != 0 {
                            new &= !Flags::SEEN;
                            entry.1.push(msg);
                        }
                    }
                }
                if let Some(flagged) = change.flagged {
                    if flagged && (is_last || !message_ids.is_empty()) && flags & Flags::FLAGGED == 0 {
                        new |= Flags::FLAGGED;
                        entry.2.push(msg);
                    } else if !flagged && flags & Flags::FLAGGED != 0 {
                        new &= !Flags::FLAGGED;
                        entry.3.push(msg);
                    }
                }
                if new != flags {
                    mail::set_message_flags(tx, msg, new)?;
                    touched.insert(thread);
                }
            }
            for (account, (seen, unseen, flag, unflag)) in &per_account {
                for (ids, add, flag_name) in [
                    (seen, true, "\\Seen"),
                    (unseen, false, "\\Seen"),
                    (flag, true, "\\Flagged"),
                    (unflag, false, "\\Flagged"),
                ] {
                    if ids.is_empty() {
                        continue;
                    }
                    let op = PendingOp::Flags {
                        message_ids: ids.clone(),
                        add: if add { vec![flag_name.into()] } else { vec![] },
                        remove: if add { vec![] } else { vec![flag_name.into()] },
                    };
                    misc::queue_op(tx, *account, &op, 0)?;
                }
            }
            let folders = mail::refresh_threads(tx, &touched)?;
            mail::refresh_folder_counts(tx, &folders)?;
            Ok(touched)
        })?;
        self.after_local_change(&per_account.keys().copied().collect::<Vec<_>>(), &threads)?;
        Ok(())
    }

    /// Marks every unread conversation of a list as read: the mailbox with its
    /// filter, inbox tab and search, as the list shows it. Returns the
    /// messages that were unread, to undo it.
    pub fn mark_read(
        &self,
        mailbox: &Mailbox,
        filter: &ListFilter,
        search: Option<&str>,
    ) -> Result<MarkedRead> {
        let search = search
            .filter(|s| !s.trim().is_empty())
            .and_then(crate::search::compile);
        let filter = ListFilter {
            unread: true,
            ..filter.clone()
        };
        let (threads, message_ids) = self.db.read(|c| {
            let sql = mail::list_sql(c, mailbox, &filter, search)?;
            let mut st = c.prepare(&format!("SELECT DISTINCT t.id FROM {}", sql.from_where))?;
            let threads: Vec<i64> = st
                .query_map(rusqlite::params_from_iter(sql.values.iter()), |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut unread = c.prepare_cached(
                "SELECT id FROM messages WHERE thread_id = ? AND (flags & 1) = 0 AND (flags & 8) = 0",
            )?;
            let mut message_ids = Vec::new();
            for t in &threads {
                for m in unread.query_map([t], |r| r.get::<_, i64>(0))? {
                    message_ids.push(m?);
                }
            }
            Ok((threads, message_ids))
        })?;
        if !threads.is_empty() {
            self.set_flags(
                &threads,
                &[],
                &FlagChange {
                    seen: Some(true),
                    flagged: None,
                },
            )?;
        }
        Ok(MarkedRead {
            threads: threads.len() as i64,
            message_ids,
        })
    }

    /// Follow-up reminders of conversations: done, remind again later, or
    /// back in Follow up (to undo either).
    pub fn set_follow_up(&self, thread_ids: &[i64], change: &FollowUpChange) -> Result<()> {
        let ids = thread_ids.to_vec();
        let accounts: Vec<i64> = self.db.write(|tx| {
            match change {
                FollowUpChange::Done => misc::followup_set(tx, &ids, misc::FOLLOWUP_DONE, None)?,
                FollowUpChange::RemindAt { at } => {
                    misc::followup_set(tx, &ids, misc::FOLLOWUP_WAITING, Some(*at))?
                }
                FollowUpChange::Due => misc::followup_set(tx, &ids, misc::FOLLOWUP_DUE, None)?,
            };
            Ok(mail::thread_summaries(tx, &ids)?
                .into_iter()
                .map(|t| t.account_id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect())
        })?;
        self.after_local_change(&accounts, &thread_ids.iter().copied().collect())
    }

    fn after_local_change(&self, accounts: &[i64], threads: &BTreeSet<i64>) -> Result<()> {
        let ids: Vec<i64> = threads.iter().copied().collect();
        let folders_by_account: HashMap<i64, Vec<i64>> = self.db.read(|c| {
            let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
            for t in &ids {
                let mut st = c.prepare_cached(
                    "SELECT t.account_id, tf.folder_id FROM threads t LEFT JOIN thread_folders tf ON tf.thread_id = t.id WHERE t.id = ?",
                )?;
                for row in st.query_map([t], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)))? {
                    let (a, f) = row?;
                    let e = map.entry(a).or_default();
                    if let Some(f) = f {
                        e.push(f);
                    }
                }
            }
            Ok(map)
        })?;
        let mut all_accounts: BTreeSet<i64> = accounts.iter().copied().collect();
        all_accounts.extend(folders_by_account.keys());
        for account_id in all_accounts {
            self.emit(EngineEvent::MailChanged {
                account_id,
                folder_ids: folders_by_account
                    .get(&account_id)
                    .cloned()
                    .unwrap_or_default(),
                thread_ids: ids.clone(),
            });
            self.wake(account_id);
        }
        Ok(())
    }

    // ----- moving ------------------------------------------------------------

    /// Archives, trashes, marks as spam or moves threads. `source` is the
    /// folder being viewed (only its messages move); `None` moves the whole
    /// conversation except sent mail and drafts.
    pub fn move_threads(
        &self,
        thread_ids: &[i64],
        source: Option<i64>,
        target: &MoveTarget,
    ) -> Result<UndoToken> {
        let mut token = UndoToken {
            account_ops: Vec::new(),
            locations: Vec::new(),
            flags: Vec::new(),
            snoozed: Vec::new(),
        };
        let mut accounts = BTreeSet::new();
        let threads: BTreeSet<i64> = thread_ids.iter().copied().collect();
        self.db.write(|tx| {
            // Group threads by account.
            let mut by_account: HashMap<i64, Vec<i64>> = HashMap::new();
            for t in thread_ids {
                let a: i64 =
                    tx.query_row("SELECT account_id FROM threads WHERE id = ?", [t], |r| {
                        r.get(0)
                    })?;
                by_account.entry(a).or_default().push(*t);
            }
            for (account_id, tids) in by_account {
                accounts.insert(account_id);
                let account = mail::get_account(tx, account_id)?;
                let folders = mail::list_folder_rows(tx, account_id)?;
                let gmail = account.kind == AccountKind::Gmail;
                let message_ids = mail::thread_message_ids(tx, &tids, None)?;
                let locations = mail::locations(tx, &message_ids)?;
                // Snapshot for undo.
                let mut snap: HashMap<i64, Vec<(i64, Option<u32>)>> = HashMap::new();
                for l in &locations {
                    snap.entry(l.message_id)
                        .or_default()
                        .push((l.folder_id, l.uid));
                }
                token.locations.extend(snap);
                for t in &tids {
                    let s: Option<i64> =
                        tx.query_row("SELECT snoozed_until FROM threads WHERE id = ?", [t], |r| {
                            r.get(0)
                        })?;
                    token.snoozed.push((*t, s));
                    tx.execute("UPDATE threads SET snoozed_until = NULL WHERE id = ?", [t])?;
                }
                let op_ids = if gmail {
                    gmail_move(tx, account_id, &folders, &locations, source, target)?
                } else {
                    imap_move(tx, account_id, &folders, &locations, source, target)?
                };
                token
                    .account_ops
                    .extend(op_ids.into_iter().map(|id| (account_id, id)));
            }
            let folders = mail::refresh_threads(tx, &threads)?;
            mail::refresh_folder_counts(tx, &folders)?;
            Ok(())
        })?;
        self.after_local_change(&accounts.iter().copied().collect::<Vec<_>>(), &threads)?;
        Ok(token)
    }

    /// Archives conversations out of their accounts' inboxes, leaving any
    /// other folder they are in alone (from a notification, a rule, muting).
    pub fn archive_from_inbox(&self, thread_ids: &[i64]) -> Result<Vec<UndoToken>> {
        let mut by_account: std::collections::BTreeMap<i64, Vec<i64>> = Default::default();
        for t in self.db.read(|c| mail::thread_summaries(c, thread_ids))? {
            by_account.entry(t.account_id).or_default().push(t.id);
        }
        let mut tokens = Vec::new();
        for (account, ids) in by_account {
            let inbox = self
                .db
                .read(|c| mail::list_folder_rows(c, account))?
                .into_iter()
                .find(|f| f.role == Some(FolderRole::Inbox))
                .map(|f| f.id);
            tokens.push(self.move_threads(&ids, inbox, &MoveTarget::Archive)?);
        }
        Ok(tokens)
    }

    /// Mutes conversations (archiving them from the inbox, like Gmail) or
    /// unmutes them. Muting returns the undo tokens of the archive.
    pub fn mute_threads(&self, thread_ids: &[i64], muted: bool) -> Result<Vec<UndoToken>> {
        let ids = thread_ids.to_vec();
        self.db.write(|tx| mail::set_muted(tx, &ids, muted))?;
        let tokens = if muted {
            self.archive_from_inbox(thread_ids)?
        } else {
            Vec::new()
        };
        let threads: BTreeSet<i64> = thread_ids.iter().copied().collect();
        let accounts: Vec<i64> = self
            .db
            .read(|c| {
                Ok(mail::thread_summaries(c, thread_ids)?
                    .into_iter()
                    .map(|t| t.account_id)
                    .collect::<BTreeSet<_>>())
            })?
            .into_iter()
            .collect();
        self.after_local_change(&accounts, &threads)?;
        Ok(tokens)
    }

    /// Reverts an action from its undo token.
    pub fn undo(&self, token: &UndoToken) -> Result<()> {
        let mut accounts = BTreeSet::new();
        let mut threads = BTreeSet::new();
        let mut all_cancelled = true;
        self.db.write(|tx| {
            for (account_id, op_id) in &token.account_ops {
                accounts.insert(*account_id);
                if !misc::cancel_op(tx, *op_id)? {
                    all_cancelled = false;
                }
            }
            for (message_id, locs) in &token.locations {
                let thread: Option<i64> = rusqlite::OptionalExtension::optional(tx.query_row(
                    "SELECT thread_id FROM messages WHERE id = ?",
                    [message_id],
                    |r| r.get(0),
                ))?;
                let Some(thread) = thread else { continue };
                threads.insert(thread);
                if all_cancelled {
                    tx.execute("DELETE FROM message_folders WHERE message_id = ?", [message_id])?;
                    for (folder, uid) in locs {
                        tx.execute(
                            "INSERT OR REPLACE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, ?)",
                            params![folder, message_id, uid],
                        )?;
                    }
                }
            }
            for (t, s) in &token.snoozed {
                tx.execute("UPDATE threads SET snoozed_until = ? WHERE id = ?", params![s, t])?;
            }
            let folders = mail::refresh_threads(tx, &threads)?;
            mail::refresh_folder_counts(tx, &folders)?;
            Ok(())
        })?;
        if !all_cancelled {
            // The server already applied the change: move the messages back.
            for (message_id, locs) in &token.locations {
                if let Some((folder, _)) = locs.first() {
                    let thread: i64 = self.db.read(|c| {
                        Ok(c.query_row(
                            "SELECT thread_id FROM messages WHERE id = ?",
                            [message_id],
                            |r| r.get(0),
                        )?)
                    })?;
                    let _ = self.move_threads(&[thread], None, &MoveTarget::Folder { id: *folder });
                }
            }
        }
        self.after_local_change(&accounts.iter().copied().collect::<Vec<_>>(), &threads)
    }

    /// Permanently deletes threads (from Trash/Junk).
    pub fn delete_forever(&self, thread_ids: &[i64]) -> Result<()> {
        let mut accounts = BTreeSet::new();
        let threads: BTreeSet<i64> = thread_ids.iter().copied().collect();
        self.db.write(|tx| {
            let message_ids = mail::thread_message_ids(tx, thread_ids, None)?;
            let locations = mail::locations(tx, &message_ids)?;
            let mut by_account: HashMap<i64, Vec<OpItem>> = HashMap::new();
            let mut kinds: HashMap<i64, AccountKind> = HashMap::new();
            for l in &locations {
                let account: i64 = tx.query_row(
                    "SELECT account_id FROM messages WHERE id = ?",
                    [l.message_id],
                    |r| r.get(0),
                )?;
                let kind = match kinds.get(&account).copied() {
                    Some(kind) => kind,
                    None => {
                        let kind = mail::get_account(tx, account)?.kind;
                        kinds.insert(account, kind);
                        kind
                    }
                };
                // Graph and Exchange messages are addressed by id in the folder
                // holding them.
                let by_id = matches!(kind, AccountKind::Microsoft | AccountKind::Exchange)
                    && !l.is_label
                    && l.remote_id.is_some();
                if l.uid.is_some() || by_id {
                    by_account.entry(account).or_default().push(OpItem {
                        message_id: l.message_id,
                        folder: l.folder_remote.clone(),
                        uid: l.uid,
                        remote_id: l.remote_id.clone(),
                    });
                }
            }
            for (account, items) in by_account {
                accounts.insert(account);
                misc::queue_op(tx, account, &PendingOp::Expunge { items }, 0)?;
            }
            for l in &locations {
                mail::remove_from_folder(tx, l.folder_id, l.message_id)?;
            }
            let folders = mail::refresh_threads(tx, &threads)?;
            mail::refresh_folder_counts(tx, &folders)?;
            Ok(())
        })?;
        self.after_local_change(&accounts.iter().copied().collect::<Vec<_>>(), &threads)
    }

    /// Snoozes threads until a time (or un-snoozes with `None`).
    pub fn snooze(&self, thread_ids: &[i64], until: Option<i64>) -> Result<()> {
        self.db
            .write(|tx| mail::set_snooze(tx, thread_ids, until))?;
        let accounts: Vec<i64> = self.db.read(|c| {
            let mut out = Vec::new();
            for t in thread_ids {
                if let Ok(a) =
                    c.query_row("SELECT account_id FROM threads WHERE id = ?", [t], |r| {
                        r.get(0)
                    })
                {
                    out.push(a);
                }
            }
            Ok(out)
        })?;
        self.after_local_change(&accounts, &thread_ids.iter().copied().collect())
    }

    /// Adds or removes a Gmail label or Outlook category (or copies to an IMAP
    /// folder when adding).
    pub fn set_label(&self, thread_ids: &[i64], folder_id: i64, add: bool) -> Result<()> {
        let folder = self.db.read(|c| mail::get_folder(c, folder_id))?;
        let account = self.account_row(folder.account_id)?;
        let category = match account.kind {
            AccountKind::Microsoft | AccountKind::Exchange => {
                crate::sync::graph::category_name(&folder)
            }
            _ => None,
        };
        if account.kind != AccountKind::Gmail && category.is_none() {
            if add {
                self.move_threads(thread_ids, None, &MoveTarget::Folder { id: folder_id })?;
            }
            return Ok(());
        }
        let threads: BTreeSet<i64> = thread_ids.iter().copied().collect();
        let label = match category {
            Some(name) => name.to_string(),
            None => gmail_label_name(&folder),
        };
        self.db.write(|tx| {
            let message_ids = mail::thread_message_ids(tx, thread_ids, None)?;
            for m in &message_ids {
                if add {
                    tx.execute(
                        "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
                        params![folder_id, m],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM message_folders WHERE folder_id = ? AND message_id = ? AND uid IS NULL",
                        params![folder_id, m],
                    )?;
                }
            }
            let op = PendingOp::Labels {
                message_ids,
                add: if add { vec![label.clone()] } else { vec![] },
                remove: if add { vec![] } else { vec![label.clone()] },
            };
            misc::queue_op(tx, account.id, &op, 0)?;
            let folders = mail::refresh_threads(tx, &threads)?;
            mail::refresh_folder_counts(tx, &folders)?;
            Ok(())
        })?;
        self.after_local_change(&[account.id], &threads)
    }

    // ----- drafts and sending ---------------------------------------------------

    /// Drafts not waiting in the outbox, with their server copies.
    pub fn drafts(&self) -> Result<Vec<Draft>> {
        self.db.read(misc::draft_list)
    }

    pub fn draft(&self, id: i64) -> Result<Draft> {
        self.db
            .read(|c| misc::draft_info(c, misc::draft_get(c, id)?))
    }

    /// Saves a draft locally; with `upload`, also stores it on the server.
    pub fn save_draft(&self, id: Option<i64>, data: &DraftInput, upload: bool) -> Result<i64> {
        // Encrypted drafts go to the server encrypted to the sender's key;
        // without one they stay on this device.
        let upload = upload
            && (data.encrypt != Some(true)
                || crate::sync::draft_identity(self, data)
                    .and_then(|(from, _)| self.own_key_for(&from.email))
                    .is_ok_and(|k| k.is_some()));
        let stored = self.db.write(|tx| {
            let stored = misc::draft_store(tx, id, data)?;
            let id = stored.id;
            if upload {
                // Coalesce: one pending upload per draft is enough.
                tx.execute(
                    "DELETE FROM pending_ops WHERE account_id = ? AND payload = ?",
                    params![
                        data.account_id,
                        serde_json::to_string(&PendingOp::SaveDraft { draft_id: id })?
                    ],
                )?;
                misc::queue_op(
                    tx,
                    data.account_id,
                    &PendingOp::SaveDraft { draft_id: id },
                    2_000,
                )?;
            }
            Ok(stored)
        })?;
        // Sent from another account now: its copy left the old account's server.
        if let Some((account, threads)) = &stored.left {
            self.after_local_change(&[*account], threads)?;
        }
        if upload {
            self.wake(data.account_id);
        }
        Ok(stored.id)
    }

    /// Discards a draft: its server copy is deleted too.
    pub fn delete_draft(&self, id: i64) -> Result<()> {
        let Some((draft, threads)) = self.db.write(|tx| misc::draft_discard(tx, id))? else {
            return Ok(());
        };
        self.forget_draft_files(&draft.data);
        self.after_local_change(&[draft.account_id], &threads)
    }

    /// Where the attachments of drafts made from server copies are kept.
    fn draft_files_dir(&self) -> PathBuf {
        self.paths.data.join("draft-files")
    }

    /// Deletes the files kept for a draft that is done with.
    pub(crate) fn forget_draft_files(&self, data: &DraftInput) {
        let dir = self.draft_files_dir();
        for a in &data.attachments {
            // Each draft's files are in a folder of their own under `dir`.
            if let Some(own) = a
                .path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).strip_prefix(&dir).ok())
                .and_then(|rest| rest.components().next())
            {
                let _ = std::fs::remove_dir_all(dir.join(own));
            }
        }
    }

    /// The draft to edit for a draft message (one in Drafts, or a draft in a
    /// conversation): the local draft whose server copy it is, or else a new
    /// local draft made from it (written on another computer, or here before
    /// Envelope knew it), keeping its recipients, text and attachments.
    /// Saving that draft replaces the server copy; sending or discarding it
    /// deletes the copy.
    pub async fn draft_for_message(&self, message_id: i64) -> Result<Draft> {
        let mine = self.db.read(|c| misc::draft_of_message(c, message_id))?;
        // Changed on the server since (edited elsewhere), and not here: brought up to date.
        let stale = mine
            .as_ref()
            .is_some_and(|d| d.uploaded() && d.synced_at.is_none());
        if let Some(d) = mine.as_ref().filter(|_| !stale) {
            return self.db.read(|c| misc::draft_info(c, d.clone()));
        }
        let m = self
            .load_bodies(vec![message_id])
            .await?
            .pop()
            .ok_or_else(|| Error::NotFound(format!("message {message_id}")))?;
        // An encrypted draft: what was decrypted (the interface opens it first).
        let opened = m
            .crypto
            .as_ref()
            .and_then(|_| self.crypto.opened(message_id));
        let encrypted = m
            .crypto
            .as_deref()
            .is_some_and(|k| k == "encrypted" || k == "inline-encrypted");
        if encrypted && opened.as_ref().is_none_or(|o| o.view.body.is_none()) {
            return Err(Error::Invalid(
                "This draft is encrypted and could not be decrypted here.".into(),
            ));
        }
        let (body, attachments, subject) = match &opened {
            Some(o) if encrypted => (
                o.view.body.clone(),
                o.view.attachments.clone(),
                o.view.subject.clone().unwrap_or_else(|| m.subject.clone()),
            ),
            _ => (m.body.clone(), m.attachments.clone(), m.subject.clone()),
        };
        let identities = self.db.read(misc::list_identities)?;
        let identity = m.from.as_ref().and_then(|f| {
            identities
                .iter()
                .find(|i| i.account_id == m.account_id && i.email.eq_ignore_ascii_case(&f.email))
        });
        let replied = match &m.in_reply_to {
            Some(irt) => self.db.read(|c| {
                Ok(rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT id FROM messages WHERE account_id = ? AND message_id = ? AND id != ? ORDER BY date DESC LIMIT 1",
                    params![m.account_id, irt, m.id],
                    |r| r.get::<_, i64>(0),
                ))?)
            })?,
            None => None,
        };
        let mut html = match &body {
            Some(MessageBody { html: Some(h), .. }) => sanitize_quoted(h),
            Some(MessageBody { text: Some(t), .. }) => text_to_paragraphs(t),
            _ => html_escape::encode_text(&m.snippet).to_string(),
        };
        // The files go next to the draft: the server copy, and the
        // attachments it has, go away once the draft is saved again.
        let dir = self
            .draft_files_dir()
            .join(uuid::Uuid::new_v4().simple().to_string());
        let mut files = Vec::new();
        for (n, a) in attachments.iter().enumerate() {
            let source = self.attachment_file(a.id).await?;
            let cid = a.content_id.as_deref().map(|c| c.trim_matches(['<', '>']));
            let pictured = cid.is_some_and(|c| html.contains(&format!("cid:{c}")));
            if pictured && a.mime_type.starts_with("image/") {
                // Pictures in the text are shown (and sent again) as they were.
                let data = tokio::fs::read(&source).await?;
                let url = format!(
                    "data:{};base64,{}",
                    a.mime_type,
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
                );
                html = html.replace(&format!("cid:{}", cid.unwrap_or_default()), &url);
                continue;
            }
            let target = dir.join(n.to_string()).join(safe_file_name(&a.filename));
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(&source, &target).await?;
            files.push(DraftAttachment {
                filename: a.filename.clone(),
                mime_type: a.mime_type.clone(),
                size: a.size,
                path: Some(target.to_string_lossy().into_owned()),
                attachment_id: None,
                message_id: None,
                content_id: a.content_id.clone(),
                inline: a.inline && cid.is_some(),
            });
        }
        let data = DraftInput {
            account_id: m.account_id,
            identity_id: identity.map(|i| i.id),
            // An address this computer has no identity for is kept as it is.
            from: if identity.is_none() {
                m.from.clone()
            } else {
                None
            },
            to: m.to.clone(),
            cc: m.cc.clone(),
            bcc: m.bcc.clone(),
            subject,
            html,
            attachments: files,
            reply_to_message_id: replied,
            mode: Some(if replied.is_some() { "reply" } else { "new" }.into()),
            encrypt: encrypted.then_some(true),
            ..Default::default()
        };
        let uid: Option<u32> = self.db.read(|c| {
            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT mf.uid FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                 WHERE mf.message_id = ? AND f.role = 'drafts' AND mf.uid IS NOT NULL",
                [message_id],
                |r| r.get(0),
            ))?)
        })?;
        let account = self.account_row(m.account_id)?;
        // Graph and Exchange address the copy by id; IMAP by its UID in
        // Drafts, or by Message-ID (Gmail lists drafts in All Mail).
        let remote_id: Option<String> =
            if matches!(account.kind, AccountKind::Microsoft | AccountKind::Exchange) {
                self.db.read(|c| {
                    Ok(c.query_row(
                        "SELECT remote_id FROM messages WHERE id = ?",
                        [message_id],
                        |r| r.get(0),
                    )?)
                })?
            } else {
                None
            };
        let id = self.db.write(|tx| match &mine {
            Some(d) => {
                tx.execute(
                    "UPDATE drafts SET data = ?, synced_at = updated_at WHERE id = ?",
                    params![serde_json::to_string(&data)?, d.id],
                )?;
                Ok(d.id)
            }
            None => misc::draft_adopt(
                tx,
                &data,
                uid,
                remote_id.as_deref(),
                m.message_id.as_deref(),
            ),
        })?;
        if let Some(old) = &mine {
            self.forget_draft_files(&old.data);
        }
        self.draft(id)
    }

    /// Queues a message for sending after `delay_ms` (undo window) or at `send_at`.
    pub async fn send(
        &self,
        draft_id: Option<i64>,
        data: &DraftInput,
        delay_ms: i64,
        send_at: Option<i64>,
    ) -> Result<i64> {
        let rendered = crate::sync::render_draft(self, data, false).await?;
        let (from, _) = crate::sync::draft_identity(self, data)?;
        let meta = misc::OutboxMeta {
            from: from.email.clone(),
            recipients: rendered.recipients.clone(),
            to: data
                .to
                .iter()
                .chain(&data.cc)
                .chain(&data.bcc)
                .cloned()
                .collect(),
            subject: data.subject.clone(),
            reply_to_message_id: data
                .reply_to_message_id
                .filter(|_| matches!(data.mode.as_deref(), Some("reply" | "replyAll"))),
            forward_of: data
                .reply_to_message_id
                .filter(|_| data.mode.as_deref() == Some("forward")),
            message_id: rendered.message_id.clone(),
        };
        let when = send_at.unwrap_or_else(|| now_ms() + delay_ms.max(0));
        let (id, left) = self.db.write(|tx| {
            let stored = misc::draft_store(tx, draft_id, data)?;
            if let Some(days) = data.follow_up_days.filter(|d| *d > 0) {
                misc::followup_add(
                    tx,
                    &rendered.message_id,
                    data.account_id,
                    &data.subject,
                    when,
                    when + i64::from(days) * 86_400_000,
                )?;
            }
            let id = misc::outbox_add(
                tx,
                data.account_id,
                Some(stored.id),
                &rendered.mime,
                &meta,
                when,
            )?;
            Ok((id, stored.left))
        })?;
        // Sent from another account than the draft was saved in: its copy
        // left that account's server.
        if let Some((account, threads)) = &left {
            self.after_local_change(&[*account], threads)?;
        }
        self.outbox_wake.notify_one();
        self.emit(EngineEvent::OutboxChanged);
        Ok(id)
    }

    /// Cancels a queued message (undo send), returning its draft id.
    pub fn cancel_send(&self, outbox_id: i64) -> Result<Option<i64>> {
        let row = self.db.write(|tx| {
            let row = misc::outbox_get(tx, outbox_id)?;
            match &row {
                Some(r) if r.state == "queued" || r.state == "failed" => {
                    misc::outbox_delete(tx, outbox_id)?;
                    misc::followup_remove(tx, &r.meta.message_id)?;
                    Ok(row)
                }
                Some(_) => Err(Error::Invalid("The message is already being sent".into())),
                None => Ok(None),
            }
        })?;
        self.emit(EngineEvent::OutboxChanged);
        Ok(row.and_then(|r| r.draft_id))
    }

    pub fn retry_send(&self, outbox_id: i64) -> Result<()> {
        self.db
            .write(|tx| misc::outbox_reschedule(tx, outbox_id, now_ms()))?;
        self.outbox_wake.notify_one();
        self.emit(EngineEvent::OutboxChanged);
        Ok(())
    }

    pub fn outbox(&self) -> Result<Vec<OutboxItem>> {
        self.db.read(misc::outbox_list)
    }

    /// Prefills a reply / reply-all / forward. The header above the quoted
    /// original is written with `quote`'s texts (the interface language).
    pub fn compose_reply(
        &self,
        message_id: i64,
        mode: &str,
        quote: &QuoteText,
    ) -> Result<DraftInput> {
        let m = self.db.read(|c| mail::get_message(c, message_id))?;
        let identities = self.db.read(misc::list_identities)?;
        let mine: Vec<&Identity> = identities
            .iter()
            .filter(|i| i.account_id == m.account_id)
            .collect();
        let is_me = |e: &str| mine.iter().any(|i| i.email.eq_ignore_ascii_case(e));
        // Reply from the address the message was sent to.
        let identity =
            m.to.iter()
                .chain(&m.cc)
                .find_map(|a| mine.iter().find(|i| i.email.eq_ignore_ascii_case(&a.email)))
                .or_else(|| mine.iter().find(|i| i.is_default))
                .or(mine.first())
                .copied();
        let from_me = m.from.as_ref().is_some_and(|f| is_me(&f.email));
        let mut draft = DraftInput {
            account_id: m.account_id,
            identity_id: identity.map(|i| i.id),
            reply_to_message_id: Some(message_id),
            mode: Some(mode.to_string()),
            ..Default::default()
        };
        // An encrypted original: quote what was decrypted, reply encrypted.
        let opened = m
            .crypto
            .as_ref()
            .and_then(|_| self.crypto.opened(message_id));
        let encrypted_original = m
            .crypto
            .as_deref()
            .is_some_and(|k| k == "encrypted" || k == "inline-encrypted");
        if encrypted_original {
            draft.encrypt = Some(true);
        }
        let subject = opened
            .as_ref()
            .and_then(|o| o.view.subject.clone())
            .unwrap_or_else(|| m.subject.clone());
        let body = match opened.as_ref().and_then(|o| o.view.body.clone()) {
            Some(b) if encrypted_original || m.crypto.as_deref() == Some("inline-signed") => {
                Some(b)
            }
            _ => m.body.clone(),
        };
        let original_html = match &body {
            Some(MessageBody { html: Some(h), .. }) => sanitize_quoted(h),
            Some(MessageBody { text: Some(t), .. }) => text_to_html(t),
            _ => html_escape::encode_text(&m.snippet).to_string(),
        };
        let date = quote
            .when
            .clone()
            .filter(|w| !w.trim().is_empty())
            .unwrap_or_else(|| {
                chrono::DateTime::from_timestamp_millis(m.date)
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%a, %b %-d, %Y at %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default()
            });
        let date = html_escape::encode_text(&date).to_string();
        // A text of the interface, escaped; English when it is missing.
        let text = |t: &Option<String>, english: &str| {
            html_escape::encode_text(
                t.as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or(english),
            )
            .to_string()
        };
        let sender = m
            .from
            .as_ref()
            .map(|f| match &f.name {
                Some(n) => format!(
                    "{} &lt;{}&gt;",
                    html_escape::encode_text(n),
                    html_escape::encode_text(&f.email)
                ),
                None => html_escape::encode_text(&f.email).to_string(),
            })
            .unwrap_or_default();
        match mode {
            "forward" => {
                draft.subject = crate::compose::prefixed_subject("Fwd: ", &subject);
                let to_list =
                    m.to.iter()
                        .map(|a| a.to_header())
                        .collect::<Vec<_>>()
                        .join(", ");
                draft.html = format!(
                    "<p></p><p></p><div class=\"envelope-forward\">{}<br>{} {sender}<br>{} {date}<br>{} {}<br>{} {}<br><br>{original_html}</div>",
                    text(&quote.forwarded, "---------- Forwarded message ---------"),
                    text(&quote.from, "From:"),
                    text(&quote.date, "Date:"),
                    text(&quote.subject, "Subject:"),
                    html_escape::encode_text(&subject),
                    text(&quote.to, "To:"),
                    html_escape::encode_text(&to_list)
                );
                let attachments = match &opened {
                    Some(o) if encrypted_original => o.view.attachments.clone(),
                    _ => m.attachments.clone(),
                };
                draft.attachments = attachments
                    .iter()
                    .filter(|a| !a.inline)
                    .map(|a| DraftAttachment {
                        filename: a.filename.clone(),
                        mime_type: a.mime_type.clone(),
                        size: a.size,
                        path: None,
                        attachment_id: Some(a.id),
                        message_id: None,
                        content_id: None,
                        inline: false,
                    })
                    .collect();
            }
            _ => {
                draft.subject = crate::compose::prefixed_subject("Re: ", &subject);
                let primary: Vec<Address> = if from_me {
                    m.to.clone()
                } else if !m.reply_to.is_empty() {
                    m.reply_to.clone()
                } else {
                    m.from.clone().into_iter().collect()
                };
                draft.to = primary.clone();
                if mode == "replyAll" {
                    let mut seen: Vec<String> =
                        primary.iter().map(|a| a.email.to_lowercase()).collect();
                    let mut push = |list: &mut Vec<Address>, a: &Address| {
                        let e = a.email.to_lowercase();
                        if !is_me(&e) && !seen.contains(&e) {
                            seen.push(e);
                            list.push(a.clone());
                        }
                    };
                    let mut to = draft.to.clone();
                    for a in &m.to {
                        push(&mut to, a);
                    }
                    let mut cc = Vec::new();
                    for a in &m.cc {
                        push(&mut cc, a);
                    }
                    draft.to = to;
                    draft.cc = cc;
                }
                let wrote = text(&quote.wrote, "On {date}, {sender} wrote:")
                    .replace("{date}", &date)
                    .replace("{sender}", &sender);
                draft.html = format!(
                    "<p></p><p></p><div class=\"envelope-quote\"><div>{wrote}</div><blockquote style=\"margin:0 0 0 .8ex;border-left:2px solid #d0d4dc;padding-left:1ex\">{original_html}</blockquote></div>"
                );
            }
        }
        if let Some(sig) = identity
            .and_then(|i| i.signature.clone())
            .filter(|s| !s.trim().is_empty())
        {
            draft.html = format!(
                "<p></p><div class=\"envelope-signature\">{sig}</div>{}",
                draft.html.trim_start_matches("<p></p>")
            );
        }
        Ok(draft)
    }

    // ----- attachments ---------------------------------------------------------

    /// Ensures an attachment is downloaded and returns its local path.
    pub async fn attachment_file(&self, attachment_id: i64) -> Result<PathBuf> {
        // Attachments inside encrypted messages are decrypted on demand.
        if attachment_id < 0 {
            return self.decrypted_attachment_file(attachment_id).await;
        }
        let att = self.db.read(|c| mail::get_attachment(c, attachment_id))?;
        let account_id: i64 = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT account_id FROM messages WHERE id = ?",
                [att.message_id],
                |r| r.get(0),
            )?)
        })?;
        let dir = self
            .paths
            .cache
            .join(format!("account-{account_id}"))
            .join(format!("att-{attachment_id}"));
        let safe_name: String = att
            .filename
            .chars()
            .map(|c| {
                if c == '/' || c == '\\' || c == '\0' {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let path = dir.join(if safe_name.trim().is_empty() {
            "attachment".to_string()
        } else {
            safe_name
        });
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(path);
        }
        let data = self.attachment_data(&att, account_id).await?;
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join(".download");
        tokio::fs::write(&tmp, &data).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(path)
    }

    async fn attachment_data(&self, att: &mail::AttachmentRow, account_id: i64) -> Result<Vec<u8>> {
        let account = self.account_row(account_id)?;
        if account.kind == AccountKind::Microsoft {
            return crate::sync::graph::attachment_data(
                self,
                account_id,
                att.message_id,
                &att.part_id,
            )
            .await;
        }
        if account.kind == AccountKind::Exchange {
            return crate::sync::ews::attachment_data(self, &account, att.message_id, &att.part_id)
                .await;
        }
        let locations = self.db.read(|c| mail::locations(c, &[att.message_id]))?;
        let location = locations
            .into_iter()
            .find(|l| l.uid.is_some())
            .ok_or_else(|| Error::NotFound("message is not on the server yet".into()))?;
        let part = att.part_id.clone();
        let encoding = att.encoding.clone().unwrap_or_default();
        self.with_imap(account_id, async |_, client| {
            crate::sync::imap::fetch_part(
                client,
                &location.folder_remote,
                location.uid.unwrap_or(0),
                &part,
                &encoding,
            )
            .await
        })
        .await
    }

    /// Bytes of an inline image referenced by `cid:`.
    pub async fn inline_image(&self, message_id: i64, cid: &str) -> Result<(String, PathBuf)> {
        let cid = cid
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string();
        if let Some(found) = self.decrypted_inline(message_id, &cid).await? {
            return Ok(found);
        }
        let att = self
            .db
            .read(|c| mail::attachment_by_cid(c, message_id, &cid))?
            .ok_or_else(|| Error::NotFound(format!("inline image {cid}")))?;
        let path = self.attachment_file(att.id).await?;
        Ok((att.mime_type, path))
    }

    /// The raw RFC 5322 source of a message.
    pub async fn message_source(&self, message_id: i64) -> Result<String> {
        let raw = self.message_source_bytes(message_id).await?;
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }

    /// The raw source of a message, as bytes.
    pub(crate) async fn message_source_bytes(&self, message_id: i64) -> Result<Vec<u8>> {
        let account_id: i64 = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT account_id FROM messages WHERE id = ?",
                [message_id],
                |r| r.get(0),
            )?)
        })?;
        let account = self.account_row(account_id)?;
        if account.kind == AccountKind::Microsoft {
            return crate::sync::graph::message_source(self, account_id, message_id).await;
        }
        if account.kind == AccountKind::Exchange {
            return crate::sync::ews::message_source(self, &account, message_id).await;
        }
        let location = self
            .db
            .read(|c| mail::locations(c, &[message_id]))?
            .into_iter()
            .find(|l| l.uid.is_some())
            .ok_or_else(|| Error::NotFound("message is not on the server".into()))?;
        self.with_imap(account_id, async |_, client| {
            crate::sync::imap::fetch_raw(client, &location.folder_remote, location.uid.unwrap_or(0))
                .await
        })
        .await
    }

    // ----- contacts, identities -------------------------------------------------

    pub fn suggest_contacts(&self, query: &str, limit: i64) -> Result<Vec<ContactSuggestion>> {
        self.db.read(|c| misc::suggest_addresses(c, query, limit))
    }

    pub fn identities(&self) -> Result<Vec<Identity>> {
        self.db.read(misc::list_identities)
    }

    pub fn save_identity(&self, identity: &Identity) -> Result<i64> {
        let id = self.db.write(|tx| misc::upsert_identity(tx, identity))?;
        self.emit(EngineEvent::AccountsChanged);
        Ok(id)
    }

    pub fn delete_identity(&self, id: i64) -> Result<()> {
        self.db.write(|tx| misc::delete_identity(tx, id))?;
        self.emit(EngineEvent::AccountsChanged);
        Ok(())
    }

    /// Performs a one-click (RFC 8058) unsubscribe when supported. Returns a
    /// URL the UI should open otherwise.
    pub async fn unsubscribe(&self, message_id: i64) -> Result<Option<String>> {
        let m = self.db.read(|c| mail::get_message(c, message_id))?;
        let info = m
            .unsubscribe
            .ok_or_else(|| Error::NotFound("no unsubscribe information".into()))?;
        if info.one_click
            && let Some(url) = &info.url
        {
            let resp = reqwest::Client::new()
                .post(url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body("List-Unsubscribe=One-Click")
                .send()
                .await?;
            if resp.status().is_success() {
                return Ok(None);
            }
        }
        if let Some(mailto) = &info.mailto {
            let addr = mailto.trim_start_matches("mailto:");
            let (to, query) = addr.split_once('?').unwrap_or((addr, ""));
            let subject = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("subject="))
                .map(|s| {
                    percent_encoding::percent_decode_str(s)
                        .decode_utf8_lossy()
                        .to_string()
                })
                .unwrap_or_else(|| "unsubscribe".into());
            let draft = DraftInput {
                account_id: m.account_id,
                to: vec![Address::new(
                    None,
                    percent_encoding::percent_decode_str(to)
                        .decode_utf8_lossy()
                        .to_string(),
                )],
                subject,
                html: "<p>unsubscribe</p>".into(),
                ..Default::default()
            };
            self.send(None, &draft, 0, None).await?;
            return Ok(None);
        }
        Ok(info.url)
    }
}

fn gmail_label_name(folder: &FolderRow) -> String {
    match folder.role {
        Some(FolderRole::Inbox) => "\\Inbox".into(),
        Some(FolderRole::Starred) => "\\Starred".into(),
        Some(FolderRole::Important) => "\\Important".into(),
        Some(FolderRole::Sent) => "\\Sent".into(),
        Some(FolderRole::Drafts) => "\\Draft".into(),
        _ => folder.remote_id.clone(),
    }
}

fn find_role(folders: &[FolderRow], role: FolderRole) -> Option<&FolderRow> {
    folders.iter().find(|f| f.role == Some(role))
}

fn target_folder<'a>(folders: &'a [FolderRow], target: &MoveTarget) -> Result<&'a FolderRow> {
    let found = match target {
        MoveTarget::Inbox => find_role(folders, FolderRole::Inbox),
        MoveTarget::Archive => {
            find_role(folders, FolderRole::Archive).or_else(|| find_role(folders, FolderRole::All))
        }
        MoveTarget::Trash => find_role(folders, FolderRole::Trash),
        MoveTarget::Junk => find_role(folders, FolderRole::Junk),
        MoveTarget::Folder { id } => folders.iter().find(|f| f.id == *id),
    };
    found.ok_or_else(|| Error::Invalid("This account has no folder for that action".into()))
}

/// Local + queued move for an account with real folders (standard IMAP,
/// Microsoft Graph). Labels (Outlook categories) stay with the messages.
/// Returns op ids.
fn imap_move(
    tx: &rusqlite::Transaction<'_>,
    account_id: i64,
    folders: &[FolderRow],
    locations: &[mail::Location],
    source: Option<i64>,
    target: &MoveTarget,
) -> Result<Vec<i64>> {
    let dest = target_folder(folders, target)?;
    if dest.kind == "label" {
        return Err(Error::Invalid(
            "Labels are added to conversations, not moved into".into(),
        ));
    }
    // Acting from a label's view moves the conversation out of its folder.
    let source = source.filter(|s| !folders.iter().any(|f| f.id == *s && f.kind == "label"));
    let keep_roles = [FolderRole::Sent, FolderRole::Drafts];
    let moving: Vec<&mail::Location> = locations
        .iter()
        .filter(|l| l.folder_id != dest.id && !l.is_label)
        .filter(|l| match source {
            Some(s) => l.folder_id == s,
            None => {
                !l.role.is_some_and(|r| keep_roles.contains(&r))
                    || matches!(target, MoveTarget::Trash)
            }
        })
        .collect();
    // Deleting from Trash means deleting forever.
    if matches!(target, MoveTarget::Trash)
        && source.is_some_and(|s| Some(s) == find_role(folders, FolderRole::Trash).map(|f| f.id))
    {
        let items: Vec<OpItem> = moving
            .iter()
            .map(|l| OpItem {
                message_id: l.message_id,
                folder: l.folder_remote.clone(),
                uid: l.uid,
                remote_id: l.remote_id.clone(),
            })
            .collect();
        for l in &moving {
            mail::remove_from_folder(tx, l.folder_id, l.message_id)?;
        }
        return Ok(vec![misc::queue_op(
            tx,
            account_id,
            &PendingOp::Expunge { items },
            UNDO_WINDOW_MS,
        )?]);
    }
    let items: Vec<OpItem> = moving
        .iter()
        .map(|l| OpItem {
            message_id: l.message_id,
            folder: l.folder_remote.clone(),
            uid: l.uid,
            remote_id: l.remote_id.clone(),
        })
        .collect();
    for l in &moving {
        tx.execute(
            "DELETE FROM message_folders WHERE folder_id = ? AND message_id = ?",
            params![l.folder_id, l.message_id],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
            params![dest.id, l.message_id],
        )?;
    }
    if items.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![misc::queue_op(
        tx,
        account_id,
        &PendingOp::Move {
            items,
            dest_folder: dest.remote_id.clone(),
        },
        UNDO_WINDOW_MS,
    )?])
}

/// Gmail semantics: archive removes the Inbox label, trash/spam move the
/// message, folders are labels.
fn gmail_move(
    tx: &rusqlite::Transaction<'_>,
    account_id: i64,
    folders: &[FolderRow],
    locations: &[mail::Location],
    source: Option<i64>,
    target: &MoveTarget,
) -> Result<Vec<i64>> {
    let inbox = find_role(folders, FolderRole::Inbox);
    let mut message_ids: Vec<i64> = locations.iter().map(|l| l.message_id).collect();
    message_ids.sort_unstable();
    message_ids.dedup();
    let mut ops = Vec::new();
    let trash = find_role(folders, FolderRole::Trash);
    let junk = find_role(folders, FolderRole::Junk);
    let in_trash_or_junk = |l: &mail::Location| {
        Some(l.folder_id) == trash.map(|f| f.id) || Some(l.folder_id) == junk.map(|f| f.id)
    };

    match target {
        MoveTarget::Archive => {
            if let Some(inbox) = inbox {
                tx.execute(
                    &format!(
                        "DELETE FROM message_folders WHERE folder_id = ? AND message_id IN ({})",
                        message_ids
                            .iter()
                            .map(|m| m.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    [inbox.id],
                )?;
                ops.push(misc::queue_op(
                    tx,
                    account_id,
                    &PendingOp::Labels {
                        message_ids: message_ids.clone(),
                        add: vec![],
                        remove: vec!["\\Inbox".into()],
                    },
                    UNDO_WINDOW_MS,
                )?);
            }
        }
        MoveTarget::Trash | MoveTarget::Junk => {
            let dest = target_folder(folders, target)?;
            if source.is_some_and(|s| s == dest.id) && matches!(target, MoveTarget::Trash) {
                let items: Vec<OpItem> = locations
                    .iter()
                    .filter(|l| l.folder_id == dest.id && l.uid.is_some())
                    .map(|l| OpItem {
                        message_id: l.message_id,
                        folder: l.folder_remote.clone(),
                        uid: l.uid,
                        remote_id: None,
                    })
                    .collect();
                for l in locations {
                    mail::remove_from_folder(tx, l.folder_id, l.message_id)?;
                }
                ops.push(misc::queue_op(
                    tx,
                    account_id,
                    &PendingOp::Expunge { items },
                    UNDO_WINDOW_MS,
                )?);
                return Ok(ops);
            }
            let items: Vec<OpItem> = locations
                .iter()
                .filter(|l| l.uid.is_some() && l.folder_id != dest.id)
                .map(|l| OpItem {
                    message_id: l.message_id,
                    folder: l.folder_remote.clone(),
                    uid: l.uid,
                    remote_id: None,
                })
                .collect();
            for m in &message_ids {
                tx.execute("DELETE FROM message_folders WHERE message_id = ?", [m])?;
                tx.execute(
                    "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
                    params![dest.id, m],
                )?;
            }
            if !items.is_empty() {
                ops.push(misc::queue_op(
                    tx,
                    account_id,
                    &PendingOp::Move {
                        items,
                        dest_folder: dest.remote_id.clone(),
                    },
                    UNDO_WINDOW_MS,
                )?);
            }
        }
        MoveTarget::Inbox | MoveTarget::Folder { .. } => {
            let dest = target_folder(folders, target)?;
            // Out of Trash/Spam: move back into the mailbox first.
            let stuck: Vec<OpItem> = locations
                .iter()
                .filter(|l| in_trash_or_junk(l) && l.uid.is_some())
                .map(|l| OpItem {
                    message_id: l.message_id,
                    folder: l.folder_remote.clone(),
                    uid: l.uid,
                    remote_id: None,
                })
                .collect();
            if !stuck.is_empty()
                && let Some(inbox) = inbox
            {
                for l in locations.iter().filter(|l| in_trash_or_junk(l)) {
                    tx.execute(
                        "DELETE FROM message_folders WHERE folder_id = ? AND message_id = ?",
                        params![l.folder_id, l.message_id],
                    )?;
                }
                ops.push(misc::queue_op(
                    tx,
                    account_id,
                    &PendingOp::Move {
                        items: stuck,
                        dest_folder: inbox.remote_id.clone(),
                    },
                    UNDO_WINDOW_MS,
                )?);
            }
            let label = gmail_label_name(dest);
            for m in &message_ids {
                tx.execute(
                    "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
                    params![dest.id, m],
                )?;
            }
            // "Move to label" also removes it from the folder being viewed.
            let mut remove = Vec::new();
            if let Some(src) = source.and_then(|s| folders.iter().find(|f| f.id == s))
                && src.id != dest.id
                && !matches!(
                    src.role,
                    Some(FolderRole::All | FolderRole::Trash | FolderRole::Junk)
                )
            {
                remove.push(gmail_label_name(src));
                for m in &message_ids {
                    tx.execute(
                        "DELETE FROM message_folders WHERE folder_id = ? AND message_id = ? AND uid IS NULL",
                        params![src.id, m],
                    )?;
                }
            }
            ops.push(misc::queue_op(
                tx,
                account_id,
                &PendingOp::Labels {
                    message_ids: message_ids.clone(),
                    add: vec![label],
                    remove,
                },
                UNDO_WINDOW_MS,
            )?);
        }
    }
    Ok(ops)
}

fn text_to_html(text: &str) -> String {
    let escaped = html_escape::encode_text(text).to_string();
    escaped.replace('\n', "<br>")
}

/// Plain text as editor paragraphs: one per block of lines.
fn text_to_paragraphs(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::new();
    for block in text.split("\n\n").map(|b| b.trim_matches('\n')) {
        if block.trim().is_empty() {
            continue;
        }
        out.push_str("<p>");
        out.push_str(&text_to_html(block));
        out.push_str("</p>");
    }
    out
}

/// A file name that stays inside its folder.
fn safe_file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    match safe.trim() {
        "" | "." | ".." => "attachment".to_string(),
        _ => safe,
    }
}

/// Strips scripts and document wrappers from HTML that gets quoted in replies.
fn sanitize_quoted(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let body = match (lower.find("<body"), lower.rfind("</body>")) {
        (Some(start), Some(end)) if start < end => {
            let open_end = lower[start..]
                .find('>')
                .map(|i| start + i + 1)
                .unwrap_or(start);
            &html[open_end..end]
        }
        _ => html,
    };
    let re = regex::Regex::new(r"(?is)<(script|style|head|title|meta)[^>]*>.*?</\s*(script|style|head|title)\s*>|<meta[^>]*>")
        .expect("valid regex");
    re.replace_all(body, "").into_owned()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod mailbox_tests;
