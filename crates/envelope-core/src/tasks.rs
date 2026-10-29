//! Task lists and tasks for the UI: the smart views (today, upcoming,
//! important, all), reading, editing, completing, ordering and deleting
//! tasks, managing lists, and the notification of tasks due at a time of
//! day.
//!
//! Edits are stored locally first and written to the server in the
//! background by [`crate::sync::tasks`], so they never wait on the network
//! and work offline. A deleted task can be brought back for a few seconds,
//! before its deletion goes anywhere. Creating, renaming and deleting a list
//! of an account happens on the server right away.

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize};

use crate::calendar::local_tz;
use crate::db::misc;
use crate::db::now_ms;
use crate::db::tasks::{self as tdb, ListCounts, ListRow, POSITION_STEP, TaskFields, TaskRow};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::EngineEvent;

/// Name of the list on this computer, which always exists (the interface
/// shows it translated).
pub const LOCAL_LIST_NAME: &str = "Tasks";

/// Completed tasks returned with a list at most.
const MAX_COMPLETED: i64 = 200;

/// How long a task deleted with `task_delete` can be brought back: its
/// deletion is carried out (uploaded, or the task removed from the list on
/// this computer) only after that.
pub const UNDO_DELETE_MS: i64 = 12_000;

/// Tasks due longer ago than this when Envelope notices them (it was not
/// running, the computer slept) are not announced any more.
const LATE_NOTICE_MS: i64 = 60 * 60_000;

/// Setting with the time zone the days of tasks due at a time of day were
/// worked out in.
pub(crate) const TIME_ZONE_KEY: &str = "tasks.timeZone";

/// Where a list lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskListKind {
    /// On this computer.
    Local,
    /// A CalDAV collection holding `VTODO`s.
    Caldav,
    /// Google Tasks.
    Google,
    /// Microsoft To Do (Graph).
    Graph,
    /// An Exchange task folder (EWS).
    Ews,
}

/// What a service stores about tasks beyond a title, notes, a due day and
/// whether they are done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListFeatures {
    /// A time of day on due dates.
    pub due_times: bool,
    /// Importance.
    pub importance: bool,
    /// Subtasks.
    pub subtasks: bool,
    /// An order of the tasks that Envelope can change.
    pub reorder: bool,
}

impl TaskListKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskListKind::Local => "local",
            TaskListKind::Caldav => "caldav",
            TaskListKind::Google => "google",
            TaskListKind::Graph => "graph",
            TaskListKind::Ews => "ews",
        }
    }

    pub fn parse(kind: &str) -> Option<TaskListKind> {
        Some(match kind {
            "local" => TaskListKind::Local,
            "caldav" => TaskListKind::Caldav,
            "google" => TaskListKind::Google,
            "graph" => TaskListKind::Graph,
            "ews" => TaskListKind::Ews,
            _ => return None,
        })
    }

    /// What lists of this kind can store. Google Tasks has due days only
    /// and no importance, Microsoft To Do keeps the time in reminders
    /// rather than on the due date, and Exchange task folders have no
    /// subtasks. Neither To Do nor Exchange lets an order be written (To Do
    /// keeps the order of steps, but puts them back in it on the next
    /// pull).
    pub fn features(self) -> ListFeatures {
        let (due_times, importance, subtasks, reorder) = match self {
            TaskListKind::Local | TaskListKind::Caldav => (true, true, true, true),
            TaskListKind::Google => (false, false, true, true),
            TaskListKind::Graph => (false, true, true, false),
            TaskListKind::Ews => (false, true, false, false),
        };
        ListFeatures {
            due_times,
            importance,
            subtasks,
            reorder,
        }
    }
}

/// A list as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskList {
    pub id: i64,
    pub account_id: Option<i64>,
    pub kind: TaskListKind,
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
    pub read_only: bool,
    /// Where new tasks for this account go.
    pub is_default: bool,
    /// Tasks not completed yet (subtasks included).
    pub open_count: i64,
    /// Completed top-level tasks: what clearing the completed ones deletes.
    pub completed_count: i64,
    pub due_times: bool,
    pub importance: bool,
    pub subtasks: bool,
    /// The order of its tasks can be changed.
    pub reorder: bool,
}

/// The message a task was made from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSource {
    /// `Message-ID` header, without angle brackets.
    pub message_id: String,
    /// The conversation, when the message is still in the database.
    pub thread_id: Option<i64>,
    pub account_id: Option<i64>,
    pub subject: String,
    /// Sender's name or address.
    pub from: Option<String>,
}

/// A task as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub list_id: i64,
    pub parent_id: Option<i64>,
    pub title: String,
    pub notes: String,
    /// The day the task is due (`YYYY-MM-DD`, local).
    pub due_date: Option<String>,
    /// When it is due, if at a time of day; `due_date` is then that local
    /// day.
    pub due_at: Option<i64>,
    /// When it was completed; `None` while open.
    pub completed_at: Option<i64>,
    pub important: bool,
    /// Repeats (kept from the server); completing it moves it to its next
    /// due date.
    pub repeats: bool,
    pub source: Option<TaskSource>,
    /// Subtasks: how many, how many done (0 for subtasks themselves).
    pub subtask_count: i64,
    pub subtasks_done: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Changed here and not on the server yet.
    pub pending: bool,
    /// Why saving it on the server failed last time.
    pub error: Option<String>,
}

