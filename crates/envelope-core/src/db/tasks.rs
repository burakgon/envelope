//! Task lists and tasks.
//!
//! A task row keeps the task as last edited here (the columns Envelope
//! shows and edits), the server copy those started from (`data`: a VTODO,
//! a Google or Graph task as JSON, …) and a `local_state` saying whether it
//! still has to be written to the server. Sync never overwrites rows with
//! local changes; the push step reconciles them with the server copy.

use std::collections::{HashMap, HashSet};

use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::db::now_ms;
use crate::error::{Error, Result};
use crate::tasks::{TaskListKind, TaskSource, day_in};

/// `local_state`: in sync with the server (or a list on this computer).
pub const SYNCED: i64 = 0;
/// `local_state`: created here, not on the server yet.
pub const CREATED: i64 = 1;
/// `local_state`: changed here, not written to the server yet.
pub const MODIFIED: i64 = 2;
/// `local_state`: deleted here, still on the server.
pub const DELETED: i64 = 3;

/// Distance between the positions of tasks added at the end, which leaves
/// room to move tasks between others without renumbering them.
pub const POSITION_STEP: i64 = 1024;

/// Color of lists on this computer.
const LOCAL_COLOR: &str = "#6d5efc";

/// Per-list sync bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ListSyncState {
    /// CalDAV `getctag`.
    pub ctag: Option<String>,
    /// CalDAV sync token.
    pub sync_token: Option<String>,
    /// What else a service needs to pull only what changed (a delta link,
    /// the time of the last pull, …).
    pub cursor: Option<serde_json::Value>,
    pub last_sync: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ListRow {
    pub id: i64,
    pub account_id: Option<i64>,
    pub kind: TaskListKind,
    pub remote_id: String,
    pub name: String,
    /// `#rrggbb`: the server's color, else the account's.
    pub color: String,
    pub read_only: bool,
    pub is_default: bool,
    pub sort_order: i64,
    pub sync_state: ListSyncState,
}

const LIST_COLUMNS: &str = "l.id, l.account_id, l.kind, l.remote_id, l.name,
    coalesce(nullif(l.color, ''), a.color, '#6d5efc') AS shown_color, l.read_only, l.is_default,
    l.sort_order, l.sync_state";

/// The order lists are shown in: this computer's first (its default list
/// on top), then by account, each account's default list first.
const LIST_ORDER: &str = "l.account_id IS NOT NULL, a.sort_order, l.account_id, l.is_default DESC,
    l.sort_order, l.name COLLATE NOCASE, l.id";

fn list_from_row(r: &Row<'_>) -> rusqlite::Result<ListRow> {
    let kind: String = r.get("kind")?;
    let state: String = r.get("sync_state")?;
    Ok(ListRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        kind: TaskListKind::parse(&kind).unwrap_or(TaskListKind::Local),
        remote_id: r.get("remote_id")?,
        name: r.get("name")?,
        color: r.get("shown_color")?,
        read_only: r.get::<_, i64>("read_only")? != 0,
        is_default: r.get::<_, i64>("is_default")? != 0,
        sort_order: r.get("sort_order")?,
        sync_state: serde_json::from_str(&state).unwrap_or_default(),
    })
}

/// Lists of this computer and of enabled accounts, in display order.
pub fn list_lists(c: &Connection) -> Result<Vec<ListRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {LIST_COLUMNS} FROM task_lists l LEFT JOIN accounts a ON a.id = l.account_id
         WHERE l.account_id IS NULL OR a.enabled = 1 ORDER BY {LIST_ORDER}"
    ))?;
    let rows = st.query_map([], list_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn account_lists(c: &Connection, account_id: i64) -> Result<Vec<ListRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {LIST_COLUMNS} FROM task_lists l LEFT JOIN accounts a ON a.id = l.account_id
         WHERE l.account_id = ? ORDER BY l.is_default DESC, l.sort_order, l.id"
    ))?;
    let rows = st.query_map([account_id], list_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_list(c: &Connection, id: i64) -> Result<ListRow> {
    c.query_row(
        &format!(
            "SELECT {LIST_COLUMNS} FROM task_lists l LEFT JOIN accounts a ON a.id = l.account_id
             WHERE l.id = ?"
        ),
        [id],
        list_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("task list {id}")))
}

/// The default list on this computer, if it exists.
pub fn local_default_list(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM task_lists WHERE account_id IS NULL AND kind = 'local' AND is_default = 1
         ORDER BY id LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()?)
}

