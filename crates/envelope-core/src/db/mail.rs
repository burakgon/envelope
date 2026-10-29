//! Accounts, folders, messages and threads.

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};
use serde::{Deserialize, Serialize};

use super::{now_ms, pack, unpack};
use crate::error::{Error, Result};
use crate::model::*;

// ---------------------------------------------------------------------------
// Accounts

#[derive(Debug, Clone)]
pub struct AccountRow {
    pub id: i64,
    pub kind: AccountKind,
    pub provider: String,
    pub email: String,
    pub name: String,
    pub label: Option<String>,
    pub color: String,
    pub config: AccountConfig,
    pub state: serde_json::Value,
    pub sort_order: i64,
    pub enabled: bool,
}

fn account_from_row(r: &Row<'_>) -> rusqlite::Result<AccountRow> {
    let config: String = r.get("config")?;
    let state: String = r.get("state")?;
    let kind: String = r.get("kind")?;
    Ok(AccountRow {
        id: r.get("id")?,
        kind: AccountKind::parse(&kind),
        provider: r.get("provider")?,
        email: r.get("email")?,
        name: r.get("name")?,
        label: r.get("label")?,
        color: r.get("color")?,
        config: serde_json::from_str(&config).unwrap_or_default(),
        state: serde_json::from_str(&state).unwrap_or(serde_json::Value::Null),
        sort_order: r.get("sort_order")?,
        enabled: r.get::<_, i64>("enabled")? != 0,
    })
}