/// Which tasks to show (`{ list: id }`, `"today"`, `"upcoming"`,
/// `"important"` or `"all"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskView {
    /// One list: its open top-level tasks by position, then (when asked
    /// for) its completed ones, most recent first.
    List(i64),
    /// Open tasks due today or earlier.
    Today,
    /// Open tasks due after today.
    Upcoming,
    /// Open important tasks.
    Important,
    /// Open top-level tasks of every list.
    All,
}

/// A task to create, or the changes to one.
///
/// When updating, fields left out keep their value and `null` clears it.
/// Giving only `dueDate` makes the task due on that day without a time;
/// `dueAt` sets both, and `dueAt: null` alone keeps the day without the
/// time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub list_id: i64,
    #[serde(default, deserialize_with = "present")]
    pub parent_id: Option<Option<i64>>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub due_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub due_at: Option<Option<i64>>,
    #[serde(default)]
    pub important: Option<bool>,
    /// Database id of a message to link the task to (only when creating).
    #[serde(default)]
    pub from_message: Option<i64>,
}

/// Tells a field given as `null` (`Some(None)`) from one left out (`None`).
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ---------------------------------------------------------------------------
// Dates

/// The local day of an instant, as `YYYY-MM-DD`.
pub(crate) fn local_day(at: i64) -> String {
    day_in(at, local_tz())
}

/// The day of an instant in `zone`, as `YYYY-MM-DD`.
pub(crate) fn day_in(at: i64, zone: Tz) -> String {
    DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .with_timezone(&zone)
        .format("%Y-%m-%d")
        .to_string()
}

/// Today, as `YYYY-MM-DD`.
fn today() -> String {
    local_day(now_ms())
}

/// Reads a `YYYY-MM-DD` date from the UI.
fn parse_day(text: &str) -> Result<String> {
    let text = text.trim();
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map(|d| d.format("%Y-%m-%d").to_string())
        .map_err(|_| Error::Invalid(format!("“{text}” is not a date.")))
}

/// The due date an input asks for: `None` when it leaves the due date
/// alone, else `(due_date, due_at)`. Clearing only the time keeps
/// `current_day`, the day the task is due now. Times are kept to the
/// second, as servers store them; lists without times on due dates keep
/// only the day.
fn due_change(
    input: &TaskInput,
    due_times: bool,
    current_day: Option<&str>,
) -> Result<Option<(Option<String>, Option<i64>)>> {
    let due = match (&input.due_at, &input.due_date) {
        (Some(Some(at)), _) => {
            let at = at - at.rem_euclid(1000);
            Some((Some(local_day(at)), Some(at)))
        }
        (_, Some(Some(day))) => Some((Some(parse_day(day)?), None)),
        (_, Some(None)) => Some((None, None)),
        (Some(None), None) => Some((current_day.map(str::to_owned), None)),
        (None, None) => None,
    };
    Ok(due.map(|(day, at)| (day, at.filter(|_| due_times))))
}

// ---------------------------------------------------------------------------
// Views of stored rows

fn list_view(list: &ListRow, counts: ListCounts) -> TaskList {
    let features = list.kind.features();
    TaskList {
        id: list.id,
        account_id: list.account_id,
        kind: list.kind,
        name: list.name.clone(),
        color: list.color.clone(),
        read_only: list.read_only,
        is_default: list.is_default,
        open_count: counts.open,
        completed_count: counts.completed,
        due_times: features.due_times,
        importance: features.importance,
        subtasks: features.subtasks,
        reorder: features.reorder,
    }
}

fn task_view(c: &rusqlite::Connection, row: &TaskRow) -> Result<Task> {
    let source = match &row.source {
        Some(source) => Some(tdb::resolve_source(c, source)?),
        None => None,
    };
    let top_level = row.parent_id.is_none();
    Ok(Task {
        id: row.id,
        list_id: row.list_id,
        parent_id: row.parent_id,
        title: row.title.clone(),
        notes: row.notes.clone(),
        due_date: row.due_date.clone(),
        due_at: row.due_at,
        completed_at: row.completed_at,
        important: row.important,
        repeats: row.repeats,
        source,
        subtask_count: if top_level { row.subtask_count } else { 0 },
        subtasks_done: if top_level { row.subtasks_done } else { 0 },
        created_at: row.created_at,
        updated_at: row.updated_at,
        pending: row.local_state != tdb::SYNCED,
        error: row.push_error.clone(),
    })
}

fn task_views(c: &rusqlite::Connection, rows: &[TaskRow]) -> Result<Vec<Task>> {
    rows.iter().map(|row| task_view(c, row)).collect()
}

// ---------------------------------------------------------------------------
// Local edits

/// Fails for lists Envelope may not change.
fn writable(list: &ListRow) -> Result<()> {
    if list.read_only {
        return Err(Error::Invalid("This list is read-only.".into()));
    }
    Ok(())
}

/// A task that exists (and is not waiting to be deleted).
fn live_task(c: &rusqlite::Connection, id: i64) -> Result<TaskRow> {
    let row = tdb::get_task(c, id)?;
    if row.local_state == tdb::DELETED {
        return Err(Error::NotFound(format!("task {id}")));
    }
    Ok(row)
}