pub fn create_local_list(tx: &Transaction<'_>, name: &str, is_default: bool) -> Result<i64> {
    let order: i64 = tx.query_row(
        "SELECT coalesce(max(sort_order), 0) + 1 FROM task_lists WHERE account_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO task_lists (account_id, kind, remote_id, name, color, is_default, sort_order)
         VALUES (NULL, 'local', ?, ?, ?, ?, ?)",
        params![
            format!("local-{}", uuid::Uuid::new_v4()),
            name,
            LOCAL_COLOR,
            is_default as i64,
            if is_default { 0 } else { order }
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// A list as reported by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListUpsert {
    pub remote_id: String,
    pub name: String,
    /// `#rrggbb`, when the server keeps one.
    pub color: Option<String>,
    pub read_only: bool,
    pub is_default: bool,
    pub sort_order: i64,
}

/// Creates/updates the lists of an account and removes the ones that
/// vanished (with their tasks). Returns the ids of lists that were added,
/// changed or removed.
pub fn sync_lists(
    tx: &Transaction<'_>,
    account_id: i64,
    kind: TaskListKind,
    lists: &[ListUpsert],
) -> Result<Vec<i64>> {
    let mut changed = Vec::new();
    let existing: HashMap<String, (i64, String, String, bool, bool)> = {
        let mut st = tx.prepare(
            "SELECT remote_id, id, name, color, read_only, is_default FROM task_lists WHERE account_id = ?",
        )?;
        st.query_map([account_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)? != 0,
                    r.get::<_, i64>(5)? != 0,
                ),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for list in lists {
        let color = list.color.clone().unwrap_or_default();
        match existing.get(&list.remote_id) {
            Some((id, name, old_color, read_only, is_default)) => {
                if *name != list.name
                    || *old_color != color
                    || *read_only != list.read_only
                    || *is_default != list.is_default
                {
                    changed.push(*id);
                }
                tx.execute(
                    "UPDATE task_lists SET name = ?, color = ?, read_only = ?, is_default = ?, sort_order = ?,
                       kind = ? WHERE id = ?",
                    params![
                        list.name,
                        color,
                        list.read_only as i64,
                        list.is_default as i64,
                        list.sort_order,
                        kind.as_str(),
                        id
                    ],
                )?;
            }
            None => changed.push(insert_list(tx, account_id, kind, list)?),
        }
    }
    let keep: HashSet<&str> = lists.iter().map(|l| l.remote_id.as_str()).collect();
    for (remote, (id, ..)) in &existing {
        if !keep.contains(remote.as_str()) {
            tx.execute("DELETE FROM task_lists WHERE id = ?", [id])?;
            changed.push(*id);
        }
    }
    Ok(changed)
}

/// Adds one list of an account (one just created on the server). Returns
/// its id; a list already known by its remote id is updated instead.
pub fn insert_list(
    tx: &Transaction<'_>,
    account_id: i64,
    kind: TaskListKind,
    list: &ListUpsert,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO task_lists (account_id, kind, remote_id, name, color, read_only, is_default, sort_order)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (account_id, remote_id) DO UPDATE SET name = excluded.name, color = excluded.color,
           read_only = excluded.read_only",
        params![
            account_id,
            kind.as_str(),
            list.remote_id,
            list.name,
            list.color.clone().unwrap_or_default(),
            list.read_only as i64,
            list.is_default as i64,
            list.sort_order
        ],
    )?;
    Ok(tx.query_row(
        "SELECT id FROM task_lists WHERE account_id = ? AND remote_id = ?",
        params![account_id, list.remote_id],
        |r| r.get(0),
    )?)
}

pub fn rename_list(tx: &Transaction<'_>, id: i64, name: &str) -> Result<()> {
    tx.execute(
        "UPDATE task_lists SET name = ? WHERE id = ?",
        params![name, id],
    )?;
    Ok(())
}

/// Deletes a list with its tasks.
pub fn delete_list(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("DELETE FROM task_lists WHERE id = ?", [id])?;
    Ok(())
}

pub fn set_list_sync_state(tx: &Transaction<'_>, id: i64, state: &ListSyncState) -> Result<()> {
    tx.execute(
        "UPDATE task_lists SET sync_state = ? WHERE id = ?",
        params![serde_json::to_string(state)?, id],
    )?;
    Ok(())
}

/// How many tasks a list has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListCounts {
    /// Not completed yet (subtasks included).
    pub open: i64,
    /// Completed top-level tasks: what clearing the completed tasks deletes.
    pub completed: i64,
}

/// The counts of every list that has tasks.
pub fn list_counts(c: &Connection) -> Result<HashMap<i64, ListCounts>> {
    let mut st = c.prepare_cached(
        "SELECT list_id, sum(completed_at IS NULL),
           sum(completed_at IS NOT NULL AND parent_id IS NULL)
         FROM tasks WHERE local_state != 3 GROUP BY list_id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get(0)?,
            ListCounts {
                open: r.get(1)?,
                completed: r.get(2)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------------------
// Task rows

/// What Envelope shows and edits of a task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFields {
    pub title: String,
    pub notes: String,
    /// `YYYY-MM-DD`, local.
    pub due_date: Option<String>,
    pub due_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub important: bool,
    pub parent_id: Option<i64>,
    /// The parent's `uid`, which links the two on the server.
    pub parent_uid: Option<String>,
    /// Order among the tasks with the same parent (lower first).
    pub position: i64,
}

/// A stored task.
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: i64,
    pub list_id: i64,
    pub parent_id: Option<i64>,
    pub parent_uid: Option<String>,
    pub uid: String,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub title: String,
    pub notes: String,
    pub due_date: Option<String>,
    pub due_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub important: bool,
    pub position: i64,
    pub source: Option<TaskSource>,
    /// The server copy local changes started from.
    pub data: Option<String>,
    pub repeats: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub local_state: i64,
    pub version: i64,
    pub push_attempts: i64,
    pub push_error: Option<String>,
    /// Subtasks (not deleted), and how many of them are done.
    pub subtask_count: i64,
    pub subtasks_done: i64,
}

impl TaskRow {
    pub fn fields(&self) -> TaskFields {
        TaskFields {
            title: self.title.clone(),
            notes: self.notes.clone(),
            due_date: self.due_date.clone(),
            due_at: self.due_at,
            completed_at: self.completed_at,
            important: self.important,
            parent_id: self.parent_id,
            parent_uid: self.parent_uid.clone(),
            position: self.position,
        }
    }
}

const TASK_COLUMNS: &str = "t.*,
    (SELECT count(*) FROM tasks c WHERE c.parent_id = t.id AND c.local_state != 3) AS subtask_count,
    (SELECT count(*) FROM tasks c WHERE c.parent_id = t.id AND c.local_state != 3
       AND c.completed_at IS NOT NULL) AS subtasks_done";

/// Tasks of visible lists.
const TASK_FROM: &str = "FROM tasks t JOIN task_lists l ON l.id = t.list_id
    LEFT JOIN accounts a ON a.id = l.account_id";

/// Tasks shown: not deleted, in lists of this computer or enabled accounts.
const VISIBLE: &str = "t.local_state != 3 AND (l.account_id IS NULL OR a.enabled = 1)";

/// Earliest due first: by day, then time, tasks without a time last within
/// a day; important tasks first among equals.
const BY_DUE: &str = "t.due_date, t.due_at IS NULL, t.due_at, t.important DESC, t.position, t.id";

fn task_from_row(r: &Row<'_>) -> rusqlite::Result<TaskRow> {
    let source: Option<String> = r.get("source")?;
    Ok(TaskRow {
        id: r.get("id")?,
        list_id: r.get("list_id")?,
        parent_id: r.get("parent_id")?,
        parent_uid: r.get("parent_uid")?,
        uid: r.get("uid")?,
        remote_id: r.get("remote_id")?,
        etag: r.get("etag")?,
        title: r.get("title")?,
        notes: r.get("notes")?,
        due_date: r.get("due_date")?,
        due_at: r.get("due_at")?,
        completed_at: r.get("completed_at")?,
        important: r.get::<_, i64>("important")? != 0,
        position: r.get("position")?,
        source: source.and_then(|s| serde_json::from_str(&s).ok()),
        data: r.get("data")?,
        repeats: r.get::<_, i64>("repeats")? != 0,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        local_state: r.get("local_state")?,
        version: r.get("version")?,
        push_attempts: r.get("push_attempts")?,
        push_error: r.get("push_error")?,
        subtask_count: r.get("subtask_count")?,
        subtasks_done: r.get("subtasks_done")?,
    })
}

fn tasks_where(
    c: &Connection,
    filter: &str,
    order: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<TaskRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {TASK_COLUMNS} {TASK_FROM} WHERE {VISIBLE} AND {filter} ORDER BY {order}"
    ))?;
    let rows = st.query_map(args, task_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_task(c: &Connection, id: i64) -> Result<TaskRow> {
    c.query_row(
        &format!("SELECT {TASK_COLUMNS} FROM tasks t WHERE t.id = ?"),
        [id],
        task_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("task {id}")))
}

/// Open top-level tasks of a list, in order.
pub fn list_open(c: &Connection, list_id: i64) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.list_id = ? AND t.parent_id IS NULL AND t.completed_at IS NULL",
        "t.position, t.created_at, t.id",
        [list_id],
    )
}

