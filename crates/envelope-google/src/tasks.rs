//! Google Tasks API v1: the user's task lists and their tasks, with
//! incremental pulls, writes and moves.
//!
//! A list is pulled page by page with [`GoogleClient::task_changes`]: the
//! first run lists every task, later runs only what changed since the
//! previous one (`updatedMin`), deletions included. Google keeps only the
//! day of a due date — `due` is midnight UTC of that day, and the time of
//! day can be neither read nor written — so it is a [`NaiveDate`] here.
//! Tasks have no importance; subtasks name their parent task (`parent`),
//! and siblings are ordered by `position`, which only
//! [`GoogleClient::move_task`] changes.

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Error, Result, ensure_page_progress};
use crate::serde_util::{non_empty, null_default};

/// Most tasks `tasks.list` returns per page.
pub const MAX_TASK_PAGE_SIZE: u32 = 100;

/// Task lists requested per page (Google's maximum).
const TASK_LIST_PAGE_SIZE: &str = "1000";

/// How far before the previous run started an incremental pull reaches
/// back, so that changes made while that run was under way are not missed.
const UPDATED_MIN_OVERLAP: Duration = Duration::minutes(1);

/// A task list (`tasklists` resource).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTaskList {
    /// Task list id.
    pub id: String,
    /// ETag of the list.
    pub etag: Option<String>,
    /// The list's name.
    pub title: String,
    /// When the list last changed.
    pub updated: Option<DateTime<Utc>>,
}

/// Whether a task is done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    /// Still to do (`needsAction`).
    #[default]
    NeedsAction,
    /// Done (`completed`).
    Completed,
}

impl TaskStatus {
    /// Google's wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::NeedsAction => "needsAction",
            TaskStatus::Completed => "completed",
        }
    }
}

/// A link Google attached to a task, such as the Gmail message the task was
/// made from. The API cannot add or change links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLink {
    /// `email`, `generic`, `chat_message`, `keep_note`, …
    pub kind: String,
    /// What the link points to (the subject, for `email`), when given.
    pub description: Option<String>,
    /// The URL.
    pub link: String,
}

/// A task (`tasks` resource).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTask {
    /// Task id; it stays the same for the life of the task.
    pub id: String,
    /// ETag of this version of the task.
    pub etag: Option<String>,
    /// Title (may be empty).
    pub title: String,
    /// Notes.
    pub notes: Option<String>,
    /// Whether the task is done.
    pub status: TaskStatus,
    /// The day the task is due.
    pub due: Option<NaiveDate>,
    /// When the task was completed.
    pub completed: Option<DateTime<Utc>>,
    /// Id of the parent task, for a subtask.
    pub parent: Option<String>,
    /// Order among the siblings: positions compare as strings.
    pub position: Option<String>,
    /// The task was deleted (reported by incremental pulls).
    pub deleted: bool,
    /// The task is completed and was cleared from the list. The Google Tasks
    /// apps clear tasks as soon as they are completed there.
    pub hidden: bool,
    /// When the task last changed.
    pub updated: Option<DateTime<Utc>>,
    /// Links Google attached (the Gmail message a task was made from, …).
    pub links: Vec<TaskLink>,
    /// The task in the Google Tasks web interface.
    pub web_view_link: Option<String>,
    /// Assigned to the user from a Google Doc or a Chat space. Such tasks
    /// cannot have notes, subtasks or a parent, and deleting one deletes it
    /// in the Doc or space as well.
    pub assigned: bool,
}

/// Which tasks `tasks.list` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskQuery {
    /// Include completed tasks.
    pub show_completed: bool,
    /// Include hidden tasks: completed tasks cleared from the list, which is
    /// what the Google Tasks apps do with every task completed in them.
    pub show_hidden: bool,
    /// Include deleted tasks (with [`GoogleTask::deleted`] set).
    pub show_deleted: bool,
    /// Include tasks assigned to the user from Google Docs and Chat spaces.
    pub show_assigned: bool,
    /// Only tasks changed at or after this instant (`updatedMin`).
    pub updated_min: Option<DateTime<Utc>>,
    /// Tasks per page, 1 to [`MAX_TASK_PAGE_SIZE`].
    pub page_size: u32,
}