/// The state of a task after a local change.
fn edited_state(list: &ListRow, current: i64) -> i64 {
    match (list.account_id.is_some(), current) {
        (false, _) => tdb::SYNCED,
        (true, tdb::CREATED) => tdb::CREATED,
        (true, _) => tdb::MODIFIED,
    }
}

fn clean_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_notes(notes: &str) -> String {
    notes.replace("\r\n", "\n").trim().to_owned()
}

fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("Give the list a name.".into()));
    }
    Ok(name.to_owned())
}

/// Checks that `parent_id` can hold a subtask in `list` and returns it.
fn subtask_parent(
    c: &rusqlite::Connection,
    list: &ListRow,
    parent_id: i64,
    child: Option<&TaskRow>,
) -> Result<TaskRow> {
    if !list.kind.features().subtasks {
        return Err(Error::Invalid(
            "Tasks in this list can't have subtasks.".into(),
        ));
    }
    let parent = live_task(c, parent_id)?;
    if parent.list_id != list.id {
        return Err(Error::Invalid(
            "A subtask has to be in the same list as its task.".into(),
        ));
    }
    if parent.parent_id.is_some() || child.is_some_and(|c| c.id == parent.id) {
        return Err(Error::Invalid("Subtasks can't have subtasks.".into()));
    }
    if child.is_some_and(|c| c.subtask_count > 0) {
        return Err(Error::Invalid(
            "A task with subtasks can't become a subtask.".into(),
        ));
    }
    Ok(parent)
}

/// Where a new task goes: after its siblings.
fn end_position(c: &rusqlite::Connection, list_id: i64, parent_id: Option<i64>) -> Result<i64> {
    Ok(tdb::max_position(c, list_id, parent_id)?.saturating_add(POSITION_STEP))
}

/// The CalDAV href a task created in `list` gets, assigned up front so that
/// repeated uploads are idempotent.
fn new_remote_id(list: &ListRow, uid: &str) -> Result<Option<String>> {
    match list.kind {
        TaskListKind::Caldav => Ok(Some(
            envelope_dav::object_href(&list.remote_id, uid, envelope_dav::ObjectKind::Calendar)
                .map_err(|e| Error::Other(e.to_string()))?,
        )),
        _ => Ok(None),
    }
}

/// Takes a task out of its list: deleted at once from a list on this
/// computer, else marked for deletion, which the next upload carries out.
/// A task that is not on the server yet is marked too: should an upload
/// under way create it there, it is deleted there as well.
fn retire(tx: &rusqlite::Transaction<'_>, list: &ListRow, row: &TaskRow) -> Result<()> {
    if list.account_id.is_none() {
        tdb::delete_row(tx, row.id)
    } else if row.local_state != tdb::DELETED {
        tdb::mark_deleted(tx, row.id)
    } else {
        Ok(())
    }
}

/// What a local write did: the lists to refresh and whether a server has
/// to hear about it.
#[derive(Debug, Default)]
struct Written {
    lists: Vec<i64>,
    remote: bool,
}

impl Written {
    fn of(list: &ListRow, changed: bool) -> Written {
        Written {
            lists: if changed { vec![list.id] } else { Vec::new() },
            remote: changed && list.account_id.is_some(),
        }
    }
}

fn create_task_tx(tx: &rusqlite::Transaction<'_>, input: &TaskInput) -> Result<(i64, Written)> {
    let list = tdb::get_list(tx, input.list_id)?;
    writable(&list)?;
    let features = list.kind.features();
    let parent = match input.parent_id.flatten() {
        Some(id) => Some(subtask_parent(tx, &list, id, None)?),
        None => None,
    };
    let source = match input.from_message {
        Some(message) => Some(tdb::message_source(tx, message)?),
        None => None,
    };
    let mut title = clean_title(&input.title);
    if title.is_empty()
        && let Some(source) = &source
    {
        title = clean_title(&source.subject);
        if title.is_empty() {
            title = "(No subject)".into();
        }
    }
    if title.is_empty() {
        return Err(Error::Invalid("Give the task a title.".into()));
    }
    let (due_date, due_at) = due_change(input, features.due_times, None)?.unwrap_or_default();
    let fields = TaskFields {
        title,
        notes: clean_notes(input.notes.as_deref().unwrap_or("")),
        due_date,
        due_at,
        completed_at: None,
        important: input.important.unwrap_or(false) && features.importance,
        parent_id: parent.as_ref().map(|p| p.id),
        parent_uid: parent.as_ref().map(|p| p.uid.clone()),
        position: end_position(tx, list.id, parent.as_ref().map(|p| p.id))?,
    };
    let uid = uuid::Uuid::new_v4().to_string();
    let remote = list.account_id.is_some();
    let id = tdb::insert_local(
        tx,
        &tdb::NewTask {
            list_id: list.id,
            uid: &uid,
            remote_id: new_remote_id(&list, &uid)?.as_deref(),
            fields: &fields,
            source: source.as_ref(),
            data: None,
            repeats: false,
            created_at: now_ms(),
            state: if remote { tdb::CREATED } else { tdb::SYNCED },
        },
    )?;
    Ok((id, Written::of(&list, true)))
}