/// Completed top-level tasks of a list, most recently completed first.
pub fn list_completed(c: &Connection, list_id: i64, limit: i64) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.list_id = ? AND t.parent_id IS NULL AND t.completed_at IS NOT NULL",
        "t.completed_at DESC, t.id DESC LIMIT ?",
        params![list_id, limit],
    )
}

/// Open tasks (subtasks too) due on or before `day`.
pub fn due_until(c: &Connection, day: &str) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.completed_at IS NULL AND t.due_date IS NOT NULL AND t.due_date <= ?",
        BY_DUE,
        [day],
    )
}

/// Open tasks (subtasks too) due after `day`.
pub fn due_after(c: &Connection, day: &str) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.completed_at IS NULL AND t.due_date > ?",
        BY_DUE,
        [day],
    )
}

/// Open important tasks (subtasks too), by due date, undated ones last.
pub fn important(c: &Connection) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.completed_at IS NULL AND t.important = 1",
        &format!("t.due_date IS NULL, {BY_DUE}"),
        [],
    )
}

/// Open top-level tasks of every visible list, list by list.
pub fn all_open(c: &Connection) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.completed_at IS NULL AND t.parent_id IS NULL",
        &format!("{LIST_ORDER}, t.position, t.created_at, t.id"),
        [],
    )
}

/// Open tasks due at a time in `[from, to)` or, without a time, on a day
/// from `first_day` to `last_day`.
pub fn due_between(
    c: &Connection,
    from: i64,
    to: i64,
    first_day: &str,
    last_day: &str,
) -> Result<Vec<TaskRow>> {
    tasks_where(
        c,
        "t.completed_at IS NULL AND ((t.due_at IS NOT NULL AND t.due_at >= ? AND t.due_at < ?)
           OR (t.due_at IS NULL AND t.due_date >= ? AND t.due_date <= ?))",
        BY_DUE,
        params![from, to, first_day, last_day],
    )
}