impl Default for TaskQuery {
    fn default() -> Self {
        TaskQuery::full()
    }
}

impl TaskQuery {
    /// Every task of the list, completed and hidden ones included.
    pub fn full() -> Self {
        TaskQuery {
            show_completed: true,
            show_hidden: true,
            show_deleted: false,
            show_assigned: false,
            updated_min: None,
            page_size: MAX_TASK_PAGE_SIZE,
        }
    }

    /// Every task changed at or after `since`, deleted ones included.
    pub fn changed_since(since: DateTime<Utc>) -> Self {
        TaskQuery {
            show_deleted: true,
            updated_min: Some(since),
            ..TaskQuery::full()
        }
    }
}

/// One page of `tasks.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPage {
    /// The tasks of the page.
    pub tasks: Vec<GoogleTask>,
    /// Token of the next page, if any.
    pub next_page_token: Option<String>,
    /// When Google answered, by its own clock.
    pub server_time: Option<DateTime<Utc>>,
}

/// Where a pull of a task list stands.
///
/// Store it as JSON together with each page's tasks: [`TaskCursor::Next`]
/// resumes an interrupted run, [`TaskCursor::Done`] starts the next
/// incremental run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TaskCursor {
    /// A run is under way: fetch `page_token` next.
    Next {
        /// Lower bound of the run (`updatedMin`); `None` for a full listing.
        updated_min: Option<DateTime<Utc>>,
        /// The page to fetch.
        page_token: String,
        /// When the run started, by Google's clock.
        started: DateTime<Utc>,
    },
    /// The run is complete: the next one lists what changed since `since`.
    Done {
        /// Lower bound (`updatedMin`) of the next run.
        since: DateTime<Utc>,
    },
}

impl TaskCursor {
    /// True when the run is complete.
    pub fn is_done(&self) -> bool {
        matches!(self, TaskCursor::Done { .. })
    }
}

/// One page of a pull started by [`GoogleClient::task_changes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskChanges {
    /// New and changed tasks; incremental runs also report deleted ones
    /// ([`GoogleTask::deleted`]).
    pub tasks: Vec<GoogleTask>,
    /// What to store and pass to the next call.
    pub next: TaskCursor,
    /// This page belongs to a full listing: once the run is done, tasks it
    /// did not list are gone from the server.
    pub full: bool,
}

/// A task to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTask {
    /// Title.
    pub title: String,
    /// Notes.
    pub notes: Option<String>,
    /// The day the task is due.
    pub due: Option<NaiveDate>,
    /// Create the task already completed, at this instant.
    pub completed: Option<DateTime<Utc>>,
}

impl NewTask {
    /// The `Task` resource to insert.
    fn to_json(&self) -> Value {
        let mut body = Map::new();
        body.insert("title".into(), Value::String(self.title.clone()));
        if let Some(notes) = self.notes.as_deref().filter(|n| !n.trim().is_empty()) {
            body.insert("notes".into(), Value::String(notes.to_owned()));
        }
        if let Some(due) = self.due {
            body.insert("due".into(), Value::String(due_value(due)));
        }
        let status = match self.completed {
            Some(at) => {
                body.insert("completed".into(), Value::String(timestamp(at)));
                TaskStatus::Completed
            }
            None => TaskStatus::NeedsAction,
        };
        body.insert("status".into(), Value::String(status.as_str().to_owned()));
        Value::Object(body)
    }
}

/// Where a task goes: under which parent and after which sibling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPosition {
    /// The parent task, for a subtask; `None` for a top-level task.
    pub parent: Option<String>,
    /// The sibling the task follows; `None` for the first position.
    pub previous: Option<String>,
}

impl TaskPosition {
    /// After `previous` (the first position when `None`) below `parent`
    /// (at the top level when `None`). The default is the first position at
    /// the top level.
    pub fn new(parent: Option<&str>, previous: Option<&str>) -> Self {
        TaskPosition {
            parent: parent.map(str::to_owned),
            previous: previous.map(str::to_owned),
        }
    }
}