/// Applies the edits of an input to a task's fields (not its list).
fn apply_edits(
    tx: &rusqlite::Transaction<'_>,
    list: &ListRow,
    row: &TaskRow,
    input: &TaskInput,
    fields: &mut TaskFields,
) -> Result<()> {
    let features = list.kind.features();
    let title = clean_title(&input.title);
    if title.is_empty() {
        return Err(Error::Invalid("Give the task a title.".into()));
    }
    fields.title = title;
    if let Some(notes) = &input.notes {
        fields.notes = clean_notes(notes);
    }
    if let Some((day, at)) = due_change(input, features.due_times, fields.due_date.as_deref())? {
        fields.due_date = day;
        fields.due_at = at;
    }
    if !features.due_times {
        fields.due_at = None;
    }
    if let Some(important) = input.important {
        fields.important = important;
    }
    fields.important &= features.importance;
    if let Some(parent) = input.parent_id
        && parent != row.parent_id.filter(|_| row.list_id == list.id)
    {
        match parent {
            Some(id) => {
                let parent = subtask_parent(tx, list, id, Some(row))?;
                fields.parent_id = Some(parent.id);
                fields.parent_uid = Some(parent.uid);
                fields.position = end_position(tx, list.id, Some(id))?;
            }
            None => {
                fields.parent_id = None;
                fields.parent_uid = None;
                fields.position = end_position(tx, list.id, None)?;
            }
        }
    }
    Ok(())
}

fn update_task_tx(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    input: &TaskInput,
) -> Result<(i64, Written)> {
    let row = live_task(tx, id)?;
    let list = tdb::get_list(tx, row.list_id)?;
    writable(&list)?;
    if input.list_id != row.list_id {
        return move_task_tx(tx, &row, &list, input);
    }
    let mut fields = row.fields();
    apply_edits(tx, &list, &row, input, &mut fields)?;
    if fields == row.fields() {
        return Ok((row.id, Written::of(&list, false)));
    }
    tdb::update_local(tx, row.id, &fields, edited_state(&list, row.local_state))?;
    Ok((row.id, Written::of(&list, true)))
}

/// Moves a task (with its subtasks) to another list. Between lists on this
/// computer the rows just move; otherwise the task is created in the new
/// list and deleted from the old one, keeping what the server stored with
/// it when both lists are on CalDAV.
fn move_task_tx(
    tx: &rusqlite::Transaction<'_>,
    row: &TaskRow,
    old_list: &ListRow,
    input: &TaskInput,
) -> Result<(i64, Written)> {
    let list = tdb::get_list(tx, input.list_id)?;
    writable(&list)?;
    // A subtask moved on its own becomes a task of the new list, unless it
    // is given a parent there.
    let mut fields = row.fields();
    fields.parent_id = None;
    fields.parent_uid = None;
    let mut moved_input = input.clone();
    moved_input.parent_id = Some(input.parent_id.flatten());
    apply_edits(tx, &list, row, &moved_input, &mut fields)?;
    if moved_input.parent_id == Some(None) {
        fields.position = end_position(tx, list.id, None)?;
    }
    let children = if fields.parent_id.is_none() {
        tdb::children(tx, row.id)?
    } else {
        Vec::new()
    };
    let lists = vec![old_list.id, list.id];
    if old_list.account_id.is_none() && list.account_id.is_none() {
        tdb::move_local(tx, row.id, list.id, &fields)?;
        for child in &children {
            tdb::move_local(tx, child.id, list.id, &child.fields())?;
        }
        return Ok((
            row.id,
            Written {
                lists,
                remote: false,
            },
        ));
    }
    let keep_data = old_list.kind == TaskListKind::Caldav && list.kind == TaskListKind::Caldav;
    let copy = |tx: &rusqlite::Transaction<'_>, from: &TaskRow, fields: &TaskFields| {
        let uid = uuid::Uuid::new_v4().to_string();
        let mut fields = fields.clone();
        fields.important &= list.kind.features().importance;
        if !list.kind.features().due_times {
            fields.due_at = None;
        }
        let id = tdb::insert_local(
            tx,
            &tdb::NewTask {
                list_id: list.id,
                uid: &uid,
                remote_id: new_remote_id(&list, &uid)?.as_deref(),
                fields: &fields,
                source: from.source.as_ref(),
                data: from.data.as_deref().filter(|_| keep_data),
                repeats: from.repeats && keep_data,
                created_at: from.created_at,
                state: if list.account_id.is_some() {
                    tdb::CREATED
                } else {
                    tdb::SYNCED
                },
            },
        )?;
        Ok::<_, Error>((id, uid))
    };
    let (new_id, new_uid) = copy(tx, row, &fields)?;
    for child in &children {
        let mut child_fields = child.fields();
        if list.kind.features().subtasks {
            child_fields.parent_id = Some(new_id);
            child_fields.parent_uid = Some(new_uid.clone());
        } else {
            // The list has no subtasks: they follow their task instead.
            child_fields.parent_id = None;
            child_fields.parent_uid = None;
            child_fields.position = end_position(tx, list.id, None)?;
        }
        copy(tx, child, &child_fields)?;
        retire(tx, old_list, child)?;
    }
    retire(tx, old_list, row)?;
    let remote = old_list.account_id.is_some() || list.account_id.is_some();
    Ok((new_id, Written { lists, remote }))
}