/// Subtasks of a task, in order, completed ones included.
pub fn children(c: &Connection, parent_id: i64) -> Result<Vec<TaskRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks t WHERE t.parent_id = ? AND t.local_state != 3
         ORDER BY t.position, t.created_at, t.id"
    ))?;
    let rows = st.query_map([parent_id], task_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Completed top-level tasks of a list, which clearing its completed
/// tasks deletes (with their subtasks). Completed subtasks of open tasks
/// stay with them.
pub fn completed_in(c: &Connection, list_id: i64) -> Result<Vec<TaskRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks t WHERE t.list_id = ? AND t.parent_id IS NULL
           AND t.local_state != 3 AND t.completed_at IS NOT NULL ORDER BY t.id"
    ))?;
    let rows = st.query_map([list_id], task_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `(id, position)` of the tasks with the same parent, in order.
pub fn siblings(c: &Connection, list_id: i64, parent_id: Option<i64>) -> Result<Vec<(i64, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT id, position FROM tasks WHERE list_id = ? AND parent_id IS ? AND local_state != 3
         ORDER BY position, created_at, id",
    )?;
    let rows = st.query_map(params![list_id, parent_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The highest position among the tasks with the same parent (0 if none).
pub fn max_position(c: &Connection, list_id: i64, parent_id: Option<i64>) -> Result<i64> {
    Ok(c.query_row(
        "SELECT coalesce(max(position), 0) FROM tasks WHERE list_id = ? AND parent_id IS ?
           AND local_state != 3",
        params![list_id, parent_id],
        |r| r.get(0),
    )?)
}

/// A task created here.
#[derive(Debug, Clone)]
pub struct NewTask<'a> {
    pub list_id: i64,
    pub uid: &'a str,
    /// Assigned up front where the client names resources (a CalDAV href),
    /// which makes repeated uploads idempotent.
    pub remote_id: Option<&'a str>,
    pub fields: &'a TaskFields,
    pub source: Option<&'a TaskSource>,
    /// A server copy to start from (a task moved between CalDAV lists).
    pub data: Option<&'a str>,
    pub repeats: bool,
    pub created_at: i64,
    pub state: i64,
}

pub fn insert_local(tx: &Transaction<'_>, task: &NewTask<'_>) -> Result<i64> {
    let f = task.fields;
    let source = task.source.map(serde_json::to_string).transpose()?;
    tx.execute(
        "INSERT INTO tasks (list_id, parent_id, parent_uid, uid, remote_id, title, notes, due_date, due_at,
           completed_at, important, position, source, data, repeats, created_at, updated_at, local_state, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
        params![
            task.list_id,
            f.parent_id,
            f.parent_uid,
            task.uid,
            task.remote_id,
            f.title,
            f.notes,
            f.due_date,
            f.due_at,
            f.completed_at,
            f.important as i64,
            f.position,
            source,
            task.data,
            task.repeats as i64,
            task.created_at,
            now_ms(),
            task.state
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Stores a local edit of a task. A new due time is announced again.
pub fn update_local(tx: &Transaction<'_>, id: i64, f: &TaskFields, state: i64) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET title = ?1, notes = ?2,
           notified = CASE WHEN due_at IS ?4 THEN notified ELSE 0 END,
           due_date = ?3, due_at = ?4, completed_at = ?5, important = ?6, parent_id = ?7,
           parent_uid = ?8, position = ?9, local_state = ?10, updated_at = ?11, version = version + 1,
           push_attempts = 0, push_after = 0, push_error = NULL
         WHERE id = ?12",
        params![
            f.title,
            f.notes,
            f.due_date,
            f.due_at,
            f.completed_at,
            f.important as i64,
            f.parent_id,
            f.parent_uid,
            f.position,
            state,
            now_ms(),
            id
        ],
    )?;
    Ok(())
}

/// Moves a task between lists on this computer.
pub fn move_local(tx: &Transaction<'_>, id: i64, list_id: i64, f: &TaskFields) -> Result<()> {
    tx.execute("UPDATE tasks SET list_id = ? WHERE id = ?", [list_id, id])?;
    update_local(tx, id, f, SYNCED)
}

/// Marks a task deleted here; the push step deletes it on the server.
pub fn mark_deleted(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET local_state = ?, version = version + 1, push_attempts = 0, push_after = 0,
           push_error = NULL WHERE id = ?",
        params![DELETED, id],
    )?;
    Ok(())
}

pub fn delete_row(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("DELETE FROM tasks WHERE id = ?", [id])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The message a task came from

/// The source of a task made from a stored message.
pub fn message_source(c: &Connection, message_id: i64) -> Result<TaskSource> {
    c.query_row(
        "SELECT message_id, thread_id, account_id, subject, from_name, from_email FROM messages
         WHERE id = ?",
        [message_id],
        |r| {
            let header: Option<String> = r.get(0)?;
            let name: Option<String> = r.get(4)?;
            let email: Option<String> = r.get(5)?;
            Ok(TaskSource {
                message_id: header
                    .unwrap_or_default()
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned(),
                thread_id: r.get(1)?,
                account_id: r.get(2)?,
                subject: r.get(3)?,
                from: name
                    .filter(|n| !n.trim().is_empty())
                    .or(email)
                    .map(|f| f.trim().to_owned()),
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("message {message_id}")))
}

/// A stored source with its conversation as it is now: found again by
/// `Message-ID` (sync may have regrouped messages), else the remembered
/// one; `None` for both when the account or message is gone.
pub fn resolve_source(c: &Connection, source: &TaskSource) -> Result<TaskSource> {
    let mut resolved = source.clone();
    let account = match source.account_id {
        Some(id) => c
            .query_row("SELECT id FROM accounts WHERE id = ?", [id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?,
        None => None,
    };
    resolved.account_id = account;
    resolved.thread_id = match account {
        None => None,
        Some(account) => {
            let found: Option<i64> = if source.message_id.is_empty() {
                None
            } else {
                c.query_row(
                    "SELECT thread_id FROM messages WHERE account_id = ? AND message_id = ?
                     ORDER BY date DESC LIMIT 1",
                    params![account, source.message_id],
                    |r| r.get(0),
                )
                .optional()?
            };
            match (found, source.thread_id) {
                (Some(thread), _) => Some(thread),
                (None, Some(thread)) => c
                    .query_row("SELECT id FROM threads WHERE id = ?", [thread], |r| {
                        r.get::<_, i64>(0)
                    })
                    .optional()?,
                (None, None) => None,
            }
        }
    };
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Due notifications

/// Open tasks due at a time up to `now` that were not announced yet:
/// `(id, title, due_at)`.
pub fn due_unnotified(c: &Connection, now: i64) -> Result<Vec<(i64, String, i64)>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT t.id, t.title, t.due_at {TASK_FROM}
         WHERE {VISIBLE} AND t.notified = 0 AND t.completed_at IS NULL AND t.due_at IS NOT NULL
           AND t.due_at <= ? ORDER BY t.due_at, t.id"
    ))?;
    let rows = st.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn mark_notified(tx: &Transaction<'_>, ids: &[i64]) -> Result<()> {
    for id in ids {
        tx.execute("UPDATE tasks SET notified = 1 WHERE id = ?", [id])?;
    }
    Ok(())
}

/// Works out again, in `zone`, the day of every task due at a time of day
/// (after the time zone changed). Returns the lists whose tasks changed.
pub fn redo_due_days(tx: &Transaction<'_>, zone: Tz) -> Result<Vec<i64>> {
    let timed: Vec<(i64, i64, i64, Option<String>)> = {
        let mut st =
            tx.prepare("SELECT id, list_id, due_at, due_date FROM tasks WHERE due_at IS NOT NULL")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut lists = Vec::new();
    for (id, list_id, at, day) in timed {
        let local = day_in(at, zone);
        if day.as_deref() != Some(local.as_str()) {
            tx.execute(
                "UPDATE tasks SET due_date = ? WHERE id = ?",
                params![local, id],
            )?;
            lists.push(list_id);
        }
    }
    lists.sort_unstable();
    lists.dedup();
    Ok(lists)
}

/// When the next open task not announced yet is due, after `now`.
pub fn next_due_at(c: &Connection, now: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(due_at) FROM tasks WHERE notified = 0 AND completed_at IS NULL AND local_state != 3
           AND due_at > ?",
        [now],
        |r| r.get(0),
    )?)
}

// ---------------------------------------------------------------------------
// Sync

/// A task as a server has it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteTask {
    /// The task's id on the server (CalDAV href, Google or Graph id).
    pub remote_id: String,
    pub etag: Option<String>,
    /// iCalendar `UID`, or the id other tasks name it by as their parent.
    pub uid: String,
    pub title: String,
    pub notes: String,
    /// `YYYY-MM-DD`, local.
    pub due_date: Option<String>,
    /// Due at a time of day.
    pub due_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub important: bool,
    /// The parent's `uid`.
    pub parent_uid: Option<String>,
    /// Order among its siblings, when the server keeps one (lower first).
    pub position: Option<i64>,
    pub repeats: bool,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    /// The server's representation, kept for lossless updates and merges.
    pub data: Option<String>,
}

/// `remote_id → (etag, local_state)` of every task of a list that exists
/// on the server.
pub type RemoteIndex = HashMap<String, (Option<String>, i64)>;

pub fn remote_index(c: &Connection, list_id: i64) -> Result<RemoteIndex> {
    let mut st = c.prepare_cached(
        "SELECT remote_id, etag, local_state FROM tasks WHERE list_id = ? AND remote_id IS NOT NULL",
    )?;
    let rows = st.query_map([list_id], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Inserts or updates a task from the server. Rows with local changes are
/// left alone (`None`); the push step reconciles them. A changed due time
/// is announced again.
pub fn upsert_synced(tx: &Transaction<'_>, list_id: i64, task: &RemoteTask) -> Result<Option<i64>> {
    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, local_state FROM tasks WHERE list_id = ? AND remote_id = ? ORDER BY id LIMIT 1",
            params![list_id, task.remote_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match existing {
        Some((_, state)) if state != SYNCED => Ok(None),
        Some((id, _)) => {
            tx.execute(
                "UPDATE tasks SET uid = ?1, etag = ?2, title = ?3, notes = ?4,
                   notified = CASE WHEN due_at IS ?6 THEN notified ELSE 0 END,
                   due_date = ?5, due_at = ?6, completed_at = ?7, important = ?8, parent_uid = ?9,
                   position = coalesce(?10, position), repeats = ?11, data = ?12,
                   created_at = coalesce(?13, created_at), updated_at = coalesce(?14, updated_at)
                 WHERE id = ?15",
                params![
                    task.uid,
                    task.etag,
                    task.title,
                    task.notes,
                    task.due_date,
                    task.due_at,
                    task.completed_at,
                    task.important as i64,
                    task.parent_uid,
                    task.position,
                    task.repeats as i64,
                    task.data,
                    task.created_at,
                    task.updated_at,
                    id
                ],
            )?;
            Ok(Some(id))
        }
        None => {
            let position = match task.position {
                Some(position) => position,
                None => {
                    tx.query_row(
                        "SELECT coalesce(max(position), 0) FROM tasks WHERE list_id = ?",
                        [list_id],
                        |r| r.get::<_, i64>(0),
                    )? + POSITION_STEP
                }
            };
            let now = now_ms();
            tx.execute(
                "INSERT INTO tasks (list_id, parent_uid, uid, remote_id, etag, title, notes, due_date, due_at,
                   completed_at, important, position, data, repeats, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    list_id,
                    task.parent_uid,
                    task.uid,
                    task.remote_id,
                    task.etag,
                    task.title,
                    task.notes,
                    task.due_date,
                    task.due_at,
                    task.completed_at,
                    task.important as i64,
                    position,
                    task.data,
                    task.repeats as i64,
                    task.created_at.unwrap_or(now),
                    task.updated_at.unwrap_or(now)
                ],
            )?;
            Ok(Some(tx.last_insert_rowid()))
        }
    }
}

/// Removes a task the server no longer has. Tasks changed here stay:
/// pushing them puts them back on the server. Returns whether a row went.
pub fn delete_synced(tx: &Transaction<'_>, list_id: i64, remote_id: &str) -> Result<bool> {
    Ok(tx.execute(
        "DELETE FROM tasks WHERE list_id = ? AND remote_id = ? AND local_state IN (?, ?)",
        params![list_id, remote_id, SYNCED, DELETED],
    )? > 0)
}

/// After a full listing: removes synced tasks the server did not report.
pub fn sweep(tx: &Transaction<'_>, list_id: i64, seen: &HashSet<String>) -> Result<usize> {
    let stale: Vec<i64> = {
        let mut st = tx.prepare(
            "SELECT id, remote_id FROM tasks WHERE list_id = ? AND remote_id IS NOT NULL AND local_state IN (?, ?)",
        )?;
        st.query_map(params![list_id, SYNCED, DELETED], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter(|(_, remote)| !seen.contains(remote))
        .map(|(id, _)| id)
        .collect()
    };
    for id in &stale {
        tx.execute("DELETE FROM tasks WHERE id = ?", [id])?;
    }
    Ok(stale.len())
}

/// Links synced tasks of a list to their parents by `parent_uid` (tasks
/// whose parent is not in the list become top-level).
pub fn resolve_parents(tx: &Transaction<'_>, list_id: i64) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET parent_id = (
           SELECT p.id FROM tasks p WHERE p.list_id = tasks.list_id AND p.uid = tasks.parent_uid
             AND p.id != tasks.id ORDER BY p.local_state = 3, p.id LIMIT 1)
         WHERE list_id = ? AND local_state = 0 AND parent_uid IS NOT NULL",
        [list_id],
    )?;
    tx.execute(
        "UPDATE tasks SET parent_id = NULL
         WHERE list_id = ? AND local_state = 0 AND parent_uid IS NULL AND parent_id IS NOT NULL",
        [list_id],
    )?;
    Ok(())
}

/// Tasks of a list with local changes due to be pushed, parents before
/// their subtasks, each in list order (servers that place a task after the
/// one before it end up in the same order as here).
pub fn dirty_rows(c: &Connection, list_id: i64) -> Result<Vec<TaskRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks t WHERE t.list_id = ? AND t.local_state != 0 AND t.push_after <= ?
         ORDER BY t.parent_id IS NOT NULL, t.position, t.created_at, t.id"
    ))?;
    let rows = st.query_map(params![list_id, now_ms()], task_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// When the next upload waiting out a retry delay comes due. Uploads that
/// are already due are left to the next regular round.
pub fn next_push_at(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(push_after) FROM tasks WHERE local_state != 0 AND push_after > ?",
        [now_ms()],
        |r| r.get(0),
    )?)
}

/// Records a successful upload of the task as it was at `version`, as the
/// server now has it. If it changed meanwhile, the newer local state is kept
/// (and pushed next time).
pub fn finish_push(tx: &Transaction<'_>, id: i64, version: i64, pushed: &RemoteTask) -> Result<()> {
    let current: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT version, local_state, list_id FROM tasks WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((current_version, state, list_id)) = current else {
        return Ok(());
    };
    if current_version == version {
        tx.execute(
            "UPDATE tasks SET remote_id = ?1, etag = ?2, uid = ?3, data = ?4, title = ?5, notes = ?6,
               notified = CASE WHEN due_at IS ?8 THEN notified ELSE 0 END,
               due_date = ?7, due_at = ?8, completed_at = ?9, important = ?10, parent_uid = ?11,
               position = coalesce(?12, position), repeats = ?13, local_state = 0, push_attempts = 0,
               push_after = 0, push_error = NULL
             WHERE id = ?14",
            params![
                pushed.remote_id,
                pushed.etag,
                pushed.uid,
                pushed.data,
                pushed.title,
                pushed.notes,
                pushed.due_date,
                pushed.due_at,
                pushed.completed_at,
                pushed.important as i64,
                pushed.parent_uid,
                pushed.position,
                pushed.repeats as i64,
                id
            ],
        )?;
        resolve_parents(tx, list_id)?;
    } else {
        // Edited again while uploading: it exists on the server now, so the
        // newer state is an update of what was just written.
        let state = if state == CREATED { MODIFIED } else { state };
        tx.execute(
            "UPDATE tasks SET remote_id = ?, etag = ?, uid = ?, data = ?, local_state = ?, push_attempts = 0,
               push_after = 0, push_error = NULL WHERE id = ?",
            params![
                pushed.remote_id,
                pushed.etag,
                pushed.uid,
                pushed.data,
                state,
                id
            ],
        )?;
    }
    Ok(())
}

/// Records a failed upload to retry at `retry_at`.
pub fn push_failed(tx: &Transaction<'_>, id: i64, error: &str, retry_at: i64) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET push_attempts = push_attempts + 1, push_error = ?, push_after = ? WHERE id = ?",
        params![error, retry_at, id],
    )?;
    Ok(())
}

/// Gives up on a local change the server keeps refusing: the row goes and
/// the list is synced from scratch, which brings back the server's copy.
pub fn discard_local_change(tx: &Transaction<'_>, id: i64, list_id: i64) -> Result<()> {
    tx.execute("DELETE FROM tasks WHERE id = ?", [id])?;
    tx.execute(
        "UPDATE task_lists SET sync_state = '{}' WHERE id = ?",
        [list_id],
    )?;
    Ok(())
}

/// Keeps a task the server refuses to create in the list on this computer
/// instead (with its subtasks that are not on the server either), so that
/// nothing the user wrote is lost.
pub fn keep_locally(tx: &Transaction<'_>, id: i64, local_list: i64) -> Result<()> {
    let position: i64 = tx.query_row(
        "SELECT coalesce(max(position), 0) + ? FROM tasks WHERE list_id = ? AND parent_id IS NULL",
        params![POSITION_STEP, local_list],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE tasks SET list_id = ?1, remote_id = NULL, etag = NULL, data = NULL, repeats = 0,
           local_state = 0, push_attempts = 0, push_after = 0, push_error = NULL,
           position = CASE WHEN id = ?3 THEN ?2 ELSE position END,
           parent_id = CASE WHEN id = ?3 THEN NULL ELSE parent_id END,
           parent_uid = CASE WHEN id = ?3 THEN NULL ELSE parent_uid END
         WHERE id = ?3 OR (parent_id = ?3 AND local_state = 1)",
        params![local_list, position, id],
    )?;
    // Subtasks already on the server stay there, without this parent.
    tx.execute(
        "UPDATE tasks SET parent_id = NULL, parent_uid = NULL WHERE parent_id = ?1 AND list_id != ?2",
        [id, local_list],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::db::mail;
    use crate::model::{AccountConfig, AccountKind};

    fn db() -> Db {
        Db::open(std::path::Path::new(":memory:")).expect("db")
    }

    fn fields(title: &str, position: i64) -> TaskFields {
        TaskFields {
            title: title.into(),
            position,
            ..TaskFields::default()
        }
    }

    fn insert(db: &Db, list: i64, f: &TaskFields, state: i64) -> i64 {
        db.write(|tx| {
            insert_local(
                tx,
                &NewTask {
                    list_id: list,
                    uid: &uuid::Uuid::new_v4().to_string(),
                    remote_id: None,
                    fields: f,
                    source: None,
                    data: None,
                    repeats: false,
                    created_at: now_ms(),
                    state,
                },
            )
        })
        .expect("insert")
    }

    fn remote(id: &str, title: &str) -> RemoteTask {
        RemoteTask {
            remote_id: id.into(),
            uid: id.into(),
            title: title.into(),
            ..RemoteTask::default()
        }
    }

    #[test]
    fn lists_are_ordered_and_synced() {
        let db = db();
        let account = db
            .write(|tx| {
                mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    "a@example.com",
                    "A",
                    "#10b981",
                    &AccountConfig::default(),
                )
            })
            .expect("account");
        let upserts = vec![
            ListUpsert {
                remote_id: "https://dav.example.com/tasks/".into(),
                name: "Tasks".into(),
                color: None,
                read_only: false,
                is_default: true,
                sort_order: 1,
            },
            ListUpsert {
                remote_id: "https://dav.example.com/shared/".into(),
                name: "Shared".into(),
                color: Some("#ff0000".into()),
                read_only: true,
                is_default: false,
                sort_order: 0,
            },
        ];
        let changed = db
            .write(|tx| sync_lists(tx, account, TaskListKind::Caldav, &upserts))
            .expect("sync");
        assert_eq!(changed.len(), 2);
        let local = db
            .write(|tx| {
                create_local_list(tx, "Tasks", true)?;
                create_local_list(tx, "Groceries", false)
            })
            .expect("local");
        let lists = db.read(list_lists).expect("lists");
        let names: Vec<(&str, &str)> = lists
            .iter()
            .map(|l| (l.name.as_str(), l.color.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Tasks", LOCAL_COLOR),
                ("Groceries", LOCAL_COLOR),
                ("Tasks", "#10b981"),
                ("Shared", "#ff0000")
            ],
            "local first, then the account's default list, then the server's order"
        );
        assert_eq!(lists[1].id, local);
        assert!(lists[3].read_only);

        // Unchanged lists are not reported; vanished ones go.
        let changed = db
            .write(|tx| sync_lists(tx, account, TaskListKind::Caldav, &upserts[..1]))
            .expect("sync");
        assert_eq!(changed, [lists[3].id]);
        assert_eq!(
            db.read(|c| account_lists(c, account)).expect("lists").len(),
            1
        );
    }

    #[test]
    fn synced_rows_keep_local_changes_and_find_their_parents() {
        let db = db();
        let list = db
            .write(|tx| create_local_list(tx, "Tasks", true))
            .expect("list");
        let mut child = remote("c", "Child");
        child.parent_uid = Some("p".into());
        child.position = Some(5);
        db.write(|tx| {
            upsert_synced(tx, list, &child)?;
            upsert_synced(tx, list, &remote("p", "Parent"))?;
            resolve_parents(tx, list)
        })
        .expect("store");
        let parent = db.read(|c| list_open(c, list)).expect("open");
        assert_eq!(parent.len(), 1, "the child is a subtask");
        assert_eq!(parent[0].title, "Parent");
        assert_eq!(parent[0].subtask_count, 1);
        let kids = db.read(|c| children(c, parent[0].id)).expect("children");
        assert_eq!((kids[0].title.as_str(), kids[0].position), ("Child", 5));

        // A local edit is not overwritten by the next server copy.
        let mut edited = kids[0].fields();
        edited.title = "Mine".into();
        db.write(|tx| update_local(tx, kids[0].id, &edited, MODIFIED))
            .expect("edit");
        let mut newer = child.clone();
        newer.title = "Theirs".into();
        assert_eq!(
            db.write(|tx| upsert_synced(tx, list, &newer))
                .expect("upsert"),
            None
        );
        assert_eq!(
            db.read(|c| get_task(c, kids[0].id)).expect("row").title,
            "Mine"
        );
        // Nor removed when the server drops it.
        let swept = db
            .write(|tx| sweep(tx, list, &HashSet::from(["p".to_owned()])))
            .expect("sweep");
        assert_eq!(swept, 0);
        let swept = db
            .write(|tx| sweep(tx, list, &HashSet::new()))
            .expect("sweep");
        assert_eq!(swept, 1, "the synced parent goes");
        let orphan = db.read(|c| get_task(c, kids[0].id)).expect("row");
        assert_eq!(orphan.parent_id, None);
    }

    #[test]
    fn pushes_record_the_server_state() {
        let db = db();
        let list = db
            .write(|tx| create_local_list(tx, "Tasks", true))
            .expect("list");
        let id = insert(&db, list, &fields("Draft", 1024), CREATED);
        let row = db.read(|c| get_task(c, id)).expect("row");
        let mut pushed = remote("https://dav.example.com/t/1.ics", "Draft");
        pushed.uid = row.uid.clone();
        pushed.etag = Some("\"1\"".into());
        pushed.data = Some("BEGIN:VCALENDAR…".into());
        db.write(|tx| finish_push(tx, id, row.version, &pushed))
            .expect("finish");
        let row = db.read(|c| get_task(c, id)).expect("row");
        assert_eq!(row.local_state, SYNCED);
        assert_eq!(row.etag.as_deref(), Some("\"1\""));
        assert_eq!(row.position, 1024, "kept when the server has no order");

        // Edited during an upload: the newer edit stays to be pushed.
        let mut f = row.fields();
        f.title = "Final".into();
        db.write(|tx| update_local(tx, id, &f, MODIFIED))
            .expect("edit");
        db.write(|tx| finish_push(tx, id, row.version, &pushed))
            .expect("finish");
        let row = db.read(|c| get_task(c, id)).expect("row");
        assert_eq!((row.title.as_str(), row.local_state), ("Final", MODIFIED));

        db.write(|tx| push_failed(tx, id, "offline", now_ms() + 60_000))
            .expect("failed");
        assert!(db.read(|c| dirty_rows(c, list)).expect("dirty").is_empty());
        assert!(db.read(next_push_at).expect("next").is_some());
        assert_eq!(
            db.read(|c| get_task(c, id))
                .expect("row")
                .push_error
                .as_deref(),
            Some("offline")
        );
    }

    #[test]
    fn days_of_timed_tasks_are_worked_out_in_the_zone() {
        let db = db();
        let list = db
            .write(|tx| create_local_list(tx, "Tasks", true))
            .expect("list");
        // 22:00 UTC on 30 October: the 31st in Tokyo, the 30th in New York.
        let mut timed = fields("Call", 1024);
        timed.due_at = Some(1_793_397_600_000);
        timed.due_date = Some("2026-10-30".into());
        let id = insert(&db, list, &timed, SYNCED);
        let mut day_only = fields("Plants", 2048);
        day_only.due_date = Some("2026-10-30".into());
        let other = insert(&db, list, &day_only, SYNCED);
        assert_eq!(
            db.write(|tx| redo_due_days(tx, Tz::Asia__Tokyo))
                .expect("redo"),
            [list]
        );
        let day = |id| db.read(|c| get_task(c, id)).expect("row").due_date;
        assert_eq!(day(id).as_deref(), Some("2026-10-31"));
        assert_eq!(day(other).as_deref(), Some("2026-10-30"));
        assert!(
            db.write(|tx| redo_due_days(tx, Tz::Asia__Tokyo))
                .expect("redo")
                .is_empty(),
            "nothing changes twice"
        );
        db.write(|tx| redo_due_days(tx, Tz::America__New_York))
            .expect("redo");
        assert_eq!(day(id).as_deref(), Some("2026-10-30"));
    }

    #[test]
    fn due_notifications_come_once_and_again_after_a_change() {
        let db = db();
        let list = db
            .write(|tx| create_local_list(tx, "Tasks", true))
            .expect("list");
        let now = now_ms();
        let mut due = fields("Call", 1024);
        due.due_at = Some(now - 1_000);
        due.due_date = Some("2026-01-01".into());
        let id = insert(&db, list, &due, SYNCED);
        let mut later = fields("Later", 2048);
        later.due_at = Some(now + 60_000);
        insert(&db, list, &later, SYNCED);
        assert_eq!(
            db.read(|c| due_unnotified(c, now)).expect("due"),
            [(id, "Call".to_owned(), now - 1_000)]
        );
        db.write(|tx| mark_notified(tx, &[id])).expect("mark");
        assert!(db.read(|c| due_unnotified(c, now)).expect("due").is_empty());
        assert_eq!(
            db.read(|c| next_due_at(c, now)).expect("next"),
            Some(now + 60_000)
        );
        // Same due time: still announced; a new one: announced again.
        let row = db.read(|c| get_task(c, id)).expect("row");
        let mut f = row.fields();
        f.title = "Call Maya".into();
        db.write(|tx| update_local(tx, id, &f, SYNCED))
            .expect("edit");
        assert!(db.read(|c| due_unnotified(c, now)).expect("due").is_empty());
        f.due_at = Some(now - 500);
        db.write(|tx| update_local(tx, id, &f, SYNCED))
            .expect("edit");
        assert_eq!(db.read(|c| due_unnotified(c, now)).expect("due").len(), 1);
    }
}