/// A partial update of a task (`tasks.patch`).
///
/// Only the fields that were set are sent. Clearing one sends an explicit
/// `null`, which is how Google removes a field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskPatch {
    fields: Map<String, Value>,
}

impl TaskPatch {
    /// An empty patch.
    pub fn new() -> Self {
        TaskPatch::default()
    }

    /// Sets the title.
    pub fn title(self, title: impl Into<String>) -> Self {
        self.set("title", Value::String(title.into()))
    }

    /// Sets the notes; `None` or blank notes clear them.
    pub fn notes(self, notes: Option<&str>) -> Self {
        let value = notes
            .filter(|n| !n.trim().is_empty())
            .map_or(Value::Null, |n| Value::String(n.to_owned()));
        self.set("notes", value)
    }

    /// Sets the day the task is due; `None` removes the due date.
    pub fn due(self, due: Option<NaiveDate>) -> Self {
        self.set(
            "due",
            due.map_or(Value::Null, |d| Value::String(due_value(d))),
        )
    }

    /// Completes the task at the given instant, or reopens it with `None`.
    /// Reopening also clears the completion time (`completed: null`), which
    /// Google needs for the task to become open again.
    pub fn completed(self, completed: Option<DateTime<Utc>>) -> Self {
        match completed {
            Some(at) => self
                .set("status", json!(TaskStatus::Completed.as_str()))
                .set("completed", Value::String(timestamp(at))),
            None => self
                .set("status", json!(TaskStatus::NeedsAction.as_str()))
                .set("completed", Value::Null),
        }
    }

    /// Sets any other field of the `Task` resource verbatim.
    pub fn raw(self, field: impl Into<String>, value: Value) -> Self {
        let mut patch = self;
        patch.fields.insert(field.into(), value);
        patch
    }

    /// True if the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The JSON body.
    pub fn to_json(&self) -> Value {
        Value::Object(self.fields.clone())
    }

    fn set(mut self, field: &str, value: Value) -> Self {
        self.fields.insert(field.to_owned(), value);
        self
    }
}

