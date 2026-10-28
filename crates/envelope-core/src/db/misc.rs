//! Settings, identities, address book, pending operations, outbox and drafts.

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
    },
    /// Append a sent message to the Sent folder.
    AppendSent { outbox_id: i64 },
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
    pub updated_at: i64,
}

fn draft_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DraftRow> {
    let data: String = r.get("data")?;
    Ok(DraftRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        data: serde_json::from_str(&data).unwrap_or_default(),
        remote_uid: r.get("remote_uid")?,
        remote_id: r.get("remote_id")?,
        updated_at: r.get("updated_at")?,
    })
}

pub fn draft_save(tx: &Transaction<'_>, id: Option<i64>, data: &DraftInput) -> Result<i64> {
    let json = serde_json::to_string(data)?;
    match id {
        Some(id) if id > 0 => {
            let n = tx.execute(
                "UPDATE drafts SET data = ?, account_id = ?, updated_at = ? WHERE id = ?",
                params![json, data.account_id, now_ms(), id],
            )?;
            if n == 0 {
                return Err(Error::NotFound(format!("draft {id}")));
            }
            Ok(id)
        }
        _ => {
            tx.execute(
                "INSERT INTO drafts (account_id, data, updated_at) VALUES (?, ?, ?)",
                params![data.account_id, json, now_ms()],
            )?;
            Ok(tx.last_insert_rowid())
        }
    }
}

pub fn draft_get(c: &Connection, id: i64) -> Result<DraftRow> {
    c.query_row("SELECT * FROM drafts WHERE id = ?", [id], draft_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("draft {id}")))
}

pub fn draft_list(c: &Connection) -> Result<Vec<Draft>> {
    let mut st = c.prepare_cached("SELECT * FROM drafts ORDER BY updated_at DESC")?;
    let rows = st.query_map([], draft_from_row)?;
    Ok(rows
        .filter_map(|r| r.ok())
        .map(|d| Draft {
            id: d.id,
            data: d.data,
            updated_at: d.updated_at,
        })
        .collect())
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

pub fn draft_delete(tx: &Transaction<'_>, id: i64) -> Result<Option<DraftRow>> {
    let row = tx
        .query_row("SELECT * FROM drafts WHERE id = ?", [id], draft_from_row)
        .optional()?;
    tx.execute("DELETE FROM drafts WHERE id = ?", [id])?;
    Ok(row)
}