pub fn list_accounts(c: &Connection) -> Result<Vec<AccountRow>> {
    let mut st = c.prepare_cached("SELECT * FROM accounts ORDER BY sort_order, id")?;
    let rows = st.query_map([], account_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_account(c: &Connection, id: i64) -> Result<AccountRow> {
    c.query_row(
        "SELECT * FROM accounts WHERE id = ?",
        [id],
        account_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("account {id}")))
}

#[allow(clippy::too_many_arguments)]
pub fn insert_account(
    tx: &Transaction<'_>,
    kind: AccountKind,
    provider: &str,
    email: &str,
    name: &str,
    color: &str,
    config: &AccountConfig,
) -> Result<i64> {
    let order: i64 = tx.query_row(
        "SELECT coalesce(max(sort_order), -1) + 1 FROM accounts",
        [],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO accounts (kind, provider, email, name, color, config, sort_order, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            kind.as_str(),
            provider,
            email,
            name,
            color,
            serde_json::to_string(config)?,
            order,
            now_ms()
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO identities (account_id, name, email, is_default) VALUES (?, ?, ?, 1)",
        params![id, name, email],
    )?;
    Ok(id)
}

pub fn set_account_state(tx: &Transaction<'_>, id: i64, state: &serde_json::Value) -> Result<()> {
    tx.execute(
        "UPDATE accounts SET state = ? WHERE id = ?",
        params![serde_json::to_string(state)?, id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Folders

#[derive(Debug, Clone)]
pub struct FolderRow {
    pub id: i64,
    pub account_id: i64,
    pub remote_id: String,
    pub name: String,
    pub path: String,
    pub parent_id: Option<i64>,
    pub role: Option<FolderRole>,
    pub kind: String,
    /// IMAP hierarchy delimiter (`/`, `.`), if the server has one.
    pub delimiter: Option<String>,
    pub selectable: bool,
    pub color: Option<String>,
    pub sync_state: FolderSyncState,
}

/// Per-folder synchronization bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderSyncState {
    pub uid_validity: Option<u32>,
    pub uid_next: Option<u32>,
    pub highest_modseq: Option<u64>,
    /// Lowest UID whose headers we have downloaded (initial sync walks downwards).
    pub oldest_synced_uid: Option<u32>,
    /// Whether the full header history has been downloaded.
    pub complete: bool,
    pub last_sync: Option<i64>,
    /// Delta link (Graph) or similar opaque cursor.
    pub cursor: Option<String>,
    pub message_count: Option<u32>,
}

fn folder_from_row(r: &Row<'_>) -> rusqlite::Result<FolderRow> {
    let role: Option<String> = r.get("role")?;
    let state: String = r.get("sync_state")?;
    Ok(FolderRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        remote_id: r.get("remote_id")?,
        name: r.get("name")?,
        path: r.get("path")?,
        parent_id: r.get("parent_id")?,
        role: role.as_deref().and_then(FolderRole::parse),
        kind: r.get("kind")?,
        delimiter: r.get("delimiter")?,
        selectable: r.get::<_, i64>("selectable")? != 0,
        color: r.get("color")?,
        sync_state: serde_json::from_str(&state).unwrap_or_default(),
    })
}

pub fn list_folder_rows(c: &Connection, account_id: i64) -> Result<Vec<FolderRow>> {
    let mut st =
        c.prepare_cached("SELECT * FROM folders WHERE account_id = ? ORDER BY sort_order, path")?;
    let rows = st.query_map([account_id], folder_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_folder(c: &Connection, id: i64) -> Result<FolderRow> {
    c.query_row("SELECT * FROM folders WHERE id = ?", [id], folder_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("folder {id}")))
}

pub fn folder_by_role(
    c: &Connection,
    account_id: i64,
    role: FolderRole,
) -> Result<Option<FolderRow>> {
    Ok(c.query_row(
        "SELECT * FROM folders WHERE account_id = ? AND role = ? ORDER BY id LIMIT 1",
        params![account_id, role.as_str()],
        folder_from_row,
    )
    .optional()?)
}

pub fn folder_by_remote(
    c: &Connection,
    account_id: i64,
    remote_id: &str,
) -> Result<Option<FolderRow>> {
    Ok(c.query_row(
        "SELECT * FROM folders WHERE account_id = ? AND remote_id = ?",
        params![account_id, remote_id],
        folder_from_row,
    )
    .optional()?)
}

/// Every folder of every enabled account, for the sidebar.
pub fn list_folders(c: &Connection) -> Result<Vec<Folder>> {
    let mut st = c.prepare_cached(
        "SELECT f.* FROM folders f JOIN accounts a ON a.id = f.account_id
         WHERE a.enabled = 1 ORDER BY a.sort_order, f.account_id, f.sort_order, f.path COLLATE NOCASE",
    )?;
    let rows = st.query_map([], folder_view_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One folder as the sidebar shows it.
pub fn folder_view(c: &Connection, id: i64) -> Result<Folder> {
    c.query_row(
        "SELECT * FROM folders WHERE id = ?",
        [id],
        folder_view_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("folder {id}")))
}

fn folder_view_from_row(r: &Row<'_>) -> rusqlite::Result<Folder> {
    let role: Option<String> = r.get("role")?;
    Ok(Folder {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        name: r.get("name")?,
        path: r.get("path")?,
        parent_id: r.get("parent_id")?,
        role: role.as_deref().and_then(FolderRole::parse),
        kind: r.get("kind")?,
        unread_count: r.get("unread_count")?,
        total_count: r.get("total_count")?,
        color: r.get("color")?,
        hidden: r.get::<_, i64>("hidden")? != 0,
        selectable: r.get::<_, i64>("selectable")? != 0,
        sort_order: r.get("sort_order")?,
        delimiter: r.get("delimiter")?,
    })
}

/// Data needed to create or update a folder from the server listing.
#[derive(Debug, Clone)]
pub struct FolderUpsert {
    pub remote_id: String,
    pub name: String,
    pub path: String,
    pub parent_remote_id: Option<String>,
    pub role: Option<FolderRole>,
    pub kind: String,
    pub delimiter: Option<String>,
    pub selectable: bool,
    pub sort_order: i64,
    pub color: Option<String>,
}

/// Creates/updates folders to match the server and removes ones that vanished.
/// Returns the ids of all folders, keyed by remote id.
pub fn sync_folders(
    tx: &Transaction<'_>,
    account_id: i64,
    folders: &[FolderUpsert],
) -> Result<HashMap<String, i64>> {
    let mut ids = HashMap::new();
    for f in folders {
        tx.execute(
            "INSERT INTO folders (account_id, remote_id, name, path, role, kind, delimiter, selectable, sort_order, color)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (account_id, remote_id) DO UPDATE SET
               name = excluded.name, path = excluded.path, role = excluded.role, kind = excluded.kind,
               delimiter = excluded.delimiter, selectable = excluded.selectable, sort_order = excluded.sort_order,
               color = coalesce(excluded.color, folders.color)",
            params![
                account_id,
                f.remote_id,
                f.name,
                f.path,
                f.role.map(|r| r.as_str()),
                f.kind,
                f.delimiter,
                f.selectable as i64,
                f.sort_order,
                f.color,
            ],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM folders WHERE account_id = ? AND remote_id = ?",
            params![account_id, f.remote_id],
            |r| r.get(0),
        )?;
        ids.insert(f.remote_id.clone(), id);
    }
    for f in folders {
        let parent = f
            .parent_remote_id
            .as_ref()
            .and_then(|p| ids.get(p))
            .copied();
        tx.execute(
            "UPDATE folders SET parent_id = ? WHERE id = ?",
            params![parent, ids[&f.remote_id]],
        )?;
    }
    // Remove folders that no longer exist on the server.
    let existing: Vec<(i64, String)> = {
        let mut st = tx.prepare("SELECT id, remote_id FROM folders WHERE account_id = ?")?;
        st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, remote) in existing {
        if !ids.contains_key(&remote) {
            delete_folder(tx, id)?;
        }
    }
    Ok(ids)
}

/// Adds one folder (or updates it) without touching the others, unlike
/// [`sync_folders`]: a folder the user just created, before the next listing.
pub fn upsert_folder(
    tx: &Transaction<'_>,
    account_id: i64,
    f: &FolderUpsert,
    parent_id: Option<i64>,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO folders (account_id, remote_id, name, path, parent_id, role, kind, delimiter, selectable, sort_order, color)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (account_id, remote_id) DO UPDATE SET
           name = excluded.name, path = excluded.path, parent_id = excluded.parent_id, kind = excluded.kind,
           delimiter = excluded.delimiter, selectable = excluded.selectable,
           color = coalesce(excluded.color, folders.color)",
        params![
            account_id,
            f.remote_id,
            f.name,
            f.path,
            parent_id,
            f.role.map(|r| r.as_str()),
            f.kind,
            f.delimiter,
            f.selectable as i64,
            f.sort_order,
            f.color,
        ],
    )?;
    Ok(tx.query_row(
        "SELECT id FROM folders WHERE account_id = ? AND remote_id = ?",
        params![account_id, f.remote_id],
        |r| r.get(0),
    )?)
}

/// A folder and every folder below it, parents before their children.
pub fn folder_subtree(c: &Connection, folder_id: i64) -> Result<Vec<FolderRow>> {
    let root = get_folder(c, folder_id)?;
    let all = list_folder_rows(c, root.account_id)?;
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].id;
        let children: Vec<FolderRow> = all
            .iter()
            .filter(|f| f.parent_id == Some(parent) && !out.iter().any(|o| o.id == f.id))
            .cloned()
            .collect();
        out.extend(children);
        i += 1;
    }
    Ok(out)
}

/// Gives a renamed or moved folder its new server id, name, path and parent,
/// and the folders below it their new paths. With `nested_ids` (IMAP, where a
/// folder's server id is its path) their server ids change along.
pub fn rehome_folder(
    tx: &Transaction<'_>,
    folder_id: i64,
    remote_id: &str,
    name: &str,
    path: &str,
    parent_id: Option<i64>,
    nested_ids: bool,
) -> Result<()> {
    let subtree = folder_subtree(tx, folder_id)?;
    let root = &subtree[0];
    let replace = |value: &str, old: &str, new: &str| match value.strip_prefix(old) {
        Some(rest) => format!("{new}{rest}"),
        None => value.to_string(),
    };
    for f in &subtree[1..] {
        let remote = if nested_ids {
            replace(&f.remote_id, &root.remote_id, remote_id)
        } else {
            f.remote_id.clone()
        };
        tx.execute(
            "UPDATE folders SET remote_id = ?, path = ? WHERE id = ?",
            params![remote, replace(&f.path, &root.path, path), f.id],
        )?;
    }
    tx.execute(
        "UPDATE folders SET remote_id = ?, name = ?, path = ?, parent_id = ? WHERE id = ?",
        params![remote_id, name, path, parent_id, folder_id],
    )?;
    Ok(())
}

/// Removes a folder and the folders below it, with every message that only
/// lived there, and recounts the folders their conversations are still in.
pub fn delete_folder_tree(tx: &Transaction<'_>, folder_id: i64) -> Result<()> {
    let subtree = folder_subtree(tx, folder_id)?;
    let mut threads = BTreeSet::new();
    for f in subtree.iter().rev() {
        let mut st =
            tx.prepare_cached("SELECT thread_id FROM thread_folders WHERE folder_id = ?")?;
        for t in st.query_map([f.id], |r| r.get::<_, i64>(0))? {
            threads.insert(t?);
        }
        delete_folder(tx, f.id)?;
    }
    let folders = refresh_threads(tx, &threads)?;
    refresh_folder_counts(tx, &folders)?;
    Ok(())
}

/// Deletes a folder and every message that only lived there.
pub fn delete_folder(tx: &Transaction<'_>, folder_id: i64) -> Result<()> {
    let messages: Vec<i64> = {
        let mut st = tx.prepare("SELECT message_id FROM message_folders WHERE folder_id = ?")?;
        st.query_map([folder_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    tx.execute(
        "DELETE FROM message_folders WHERE folder_id = ?",
        [folder_id],
    )?;
    let mut threads = BTreeSet::new();
    for m in messages {
        if let Some(t) = delete_message_if_orphan(tx, m)? {
            threads.insert(t);
        }
    }
    tx.execute("DELETE FROM folders WHERE id = ?", [folder_id])?;
    refresh_threads(tx, &threads)?;
    Ok(())
}

pub fn set_folder_sync_state(
    tx: &Transaction<'_>,
    folder_id: i64,
    state: &FolderSyncState,
) -> Result<()> {
    tx.execute(
        "UPDATE folders SET sync_state = ? WHERE id = ?",
        params![serde_json::to_string(state)?, folder_id],
    )?;
    Ok(())
}

/// Recomputes unread/total thread counts for folders.
pub fn refresh_folder_counts(tx: &Transaction<'_>, folder_ids: &BTreeSet<i64>) -> Result<()> {
    let mut st = tx.prepare_cached(
        "UPDATE folders SET
           total_count = (SELECT count(*) FROM thread_folders WHERE folder_id = ?1),
           unread_count = (SELECT count(*) FROM thread_folders tf JOIN threads t ON t.id = tf.thread_id
                           WHERE tf.folder_id = ?1 AND tf.unread > 0 AND t.snoozed_until IS NULL)
         WHERE id = ?1",
    )?;
    for id in folder_ids {
        st.execute([id])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Messages

/// Everything we know about a message from its headers.
#[derive(Debug, Clone, Default)]
pub struct MessageMeta {
    pub remote_id: Option<String>,
    pub remote_thread_id: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub date: i64,
    pub flags: i64,
    pub keywords: Vec<String>,
    pub size: i64,
    pub has_attachments: bool,
    pub structure: Option<String>,
    pub snippet: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub unsubscribe_post: bool,
    pub list_id: Option<String>,
    pub is_auto: bool,
    pub calendar_method: Option<String>,
    /// The `Autocrypt` header (only when the message has exactly one).
    pub autocrypt: Option<String>,
}

/// Result of storing a message.
#[derive(Debug, Clone, Copy)]
pub struct Stored {
    pub message_id: i64,
    pub thread_id: i64,
    pub is_new: bool,
}

fn addrs_json(a: &[Address]) -> String {
    serde_json::to_string(a).unwrap_or_else(|_| "[]".into())
}

fn parse_addrs(s: &str) -> Vec<Address> {
    serde_json::from_str(s).unwrap_or_default()
}

/// Finds or creates the thread for a new message.
fn assign_thread(tx: &Transaction<'_>, account_id: i64, meta: &MessageMeta) -> Result<i64> {
    if let Some(remote) = &meta.remote_thread_id {
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM threads WHERE account_id = ? AND remote_thread_id = ?",
                params![account_id, remote],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        tx.execute(
            "INSERT INTO threads (account_id, remote_thread_id, subject, last_date) VALUES (?, ?, ?, ?)",
            params![account_id, remote, meta.subject, meta.date],
        )?;
        return Ok(tx.last_insert_rowid());
    }

    let mut candidates: Vec<&str> = Vec::new();
    if let Some(m) = &meta.message_id {
        candidates.push(m);
    }
    if let Some(r) = &meta.in_reply_to {
        candidates.push(r);
    }
    for r in meta.references.iter().rev().take(30) {
        candidates.push(r);
    }
    candidates.sort_unstable();
    candidates.dedup();

    let mut found: Vec<i64> = Vec::new();
    if !candidates.is_empty() {
        let placeholders = vec!["?"; candidates.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT thread_id FROM messages WHERE account_id = ? AND message_id IN ({placeholders})"
        );
        let mut st = tx.prepare_cached(&sql)?;
        let mut values: Vec<rusqlite::types::Value> = vec![account_id.into()];
        values.extend(
            candidates
                .iter()
                .map(|c| rusqlite::types::Value::from(c.to_string())),
        );
        let rows = st.query_map(params_from_iter(values), |r| r.get::<_, i64>(0))?;
        for r in rows {
            found.push(r?);
        }
    }
    // Replies that arrived before this message point at it.
    if let Some(mid) = &meta.message_id {
        let mut st = tx.prepare_cached(
            "SELECT DISTINCT thread_id FROM messages WHERE account_id = ? AND in_reply_to = ?",
        )?;
        for r in st.query_map(params![account_id, mid], |r| r.get::<_, i64>(0))? {
            let t = r?;
            if !found.contains(&t) {
                found.push(t);
            }
        }
    }

    match found.len() {
        0 => {
            tx.execute(
                "INSERT INTO threads (account_id, subject, last_date) VALUES (?, ?, ?)",
                params![account_id, meta.subject, meta.date],
            )?;
            Ok(tx.last_insert_rowid())
        }
        1 => Ok(found[0]),
        _ => {
            // This message links several threads together: merge them.
            let target = found[0];
            for other in &found[1..] {
                tx.execute(
                    "UPDATE messages SET thread_id = ? WHERE thread_id = ?",
                    params![target, other],
                )?;
                tx.execute("DELETE FROM threads WHERE id = ?", [other])?;
            }
            Ok(target)
        }
    }
}

/// Stores a message found in `folder_id` (with an optional IMAP `uid`).
///
/// Messages are matched by remote id (Gmail/Graph) or by folder+uid (IMAP).
/// Existing messages get their flags (and location) refreshed.
pub fn upsert_message(
    tx: &Transaction<'_>,
    account_id: i64,
    folder_id: i64,
    uid: Option<u32>,
    meta: &MessageMeta,
) -> Result<Stored> {
    let existing: Option<(i64, i64)> = if let Some(remote) = &meta.remote_id {
        tx.query_row(
            "SELECT id, thread_id FROM messages WHERE account_id = ? AND remote_id = ?",
            params![account_id, remote],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    } else if let Some(uid) = uid {
        tx.query_row(
            "SELECT m.id, m.thread_id FROM message_folders mf JOIN messages m ON m.id = mf.message_id
             WHERE mf.folder_id = ? AND mf.uid = ?",
            params![folder_id, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    } else {
        None
    };
    // A message we moved locally (location known, UID not yet) shows up on the
    // server with its new UID: claim the placeholder instead of duplicating.
    let existing = match (existing, uid, &meta.message_id) {
        (None, Some(_), Some(mid)) => tx
            .query_row(
                "SELECT m.id, m.thread_id FROM messages m JOIN message_folders mf ON mf.message_id = m.id
                 WHERE m.account_id = ? AND m.message_id = ? AND mf.folder_id = ? AND mf.uid IS NULL LIMIT 1",
                params![account_id, mid, folder_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        (e, _, _) => e,
    };

    if let Some((id, thread_id)) = existing {
        tx.execute(
            "UPDATE messages SET flags = ?, keywords = ? WHERE id = ?",
            params![meta.flags, keywords_json(&meta.keywords), id],
        )?;
        tx.execute(
            "INSERT INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, ?)
             ON CONFLICT (folder_id, message_id) DO UPDATE SET uid = coalesce(excluded.uid, message_folders.uid)",
            params![folder_id, id, uid],
        )?;
        return Ok(Stored {
            message_id: id,
            thread_id,
            is_new: false,
        });
    }

    let thread_id = assign_thread(tx, account_id, meta)?;
    let (from_name, from_email) = meta
        .from
        .as_ref()
        .map(|a| (a.name.clone(), Some(a.email.to_lowercase())))
        .unwrap_or((None, None));
    tx.execute(
        "INSERT INTO messages (account_id, thread_id, remote_id, message_id, in_reply_to, refs, subject,
            from_name, from_email, to_json, cc_json, bcc_json, reply_to_json, date, snippet, flags, keywords,
            size, has_attachments, structure, list_unsubscribe, unsubscribe_post, list_id, is_auto, calendar_method)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            account_id,
            thread_id,
            meta.remote_id,
            meta.message_id,
            meta.in_reply_to,
            if meta.references.is_empty() { None } else { Some(meta.references.join(" ")) },
            meta.subject,
            from_name,
            from_email,
            addrs_json(&meta.to),
            addrs_json(&meta.cc),
            addrs_json(&meta.bcc),
            addrs_json(&meta.reply_to),
            meta.date,
            meta.snippet.clone().unwrap_or_default(),
            meta.flags,
            keywords_json(&meta.keywords),
            meta.size,
            meta.has_attachments as i64,
            meta.structure,
            meta.list_unsubscribe,
            meta.unsubscribe_post as i64,
            meta.list_id,
            meta.is_auto as i64,
            meta.calendar_method,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, ?)",
        params![folder_id, id, uid],
    )?;
    index_message(tx, id, None)?;
    Ok(Stored {
        message_id: id,
        thread_id,
        is_new: true,
    })
}

fn keywords_json(k: &[String]) -> Option<String> {
    if k.is_empty() {
        None
    } else {
        serde_json::to_string(k).ok()
    }
}

/// Replaces the set of labels (non-UID folder locations) of a message (Gmail).
pub fn set_message_labels(
    tx: &Transaction<'_>,
    message_id: i64,
    label_folders: &[i64],
) -> Result<()> {
    let current: Vec<i64> = {
        let mut st = tx.prepare_cached(
            "SELECT folder_id FROM message_folders WHERE message_id = ? AND uid IS NULL",
        )?;
        st.query_map([message_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for f in &current {
        if !label_folders.contains(f) {
            tx.execute(
                "DELETE FROM message_folders WHERE message_id = ? AND folder_id = ?",
                params![message_id, f],
            )?;
        }
    }
    for f in label_folders {
        tx.execute(
            "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
            params![f, message_id],
        )?;
    }
    Ok(())
}

pub fn set_message_flags(tx: &Transaction<'_>, message_id: i64, flags: i64) -> Result<()> {
    tx.execute(
        "UPDATE messages SET flags = ? WHERE id = ?",
        params![flags, message_id],
    )?;
    Ok(())
}

/// Removes a message from a folder (expunged or moved on the server). The
/// message is deleted once it has no locations left. Returns its thread.
pub fn remove_from_folder(
    tx: &Transaction<'_>,
    folder_id: i64,
    message_id: i64,
) -> Result<Option<i64>> {
    tx.execute(
        "DELETE FROM message_folders WHERE folder_id = ? AND message_id = ?",
        params![folder_id, message_id],
    )?;
    let thread: Option<i64> = tx
        .query_row(
            "SELECT thread_id FROM messages WHERE id = ?",
            [message_id],
            |r| r.get(0),
        )
        .optional()?;
    delete_message_if_orphan(tx, message_id)?;
    Ok(thread)
}

/// Removes a message from every folder here, ahead of the server deleting it
/// (a draft's replaced or discarded server copy). Returns its thread.
pub fn forget_message(tx: &Transaction<'_>, message_id: i64) -> Result<Option<i64>> {
    let thread: Option<i64> = tx
        .query_row(
            "SELECT thread_id FROM messages WHERE id = ?",
            [message_id],
            |r| r.get(0),
        )
        .optional()?;
    tx.execute(
        "DELETE FROM message_folders WHERE message_id = ?",
        [message_id],
    )?;
    delete_message_if_orphan(tx, message_id)?;
    Ok(thread)
}

fn delete_message_if_orphan(tx: &Transaction<'_>, message_id: i64) -> Result<Option<i64>> {
    let remaining: i64 = tx.query_row(
        "SELECT count(*) FROM message_folders WHERE message_id = ?",
        [message_id],
        |r| r.get(0),
    )?;
    if remaining > 0 {
        return Ok(None);
    }
    let thread: Option<i64> = tx
        .query_row(
            "SELECT thread_id FROM messages WHERE id = ?",
            [message_id],
            |r| r.get(0),
        )
        .optional()?;
    tx.execute("DELETE FROM message_fts WHERE rowid = ?", [message_id])?;
    tx.execute("DELETE FROM messages WHERE id = ?", [message_id])?;
    Ok(thread)
}

/// `(uid, message_id, flags)` for every message with a UID in a folder.
pub fn folder_uids(c: &Connection, folder_id: i64) -> Result<Vec<(u32, i64, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT mf.uid, m.id, m.flags FROM message_folders mf JOIN messages m ON m.id = mf.message_id
         WHERE mf.folder_id = ? AND mf.uid IS NOT NULL",
    )?;
    let rows = st.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A message location that can be acted upon on the server.
#[derive(Debug, Clone)]
pub struct Location {
    pub message_id: i64,
    pub folder_id: i64,
    pub folder_remote: String,
    pub role: Option<FolderRole>,
    pub uid: Option<u32>,
    pub remote_id: Option<String>,
    /// The folder is a label (Gmail label, Outlook category) rather than a
    /// place the message is stored in.
    pub is_label: bool,
}

/// Server locations (with UIDs) of the given messages.
pub fn locations(c: &Connection, message_ids: &[i64]) -> Result<Vec<Location>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; message_ids.len()].join(",");
    let sql = format!(
        "SELECT mf.message_id, mf.folder_id, f.remote_id, f.role, mf.uid, m.remote_id, f.kind
         FROM message_folders mf JOIN folders f ON f.id = mf.folder_id JOIN messages m ON m.id = mf.message_id
         WHERE mf.message_id IN ({placeholders})"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(message_ids.iter()), |r| {
        let role: Option<String> = r.get(3)?;
        let kind: String = r.get(6)?;
        Ok(Location {
            message_id: r.get(0)?,
            folder_id: r.get(1)?,
            folder_remote: r.get(2)?,
            role: role.as_deref().and_then(FolderRole::parse),
            uid: r.get(4)?,
            remote_id: r.get(5)?,
            is_label: kind == "label",
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------------------
// Providers addressing messages by id (Microsoft Graph)
//
// A Graph message lives in exactly one folder and keeps its (immutable) id
// when it moves; Outlook categories are labels that travel with it.

/// Local id, thread and flags of the message with a provider id.
pub fn message_by_remote(
    c: &Connection,
    account_id: i64,
    remote_id: &str,
) -> Result<Option<(i64, i64, i64)>> {
    Ok(c.query_row(
        "SELECT id, thread_id, flags FROM messages WHERE account_id = ? AND remote_id = ?",
        params![account_id, remote_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?)
}

/// Provider ids of messages, as `(message id, remote id)`.
pub fn remote_ids(c: &Connection, message_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; message_ids.len()].join(",");
    let mut st = c.prepare(&format!(
        "SELECT id, remote_id FROM messages WHERE id IN ({placeholders}) AND remote_id IS NOT NULL"
    ))?;
    let rows = st.query_map(params_from_iter(message_ids.iter()), |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `(message id, thread id, remote id)` of the messages in a folder.
pub fn folder_remote_ids(c: &Connection, folder_id: i64) -> Result<Vec<(i64, i64, String)>> {
    let mut st = c.prepare_cached(
        "SELECT m.id, m.thread_id, m.remote_id FROM message_folders mf JOIN messages m ON m.id = mf.message_id
         WHERE mf.folder_id = ? AND m.remote_id IS NOT NULL",
    )?;
    let rows = st.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records the new provider id of a message (ids that change on moves).
pub fn set_remote_id(tx: &Transaction<'_>, message_id: i64, remote_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE messages SET remote_id = ? WHERE id = ?",
        params![remote_id, message_id],
    )?;
    Ok(())
}

/// Updates the server-controlled state of an already stored message.
pub fn set_flags_and_keywords(
    tx: &Transaction<'_>,
    message_id: i64,
    flags: i64,
    keywords: &[String],
) -> Result<()> {
    tx.execute(
        "UPDATE messages SET flags = ?, keywords = ? WHERE id = ?",
        params![flags, keywords_json(keywords), message_id],
    )?;
    Ok(())
}

/// Makes `folder_id` the only folder (labels aside) that holds a message.
/// Returns whether its location changed.
pub fn set_only_folder(tx: &Transaction<'_>, message_id: i64, folder_id: i64) -> Result<bool> {
    let added = tx.execute(
        "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
        params![folder_id, message_id],
    )?;
    let removed = tx.execute(
        "DELETE FROM message_folders WHERE message_id = ?1 AND folder_id != ?2
           AND folder_id IN (SELECT id FROM folders WHERE kind != 'label')",
        params![message_id, folder_id],
    )?;
    Ok(added + removed > 0)
}

/// Replaces the label folders of a message, leaving its folder alone.
/// Returns whether anything changed.
pub fn set_label_folders(
    tx: &Transaction<'_>,
    message_id: i64,
    label_folders: &[i64],
) -> Result<bool> {
    let current: Vec<i64> = {
        let mut st = tx.prepare_cached(
            "SELECT mf.folder_id FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
             WHERE mf.message_id = ? AND f.kind = 'label'",
        )?;
        st.query_map([message_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut changed = false;
    for f in &current {
        if !label_folders.contains(f) {
            tx.execute(
                "DELETE FROM message_folders WHERE message_id = ? AND folder_id = ?",
                params![message_id, f],
            )?;
            changed = true;
        }
    }
    for f in label_folders {
        if !current.contains(f) {
            changed |= tx.execute(
                "INSERT OR IGNORE INTO message_folders (folder_id, message_id, uid) VALUES (?, ?, NULL)",
                params![f, message_id],
            )? > 0;
        }
    }
    Ok(changed)
}

/// Removes a message from a folder but keeps it even when that was its last
/// location: it may be moving to a folder that has not been synchronized yet
/// (see [`delete_orphan_messages`]). Returns its thread.
pub fn detach_from_folder(
    tx: &Transaction<'_>,
    folder_id: i64,
    message_id: i64,
) -> Result<Option<i64>> {
    let removed = tx.execute(
        "DELETE FROM message_folders WHERE folder_id = ? AND message_id = ?",
        params![folder_id, message_id],
    )?;
    if removed == 0 {
        return Ok(None);
    }
    Ok(tx
        .query_row(
            "SELECT thread_id FROM messages WHERE id = ?",
            [message_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Deletes the messages of an account that are no longer in any folder;
/// labels alone do not keep a message (accounts whose labels travel with
/// messages, such as Outlook categories). Returns `(message id, thread id)`
/// of each.
pub fn delete_orphan_messages(tx: &Transaction<'_>, account_id: i64) -> Result<Vec<(i64, i64)>> {
    let orphans: Vec<(i64, i64)> = {
        let mut st = tx.prepare(
            "SELECT id, thread_id FROM messages m WHERE account_id = ?
               AND NOT EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                               WHERE mf.message_id = m.id AND f.kind != 'label')",
        )?;
        st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, _) in &orphans {
        tx.execute("DELETE FROM message_folders WHERE message_id = ?", [id])?;
        delete_message_if_orphan(tx, *id)?;
    }
    Ok(orphans)
}

/// Brings a draft that was edited elsewhere up to date; its body is
/// downloaded again when needed.
pub fn refresh_draft(tx: &Transaction<'_>, message_id: i64, meta: &MessageMeta) -> Result<()> {
    tx.execute(
        "UPDATE messages SET subject = ?, to_json = ?, cc_json = ?, bcc_json = ?, reply_to_json = ?,
           date = ?, snippet = ?, has_attachments = ?, body_state = 0
         WHERE id = ?",
        params![
            meta.subject,
            addrs_json(&meta.to),
            addrs_json(&meta.cc),
            addrs_json(&meta.bcc),
            addrs_json(&meta.reply_to),
            meta.date,
            meta.snippet.clone().unwrap_or_default(),
            meta.has_attachments as i64,
            message_id
        ],
    )?;
    tx.execute("DELETE FROM bodies WHERE message_id = ?", [message_id])?;
    tx.execute("DELETE FROM attachments WHERE message_id = ?", [message_id])?;
    index_message(tx, message_id, None)?;
    // A draft of this computer whose server copy this is, without changes of its
    // own since: it no longer holds what the server has, and is brought up to
    // date from the copy when it is opened.
    tx.execute(
        "UPDATE drafts SET synced_at = NULL
         WHERE synced_at IS NOT NULL AND updated_at <= synced_at
           AND account_id = (SELECT account_id FROM messages WHERE id = ?1)
           AND ((remote_id IS NOT NULL AND remote_id = (SELECT remote_id FROM messages WHERE id = ?1))
             OR (message_id IS NOT NULL AND message_id = (SELECT message_id FROM messages WHERE id = ?1)))",
        [message_id],
    )?;
    Ok(())
}

/// Fills in what only the full message source tells (threading headers,
/// mailing list and auto-reply markers, invitations, size) for messages that
/// were listed without their raw headers.
pub fn update_from_source(
    tx: &Transaction<'_>,
    message_id: i64,
    meta: &MessageMeta,
    size: i64,
) -> Result<()> {
    let refs = (!meta.references.is_empty()).then(|| meta.references.join(" "));
    tx.execute(
        "UPDATE messages SET
           message_id = coalesce(message_id, ?),
           in_reply_to = coalesce(?, in_reply_to),
           refs = coalesce(?, refs),
           list_unsubscribe = coalesce(?, list_unsubscribe),
           unsubscribe_post = max(unsubscribe_post, ?),
           list_id = coalesce(?, list_id),
           is_auto = max(is_auto, ?),
           calendar_method = coalesce(?, calendar_method),
           size = CASE WHEN ? > 0 THEN ? ELSE size END
         WHERE id = ?",
        params![
            meta.message_id,
            meta.in_reply_to,
            refs,
            meta.list_unsubscribe,
            meta.unsubscribe_post as i64,
            meta.list_id,
            meta.is_auto as i64,
            meta.calendar_method,
            size,
            size,
            message_id
        ],
    )?;
    Ok(())
}

/// Hides label folders no conversation carries (and shows them again once one
/// does). Returns whether any visibility changed.
pub fn hide_unused_labels(tx: &Transaction<'_>, account_id: i64) -> Result<bool> {
    let changed = tx.execute(
        "UPDATE folders SET hidden = NOT EXISTS (SELECT 1 FROM thread_folders tf WHERE tf.folder_id = folders.id)
         WHERE account_id = ? AND kind = 'label'
           AND hidden != NOT EXISTS (SELECT 1 FROM thread_folders tf WHERE tf.folder_id = folders.id)",
        [account_id],
    )?;
    Ok(changed > 0)
}

/// Message ids of the given threads, optionally only those in `folder_id`.
pub fn thread_message_ids(
    c: &Connection,
    thread_ids: &[i64],
    folder_id: Option<i64>,
) -> Result<Vec<i64>> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; thread_ids.len()].join(",");
    let mut values: Vec<rusqlite::types::Value> = thread_ids.iter().map(|t| (*t).into()).collect();
    let sql = match folder_id {
        Some(f) => {
            values.push(f.into());
            format!(
                "SELECT m.id FROM messages m JOIN message_folders mf ON mf.message_id = m.id
                 WHERE m.thread_id IN ({placeholders}) AND mf.folder_id = ?"
            )
        }
        None => format!("SELECT id FROM messages WHERE thread_id IN ({placeholders})"),
    };
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(values), |r| r.get(0))?;
    let mut out: Vec<i64> = rows.collect::<rusqlite::Result<_>>()?;
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Bodies, attachments and search index

#[derive(Debug, Clone, Default)]
pub struct AttachmentMeta {
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
    pub encoding: Option<String>,
}

/// Stores message content and refreshes snippet and search index.
pub fn store_body(
    tx: &Transaction<'_>,
    message_id: i64,
    html: Option<&str>,
    text: Option<&str>,
    snippet: &str,
    attachments: Option<&[AttachmentMeta]>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO bodies (message_id, html, text, fetched_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (message_id) DO UPDATE SET html = excluded.html, text = excluded.text, fetched_at = excluded.fetched_at",
        params![message_id, html.map(pack), text.map(pack), now_ms()],
    )?;
    tx.execute(
        "UPDATE messages SET snippet = ?, body_state = 1 WHERE id = ?",
        params![snippet, message_id],
    )?;
    if let Some(atts) = attachments {
        replace_attachments(tx, message_id, atts)?;
    }
    index_message(tx, message_id, text)?;
    Ok(())
}

pub fn replace_attachments(
    tx: &Transaction<'_>,
    message_id: i64,
    atts: &[AttachmentMeta],
) -> Result<()> {
    tx.execute("DELETE FROM attachments WHERE message_id = ?", [message_id])?;
    let mut st = tx.prepare_cached(
        "INSERT INTO attachments (message_id, part_id, filename, mime_type, size, content_id, inline, encoding)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for a in atts {
        st.execute(params![
            message_id,
            a.part_id,
            a.filename,
            a.mime_type,
            a.size,
            a.content_id,
            a.inline as i64,
            a.encoding
        ])?;
    }
    let has = atts.iter().any(|a| !a.inline);
    tx.execute(
        "UPDATE messages SET has_attachments = ? WHERE id = ?",
        params![has as i64, message_id],
    )?;
    Ok(())
}

pub fn mark_body_failed(tx: &Transaction<'_>, message_id: i64) -> Result<()> {
    tx.execute(
        "UPDATE messages SET body_state = 2 WHERE id = ?",
        [message_id],
    )?;
    Ok(())
}

/// (Re)builds the full-text index row of a message.
pub fn index_message(tx: &Transaction<'_>, message_id: i64, body_text: Option<&str>) -> Result<()> {
    let row = tx
        .query_row(
            "SELECT subject, from_name, from_email, to_json, cc_json FROM messages WHERE id = ?",
            [message_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((subject, from_name, from_email, to, cc)) = row else {
        return Ok(());
    };
    let sender = format!(
        "{} {}",
        from_name.unwrap_or_default(),
        from_email.unwrap_or_default()
    );
    let recipients: String = parse_addrs(&to)
        .into_iter()
        .chain(parse_addrs(&cc))
        .map(|a| format!("{} {}", a.name.unwrap_or_default(), a.email))
        .collect::<Vec<_>>()
        .join(" ");
    let attachments: String = {
        let mut st = tx.prepare_cached(
            "SELECT filename FROM attachments WHERE message_id = ? AND inline = 0",
        )?;
        st.query_map([message_id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let body: String = match body_text {
        Some(t) => t.to_string(),
        None => tx
            .query_row(
                "SELECT text FROM bodies WHERE message_id = ?",
                [message_id],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten()
            .map(|b| unpack(&b))
            .unwrap_or_default(),
    };
    // Keep the index lean: very long bodies rarely add useful terms.
    let body: String = body.chars().take(20_000).collect();
    tx.execute("DELETE FROM message_fts WHERE rowid = ?", [message_id])?;
    tx.execute(
        "INSERT INTO message_fts (rowid, subject, sender, recipients, body, attachments) VALUES (?, ?, ?, ?, ?, ?)",
        params![message_id, subject, sender, recipients, body, attachments],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Threads

/// Recomputes aggregates and folder memberships of the given threads.
/// Returns the folders whose membership may have changed.
pub fn refresh_threads(tx: &Transaction<'_>, thread_ids: &BTreeSet<i64>) -> Result<BTreeSet<i64>> {
    let mut touched_folders = BTreeSet::new();
    for &tid in thread_ids {
        // Folders the thread used to be in.
        {
            let mut st =
                tx.prepare_cached("SELECT folder_id FROM thread_folders WHERE thread_id = ?")?;
            for f in st.query_map([tid], |r| r.get::<_, i64>(0))? {
                touched_folders.insert(f?);
            }
        }
        let rows: Vec<(
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
            bool,
            Option<String>,
            Option<String>,
            bool,
            Option<String>,
            Option<String>,
        )> = {
            let mut st = tx.prepare_cached(
                "SELECT date, flags, subject, from_name, from_email, snippet, has_attachments, calendar_method,
                        list_unsubscribe, is_auto, list_id, keywords
                 FROM messages WHERE thread_id = ? ORDER BY date ASC",
            )?;
            st.query_map([tid], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get::<_, i64>(6)? != 0,
                    r.get(7)?,
                    r.get(8)?,
                    r.get::<_, i64>(9)? != 0,
                    r.get(10)?,
                    r.get(11)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        if rows.is_empty() {
            tx.execute("DELETE FROM threads WHERE id = ?", [tid])?;
            continue;
        }
        let account_id: i64 =
            tx.query_row("SELECT account_id FROM threads WHERE id = ?", [tid], |r| {
                r.get(0)
            })?;
        let my_addresses = identity_emails(tx, account_id)?;

        let subject = rows
            .iter()
            .map(|r| r.2.as_str())
            .find(|s| !s.trim().is_empty())
            .unwrap_or("")
            .to_string();
        let last_date = rows.iter().map(|r| r.0).max().unwrap_or(0);
        let unread = rows
            .iter()
            .filter(|r| r.1 & Flags::SEEN == 0 && r.1 & Flags::DRAFT == 0)
            .count() as i64;
        let flagged = rows.iter().any(|r| r.1 & Flags::FLAGGED != 0);
        let drafts = rows.iter().filter(|r| r.1 & Flags::DRAFT != 0).count() as i64;
        let has_att = rows.iter().any(|r| r.6);
        let is_invite = rows.iter().any(|r| r.7.as_deref() == Some("REQUEST"));
        let snippet = rows
            .iter()
            .rev()
            .find(|r| r.1 & Flags::DRAFT == 0 && !r.5.is_empty())
            .or(rows.last())
            .map(|r| r.5.clone())
            .unwrap_or_default();

        let mut participants: Vec<Address> = Vec::new();
        for r in &rows {
            if let Some(email) = &r.4
                && !participants.iter().any(|p| &p.email == email)
            {
                participants.push(Address::new(r.3.clone(), email.clone()));
            }
        }
        // Keep the conversation starter plus the most recent senders.
        if participants.len() > 6 {
            let first = participants[0].clone();
            let tail: Vec<Address> = participants[participants.len() - 5..].to_vec();
            participants = std::iter::once(first).chain(tail).collect();
        }

        let last_sender = rows
            .iter()
            .rev()
            .find(|r| r.1 & Flags::DRAFT == 0 && r.4.is_some())
            .or(rows.last())
            .and_then(|r| r.4.clone().map(|e| Address::new(r.3.clone(), e)));
        let wrote_myself = rows
            .iter()
            .any(|r| r.4.as_ref().is_some_and(|e| my_addresses.contains(e)));
        let last = rows.last().expect("non-empty");
        let category = classify(
            wrote_myself,
            last.4.as_deref().unwrap_or(""),
            last.8.is_some(),
            last.9,
            last.10.is_some(),
            last.11
                .as_deref()
                .is_some_and(|k| has_keyword(k, OTHER_INBOX_KEYWORD)),
        );

        tx.execute(
            "UPDATE threads SET subject = ?, snippet = ?, last_date = ?, message_count = ?, unread_count = ?,
               flagged = ?, has_attachments = ?, draft_count = ?, participants = ?, category = ?, is_invite = ?,
               last_sender = ?
             WHERE id = ?",
            params![
                subject,
                snippet,
                last_date,
                rows.len() as i64,
                unread,
                flagged as i64,
                has_att as i64,
                drafts,
                serde_json::to_string(&participants)?,
                category,
                is_invite as i64,
                last_sender.as_ref().map(serde_json::to_string).transpose()?,
                tid
            ],
        )?;
        tx.execute("DELETE FROM thread_folders WHERE thread_id = ?", [tid])?;
        tx.execute(
            "INSERT INTO thread_folders (folder_id, thread_id, last_date, unread)
             SELECT mf.folder_id, ?1, ?2, sum(CASE WHEN m.flags & 9 = 0 THEN 1 ELSE 0 END)
             FROM message_folders mf JOIN messages m ON m.id = mf.message_id
             WHERE m.thread_id = ?1 GROUP BY mf.folder_id",
            params![tid, last_date],
        )?;
        let mut st =
            tx.prepare_cached("SELECT folder_id FROM thread_folders WHERE thread_id = ?")?;
        for f in st.query_map([tid], |r| r.get::<_, i64>(0))? {
            touched_folders.insert(f?);
        }
    }
    Ok(touched_folders)
}

fn identity_emails(c: &Connection, account_id: i64) -> Result<HashSet<String>> {
    let mut st = c.prepare_cached(
        "SELECT lower(email) FROM identities WHERE account_id = ? UNION SELECT lower(email) FROM accounts WHERE id = ?",
    )?;
    let rows = st.query_map(params![account_id, account_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Keyword of messages the provider filed outside the user's focused inbox
/// (Outlook's "Other" tab).
pub const OTHER_INBOX_KEYWORD: &str = "$OtherInbox";

fn has_keyword(keywords_json: &str, keyword: &str) -> bool {
    serde_json::from_str::<Vec<String>>(keywords_json)
        .is_ok_and(|k| k.iter().any(|k| k.eq_ignore_ascii_case(keyword)))
}

/// Heuristic split-inbox category of a thread. `other_inbox` is the provider's
/// own verdict that the newest message is not important (Outlook's Focused
/// Inbox); the local mailing-list heuristics still decide between newsletters
/// and notifications.
pub fn classify(
    wrote_myself: bool,
    from: &str,
    has_unsubscribe: bool,
    is_auto: bool,
    has_list_id: bool,
    other_inbox: bool,
) -> &'static str {
    if wrote_myself {
        return "people";
    }
    let local = from.split('@').next().unwrap_or("").to_ascii_lowercase();
    const AUTOMATED_PREFIXES: [&str; 16] = [
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "do_not_reply",
        "notification",
        "notify",
        "alert",
        "mailer-daemon",
        "postmaster",
        "bounce",
        "security",
        "receipt",
        "billing",
        "account",
    ];
    let automated_sender = AUTOMATED_PREFIXES.iter().any(|p| local.starts_with(p))
        || local.contains("noreply")
        || local.contains("no-reply");
    if is_auto || automated_sender {
        return "notifications";
    }
    if has_unsubscribe || has_list_id {
        return "newsletters";
    }
    if other_inbox {
        return "notifications";
    }
    "people"
}

fn summary_from_row(r: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    let participants: String = r.get("participants")?;
    let last_sender: Option<String> = r.get("last_sender")?;
    let folders: Option<String> = r.get("folder_ids")?;
    Ok(ThreadSummary {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        subject: r.get("subject")?,
        snippet: r.get("snippet")?,
        last_date: r.get("last_date")?,
        message_count: r.get("message_count")?,
        unread_count: r.get("unread_count")?,
        flagged: r.get::<_, i64>("flagged")? != 0,
        has_attachments: r.get::<_, i64>("has_attachments")? != 0,
        draft_count: r.get("draft_count")?,
        participants: parse_addrs(&participants),
        last_sender: last_sender.and_then(|s| serde_json::from_str(&s).ok()),
        folder_ids: folders
            .map(|f| f.split(',').filter_map(|x| x.parse().ok()).collect())
            .unwrap_or_default(),
        category: r.get("category")?,
        is_invite: r.get::<_, i64>("is_invite")? != 0,
        muted: r.get::<_, i64>("muted")? != 0,
        snoozed_until: r.get("snoozed_until")?,
    })
}

const SUMMARY_COLUMNS: &str = "t.id, t.account_id, t.subject, t.snippet, t.last_date, t.message_count, t.unread_count,
    t.flagged, t.has_attachments, t.draft_count, t.participants, t.last_sender, t.category, t.is_invite, t.muted, t.snoozed_until,
    (SELECT group_concat(folder_id) FROM thread_folders x WHERE x.thread_id = t.id) AS folder_ids";

pub fn thread_summary(c: &Connection, thread_id: i64) -> Result<ThreadSummary> {
    c.query_row(
        &format!("SELECT {SUMMARY_COLUMNS} FROM threads t WHERE t.id = ?"),
        [thread_id],
        summary_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("thread {thread_id}")))
}

/// Mutes or unmutes conversations.
pub fn set_muted(tx: &Transaction<'_>, thread_ids: &[i64], muted: bool) -> Result<()> {
    let mut st = tx.prepare_cached("UPDATE threads SET muted = ? WHERE id = ?")?;
    for id in thread_ids {
        st.execute(params![muted as i64, id])?;
    }
    Ok(())
}

/// The muted conversations among `thread_ids`.
pub fn muted_among(c: &Connection, thread_ids: &[i64]) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached("SELECT muted FROM threads WHERE id = ?")?;
    let mut out = Vec::new();
    for id in thread_ids {
        let muted: Option<i64> = st.query_row([id], |r| r.get(0)).optional()?;
        if muted == Some(1) && !out.contains(id) {
            out.push(*id);
        }
    }
    Ok(out)
}

pub fn thread_summaries(c: &Connection, ids: &[i64]) -> Result<Vec<ThreadSummary>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(s) = thread_summary(c, *id) {
            out.push(s);
        }
    }
    Ok(out)
}

/// A compiled message-list query (mailbox + filters + optional search).
pub struct ListSql {
    pub from_where: String,
    pub values: Vec<rusqlite::types::Value>,
    pub order: String,
    /// A cheaper `FROM … WHERE …` for counting, when the conditions don't need
    /// the threads table (uses the same values).
    pub count_from_where: Option<String>,
}

/// Builds the SQL for listing threads in a mailbox.
pub fn list_sql(
    c: &Connection,
    mailbox: &Mailbox,
    filter: &ListFilter,
    search: Option<(String, Vec<rusqlite::types::Value>)>,
) -> Result<ListSql> {
    let mut wheres: Vec<String> = Vec::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    let mut order = "t.last_date DESC".to_string();
    let mut from = "threads t".to_string();
    // Folder lists only touch `thread_folders` (and its covering index) unless a
    // condition needs thread columns; counting them then skips the threads table.
    let mut folder_only = false;
    // Snoozed threads leave the inbox; the partial index keeps this lookup tiny.
    const NOT_SNOOZED: &str =
        "tf.thread_id NOT IN (SELECT id FROM threads WHERE snoozed_until IS NOT NULL)";

    match mailbox {
        Mailbox::Folder { id } => {
            from = "thread_folders tf JOIN threads t ON t.id = tf.thread_id".into();
            folder_only = true;
            wheres.push("tf.folder_id = ?".into());
            values.push((*id).into());
            order = "tf.last_date DESC".into();
            let role: Option<String> = c
                .query_row("SELECT role FROM folders WHERE id = ?", [id], |r| r.get(0))
                .optional()?
                .flatten();
            if role.as_deref() == Some("inbox") {
                wheres.push(NOT_SNOOZED.into());
            }
            if filter.unread {
                wheres.push("tf.unread > 0".into());
            }
        }
        Mailbox::Unified { role } => {
            from = "thread_folders tf JOIN threads t ON t.id = tf.thread_id".into();
            folder_only = true;
            // Resolve the folders up front: with a single account, `folder_id = ?`
            // lets the (folder_id, last_date) index return rows already sorted.
            let ids: Vec<i64> = c
                .prepare_cached(
                    "SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id
                     WHERE f.role = ? AND a.enabled = 1",
                )?
                .query_map([role.as_str()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            match ids.as_slice() {
                [] => wheres.push("0".into()),
                [one] => {
                    wheres.push("tf.folder_id = ?".into());
                    values.push((*one).into());
                }
                many => {
                    wheres.push(format!(
                        "tf.folder_id IN ({})",
                        vec!["?"; many.len()].join(",")
                    ));
                    values.extend(many.iter().map(|id| rusqlite::types::Value::from(*id)));
                }
            }
            order = "tf.last_date DESC".into();
            if *role == FolderRole::Inbox {
                wheres.push(NOT_SNOOZED.into());
            }
            if filter.unread {
                wheres.push("tf.unread > 0".into());
            }
        }
        Mailbox::Starred => {
            wheres.push("t.flagged = 1".into());
            wheres.push(NOT_TRASH.into());
        }
        Mailbox::Vip => {
            wheres.push(format!(
                "t.id IN (SELECT m.thread_id FROM messages m WHERE {})",
                crate::vip::FROM_VIP
            ));
            wheres.push(NOT_TRASH.into());
        }
        Mailbox::FollowUp => {
            wheres.push(
                "t.id IN (SELECT m.thread_id FROM followups f
                          JOIN messages m ON m.account_id = f.account_id AND m.message_id = f.message_id
                          WHERE f.notified = 1)"
                    .into(),
            );
        }
        Mailbox::Snoozed => {
            wheres.push("t.snoozed_until IS NOT NULL".into());
            order = "t.snoozed_until ASC".into();
        }
        Mailbox::Everything => {
            wheres.push(NOT_TRASH.into());
        }
    }
    if filter.unread && !matches!(mailbox, Mailbox::Folder { .. } | Mailbox::Unified { .. }) {
        wheres.push("t.unread_count > 0".into());
    }
    if filter.starred {
        wheres.push("t.flagged = 1".into());
        folder_only = false;
    }
    if filter.attachments {
        wheres.push("t.has_attachments = 1".into());
        folder_only = false;
    }
    if let Some(cat) = &filter.category {
        wheres.push("t.category = ?".into());
        values.push(cat.clone().into());
        folder_only = false;
    }
    if let Some((cond, vals)) = search {
        // Applies to the folder row's thread, so it works for counting too.
        let column = if from.starts_with("thread_folders") {
            "tf.thread_id"
        } else {
            "t.id"
        };
        wheres.push(format!(
            "{column} IN (SELECT m.thread_id FROM messages m WHERE {cond})"
        ));
        values.extend(vals);
    }
    let conditions = if wheres.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", wheres.join(" AND "))
    };
    Ok(ListSql {
        count_from_where: folder_only.then(|| format!("thread_folders tf{conditions}")),
        from_where: format!("{from}{conditions}"),
        values,
        order,
    })
}

const NOT_TRASH: &str =
    "NOT EXISTS (SELECT 1 FROM thread_folders z JOIN folders zf ON zf.id = z.folder_id
     WHERE z.thread_id = t.id AND zf.role IN ('trash', 'junk'))
     AND EXISTS (SELECT 1 FROM thread_folders y WHERE y.thread_id = t.id)";

/// Runs a list query and returns one page.
pub fn query_threads(c: &Connection, sql: &ListSql, offset: i64, limit: i64) -> Result<ThreadPage> {
    let total: i64 = c.query_row(
        &format!(
            "SELECT count(*) FROM {}",
            sql.count_from_where.as_deref().unwrap_or(&sql.from_where)
        ),
        params_from_iter(sql.values.iter()),
        |r| r.get(0),
    )?;
    let mut values = sql.values.clone();
    values.push(limit.into());
    values.push(offset.into());
    let mut st = c.prepare_cached(&format!(
        "SELECT {SUMMARY_COLUMNS} FROM {} ORDER BY {} LIMIT ? OFFSET ?",
        sql.from_where, sql.order
    ))?;
    let items = st
        .query_map(params_from_iter(values), summary_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ThreadPage {
        total,
        offset,
        items,
    })
}

/// Full conversation with message bodies (when downloaded).
pub fn get_thread(c: &Connection, thread_id: i64) -> Result<ThreadDetail> {
    let thread = thread_summary(c, thread_id)?;
    let mut st = c.prepare_cached(
        "SELECT m.*, (SELECT group_concat(folder_id) FROM message_folders x WHERE x.message_id = m.id) AS folder_ids,
           (SELECT kind FROM message_crypto k WHERE k.message_id = m.id) AS crypto_kind
         FROM messages m WHERE m.thread_id = ? ORDER BY m.date ASC, m.id ASC",
    )?;
    let mut messages: Vec<MessageView> = st
        .query_map([thread_id], message_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    // The same message can exist in several folders (copies); show it once.
    let mut seen_ids: HashSet<String> = HashSet::new();
    messages.retain(|m| match &m.message_id {
        Some(mid) if !m.flags.draft => seen_ids.insert(mid.clone()),
        _ => true,
    });
    for m in &mut messages {
        m.attachments = attachments_of(c, m.id)?;
        m.body = body_of(c, m.id)?;
    }
    Ok(ThreadDetail { thread, messages })
}

pub fn get_message(c: &Connection, message_id: i64) -> Result<MessageView> {
    let mut m = c
        .query_row(
            "SELECT m.*, (SELECT group_concat(folder_id) FROM message_folders x WHERE x.message_id = m.id) AS folder_ids,
               (SELECT kind FROM message_crypto k WHERE k.message_id = m.id) AS crypto_kind
             FROM messages m WHERE m.id = ?",
            [message_id],
            message_from_row,
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("message {message_id}")))?;
    m.attachments = attachments_of(c, m.id)?;
    m.body = body_of(c, m.id)?;
    Ok(m)
}

fn message_from_row(r: &Row<'_>) -> rusqlite::Result<MessageView> {
    let from_email: Option<String> = r.get("from_email")?;
    let folders: Option<String> = r.get("folder_ids")?;
    let refs: Option<String> = r.get("refs")?;
    let unsub: Option<String> = r.get("list_unsubscribe")?;
    let unsub_post: i64 = r.get("unsubscribe_post")?;
    Ok(MessageView {
        id: r.get("id")?,
        thread_id: r.get("thread_id")?,
        account_id: r.get("account_id")?,
        folder_ids: folders
            .map(|f| f.split(',').filter_map(|x| x.parse().ok()).collect())
            .unwrap_or_default(),
        message_id: r.get("message_id")?,
        in_reply_to: r.get("in_reply_to")?,
        references: refs
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        subject: r.get("subject")?,
        from: from_email.map(|e| Address::new(r.get("from_name").ok().flatten(), e)),
        to: parse_addrs(&r.get::<_, String>("to_json")?),
        cc: parse_addrs(&r.get::<_, String>("cc_json")?),
        bcc: parse_addrs(&r.get::<_, String>("bcc_json")?),
        reply_to: parse_addrs(&r.get::<_, String>("reply_to_json")?),
        date: r.get("date")?,
        flags: Flags::from_bits(r.get("flags")?),
        snippet: r.get("snippet")?,
        size: r.get("size")?,
        has_attachments: r.get::<_, i64>("has_attachments")? != 0,
        attachments: Vec::new(),
        body: None,
        unsubscribe: unsub.map(|u| crate::mime::parse_unsubscribe(&u, unsub_post != 0)),
        calendar_method: r.get("calendar_method")?,
        is_auto: r.get::<_, i64>("is_auto")? != 0,
        crypto: r.get("crypto_kind")?,
    })
}

pub fn attachments_of(c: &Connection, message_id: i64) -> Result<Vec<AttachmentInfo>> {
    let mut st = c.prepare_cached(
        "SELECT id, filename, mime_type, size, content_id, inline FROM attachments WHERE message_id = ? ORDER BY id",
    )?;
    let rows = st.query_map([message_id], |r| {
        Ok(AttachmentInfo {
            id: r.get(0)?,
            filename: r.get(1)?,
            mime_type: r.get(2)?,
            size: r.get(3)?,
            content_id: r.get(4)?,
            inline: r.get::<_, i64>(5)? != 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn body_of(c: &Connection, message_id: i64) -> Result<Option<MessageBody>> {
    Ok(c.query_row(
        "SELECT html, text FROM bodies WHERE message_id = ?",
        [message_id],
        |r| {
            let html: Option<Vec<u8>> = r.get(0)?;
            let text: Option<Vec<u8>> = r.get(1)?;
            Ok(MessageBody {
                html: html.map(|h| unpack(&h)),
                text: text.map(|t| unpack(&t)),
            })
        },
    )
    .optional()?)
}

#[derive(Debug, Clone)]
pub struct AttachmentRow {
    pub id: i64,
    pub message_id: i64,
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub encoding: Option<String>,
}

pub fn get_attachment(c: &Connection, id: i64) -> Result<AttachmentRow> {
    c.query_row(
        "SELECT id, message_id, part_id, filename, mime_type, size, content_id, encoding FROM attachments WHERE id = ?",
        [id],
        |r| {
            Ok(AttachmentRow {
                id: r.get(0)?,
                message_id: r.get(1)?,
                part_id: r.get(2)?,
                filename: r.get(3)?,
                mime_type: r.get(4)?,
                size: r.get(5)?,
                content_id: r.get(6)?,
                encoding: r.get(7)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("attachment {id}")))
}

pub fn attachment_by_cid(
    c: &Connection,
    message_id: i64,
    cid: &str,
) -> Result<Option<AttachmentRow>> {
    let id: Option<i64> = c
        .query_row(
            "SELECT id FROM attachments WHERE message_id = ? AND content_id = ?",
            params![message_id, cid],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|id| get_attachment(c, id)).transpose()
}

/// Messages of an account still missing a body, newest first. With
/// `skip_attachments`, messages known to carry attachments are left out (their
/// download waits until they are opened).
pub fn messages_without_body(
    c: &Connection,
    account_id: i64,
    since: Option<i64>,
    skip_attachments: bool,
    limit: i64,
) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached(
        "SELECT id FROM messages WHERE account_id = ? AND body_state = 0 AND date >= ?
           AND (? = 0 OR has_attachments = 0)
         ORDER BY date DESC LIMIT ?",
    )?;
    let rows = st.query_map(
        params![
            account_id,
            since.unwrap_or(0),
            skip_attachments as i64,
            limit
        ],
        |r| r.get(0),
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Raw headers needed to act on a message (structure for partial fetches).
pub fn message_structure(c: &Connection, message_id: i64) -> Result<(Option<String>, i64)> {
    Ok(c.query_row(
        "SELECT structure, size FROM messages WHERE id = ?",
        [message_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Snoozes or un-snoozes threads.
pub fn set_snooze(
    tx: &Transaction<'_>,
    thread_ids: &[i64],
    until: Option<i64>,
) -> Result<BTreeSet<i64>> {
    let mut folders = BTreeSet::new();
    for t in thread_ids {
        tx.execute(
            "UPDATE threads SET snoozed_until = ? WHERE id = ?",
            params![until, t],
        )?;
        if until.is_none() {
            // Bring the conversation back to the top of the inbox.
            tx.execute(
                "UPDATE thread_folders SET last_date = max(last_date, ?) WHERE thread_id = ?",
                params![now_ms(), t],
            )?;
        }
        let mut st =
            tx.prepare_cached("SELECT folder_id FROM thread_folders WHERE thread_id = ?")?;
        for f in st.query_map([t], |r| r.get::<_, i64>(0))? {
            folders.insert(f?);
        }
    }
    refresh_folder_counts(tx, &folders)?;
    Ok(folders)
}

/// Threads whose snooze time has passed.
pub fn due_snoozes(c: &Connection, now: i64) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached(
        "SELECT id FROM threads WHERE snoozed_until IS NOT NULL AND snoozed_until <= ?",
    )?;
    let rows = st.query_map([now], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