fn complete_task_tx(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    completed: bool,
) -> Result<(i64, Written)> {
    let row = live_task(tx, id)?;
    let list = tdb::get_list(tx, row.list_id)?;
    writable(&list)?;
    let mut fields = row.fields();
    if completed == row.completed_at.is_some() {
        return Ok((row.id, Written::of(&list, false)));
    }
    if !completed {
        fields.completed_at = None;
    } else if let Some((day, at)) = crate::sync::tasks::caldav::next_due(&list, &row) {
        // A repeating task: on to its next occurrence, still open.
        fields.due_date = day;
        fields.due_at = at;
    } else {
        fields.completed_at = Some(now_ms());
    }
    tdb::update_local(tx, row.id, &fields, edited_state(&list, row.local_state))?;
    Ok((row.id, Written::of(&list, true)))
}

fn reorder_task_tx(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    before: Option<i64>,
) -> Result<Written> {
    let row = live_task(tx, id)?;
    let list = tdb::get_list(tx, row.list_id)?;
    writable(&list)?;
    if !list.kind.features().reorder {
        return Err(Error::Invalid(
            "The order of the tasks in this list can't be changed.".into(),
        ));
    }
    if before == Some(row.id) {
        return Ok(Written::of(&list, false));
    }
    let siblings: Vec<(i64, i64)> = tdb::siblings(tx, list.id, row.parent_id)?
        .into_iter()
        .filter(|(sibling, _)| *sibling != row.id)
        .collect();
    let index = match before {
        Some(before) => siblings
            .iter()
            .position(|(sibling, _)| *sibling == before)
            .ok_or_else(|| {
                Error::Invalid("Tasks can only be moved among the tasks next to them.".into())
            })?,
        None => siblings.len(),
    };
    let previous = index.checked_sub(1).map(|i| siblings[i].1);
    let next = siblings.get(index).map(|(_, position)| *position);
    let gap = match (previous, next) {
        (None, None) => Some(row.position),
        (Some(p), None) => Some(p.saturating_add(POSITION_STEP)),
        (None, Some(n)) => Some(n.saturating_sub(POSITION_STEP)),
        (Some(p), Some(n)) if n - p >= 2 => Some(p + (n - p) / 2),
        _ => None,
    };
    let mut moves: Vec<(i64, i64)> = Vec::new();
    match gap {
        Some(position) => moves.push((row.id, position)),
        None => {
            // No room between the neighbours: space the siblings out again.
            let mut order: Vec<(i64, i64)> = siblings.clone();
            order.insert(index, (row.id, row.position));
            for (n, (sibling, position)) in (1..).zip(order) {
                let wanted = n * POSITION_STEP;
                if wanted != position {
                    moves.push((sibling, wanted));
                }
            }
        }
    }
    let mut changed = false;
    for (task, position) in moves {
        let current = tdb::get_task(tx, task)?;
        if current.position == position {
            continue;
        }
        let mut fields = current.fields();
        fields.position = position;
        tdb::update_local(tx, task, &fields, edited_state(&list, current.local_state))?;
        changed = true;
    }
    Ok(Written::of(&list, changed))
}

