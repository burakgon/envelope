//! Settings, identities, address book, pending operations, outbox and drafts.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::now_ms;
use crate::error::{Error, Result};
use crate::model::*;

// ---------------------------------------------------------------------------
// Key/value settings

pub fn kv_get<T: DeserializeOwned>(c: &Connection, key: &str) -> Result<Option<T>> {
    let v: Option<String> = c
        .query_row("SELECT value FROM kv WHERE key = ?", [key], |r| r.get(0))
        .optional()?;
    Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
}

pub fn kv_set<T: Serialize>(tx: &Transaction<'_>, key: &str, value: &T) -> Result<()> {
    tx.execute(
        "INSERT INTO kv (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, serde_json::to_string(value)?],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Identities

pub fn list_identities(c: &Connection) -> Result<Vec<Identity>> {
    let mut st = c.prepare_cached(
        "SELECT i.id, i.account_id, i.name, i.email, i.reply_to, i.signature, i.is_default
         FROM identities i JOIN accounts a ON a.id = i.account_id
         ORDER BY a.sort_order, i.account_id, i.is_default DESC, i.sort_order, i.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Identity {
            id: r.get(0)?,
            account_id: r.get(1)?,
            name: r.get(2)?,
            email: r.get(3)?,
            reply_to: r.get(4)?,
            signature: r.get(5)?,
            is_default: r.get::<_, i64>(6)? != 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_identity(c: &Connection, id: i64) -> Result<Identity> {
    list_identities(c)?
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| Error::NotFound(format!("identity {id}")))
}

pub fn upsert_identity(tx: &Transaction<'_>, identity: &Identity) -> Result<i64> {
    if identity.is_default {
        tx.execute(
            "UPDATE identities SET is_default = 0 WHERE account_id = ?",
            [identity.account_id],
        )?;
    }
    if identity.id > 0 {
        tx.execute(
            "UPDATE identities SET name = ?, email = ?, reply_to = ?, signature = ?, is_default = ? WHERE id = ?",
            params![
                identity.name,
                identity.email,
                identity.reply_to,
                identity.signature,
                identity.is_default as i64,
                identity.id
            ],
        )?;
        Ok(identity.id)
    } else {
        tx.execute(
            "INSERT INTO identities (account_id, name, email, reply_to, signature, is_default) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                identity.account_id,
                identity.name,
                identity.email,
                identity.reply_to,
                identity.signature,
                identity.is_default as i64
            ],
        )?;
        Ok(tx.last_insert_rowid())
    }
}

pub fn delete_identity(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM identities WHERE id = ? AND is_default = 0",
        [id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Address book (autocomplete)

/// Records that we received mail from / sent mail to these addresses.
pub fn record_addresses(
    tx: &Transaction<'_>,
    addresses: &[Address],
    sent: bool,
    when: i64,
) -> Result<()> {
    let mut st = tx.prepare_cached(
        "INSERT INTO addresses (email, name, sent_count, received_count, last_used) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (email) DO UPDATE SET
           name = coalesce(excluded.name, addresses.name),
           sent_count = addresses.sent_count + excluded.sent_count,
           received_count = addresses.received_count + excluded.received_count,
           last_used = max(addresses.last_used, excluded.last_used)",
    )?;
    for a in addresses {
        let email = a.email.trim().to_lowercase();
        if email.is_empty() || !email.contains('@') {
            continue;
        }
        st.execute(params![
            email,
            a.name
                .as_deref()
                .filter(|n| !n.is_empty() && !n.contains('@')),
            sent as i64,
            (!sent) as i64,
            when
        ])?;
    }
    Ok(())
}

/// Ranked address suggestions for a prefix (name or e-mail): address book
/// contacts (every address, primary first) merged with the addresses seen in
/// mail, ranked by how much the user writes to them.
pub fn suggest_addresses(
    c: &Connection,
    query: &str,
    limit: i64,
) -> Result<Vec<ContactSuggestion>> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let like = format!("{}%", q.replace(['%', '_'], ""));
    let word = format!("% {}%", q.replace(['%', '_'], ""));
    let now = now_ms();
    let mut out: Vec<ContactSuggestion> = Vec::new();
    // Address book contacts first: a saved contact outranks an address that
    // was only seen in mail, but not one the user writes to all the time.
    for found in crate::db::contacts::email_matches(c, &q, 50)? {
        for (i, email) in found.emails.iter().enumerate() {
            if out.iter().any(|s| s.email.eq_ignore_ascii_case(email)) {
                continue;
            }
            let local_part = email.split('@').next().unwrap_or("");
            let address_match = email.starts_with(&q)
                || local_part
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|w| w.starts_with(&q));
            if !found.name_match && !address_match {
                continue;
            }
            let name_bonus = if found.name_match { 10.0 } else { 0.0 };
            let primary_bonus = if i == 0 { 5.0 } else { 0.0 };
            let score = 40.0 + name_bonus + primary_bonus - (i as f64).min(5.0);
            out.push(ContactSuggestion {
                name: Some(found.name.clone()).filter(|n| !n.is_empty() && !n.contains('@')),
                email: email.clone(),
                score,
            });
        }
    }
    let mut st = c.prepare_cached(
        "SELECT email, name, sent_count, received_count, last_used FROM addresses
         WHERE email LIKE ?1 OR lower(name) LIKE ?1 OR lower(name) LIKE ?2
         ORDER BY sent_count DESC, last_used DESC LIMIT 100",
    )?;
    let rows = st.query_map(params![like, word], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (email, name, sent, received, last) = row?;
        let age_days = ((now - last).max(0) as f64) / 86_400_000.0;
        let recency = 1.0 / (1.0 + age_days / 30.0);
        let automated = email.contains("noreply")
            || email.contains("no-reply")
            || email.contains("notification");
        let mut score = (sent as f64) * 4.0 + (received as f64).sqrt() + recency * 10.0;
        if automated {
            score *= 0.1;
        }
        if let Some(existing) = out
            .iter_mut()
            .find(|s| s.email.eq_ignore_ascii_case(&email))
        {
            existing.score += score;
            if existing.name.is_none() {
                existing.name = name;
            }
        } else {
            out.push(ContactSuggestion { name, email, score });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit as usize);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pending operations

/// A local change waiting to be applied on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "op")]
pub enum PendingOp {
    /// Set/clear flags on messages.
    Flags {
        message_ids: Vec<i64>,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// Move messages from a folder to another folder.
    Move {
        /// (message_id, source folder remote id, uid / remote id)
        items: Vec<OpItem>,
        dest_folder: String,
    },
    /// Gmail: add/remove labels.
    Labels {
        message_ids: Vec<i64>,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// Permanently delete messages.
    Expunge { items: Vec<OpItem> },
    /// Upload a draft (replacing the previous server copy).
    SaveDraft { draft_id: i64 },
    /// Delete the server copy of a draft.
    DeleteDraft {
        remote_uid: Option<u32>,
        remote_id: Option<String>,
        /// Its Message-ID, to find it on IMAP servers when the UID is unknown
        /// (Gmail drafts synced from All Mail, servers without UIDPLUS).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    /// Append a sent message to the Sent folder.
    AppendSent { outbox_id: i64 },
    /// Delete for good everything in a folder (Trash, Spam) and the folders
    /// below it, as it was when the user asked.
    EmptyFolder {
        folder: String,
        /// IMAP: the highest UID known then (mail arriving later stays).
        #[serde(default)]
        max_uid: Option<u32>,
        /// Folders below it, deepest first.
        #[serde(default)]
        subfolders: Vec<String>,
    },
}

/// A message as it was when the operation was queued (it may be gone locally by
/// the time the operation runs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpItem {
    pub message_id: i64,
    pub folder: String,
    pub uid: Option<u32>,
    pub remote_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingRow {
    pub id: i64,
    pub account_id: i64,
    pub op: PendingOp,
    pub attempts: i64,
}

pub fn queue_op(
    tx: &Transaction<'_>,
    account_id: i64,
    op: &PendingOp,
    delay_ms: i64,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO pending_ops (account_id, kind, payload, created_at, not_before) VALUES (?, ?, ?, ?, ?)",
        params![
            account_id,
            "op",
            serde_json::to_string(op)?,
            now_ms(),
            now_ms() + delay_ms
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn due_ops(c: &Connection, account_id: i64) -> Result<Vec<PendingRow>> {
    let mut st = c.prepare_cached(
        "SELECT id, account_id, payload, attempts FROM pending_ops WHERE account_id = ? AND not_before <= ? ORDER BY id LIMIT 200",
    )?;
    let rows = st.query_map(params![account_id, now_ms()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, account_id, payload, attempts) = row?;
        match serde_json::from_str(&payload) {
            Ok(op) => out.push(PendingRow {
                id,
                account_id,
                op,
                attempts,
            }),
            Err(e) => tracing::warn!("dropping unreadable pending op {id}: {e}"),
        }
    }
    Ok(out)
}

/// Every queued operation of an account, due or not, oldest first.
pub fn queued_ops(c: &Connection, account_id: i64) -> Result<Vec<PendingOp>> {
    let mut st =
        c.prepare_cached("SELECT payload FROM pending_ops WHERE account_id = ? ORDER BY id")?;
    let rows = st.query_map([account_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(op) = serde_json::from_str(&row?) {
            out.push(op);
        }
    }
    Ok(out)
}

pub fn next_op_time(c: &Connection, account_id: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(not_before) FROM pending_ops WHERE account_id = ?",
        [account_id],
        |r| r.get(0),
    )?)
}

pub fn complete_op(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("DELETE FROM pending_ops WHERE id = ?", [id])?;
    Ok(())
}

pub fn fail_op(tx: &Transaction<'_>, id: i64, error: &str, retry_in_ms: i64) -> Result<()> {
    tx.execute(
        "UPDATE pending_ops SET attempts = attempts + 1, last_error = ?, not_before = ? WHERE id = ?",
        params![error, now_ms() + retry_in_ms, id],
    )?;
    Ok(())
}

pub fn cancel_op(tx: &Transaction<'_>, id: i64) -> Result<bool> {
    Ok(tx.execute("DELETE FROM pending_ops WHERE id = ?", [id])? > 0)
}

/// Points queued operations at the new path of an IMAP folder that was
/// renamed or moved (`old`, and the folders below it, `old` + delimiter + …).
pub fn rename_folder_in_ops(
    tx: &Transaction<'_>,
    account_id: i64,
    old: &str,
    new: &str,
    delimiter: &str,
) -> Result<()> {
    let rename = |folder: &mut String| {
        if folder == old {
            *folder = new.to_string();
        } else if let Some(rest) = folder
            .strip_prefix(old)
            .filter(|rest| !delimiter.is_empty() && rest.starts_with(delimiter))
        {
            *folder = format!("{new}{rest}");
        }
    };
    let rows: Vec<(i64, String)> = {
        let mut st = tx.prepare("SELECT id, payload FROM pending_ops WHERE account_id = ?")?;
        st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, payload) in rows {
        let Ok(mut op) = serde_json::from_str::<PendingOp>(&payload) else {
            continue;
        };
        match &mut op {
            PendingOp::Move { items, dest_folder } => {
                rename(dest_folder);
                items.iter_mut().for_each(|i| rename(&mut i.folder));
            }
            PendingOp::Expunge { items } => items.iter_mut().for_each(|i| rename(&mut i.folder)),
            // Gmail names labels by path.
            PendingOp::Labels { add, remove, .. } => {
                add.iter_mut().for_each(rename);
                remove.iter_mut().for_each(rename);
            }
            PendingOp::EmptyFolder {
                folder, subfolders, ..
            } => {
                rename(folder);
                subfolders.iter_mut().for_each(rename);
            }
            _ => continue,
        }
        let updated = serde_json::to_string(&op)?;
        if updated != payload {
            tx.execute(
                "UPDATE pending_ops SET payload = ? WHERE id = ?",
                params![updated, id],
            )?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Outbox

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutboxMeta {
    pub from: String,
    pub recipients: Vec<String>,
    pub to: Vec<Address>,
    pub subject: String,
    pub reply_to_message_id: Option<i64>,
    pub forward_of: Option<i64>,
    pub message_id: String,
}

#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: i64,
    pub account_id: i64,
    pub draft_id: Option<i64>,
    pub mime: Vec<u8>,
    pub meta: OutboxMeta,
    pub send_at: i64,
    pub state: String,
    pub attempts: i64,
}

pub fn outbox_add(
    tx: &Transaction<'_>,
    account_id: i64,
    draft_id: Option<i64>,
    mime: &[u8],
    meta: &OutboxMeta,
    send_at: i64,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO outbox (account_id, draft_id, mime, meta, send_at, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![account_id, draft_id, mime, serde_json::to_string(meta)?, send_at, now_ms()],
    )?;
    Ok(tx.last_insert_rowid())
}

fn outbox_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxRow> {
    let meta: String = r.get("meta")?;
    Ok(OutboxRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        draft_id: r.get("draft_id")?,
        mime: r.get("mime")?,
        meta: serde_json::from_str(&meta).unwrap_or_default(),
        send_at: r.get("send_at")?,
        state: r.get("state")?,
        attempts: r.get("attempts")?,
    })
}

pub fn outbox_get(c: &Connection, id: i64) -> Result<Option<OutboxRow>> {
    Ok(
        c.query_row("SELECT * FROM outbox WHERE id = ?", [id], outbox_from_row)
            .optional()?,
    )
}

pub fn outbox_due(c: &Connection, now: i64) -> Result<Vec<OutboxRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM outbox WHERE state = 'queued' AND send_at <= ? ORDER BY send_at LIMIT 20",
    )?;
    let rows = st.query_map([now], outbox_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn outbox_next_time(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(send_at) FROM outbox WHERE state = 'queued'",
        [],
        |r| r.get(0),
    )?)
}

pub fn outbox_list(c: &Connection) -> Result<Vec<OutboxItem>> {
    let mut st = c.prepare_cached("SELECT * FROM outbox ORDER BY send_at")?;
    let rows = st.query_map([], |r| {
        let row = outbox_from_row(r)?;
        let error: Option<String> = r.get("error")?;
        Ok(OutboxItem {
            id: row.id,
            account_id: row.account_id,
            subject: row.meta.subject,
            to: row.meta.to,
            send_at: row.send_at,
            state: row.state,
            error,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn outbox_set_state(
    tx: &Transaction<'_>,
    id: i64,
    state: &str,
    error: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE outbox SET state = ?, error = ?, attempts = attempts + (CASE WHEN ? = 'failed' THEN 1 ELSE 0 END) WHERE id = ?",
        params![state, error, state, id],
    )?;
    Ok(())
}

pub fn outbox_reschedule(tx: &Transaction<'_>, id: i64, send_at: i64) -> Result<()> {
    tx.execute(
        "UPDATE outbox SET state = 'queued', send_at = ?, error = NULL WHERE id = ?",
        params![send_at, id],
    )?;
    Ok(())
}

pub fn outbox_delete(tx: &Transaction<'_>, id: i64) -> Result<Option<OutboxRow>> {
    let row = tx
        .query_row("SELECT * FROM outbox WHERE id = ?", [id], outbox_from_row)
        .optional()?;
    tx.execute("DELETE FROM outbox WHERE id = ?", [id])?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Drafts

#[derive(Debug, Clone)]
pub struct DraftRow {
    pub id: i64,
    pub account_id: i64,
    pub data: DraftInput,
    pub remote_uid: Option<u32>,
    pub remote_id: Option<String>,
    /// Message-ID of the server copy.
    pub message_id: Option<String>,
    /// `updated_at` of the version the server copy holds.
    pub synced_at: Option<i64>,
    pub updated_at: i64,
}

impl DraftRow {
    /// Whether a copy was ever stored on the server.
    pub fn uploaded(&self) -> bool {
        self.remote_uid.is_some() || self.remote_id.is_some() || self.message_id.is_some()
    }

    /// The operation that deletes its server copy, if it has one.
    pub fn delete_op(&self) -> Option<PendingOp> {
        self.uploaded().then(|| PendingOp::DeleteDraft {
            remote_uid: self.remote_uid,
            remote_id: self.remote_id.clone(),
            message_id: self.message_id.clone(),
        })
    }
}

fn draft_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DraftRow> {
    let data: String = r.get("data")?;
    Ok(DraftRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        data: serde_json::from_str(&data).unwrap_or_default(),
        remote_uid: r.get("remote_uid")?,
        remote_id: r.get("remote_id")?,
        message_id: r.get("message_id")?,
        synced_at: r.get("synced_at")?,
        updated_at: r.get("updated_at")?,
    })
}

/// Saves a draft and returns its id: a new one when `id` is none, or when the
/// draft is gone meanwhile (deleted with its server copy elsewhere) and what
/// is still being written must not be lost.
pub fn draft_save(tx: &Transaction<'_>, id: Option<i64>, data: &DraftInput) -> Result<i64> {
    Ok(draft_store(tx, id, data)?.id)
}

/// What saving a draft did.
#[derive(Debug, Default)]
pub struct DraftStored {
    pub id: i64,
    /// The account the draft left (now sent from another one), whose server
    /// copy is deleted, and the conversations that changed with it.
    pub left: Option<(i64, BTreeSet<i64>)>,
}

/// [`draft_save`], telling what changed besides the draft.
pub fn draft_store(
    tx: &Transaction<'_>,
    id: Option<i64>,
    data: &DraftInput,
) -> Result<DraftStored> {
    let json = serde_json::to_string(data)?;
    if let Some(id) = id.filter(|id| *id > 0) {
        let mut left = None;
        // From another account now: the copy on the old account's server goes.
        if let Some(old) = tx
            .query_row("SELECT * FROM drafts WHERE id = ?", [id], draft_from_row)
            .optional()?
            .filter(|old| old.account_id != data.account_id)
            && let Some(op) = old.delete_op()
        {
            let mut threads = BTreeSet::new();
            if let Some(copy) = draft_server_copy(tx, &old)? {
                threads.extend(super::mail::forget_message(tx, copy)?);
            }
            queue_op(tx, old.account_id, &op, 0)?;
            tx.execute(
                "UPDATE drafts SET remote_uid = NULL, remote_id = NULL, message_id = NULL, synced_at = NULL
                 WHERE id = ?",
                [id],
            )?;
            let folders = super::mail::refresh_threads(tx, &threads)?;
            super::mail::refresh_folder_counts(tx, &folders)?;
            left = Some((old.account_id, threads));
        }
        let n = tx.execute(
            "UPDATE drafts SET data = ?, account_id = ?, updated_at = ? WHERE id = ?",
            params![json, data.account_id, now_ms(), id],
        )?;
        if n > 0 {
            return Ok(DraftStored { id, left });
        }
    }
    tx.execute(
        "INSERT INTO drafts (account_id, data, updated_at) VALUES (?, ?, ?)",
        params![data.account_id, json, now_ms()],
    )?;
    Ok(DraftStored {
        id: tx.last_insert_rowid(),
        left: None,
    })
}

/// A new local draft for a copy that is on the server already (written
/// elsewhere), so that saving it replaces that copy and sending deletes it.
pub fn draft_adopt(
    tx: &Transaction<'_>,
    data: &DraftInput,
    remote_uid: Option<u32>,
    remote_id: Option<&str>,
    message_id: Option<&str>,
) -> Result<i64> {
    let now = now_ms();
    tx.execute(
        "INSERT INTO drafts (account_id, data, remote_uid, remote_id, message_id, synced_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            data.account_id,
            serde_json::to_string(data)?,
            remote_uid,
            remote_id,
            message_id,
            now,
            now
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn draft_get(c: &Connection, id: i64) -> Result<DraftRow> {
    c.query_row("SELECT * FROM drafts WHERE id = ?", [id], draft_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("draft {id}")))
}

/// The synced message that is a draft's server copy: found by Message-ID,
/// by server id (Graph, Exchange) or by its UID in the Drafts folder.
pub fn draft_server_copy(c: &Connection, d: &DraftRow) -> Result<Option<i64>> {
    if let Some(mid) = &d.message_id {
        let found: Option<i64> = c
            .query_row(
                "SELECT id FROM messages WHERE account_id = ? AND message_id = ? ORDER BY id DESC LIMIT 1",
                params![d.account_id, mid],
                |r| r.get(0),
            )
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    if let Some(rid) = &d.remote_id {
        let found: Option<i64> = c
            .query_row(
                "SELECT id FROM messages WHERE account_id = ? AND remote_id = ? ORDER BY id DESC LIMIT 1",
                params![d.account_id, rid],
                |r| r.get(0),
            )
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    if let Some(uid) = d.remote_uid {
        return Ok(c
            .query_row(
                "SELECT mf.message_id FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                 WHERE f.account_id = ? AND f.role = 'drafts' AND mf.uid = ?",
                params![d.account_id, uid],
                |r| r.get(0),
            )
            .optional()?);
    }
    Ok(None)
}

/// The local draft whose server copy is the message `message_id` (drafts
/// waiting in the outbox aside: they are on their way out).
pub fn draft_of_message(c: &Connection, message_id: i64) -> Result<Option<DraftRow>> {
    let Some((account, mid, rid)) = c
        .query_row(
            "SELECT account_id, message_id, remote_id FROM messages WHERE id = ?",
            [message_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut st = c.prepare_cached(
        "SELECT * FROM drafts d WHERE account_id = ?
           AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.draft_id = d.id AND o.state != 'sent')
         ORDER BY updated_at DESC",
    )?;
    let drafts = st
        .query_map([account], draft_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let by_id = drafts.iter().find(|d| {
        (d.message_id.is_some() && d.message_id == mid)
            || (d.remote_id.is_some() && d.remote_id == rid)
    });
    if let Some(d) = by_id {
        return Ok(Some(d.clone()));
    }
    let uid: Option<u32> = c
        .query_row(
            "SELECT mf.uid FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
             WHERE mf.message_id = ? AND f.role = 'drafts' AND mf.uid IS NOT NULL",
            [message_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(uid.and_then(|uid| {
        drafts
            .into_iter()
            .find(|d| d.message_id.is_none() && d.remote_uid == Some(uid))
    }))
}

/// Drafts that are not waiting in the outbox, newest first.
pub fn draft_list(c: &Connection) -> Result<Vec<Draft>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM drafts d WHERE NOT EXISTS (SELECT 1 FROM outbox o WHERE o.draft_id = d.id AND o.state != 'sent')
         ORDER BY updated_at DESC",
    )?;
    let rows = st
        .query_map([], draft_from_row)?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    rows.into_iter().map(|d| draft_info(c, d)).collect()
}

/// A draft as the interface sees it: with its server copy, if synced.
pub fn draft_info(c: &Connection, d: DraftRow) -> Result<Draft> {
    let server_message_id = draft_server_copy(c, &d)?;
    // A server copy that went away elsewhere only matters while this
    // computer has changes it never got.
    let changed_here = d.synced_at.is_some_and(|s| d.updated_at > s);
    let local_only = server_message_id.is_none() && (!d.uploaded() || changed_here);
    Ok(Draft {
        id: d.id,
        data: d.data,
        updated_at: d.updated_at,
        server_message_id,
        local_only,
    })
}

pub fn draft_set_remote(
    tx: &Transaction<'_>,
    id: i64,
    uid: Option<u32>,
    remote_id: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE drafts SET remote_uid = ?, remote_id = ? WHERE id = ?",
        params![uid, remote_id, id],
    )?;
    Ok(())
}

/// Records a draft's new server copy, holding version `synced_at` of it, and
/// drops the synced message of the copy it replaces (which the server deletes)
/// so that Drafts does not list both until the next sync. Returns the
/// conversations that changed.
pub fn draft_uploaded(
    tx: &Transaction<'_>,
    id: i64,
    uid: Option<u32>,
    remote_id: Option<&str>,
    message_id: &str,
    synced_at: i64,
) -> Result<BTreeSet<i64>> {
    let old = draft_get(tx, id)?;
    let mut threads = BTreeSet::new();
    if let Some(stale) = draft_server_copy(tx, &old)? {
        threads.extend(super::mail::forget_message(tx, stale)?);
    }
    tx.execute(
        "UPDATE drafts SET remote_uid = ?, remote_id = ?, message_id = ?, synced_at = ? WHERE id = ?",
        params![
            uid,
            remote_id,
            message_id.trim_matches(['<', '>']),
            synced_at,
            id
        ],
    )?;
    let folders = super::mail::refresh_threads(tx, &threads)?;
    super::mail::refresh_folder_counts(tx, &folders)?;
    Ok(threads)
}

pub fn draft_delete(tx: &Transaction<'_>, id: i64) -> Result<Option<DraftRow>> {
    let row = tx
        .query_row("SELECT * FROM drafts WHERE id = ?", [id], draft_from_row)
        .optional()?;
    tx.execute("DELETE FROM drafts WHERE id = ?", [id])?;
    Ok(row)
}

/// Deletes a draft that is done with (sent or discarded). Its server copy is
/// deleted on the server and, when synced, here right away so that it leaves
/// Drafts at once. Returns the draft and the conversations that changed.
pub fn draft_discard(tx: &Transaction<'_>, id: i64) -> Result<Option<(DraftRow, BTreeSet<i64>)>> {
    let Some(d) = tx
        .query_row("SELECT * FROM drafts WHERE id = ?", [id], draft_from_row)
        .optional()?
    else {
        return Ok(None);
    };
    let mut threads = BTreeSet::new();
    if let Some(copy) = draft_server_copy(tx, &d)? {
        threads.extend(super::mail::forget_message(tx, copy)?);
    }
    tx.execute("DELETE FROM drafts WHERE id = ?", [id])?;
    if let Some(op) = d.delete_op() {
        queue_op(tx, d.account_id, &op, 0)?;
    }
    let folders = super::mail::refresh_threads(tx, &threads)?;
    super::mail::refresh_folder_counts(tx, &folders)?;
    Ok(Some((d, threads)))
}

// ----- follow-up reminders ---------------------------------------------------------

/// Starts watching a sent message for replies.
pub fn followup_add(
    tx: &Transaction<'_>,
    message_id: &str,
    account_id: i64,
    subject: &str,
    since: i64,
    due_at: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO followups (message_id, account_id, subject, since, due_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (message_id) DO UPDATE SET since = excluded.since, due_at = excluded.due_at, notified = 0",
        params![message_id.trim_matches(['<', '>']), account_id, subject, since, due_at],
    )?;
    Ok(())
}

pub fn followup_remove(tx: &Transaction<'_>, message_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM followups WHERE message_id = ?",
        [message_id.trim_matches(['<', '>'])],
    )?;
    Ok(())
}

/// A watched message, with its conversation once the sent copy has synced.
#[derive(Debug, Clone)]
pub struct FollowUp {
    pub message_id: String,
    pub subject: String,
    pub notified: bool,
    pub thread_id: Option<i64>,
    /// Someone other than the user wrote in the conversation since.
    pub replied: bool,
}

/// Watched messages that are due (or already notified, to notice late replies).
pub fn followups_to_check(c: &Connection, now: i64) -> Result<Vec<FollowUp>> {
    let mut st = c.prepare_cached(
        "SELECT f.message_id, f.subject, f.notified, m.thread_id,
                EXISTS (SELECT 1 FROM messages r
                        WHERE r.thread_id = m.thread_id AND r.date > f.since
                          AND r.from_email IS NOT NULL
                          AND r.from_email NOT IN (SELECT lower(email) FROM accounts
                                                   UNION SELECT lower(email) FROM identities))
         FROM followups f
         LEFT JOIN messages m ON m.account_id = f.account_id AND m.message_id = f.message_id
         WHERE f.notified = 1 OR f.due_at <= ?
         GROUP BY f.message_id",
    )?;
    let rows = st.query_map([now], |r| {
        Ok(FollowUp {
            message_id: r.get(0)?,
            subject: r.get(1)?,
            notified: r.get::<_, i64>(2)? != 0,
            thread_id: r.get(3)?,
            replied: r.get::<_, Option<bool>>(4)?.unwrap_or(false),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// States of a follow-up reminder (`followups.notified`): not due yet.
pub const FOLLOWUP_WAITING: i64 = 0;
/// Due without a reply: the conversation is in Follow up.
pub const FOLLOWUP_DUE: i64 = 1;
/// Dismissed ("Done"): no more reminders; forgotten once a reply comes.
pub const FOLLOWUP_DONE: i64 = 2;

/// Sets the reminder state of the messages watched in conversations, and
/// their due time when given. Returns how many reminders changed.
pub fn followup_set(
    tx: &Transaction<'_>,
    thread_ids: &[i64],
    state: i64,
    due_at: Option<i64>,
) -> Result<usize> {
    let mut st = tx.prepare_cached(
        "UPDATE followups SET notified = ?1, due_at = coalesce(?2, due_at)
         WHERE EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = ?3
                         AND m.account_id = followups.account_id AND m.message_id = followups.message_id)",
    )?;
    let mut changed = 0;
    for t in thread_ids {
        changed += st.execute(params![state, due_at, t])?;
    }
    Ok(changed)
}

pub fn followup_mark_notified(tx: &Transaction<'_>, message_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE followups SET notified = 1 WHERE message_id = ?",
        [message_id],
    )?;
    Ok(())
}