impl GoogleClient {
    /// Lists the user's task lists.
    pub async fn list_task_lists(&self) -> Result<Vec<GoogleTaskList>> {
        let mut lists = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let request = ApiRequest::get(Service::Tasks, &["users", "@me", "lists"])
                .query("maxResults", TASK_LIST_PAGE_SIZE)
                .query_opt("pageToken", page_token.clone());
            let page: WireTaskLists = self.send_json(&request, "tasklists.list").await?;
            ensure_page_progress(
                page_token.as_deref(),
                page.next_page_token.as_deref(),
                "tasklists.list",
            )?;
            for item in page.items {
                match WireTaskList::deserialize(&item).map(map_task_list) {
                    Ok(Some(list)) => lists.push(list),
                    Ok(None) => tracing::warn!("skipping task list without id"),
                    Err(err) => tracing::warn!(error = %err, "skipping malformed task list"),
                }
            }
            match non_empty(page.next_page_token) {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        Ok(lists)
    }

    /// The user's default task list (`@default`, "My Tasks" unless renamed),
    /// where tasks go when no list is chosen.
    pub async fn default_task_list(&self) -> Result<GoogleTaskList> {
        let request = ApiRequest::get(Service::Tasks, &["users", "@me", "lists", "@default"]);
        task_list_response(self.send_json(&request, "tasklists.get").await?)
    }

    /// Creates a task list.
    pub async fn insert_task_list(&self, title: &str) -> Result<GoogleTaskList> {
        let request = ApiRequest::new(Method::POST, Service::Tasks, &["users", "@me", "lists"])
            .json(&json!({ "title": title }))?;
        task_list_response(self.send_json(&request, "tasklists.insert").await?)
    }

    /// Renames a task list.
    pub async fn rename_task_list(&self, list_id: &str, title: &str) -> Result<GoogleTaskList> {
        let request = ApiRequest::new(
            Method::PATCH,
            Service::Tasks,
            &["users", "@me", "lists", list_id],
        )
        .json(&json!({ "title": title }))?;
        task_list_response(self.send_json(&request, "tasklists.patch").await?)
    }

    /// Deletes a task list and its tasks. Deleting a list that is already
    /// gone (`410`) succeeds.
    pub async fn delete_task_list(&self, list_id: &str) -> Result<()> {
        let request = ApiRequest::new(
            Method::DELETE,
            Service::Tasks,
            &["users", "@me", "lists", list_id],
        );
        match self.execute(&request).await {
            Ok(_) => Ok(()),
            Err(err) if err.is_gone() => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Fetches one page of `tasks.list`.
    ///
    /// Use [`GoogleClient::task_changes`] to synchronise a list; this
    /// lower-level call is for callers that choose the query themselves. A
    /// query with `updated_min` is treated like a sync request: when Google
    /// can no longer list changes that far back, it fails with
    /// [`Error::SyncTokenExpired`] and the list has to be listed in full.
    pub async fn tasks_page(
        &self,
        list_id: &str,
        query: &TaskQuery,
        page_token: Option<&str>,
    ) -> Result<TaskPage> {
        let mut request = ApiRequest::get(Service::Tasks, &["lists", list_id, "tasks"])
            .query(
                "maxResults",
                query.page_size.clamp(1, MAX_TASK_PAGE_SIZE).to_string(),
            )
            .query("showCompleted", bool_value(query.show_completed))
            .query("showHidden", bool_value(query.show_hidden))
            .query("showDeleted", bool_value(query.show_deleted))
            .query_if(query.show_assigned, "showAssigned", "true")
            .query_opt("updatedMin", query.updated_min.map(timestamp))
            .query_opt("pageToken", page_token);
        if query.updated_min.is_some() {
            request = request.sync_request();
        }
        let (body, server_time) = self.execute_dated(&request).await?;
        let list: WireTasks = serde_json::from_slice(&body).map_err(|source| Error::Decode {
            context: "tasks.list",
            source,
        })?;
        let next_page_token = non_empty(list.next_page_token);
        ensure_page_progress(page_token, next_page_token.as_deref(), "tasks.list")?;
        let mut tasks = Vec::with_capacity(list.items.len());
        for item in list.items {
            match WireTask::deserialize(&item).map(map_task) {
                Ok(Some(task)) => tasks.push(task),
                Ok(None) => tracing::warn!("skipping task without id"),
                Err(err) => tracing::warn!(error = %err, "skipping malformed task"),
            }
        }
        Ok(TaskPage {
            tasks,
            next_page_token,
            server_time,
        })
    }

    /// Fetches the next page of a pull of `list_id`: without a cursor a full
    /// listing begins, [`TaskCursor::Next`] continues a run and
    /// [`TaskCursor::Done`] begins an incremental one.
    ///
    /// Persist each page's tasks and then [`TaskChanges::next`]. Full
    /// listings include completed and hidden tasks; incremental runs report
    /// deletions as tasks with [`GoogleTask::deleted`] set. Incremental runs
    /// start a minute before the previous run did (by Google's clock), so
    /// they may repeat a few unchanged tasks; compare etags. On
    /// [`Error::SyncTokenExpired`], start over without a cursor.
    pub async fn task_changes(
        &self,
        list_id: &str,
        cursor: Option<&TaskCursor>,
    ) -> Result<TaskChanges> {
        let (query, page_token, started) = match cursor {
            None => (TaskQuery::full(), None, None),
            Some(TaskCursor::Done { since }) => (TaskQuery::changed_since(*since), None, None),
            Some(TaskCursor::Next {
                updated_min,
                page_token,
                started,
            }) => (
                updated_min.map_or_else(TaskQuery::full, TaskQuery::changed_since),
                Some(page_token.as_str()),
                Some(*started),
            ),
        };
        let asked = Utc::now();
        let page = self.tasks_page(list_id, &query, page_token).await?;
        // The run's watermark is Google's clock when it began; the local
        // clock only stands in when Google sent no `Date`.
        let started = started.or(page.server_time).unwrap_or(asked);
        let next = match page.next_page_token {
            Some(page_token) => TaskCursor::Next {
                updated_min: query.updated_min,
                page_token,
                started,
            },
            None => TaskCursor::Done {
                since: started - UPDATED_MIN_OVERLAP,
            },
        };
        Ok(TaskChanges {
            tasks: page.tasks,
            next,
            full: query.updated_min.is_none(),
        })
    }

    /// Fetches one task.
    pub async fn get_task(&self, list_id: &str, task_id: &str) -> Result<GoogleTask> {
        let request = ApiRequest::get(Service::Tasks, &["lists", list_id, "tasks", task_id]);
        task_response(self.send_json(&request, "tasks.get").await?)
    }

    /// Creates a task at `position` and returns it as stored.
    pub async fn insert_task(
        &self,
        list_id: &str,
        task: &NewTask,
        position: &TaskPosition,
    ) -> Result<GoogleTask> {
        let request = ApiRequest::new(Method::POST, Service::Tasks, &["lists", list_id, "tasks"])
            .query_opt("parent", position.parent.clone())
            .query_opt("previous", position.previous.clone())
            .json(&task.to_json())?;
        task_response(self.send_json(&request, "tasks.insert").await?)
    }

    /// Changes the fields set in `patch` and returns the task as stored. An
    /// empty patch only reads the task.
    pub async fn patch_task(
        &self,
        list_id: &str,
        task_id: &str,
        patch: &TaskPatch,
    ) -> Result<GoogleTask> {
        if patch.is_empty() {
            return self.get_task(list_id, task_id).await;
        }
        let request = ApiRequest::new(
            Method::PATCH,
            Service::Tasks,
            &["lists", list_id, "tasks", task_id],
        )
        .json(&patch.to_json())?;
        task_response(self.send_json(&request, "tasks.patch").await?)
    }

    /// Deletes a task. Deleting a task that is already gone (`410`)
    /// succeeds.
    pub async fn delete_task(&self, list_id: &str, task_id: &str) -> Result<()> {
        let request = ApiRequest::new(
            Method::DELETE,
            Service::Tasks,
            &["lists", list_id, "tasks", task_id],
        );
        match self.execute(&request).await {
            Ok(_) => Ok(()),
            Err(err) if err.is_gone() => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Moves a task to `position`, optionally into another list, and returns
    /// it as stored there.
    ///
    /// Google refuses some moves: a parent must not be hidden, a completed
    /// and hidden task can only become the first top-level task, repeating
    /// and assigned tasks can neither have a parent nor become one, and
    /// repeating tasks cannot move to another list.
    pub async fn move_task(
        &self,
        list_id: &str,
        task_id: &str,
        position: &TaskPosition,
        destination_list_id: Option<&str>,
    ) -> Result<GoogleTask> {
        let request = ApiRequest::new(
            Method::POST,
            Service::Tasks,
            &["lists", list_id, "tasks", task_id, "move"],
        )
        .query_opt("parent", position.parent.clone())
        .query_opt("previous", position.previous.clone())
        .query_opt("destinationTasklist", destination_list_id);
        task_response(self.send_json(&request, "tasks.move").await?)
    }

    /// Clears the completed tasks of a list: Google hides them (they stay
    /// listed with [`GoogleTask::hidden`] set).
    pub async fn clear_completed_tasks(&self, list_id: &str) -> Result<()> {
        let request = ApiRequest::new(Method::POST, Service::Tasks, &["lists", list_id, "clear"]);
        self.execute(&request).await.map(drop)
    }
}

/// `due` as Google writes it: midnight UTC of the day.
fn due_value(day: NaiveDate) -> String {
    format!("{}T00:00:00.000Z", day.format("%Y-%m-%d"))
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn bool_value(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn parse_timestamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw?.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The day of a `due` value. Google only keeps the date, which is the date
/// written in the timestamp.
fn parse_due(raw: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw?.trim().get(..10)?, "%Y-%m-%d").ok()
}

fn map_task_list(wire: WireTaskList) -> Option<GoogleTaskList> {
    Some(GoogleTaskList {
        id: non_empty(wire.id)?,
        etag: non_empty(wire.etag),
        title: wire.title.unwrap_or_default(),
        updated: parse_timestamp(wire.updated.as_deref()),
    })
}

fn map_task(wire: WireTask) -> Option<GoogleTask> {
    Some(GoogleTask {
        id: non_empty(wire.id)?,
        etag: non_empty(wire.etag),
        title: wire.title.unwrap_or_default(),
        notes: non_empty(wire.notes),
        status: match wire.status.as_deref() {
            Some("completed") => TaskStatus::Completed,
            _ => TaskStatus::NeedsAction,
        },
        due: parse_due(wire.due.as_deref()),
        completed: parse_timestamp(wire.completed.as_deref()),
        parent: non_empty(wire.parent),
        position: non_empty(wire.position),
        deleted: wire.deleted.unwrap_or(false),
        hidden: wire.hidden.unwrap_or(false),
        updated: parse_timestamp(wire.updated.as_deref()),
        links: wire
            .links
            .into_iter()
            .filter_map(|link| {
                Some(TaskLink {
                    kind: link.kind.unwrap_or_default(),
                    description: non_empty(link.description),
                    link: non_empty(link.link)?,
                })
            })
            .collect(),
        web_view_link: non_empty(wire.web_view_link),
        assigned: wire.assignment_info.is_some_and(|info| !info.is_null()),
    })
}

fn task_list_response(wire: WireTaskList) -> Result<GoogleTaskList> {
    map_task_list(wire)
        .ok_or_else(|| Error::InvalidResponse("Google returned a task list without id".to_owned()))
}

fn task_response(wire: WireTask) -> Result<GoogleTask> {
    map_task(wire)
        .ok_or_else(|| Error::InvalidResponse("Google returned a task without id".to_owned()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTaskLists {
    #[serde(default, deserialize_with = "null_default")]
    items: Vec<Value>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTaskList {
    id: Option<String>,
    etag: Option<String>,
    title: Option<String>,
    updated: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTasks {
    #[serde(default, deserialize_with = "null_default")]
    items: Vec<Value>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTask {
    id: Option<String>,
    etag: Option<String>,
    title: Option<String>,
    updated: Option<String>,
    parent: Option<String>,
    position: Option<String>,
    notes: Option<String>,
    status: Option<String>,
    due: Option<String>,
    completed: Option<String>,
    deleted: Option<bool>,
    hidden: Option<bool>,
    #[serde(default, deserialize_with = "null_default")]
    links: Vec<WireLink>,
    web_view_link: Option<String>,
    assignment_info: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct WireLink {
    #[serde(rename = "type")]
    kind: Option<String>,
    description: Option<String>,
    link: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn task(value: Value) -> Option<GoogleTask> {
        map_task(WireTask::deserialize(&value).expect("task"))
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("instant")
    }

    #[test]
    fn maps_tasks() {
        let mapped = task(json!({
            "kind": "tasks#task",
            "id": "dGFzazE",
            "etag": "\"LTE4NTk\"",
            "title": "Send the invoice",
            "updated": "2026-10-28T09:12:30.512Z",
            "parent": "cGFyZW50",
            "position": "00000000000000000002",
            "notes": "Ask Maya for the PO number",
            "status": "completed",
            "due": "2026-11-02T00:00:00.000Z",
            "completed": "2026-10-28T09:12:30.000Z",
            "hidden": true,
            "links": [
                {"type": "email", "description": "Invoice for October", "link": "https://mail.google.com/mail/#all/18f"},
                {"type": "generic", "link": ""}
            ],
            "webViewLink": "https://tasks.google.com/task/dGFzazE"
        }))
        .expect("maps");
        assert_eq!(mapped.title, "Send the invoice");
        assert_eq!(mapped.status, TaskStatus::Completed);
        assert_eq!(mapped.due, NaiveDate::from_ymd_opt(2026, 11, 2));
        assert_eq!(
            mapped.completed,
            Some(utc(2026, 10, 28, 9, 12) + Duration::seconds(30))
        );
        assert_eq!(mapped.parent.as_deref(), Some("cGFyZW50"));
        assert!(mapped.hidden && !mapped.deleted && !mapped.assigned);
        assert_eq!(mapped.links.len(), 1, "links without a URL are dropped");
        assert_eq!(mapped.links[0].kind, "email");
        assert_eq!(
            mapped.links[0].description.as_deref(),
            Some("Invoice for October")
        );

        let bare = task(
            json!({"id": "b", "deleted": true, "assignmentInfo": {"surfaceType": "DOCUMENT"}}),
        )
        .expect("maps");
        assert_eq!(bare.title, "");
        assert_eq!(bare.status, TaskStatus::NeedsAction);
        assert!(bare.deleted && bare.assigned);
        assert_eq!(bare.due, None);
        assert!(task(json!({"title": "no id"})).is_none());
    }

    #[test]
    fn due_dates_are_the_written_day() {
        assert_eq!(
            parse_due(Some("2026-11-02T00:00:00.000Z")),
            NaiveDate::from_ymd_opt(2026, 11, 2)
        );
        // Even with an offset, the day is the one written.
        assert_eq!(
            parse_due(Some("2026-11-02T00:00:00+03:00")),
            NaiveDate::from_ymd_opt(2026, 11, 2)
        );
        assert_eq!(parse_due(Some("tomorrow")), None);
        assert_eq!(
            due_value(NaiveDate::from_ymd_opt(2026, 1, 5).expect("date")),
            "2026-01-05T00:00:00.000Z"
        );
    }

    #[test]
    fn new_tasks_carry_status_and_due() {
        let open = NewTask {
            title: "Call Maya".into(),
            notes: Some("  ".into()),
            due: NaiveDate::from_ymd_opt(2026, 10, 30),
            completed: None,
        };
        assert_eq!(
            open.to_json(),
            json!({"title": "Call Maya", "due": "2026-10-30T00:00:00.000Z", "status": "needsAction"})
        );
        let done = NewTask {
            title: "Paid".into(),
            completed: Some(utc(2026, 10, 29, 8, 0)),
            ..NewTask::default()
        };
        assert_eq!(
            done.to_json(),
            json!({"title": "Paid", "completed": "2026-10-29T08:00:00.000Z", "status": "completed"})
        );
    }

    #[test]
    fn patches_send_nulls_to_clear() {
        let patch = TaskPatch::new()
            .title("Renamed")
            .notes(None)
            .due(None)
            .completed(None);
        assert_eq!(
            patch.to_json(),
            json!({"title": "Renamed", "notes": null, "due": null, "status": "needsAction", "completed": null})
        );
        let complete = TaskPatch::new()
            .due(NaiveDate::from_ymd_opt(2026, 12, 24))
            .completed(Some(utc(2026, 12, 23, 18, 30)));
        assert_eq!(
            complete.to_json(),
            json!({"due": "2026-12-24T00:00:00.000Z", "status": "completed", "completed": "2026-12-23T18:30:00.000Z"})
        );
        assert!(TaskPatch::new().is_empty());
        assert!(!TaskPatch::new().raw("deleted", json!(true)).is_empty());
    }

    #[test]
    fn cursors_round_trip_through_json() {
        let next = TaskCursor::Next {
            updated_min: None,
            page_token: "p2".into(),
            started: utc(2026, 10, 28, 10, 0),
        };
        let text = serde_json::to_string(&next).expect("serializes");
        assert_eq!(
            text,
            r#"{"state":"next","updatedMin":null,"pageToken":"p2","started":"2026-10-28T10:00:00Z"}"#
        );
        assert_eq!(
            serde_json::from_str::<TaskCursor>(&text).expect("parses"),
            next
        );
        let done = TaskCursor::Done {
            since: utc(2026, 10, 28, 9, 59),
        };
        assert!(done.is_done() && !next.is_done());
        assert_eq!(
            serde_json::to_string(&done).expect("serializes"),
            r#"{"state":"done","since":"2026-10-28T09:59:00Z"}"#
        );
    }

    #[test]
    fn queries() {
        let full = TaskQuery::full();
        assert!(full.show_completed && full.show_hidden && !full.show_deleted);
        assert_eq!(full.updated_min, None);
        let since = utc(2026, 10, 1, 0, 0);
        let changed = TaskQuery::changed_since(since);
        assert!(changed.show_deleted && changed.show_hidden && changed.show_completed);
        assert_eq!(changed.updated_min, Some(since));
        assert_eq!(TaskQuery::default(), full);
    }
}