/// Deletes a task with its subtasks so that the deletion can be undone for
/// a while ([`UNDO_DELETE_MS`]); it is carried out after that.
fn delete_task_tx(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<Written> {
    let row = live_task(tx, id)?;
    let list = tdb::get_list(tx, row.list_id)?;
    writable(&list)?;
    let until = now_ms() + UNDO_DELETE_MS;
    for child in tdb::children(tx, row.id)? {
        tdb::delete_undoably(tx, child.id, row.id, until)?;
    }
    tdb::delete_undoably(tx, row.id, row.id, until)?;
    // Lists of accounts wake the sync, which then waits out the time the
    // deletion can be undone.
    Ok(Written::of(&list, true))
}

fn restore_task_tx(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<(i64, Written)> {
    let too_late = || Error::Invalid("The task can't be brought back any more.".into());
    let row = match tdb::get_task(tx, id) {
        Ok(row) => row,
        Err(Error::NotFound(_)) => return Err(too_late()),
        Err(e) => return Err(e),
    };
    let list = tdb::get_list(tx, row.list_id)?;
    if row.local_state != tdb::DELETED {
        // Brought back already.
        return Ok((row.id, Written::of(&list, false)));
    }
    if row.deleted_state.is_none() || row.push_after <= now_ms() {
        return Err(too_late());
    }
    if let Some(parent) = row.parent_id {
        let parent = tdb::get_task(tx, parent)?;
        if parent.local_state == tdb::DELETED
            || parent.list_id != row.list_id
            || parent.parent_id.is_some()
        {
            return Err(Error::Invalid(
                "The task it belonged to isn't there any more.".into(),
            ));
        }
    }
    let pending = tdb::restore_deleted(tx, row.id)?;
    Ok((
        row.id,
        Written {
            lists: vec![list.id],
            remote: pending && list.account_id.is_some(),
        },
    ))
}

fn clear_completed_tx(tx: &rusqlite::Transaction<'_>, list_id: i64) -> Result<Written> {
    let list = tdb::get_list(tx, list_id)?;
    writable(&list)?;
    let done = tdb::completed_in(tx, list.id)?;
    for row in &done {
        for child in tdb::children(tx, row.id)? {
            retire(tx, &list, &child)?;
        }
        retire(tx, &list, row)?;
    }
    Ok(Written::of(&list, !done.is_empty()))
}

impl Engine {
    /// Makes sure there is somewhere to put tasks: the list on this
    /// computer exists whether or not accounts have lists.
    pub fn ensure_local_task_list(&self) -> Result<i64> {
        if let Some(id) = self.db.read(tdb::local_default_list)? {
            return Ok(id);
        }
        self.db.write(|tx| {
            // Re-check inside the write: another caller may have won.
            match tdb::local_default_list(tx)? {
                Some(id) => Ok(id),
                None => tdb::create_local_list(tx, LOCAL_LIST_NAME, true),
            }
        })
    }

    /// Lists of this computer and of enabled accounts: the local ones first,
    /// then by account, then in the server's order.
    pub fn task_lists(&self) -> Result<Vec<TaskList>> {
        self.ensure_local_task_list()?;
        let (lists, counts) = self
            .db
            .read(|c| Ok((tdb::list_lists(c)?, tdb::list_counts(c)?)))?;
        Ok(lists
            .iter()
            .map(|l| list_view(l, counts.get(&l.id).copied().unwrap_or_default()))
            .collect())
    }

    fn task_list(&self, id: i64) -> Result<TaskList> {
        let (list, counts) = self
            .db
            .read(|c| Ok((tdb::get_list(c, id)?, tdb::list_counts(c)?)))?;
        Ok(list_view(
            &list,
            counts.get(&id).copied().unwrap_or_default(),
        ))
    }

    /// The tasks of a view, in display order (see [`TaskView`]).
    /// `include_completed` only applies to a single list.
    pub async fn tasks_query(&self, view: TaskView, include_completed: bool) -> Result<Vec<Task>> {
        let today = today();
        self.db
            .read_async(move |c| {
                let rows = match view {
                    TaskView::List(list) => {
                        let mut rows = tdb::list_open(c, list)?;
                        if include_completed {
                            rows.extend(tdb::list_completed(c, list, MAX_COMPLETED)?);
                        }
                        rows
                    }
                    TaskView::Today => tdb::due_until(c, &today)?,
                    TaskView::Upcoming => tdb::due_after(c, &today)?,
                    TaskView::Important => tdb::important(c)?,
                    TaskView::All => tdb::all_open(c)?,
                };
                task_views(c, &rows)
            })
            .await
    }

    /// Open tasks due in `[from, to)`, for the calendar: those due at a time
    /// in it, and those due on a day it overlaps.
    pub async fn tasks_due(&self, from: i64, to: i64) -> Result<Vec<Task>> {
        if to <= from {
            return Ok(Vec::new());
        }
        let (first, last) = (local_day(from), local_day(to - 1));
        self.db
            .read_async(move |c| {
                let rows = tdb::due_between(c, from, to, &first, &last)?;
                task_views(c, &rows)
            })
            .await
    }

    /// One task.
    pub async fn task(&self, id: i64) -> Result<Task> {
        self.db
            .read_async(move |c| task_view(c, &live_task(c, id)?))
            .await
    }

    /// The subtasks of a task, in order, completed ones included.
    pub async fn task_subtasks(&self, id: i64) -> Result<Vec<Task>> {
        self.db
            .read_async(move |c| {
                live_task(c, id)?;
                task_views(c, &tdb::children(c, id)?)
            })
            .await
    }

    /// Records what a write did: wakes the sync when a server has to hear
    /// about it and tells the UI.
    fn written(&self, written: Written) {
        if written.remote {
            crate::sync::tasks::wake(self);
        }
        if !written.lists.is_empty() {
            let mut list_ids = written.lists;
            list_ids.sort_unstable();
            list_ids.dedup();
            self.emit(EngineEvent::TasksChanged { list_ids });
        }
    }

    /// Creates a task (at the end of its list or task). It is stored here
    /// at once and written to the server in the background.
    pub async fn create_task(&self, input: TaskInput) -> Result<Task> {
        let (id, written) = self
            .db
            .write_async(move |tx| create_task_tx(tx, &input))
            .await?;
        self.written(written);
        self.task(id).await
    }

    /// Changes a task; a new `listId` moves it (with its subtasks) to that
    /// list, which may give it a new id.
    pub async fn update_task(&self, id: i64, input: TaskInput) -> Result<Task> {
        let (id, written) = self
            .db
            .write_async(move |tx| update_task_tx(tx, id, &input))
            .await?;
        self.written(written);
        self.task(id).await
    }

    /// Completes or reopens a task. A repeating task comes back open with
    /// its next due date.
    pub async fn complete_task(&self, id: i64, completed: bool) -> Result<Task> {
        let (id, written) = self
            .db
            .write_async(move |tx| complete_task_tx(tx, id, completed))
            .await?;
        self.written(written);
        self.task(id).await
    }

    /// Moves a task before `before` (a task with the same parent), or to the
    /// end with `None`.
    pub async fn reorder_task(&self, id: i64, before: Option<i64>) -> Result<()> {
        let written = self
            .db
            .write_async(move |tx| reorder_task_tx(tx, id, before))
            .await?;
        self.written(written);
        Ok(())
    }

    /// Deletes a task and its subtasks. For [`UNDO_DELETE_MS`] they can be
    /// brought back with [`Engine::restore_task`]; until then nothing
    /// reaches the server.
    pub async fn delete_task(&self, id: i64) -> Result<()> {
        let written = self
            .db
            .write_async(move |tx| delete_task_tx(tx, id))
            .await?;
        self.written(written);
        Ok(())
    }

    /// Brings back a task deleted with [`Engine::delete_task`], with the
    /// subtasks deleted with it, as they were (changes not uploaded yet
    /// included). Fails once the deletion has been carried out.
    pub async fn restore_task(&self, id: i64) -> Result<Task> {
        let (id, written) = self
            .db
            .write_async(move |tx| restore_task_tx(tx, id))
            .await?;
        self.written(written);
        self.task(id).await
    }

    /// Removes the tasks deleted on this computer that can't be brought
    /// back after `until` (when Envelope starts, all of them).
    pub(crate) fn purge_deleted_tasks(&self, until: i64) -> Result<()> {
        self.db.write(|tx| tdb::purge_deleted(tx, until)).map(drop)
    }

    /// Deletes the completed top-level tasks of a list, with their
    /// subtasks; completed subtasks of open tasks stay.
    pub async fn clear_completed_tasks(&self, list_id: i64) -> Result<()> {
        let written = self
            .db
            .write_async(move |tx| clear_completed_tx(tx, list_id))
            .await?;
        self.written(written);
        Ok(())
    }

    /// Creates a list on this computer (`None`) or in an account, on its
    /// server.
    pub async fn create_task_list(&self, account_id: Option<i64>, name: &str) -> Result<TaskList> {
        let name = clean_name(name)?;
        let id = match account_id {
            None => self
                .db
                .write(|tx| tdb::create_local_list(tx, &name, false))?,
            Some(account_id) => {
                let account = self.account_row(account_id)?;
                let service = crate::sync::tasks::service(self, &account)
                    .await?
                    .ok_or_else(|| Error::Invalid("This account has no task lists.".into()))?;
                let created = service.create_list(&name).await?;
                self.db
                    .write(|tx| tdb::insert_list(tx, account.id, service.kind(), &created))?
            }
        };
        self.emit(EngineEvent::TasksChanged {
            list_ids: Vec::new(),
        });
        self.task_list(id)
    }

    /// Renames a list (on its server too).
    pub async fn rename_task_list(&self, id: i64, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        let list = self.db.read(|c| tdb::get_list(c, id))?;
        writable(&list)?;
        if list.name == name {
            return Ok(());
        }
        if let Some(account_id) = list.account_id {
            let account = self.account_row(account_id)?;
            let service = crate::sync::tasks::service(self, &account)
                .await?
                .ok_or_else(|| Error::Invalid("This account has no task lists.".into()))?;
            service.rename_list(&list, &name).await?;
        }
        self.db.write(|tx| tdb::rename_list(tx, id, &name))?;
        self.emit(EngineEvent::TasksChanged {
            list_ids: Vec::new(),
        });
        Ok(())
    }

    /// Deletes a list with its tasks (on its server too). The list on this
    /// computer and an account's default list stay.
    pub async fn delete_task_list(&self, id: i64) -> Result<()> {
        let list = self.db.read(|c| tdb::get_list(c, id))?;
        writable(&list)?;
        if list.is_default {
            return Err(Error::Invalid(match list.account_id {
                None => "The Tasks list on this computer can't be deleted.".into(),
                Some(_) => "An account's default list can't be deleted.".into(),
            }));
        }
        if let Some(account_id) = list.account_id {
            let account = self.account_row(account_id)?;
            let service = crate::sync::tasks::service(self, &account)
                .await?
                .ok_or_else(|| Error::Invalid("This account has no task lists.".into()))?;
            service.delete_list(&list).await?;
        }
        self.db.write(|tx| tdb::delete_list(tx, id))?;
        self.emit(EngineEvent::TasksChanged {
            list_ids: Vec::new(),
        });
        Ok(())
    }

    /// Follows a change of the time zone. The day of a task due at a time
    /// of day, which the views of today and what is upcoming go by, is
    /// worked out when its time is set, in the zone of the moment; when
    /// the zone is not the one they were worked out in, they are worked out
    /// again. Cheap when it is.
    pub(crate) fn follow_time_zone(&self) -> Result<()> {
        let zone = local_tz();
        let known: Option<String> = self.db.read(|c| misc::kv_get(c, TIME_ZONE_KEY))?;
        if known.as_deref() == Some(zone.name()) {
            return Ok(());
        }
        let list_ids = self.db.write(|tx| {
            let changed = tdb::redo_due_days(tx, zone)?;
            misc::kv_set(tx, TIME_ZONE_KEY, &zone.name())?;
            Ok(changed)
        })?;
        if !list_ids.is_empty() {
            self.emit(EngineEvent::TasksChanged { list_ids });
        }
        Ok(())
    }

    /// Announces open tasks that became due at a time of day, once each
    /// (late ones only within the hour). Returns when the next one is due.
    pub(crate) fn notify_due_tasks(&self, now: i64) -> Result<Option<i64>> {
        // Found and marked in one write, so that no task is announced twice.
        let due = self.db.write(|tx| {
            let due = tdb::due_unnotified(tx, now)?;
            let ids: Vec<i64> = due.iter().map(|(id, ..)| *id).collect();
            tdb::mark_notified(tx, &ids)?;
            Ok(due)
        })?;
        for (task_id, title, due_at) in due {
            if due_at > now - LATE_NOTICE_MS {
                self.emit(EngineEvent::TaskDue {
                    task_id,
                    title,
                    due_at,
                });
            }
        }
        self.db.read(|c| tdb::next_due_at(c, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_and_inputs_use_the_wire_format() {
        assert_eq!(
            serde_json::from_value::<TaskView>(serde_json::json!({ "list": 4 })).ok(),
            Some(TaskView::List(4))
        );
        assert_eq!(
            serde_json::from_value::<TaskView>(serde_json::json!("upcoming")).ok(),
            Some(TaskView::Upcoming)
        );
        let input: TaskInput = serde_json::from_value(serde_json::json!({
            "listId": 2,
            "title": "Call Maya",
            "dueDate": null,
            "dueAt": 1_700_000_000_000_i64,
            "fromMessage": 9
        }))
        .unwrap_or_default();
        assert_eq!(input.list_id, 2);
        assert_eq!(input.due_date, Some(None), "null clears");
        assert_eq!(input.due_at, Some(Some(1_700_000_000_000)));
        assert_eq!(input.parent_id, None, "left out keeps");
        assert_eq!(input.notes, None);
        assert_eq!(input.from_message, Some(9));
        assert_eq!(
            serde_json::to_value(TaskListKind::Caldav).ok(),
            Some(serde_json::json!("caldav"))
        );
    }

    #[test]
    fn lists_and_tasks_use_the_wire_format() {
        let list = list_view(
            &ListRow {
                id: 3,
                account_id: None,
                kind: TaskListKind::Local,
                remote_id: "local-1".into(),
                name: LOCAL_LIST_NAME.into(),
                color: "#6d5efc".into(),
                read_only: false,
                is_default: true,
                sort_order: 0,
                sync_state: Default::default(),
            },
            ListCounts {
                open: 2,
                completed: 1,
            },
        );
        assert_eq!(
            serde_json::to_value(&list).ok(),
            Some(serde_json::json!({
                "id": 3, "accountId": null, "kind": "local", "name": "Tasks", "color": "#6d5efc",
                "readOnly": false, "isDefault": true, "openCount": 2, "completedCount": 1,
                "dueTimes": true, "importance": true, "subtasks": true, "reorder": true
            }))
        );
        let task = Task {
            id: 1,
            list_id: 3,
            parent_id: None,
            title: "Call Maya".into(),
            notes: String::new(),
            due_date: Some("2026-10-30".into()),
            due_at: Some(1_793_365_200_000),
            completed_at: None,
            important: true,
            repeats: false,
            source: Some(TaskSource {
                message_id: "m@x".into(),
                thread_id: Some(9),
                account_id: Some(2),
                subject: "Offer".into(),
                from: None,
            }),
            subtask_count: 0,
            subtasks_done: 0,
            created_at: 1,
            updated_at: 2,
            pending: true,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&task).ok(),
            Some(serde_json::json!({
                "id": 1, "listId": 3, "parentId": null, "title": "Call Maya", "notes": "",
                "dueDate": "2026-10-30", "dueAt": 1_793_365_200_000_i64, "completedAt": null,
                "important": true, "repeats": false,
                "source": { "messageId": "m@x", "threadId": 9, "accountId": 2, "subject": "Offer", "from": null },
                "subtaskCount": 0, "subtasksDone": 0, "createdAt": 1, "updatedAt": 2,
                "pending": true, "error": null
            }))
        );
    }

    #[test]
    fn due_inputs() {
        let at = 1_700_000_000_000;
        let input = |due_date: Option<Option<&str>>, due_at: Option<Option<i64>>| TaskInput {
            due_date: due_date.map(|d| d.map(str::to_owned)),
            due_at,
            ..TaskInput::default()
        };
        let now = Some("2026-02-01");
        assert_eq!(
            due_change(&input(None, Some(Some(at))), true, now).ok(),
            Some(Some((Some(local_day(at)), Some(at))))
        );
        assert_eq!(
            due_change(&input(None, Some(Some(at))), false, now).ok(),
            Some(Some((Some(local_day(at)), None))),
            "lists without times keep the day"
        );
        assert_eq!(
            due_change(&input(Some(Some("2026-03-01")), None), true, now).ok(),
            Some(Some((Some("2026-03-01".into()), None))),
            "only a day clears the time"
        );
        assert_eq!(
            due_change(&input(Some(None), Some(None)), true, now).ok(),
            Some(Some((None, None)))
        );
        assert_eq!(
            due_change(&input(None, Some(None)), true, now).ok(),
            Some(Some((Some("2026-02-01".into()), None))),
            "clearing only the time keeps the day"
        );
        assert_eq!(
            due_change(&input(None, Some(None)), true, None).ok(),
            Some(Some((None, None)))
        );
        assert_eq!(due_change(&input(None, None), true, now).ok(), Some(None));
        assert!(due_change(&input(Some(Some("tomorrow")), None), true, now).is_err());
    }
}
