//! Microsoft To Do: task lists, tasks with per-list delta sync, steps
//! (`checklistItems`) and linked resources.
//!
//! To Do keeps only the day of a due date, and Graph reports it as the
//! instant that day began where it was set: midnight UTC for days written
//! through Graph in UTC, but local midnight for days set in the To Do and
//! Outlook apps (`2023-04-14T22:00:00` UTC for 15 April set in UTC+2). So
//! [`TodoTask::due_date`] needs the user's time zone to name the day.
//! Envelope writes due dates as midnight UTC of the day, which the apps
//! show as that day in every time zone. Completion times keep only their
//! day too (midnight UTC); [`TodoTask::completed_at`] recovers the time
//! from the last change where it can.
//!
//! Delta pages do not carry a task's steps or linked resources: fetch them
//! with [`GraphClient::todo_checklists`] (batched) and
//! [`GraphClient::todo_linked_resources`].

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use crate::batch::BatchRequest;
use crate::client::{Collection, GraphClient, GraphRequest};
use crate::delta::{DeltaCursor, DeltaPage};
use crate::error::GraphError;
use crate::html::html_to_text;
use crate::serde_util::{graph_enum, null_default};
use crate::types::{BodyType, DateTimeTimeZone, Importance, ItemBody, is_utc_zone, parse_local};

/// `applicationName` of the linked resources Envelope creates.
pub const ENVELOPE_APPLICATION: &str = "Envelope";

/// Asks for task bodies as plain text.
const PREFER_TEXT_BODY: &str = "outlook.body-content-type=\"text\"";

graph_enum! {
    /// Which built-in list a task list is.
    pub enum WellknownListName {
        /// A list the user created.
        None = "none",
        /// The built-in "Tasks" list, where new tasks go by default.
        DefaultList = "defaultList",
        /// The built-in "Flagged email" list: flagged messages, as tasks.
        FlaggedEmails = "flaggedEmails",
        /// A value Graph added after this client was written.
        UnknownFutureValue = "unknownFutureValue",
    }
    unknown = UnknownFutureValue;
    default = None;
}

graph_enum! {
    /// Progress of a task.
    pub enum TaskStatus {
        /// Not started.
        NotStarted = "notStarted",
        /// In progress.
        InProgress = "inProgress",
        /// Done.
        Completed = "completed",
        /// Waiting on others.
        WaitingOnOthers = "waitingOnOthers",
        /// Deferred.
        Deferred = "deferred",
    }
    unknown = NotStarted;
    default = NotStarted;
}

/// A To Do task list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoTaskList {
    /// List id.
    pub id: String,
    /// The list's name.
    #[serde(default, deserialize_with = "null_default")]
    pub display_name: String,
    /// Whether the user owns the list.
    #[serde(default, deserialize_with = "null_default")]
    pub is_owner: bool,
    /// Whether the list is shared with other users.
    #[serde(default, deserialize_with = "null_default")]
    pub is_shared: bool,
    /// Which built-in list this is, if any.
    #[serde(default, deserialize_with = "null_default")]
    pub wellknown_list_name: WellknownListName,
}

impl TodoTaskList {
    /// True for the built-in "Tasks" list. It cannot be renamed or deleted.
    pub fn is_default(&self) -> bool {
        self.wellknown_list_name == WellknownListName::DefaultList
    }

    /// True for the built-in "Flagged email" list, whose tasks are the
    /// user's flagged messages. It cannot be renamed or deleted.
    pub fn is_flagged_emails(&self) -> bool {
        self.wellknown_list_name == WellknownListName::FlaggedEmails
    }
}

/// A step of a task (`checklistItem`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistItem {
    /// Step id.
    pub id: String,
    /// Title.
    #[serde(default, deserialize_with = "null_default")]
    pub display_name: String,
    /// Whether the step is done.
    #[serde(default, deserialize_with = "null_default")]
    pub is_checked: bool,
    /// When the step was checked off.
    #[serde(default, deserialize_with = "lenient_instant")]
    pub checked_date_time: Option<DateTime<Utc>>,
    /// When the step was created.
    #[serde(default, deserialize_with = "lenient_instant")]
    pub created_date_time: Option<DateTime<Utc>>,
}

/// Changes to a step; unset fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistItemPatch {
    /// New title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Checks the step off, or opens it again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_checked: Option<bool>,
}

/// A link from a task to an item elsewhere, such as the message the task
/// was made from (`linkedResource`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedResource {
    /// Link id.
    pub id: String,
    /// Deep link to the item, when it has one.
    #[serde(default)]
    pub web_url: Option<String>,
    /// Name of the application the item belongs to.
    #[serde(default)]
    pub application_name: Option<String>,
    /// Title of the item.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Id of the item in its application.
    #[serde(default)]
    pub external_id: Option<String>,
}

/// A linked resource to create.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLinkedResource {
    /// Deep link to the item; optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// Name of the application the item belongs to.
    pub application_name: String,
    /// Title of the item.
    pub display_name: String,
    /// Id of the item in its application.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

impl NewLinkedResource {
    /// A link to the e-mail message a task was made from: `subject` as the
    /// title, the `Message-ID` (angle brackets removed) as the external id
    /// and [`ENVELOPE_APPLICATION`] as the application.
    pub fn message(subject: &str, message_id: &str, web_url: Option<&str>) -> Self {
        let message_id = message_id.trim();
        let message_id = message_id
            .strip_prefix('<')
            .and_then(|id| id.strip_suffix('>'))
            .unwrap_or(message_id);
        NewLinkedResource {
            web_url: web_url.map(str::to_owned),
            application_name: ENVELOPE_APPLICATION.to_owned(),
            display_name: subject.to_owned(),
            external_id: Some(message_id.to_owned()),
        }
    }
}

/// A To Do task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoTask {
    /// Task id. Graph documents that it changes, by default, when the task
    /// moves to another list.
    pub id: String,
    /// ETag of this version of the task.
    #[serde(
        rename = "@odata.etag",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub etag: Option<String>,
    /// Title.
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    /// The notes; [`TodoTask::notes`] has them as plain text.
    #[serde(default, deserialize_with = "lenient")]
    pub body: Option<ItemBody>,
    /// Importance (`high` is To Do's "important").
    #[serde(default, deserialize_with = "null_default")]
    pub importance: Importance,
    /// Progress.
    #[serde(default, deserialize_with = "null_default")]
    pub status: TaskStatus,
    /// When the task is due, as Graph reports it; [`TodoTask::due_date`]
    /// names the day.
    #[serde(default, deserialize_with = "lenient")]
    pub due_date_time: Option<DateTimeTimeZone>,
    /// When work on the task is to start (a day, like the due date).
    #[serde(default, deserialize_with = "lenient")]
    pub start_date_time: Option<DateTimeTimeZone>,
    /// When the task was completed: midnight UTC of the day, the time of
    /// day is lost (see [`TodoTask::completed_at`]).
    #[serde(default, deserialize_with = "lenient")]
    pub completed_date_time: Option<DateTimeTimeZone>,
    /// When the reminder goes off.
    #[serde(default, deserialize_with = "lenient")]
    pub reminder_date_time: Option<DateTimeTimeZone>,
    /// Whether a reminder is set.
    #[serde(default, deserialize_with = "null_default")]
    pub is_reminder_on: bool,
    /// How the task repeats (a `patternedRecurrence`), as Graph sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Value>,
    /// Outlook categories.
    #[serde(default, deserialize_with = "null_default")]
    pub categories: Vec<String>,
    /// Whether files are attached.
    #[serde(default, deserialize_with = "null_default")]
    pub has_attachments: bool,
    /// When the task was created.
    #[serde(default, deserialize_with = "lenient_instant")]
    pub created_date_time: Option<DateTime<Utc>>,
    /// When the task last changed.
    #[serde(default, deserialize_with = "lenient_instant")]
    pub last_modified_date_time: Option<DateTime<Utc>>,
    /// The steps, when the response included them; delta pages do not.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub checklist_items: Option<Vec<ChecklistItem>>,
    /// The linked resources, when the response included them (a task just
    /// created with some does); delta pages do not.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub linked_resources: Option<Vec<LinkedResource>>,
}

impl TodoTask {
    /// The notes as plain text, with `\n` line breaks.
    pub fn notes(&self) -> String {
        let Some(body) = &self.body else {
            return String::new();
        };
        let text = match body.content_type {
            BodyType::Html => html_to_text(&body.content),
            BodyType::Text => body.content.clone(),
        };
        text.replace("\r\n", "\n")
    }

    /// The day the task is due, for a user in time zone `local`.
    ///
    /// A value at midnight is that day. Any other value is the start of the
    /// day where it was set (the To Do apps set due dates at local
    /// midnight), so it is moved to `local` and rounded to the nearest
    /// midnight there.
    pub fn due_date(&self, local: Tz) -> Option<NaiveDate> {
        self.due_date_time
            .as_ref()
            .and_then(|due| task_day(due, local))
    }

    /// True when the task repeats.
    pub fn repeats(&self) -> bool {
        self.recurrence.as_ref().is_some_and(|r| !r.is_null())
    }

    /// When the task was completed, as well as To Do tells: the last change
    /// when it falls on the completion day (completing was most likely
    /// that change), else midnight UTC of that day. `None` for open tasks.
    pub fn completed_at(&self) -> Option<DateTime<Utc>> {
        if self.status != TaskStatus::Completed {
            return None;
        }
        let day = self.completed_date_time.as_ref().and_then(instant);
        match (day, self.last_modified_date_time) {
            (Some(day), Some(modified))
                if modified >= day && modified - day < Duration::days(1) =>
            {
                Some(modified)
            }
            (Some(day), _) => Some(day),
            (None, modified) => modified,
        }
    }
}

/// A task to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTodoTask {
    /// Title.
    pub title: String,
    /// Notes, as plain text.
    pub notes: Option<String>,
    /// Importance (`high` marks the task important).
    pub importance: Importance,
    /// The day the task is due.
    pub due_date: Option<NaiveDate>,
    /// Create the task already completed, at this instant.
    pub completed: Option<DateTime<Utc>>,
    /// Links to where the task came from, created with the task.
    pub linked_resources: Vec<NewLinkedResource>,
}

impl NewTodoTask {
    fn to_json(&self) -> Result<Value, GraphError> {
        let mut body = Map::new();
        body.insert("title".into(), Value::String(self.title.clone()));
        if let Some(notes) = self.notes.as_deref().filter(|n| !n.is_empty()) {
            body.insert("body".into(), text_body(notes));
        }
        body.insert("importance".into(), json!(self.importance));
        if let Some(due) = self.due_date {
            body.insert("dueDateTime".into(), due_value(due));
        }
        match self.completed {
            Some(at) => {
                body.insert("status".into(), json!(TaskStatus::Completed));
                body.insert("completedDateTime".into(), json!(DateTimeTimeZone::utc(at)));
            }
            None => {
                body.insert("status".into(), json!(TaskStatus::NotStarted));
            }
        }
        if !self.linked_resources.is_empty() {
            let links = serde_json::to_value(&self.linked_resources)
                .map_err(|e| GraphError::InvalidInput(format!("linked resources: {e}")))?;
            body.insert("linkedResources".into(), links);
        }
        Ok(Value::Object(body))
    }
}

/// A partial update of a task: only the fields that were set are sent, and
/// clearing a date sends an explicit `null`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TodoTaskPatch {
    fields: Map<String, Value>,
}

impl TodoTaskPatch {
    /// An empty patch.
    pub fn new() -> Self {
        TodoTaskPatch::default()
    }

    /// Sets the title.
    pub fn title(self, title: impl Into<String>) -> Self {
        self.set("title", Value::String(title.into()))
    }

    /// Replaces the notes with plain text (empty clears them).
    pub fn notes(self, notes: &str) -> Self {
        self.set("body", text_body(notes))
    }

    /// Sets the importance.
    pub fn importance(self, importance: Importance) -> Self {
        self.set("importance", json!(importance))
    }

    /// Sets the day the task is due; `None` removes the due date.
    pub fn due_date(self, due: Option<NaiveDate>) -> Self {
        self.set("dueDateTime", due.map_or(Value::Null, due_value))
    }

    /// Sets the progress.
    pub fn status(self, status: TaskStatus) -> Self {
        self.set("status", json!(status))
    }

    /// Completes the task at the given instant, or reopens it (as not
    /// started) with `None`, which also clears the completion date.
    pub fn completed(self, completed: Option<DateTime<Utc>>) -> Self {
        match completed {
            Some(at) => self
                .status(TaskStatus::Completed)
                .set("completedDateTime", json!(DateTimeTimeZone::utc(at))),
            None => self
                .status(TaskStatus::NotStarted)
                .set("completedDateTime", Value::Null),
        }
    }

    /// Sets any other property of the `todoTask` verbatim.
    pub fn raw(self, property: impl Into<String>, value: Value) -> Self {
        let mut patch = self;
        patch.fields.insert(property.into(), value);
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

    fn set(mut self, property: &str, value: Value) -> Self {
        self.fields.insert(property.to_owned(), value);
        self
    }
}

fn text_body(text: &str) -> Value {
    json!({ "content": text, "contentType": BodyType::Text })
}

/// A due date as Envelope writes it: midnight UTC of the day.
fn due_value(day: NaiveDate) -> Value {
    json!(DateTimeTimeZone::new(day.and_time(NaiveTime::MIN), "UTC"))
}

/// The zone a `dateTimeTimeZone` is expressed in (UTC when unnamed or
/// unknown).
fn zone_of(value: &DateTimeTimeZone) -> Tz {
    match value.time_zone.as_deref() {
        Some(name) if !is_utc_zone(name) => envelope_ical::resolve_tzid(name).unwrap_or(Tz::UTC),
        _ => Tz::UTC,
    }
}

/// The instant of a `dateTimeTimeZone`.
fn instant(value: &DateTimeTimeZone) -> Option<DateTime<Utc>> {
    let wall = value.local()?;
    let zone = zone_of(value);
    let at = zone
        .from_local_datetime(&wall)
        .earliest()
        .map(|at| at.with_timezone(&Utc));
    Some(at.unwrap_or_else(|| wall.and_utc()))
}

/// The calendar day of a date-only value: midnight where it was set.
fn task_day(value: &DateTimeTimeZone, local: Tz) -> Option<NaiveDate> {
    let wall = value.local()?;
    if wall.time() == NaiveTime::MIN {
        return Some(wall.date());
    }
    let there = instant(value)?.with_timezone(&local).naive_local();
    let noon = NaiveTime::from_hms_opt(12, 0, 0)?;
    if there.time() >= noon {
        there.date().succ_opt()
    } else {
        Some(there.date())
    }
}

/// Deserializes an optional value, treating one of the wrong shape as
/// absent rather than failing the whole task.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

/// Accepts RFC 3339 timestamps and ones without an offset (taken as UTC).
fn lenient_instant<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.as_ref().and_then(Value::as_str).and_then(|raw| {
        DateTime::parse_from_rfc3339(raw.trim())
            .map(|at| at.with_timezone(&Utc))
            .ok()
            .or_else(|| parse_local(raw).map(|local| local.and_utc()))
    }))
}

fn list_path(list_id: &str) -> Vec<String> {
    vec!["me".into(), "todo".into(), "lists".into(), list_id.into()]
}

fn task_path(list_id: &str, task_id: &str) -> Vec<String> {
    let mut path = list_path(list_id);
    path.extend(["tasks".into(), task_id.into()]);
    path
}

fn checklist_path(list_id: &str, task_id: &str) -> Vec<String> {
    let mut path = task_path(list_id, task_id);
    path.push("checklistItems".into());
    path
}

fn linked_resources_path(list_id: &str, task_id: &str) -> Vec<String> {
    let mut path = task_path(list_id, task_id);
    path.push("linkedResources".into());
    path
}

impl GraphClient {
    /// The user's To Do task lists, the built-in ones included.
    pub async fn todo_lists(&self) -> Result<Vec<TodoTaskList>, GraphError> {
        self.get_all(&GraphRequest::get(["me", "todo", "lists"]), None)
            .await
    }

    /// Creates a task list.
    pub async fn create_todo_list(&self, display_name: &str) -> Result<TodoTaskList, GraphError> {
        let request = GraphRequest::post(["me", "todo", "lists"])
            .json(&json!({ "displayName": display_name }))?;
        self.send_json(&request).await
    }

    /// Renames a task list (the built-in lists cannot be renamed).
    pub async fn rename_todo_list(
        &self,
        list_id: &str,
        display_name: &str,
    ) -> Result<TodoTaskList, GraphError> {
        let request = GraphRequest::patch(list_path(list_id))
            .json(&json!({ "displayName": display_name }))?;
        self.send_json(&request).await
    }

    /// Deletes a task list with its tasks (the built-in lists cannot be
    /// deleted).
    pub async fn delete_todo_list(&self, list_id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::delete(list_path(list_id)))
            .await
    }

    /// One page of the task delta of a list.
    ///
    /// Start a round with `cursor = None`; continue with the returned
    /// [`DeltaCursor`] exactly as for [`GraphClient::message_delta`]. Removed
    /// ids are deleted tasks and tasks moved to another list. Steps and
    /// linked resources are not included. When the stored link has expired
    /// the error satisfies [`GraphError::is_resync_required`]; restart with
    /// `None` and reconcile.
    pub async fn todo_tasks_delta(
        &self,
        list_id: &str,
        cursor: Option<&DeltaCursor>,
        page_size: Option<u32>,
    ) -> Result<DeltaPage<TodoTask>, GraphError> {
        let mut request = match cursor {
            Some(cursor) => GraphRequest::link(Method::GET, cursor.url()),
            None => {
                let mut path = list_path(list_id);
                path.extend(["tasks".into(), "delta".into()]);
                GraphRequest::get(path)
            }
        }
        .prefer(PREFER_TEXT_BODY);
        if let Some(size) = page_size {
            request = request.prefer(format!("odata.maxpagesize={size}"));
        }
        self.delta_page_raw(&request).await
    }

    /// One task.
    pub async fn get_todo_task(
        &self,
        list_id: &str,
        task_id: &str,
    ) -> Result<TodoTask, GraphError> {
        let request = GraphRequest::get(task_path(list_id, task_id)).prefer(PREFER_TEXT_BODY);
        self.send_json(&request).await
    }

    /// Creates a task, with its linked resources, and returns it as stored.
    pub async fn create_todo_task(
        &self,
        list_id: &str,
        task: &NewTodoTask,
    ) -> Result<TodoTask, GraphError> {
        let mut path = list_path(list_id);
        path.push("tasks".into());
        let request = GraphRequest::post(path)
            .prefer(PREFER_TEXT_BODY)
            .json(&task.to_json()?)?;
        self.send_json(&request).await
    }

    /// Changes the properties set in `patch` and returns the task as stored.
    /// An empty patch only reads the task.
    pub async fn update_todo_task(
        &self,
        list_id: &str,
        task_id: &str,
        patch: &TodoTaskPatch,
    ) -> Result<TodoTask, GraphError> {
        if patch.is_empty() {
            return self.get_todo_task(list_id, task_id).await;
        }
        let request = GraphRequest::patch(task_path(list_id, task_id))
            .prefer(PREFER_TEXT_BODY)
            .json(&patch.to_json())?;
        self.send_json(&request).await
    }

    /// Deletes a task with its steps.
    pub async fn delete_todo_task(&self, list_id: &str, task_id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::delete(task_path(list_id, task_id)))
            .await
    }

    /// The steps of a task.
    pub async fn todo_checklist_items(
        &self,
        list_id: &str,
        task_id: &str,
    ) -> Result<Vec<ChecklistItem>, GraphError> {
        self.get_all(&GraphRequest::get(checklist_path(list_id, task_id)), None)
            .await
    }

    /// The steps of several tasks (`(list id, task id)` pairs), fetched in
    /// JSON batches. One result per task, in input order; a task that is
    /// gone fails on its own. `Err` is returned only for invalid input.
    pub async fn todo_checklists(
        &self,
        tasks: &[(&str, &str)],
    ) -> Result<Vec<Result<Vec<ChecklistItem>, GraphError>>, GraphError> {
        let requests = tasks
            .iter()
            .enumerate()
            .map(|(i, (list_id, task_id))| {
                BatchRequest::new(
                    i.to_string(),
                    GraphRequest::get(checklist_path(list_id, task_id)),
                )
            })
            .collect();
        let responses = self.batch(requests).await?;
        let mut results = Vec::with_capacity(responses.len());
        for response in responses {
            let page = response.json::<Collection<ChecklistItem>>();
            results.push(match page {
                Ok(Collection {
                    value,
                    next_link: Some(link),
                    ..
                }) => {
                    // More steps than one page holds: fetch the rest directly.
                    let rest = self
                        .get_all::<ChecklistItem>(&GraphRequest::link(Method::GET, link), None)
                        .await;
                    rest.map(|rest| value.into_iter().chain(rest).collect())
                }
                Ok(page) => Ok(page.value),
                Err(err) => Err(err),
            });
        }
        Ok(results)
    }

    /// Adds a step to a task.
    pub async fn create_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        display_name: &str,
        checked: bool,
    ) -> Result<ChecklistItem, GraphError> {
        let request = GraphRequest::post(checklist_path(list_id, task_id))
            .json(&json!({ "displayName": display_name, "isChecked": checked }))?;
        self.send_json(&request).await
    }

    /// Changes a step.
    pub async fn update_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        item_id: &str,
        patch: &ChecklistItemPatch,
    ) -> Result<ChecklistItem, GraphError> {
        let mut path = checklist_path(list_id, task_id);
        path.push(item_id.into());
        let request = GraphRequest::patch(path).json(patch)?;
        self.send_json(&request).await
    }

    /// Removes a step.
    pub async fn delete_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        item_id: &str,
    ) -> Result<(), GraphError> {
        let mut path = checklist_path(list_id, task_id);
        path.push(item_id.into());
        self.send_empty(&GraphRequest::delete(path)).await
    }

    /// The linked resources of a task.
    pub async fn todo_linked_resources(
        &self,
        list_id: &str,
        task_id: &str,
    ) -> Result<Vec<LinkedResource>, GraphError> {
        self.get_all(
            &GraphRequest::get(linked_resources_path(list_id, task_id)),
            None,
        )
        .await
    }

    /// Links a task to an item elsewhere.
    pub async fn create_linked_resource(
        &self,
        list_id: &str,
        task_id: &str,
        resource: &NewLinkedResource,
    ) -> Result<LinkedResource, GraphError> {
        let request = GraphRequest::post(linked_resources_path(list_id, task_id)).json(resource)?;
        self.send_json(&request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(value: Value) -> TodoTask {
        serde_json::from_value(value).unwrap_or_else(|e| panic!("{e}"))
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap_or_default()
    }

    fn due(date_time: &str, time_zone: &str) -> TodoTask {
        task(json!({"id": "t", "dueDateTime": {"dateTime": date_time, "timeZone": time_zone}}))
    }

    #[test]
    fn due_days_are_midnight_where_they_were_set() {
        let (istanbul, new_york, auckland) = (
            Tz::Europe__Istanbul,
            Tz::America__New_York,
            Tz::Pacific__Auckland,
        );
        // Written through Graph in UTC: the day, wherever the user is.
        let utc = due("2026-11-06T00:00:00.0000000", "UTC");
        for zone in [istanbul, new_york, auckland, Tz::UTC] {
            assert_eq!(utc.due_date(zone), Some(date(2026, 11, 6)), "{zone}");
        }
        // Set in the To Do apps: local midnight.
        assert_eq!(
            due("2026-11-05T21:00:00.0000000", "UTC").due_date(istanbul),
            Some(date(2026, 11, 6))
        );
        assert_eq!(
            due("2026-11-06T05:00:00.0000000", "UTC").due_date(new_york),
            Some(date(2026, 11, 6))
        );
        // New Zealand summer time is UTC+13.
        assert_eq!(
            due("2026-11-05T11:00:00.0000000", "UTC").due_date(auckland),
            Some(date(2026, 11, 6))
        );
        // Reported in another zone (Prefer: outlook.timezone).
        assert_eq!(
            due("2026-11-06T00:00:00.0000000", "Turkey Standard Time").due_date(new_york),
            Some(date(2026, 11, 6))
        );
        assert_eq!(task(json!({"id": "t"})).due_date(istanbul), None);
        assert_eq!(due("someday", "UTC").due_date(istanbul), None);
    }

    #[test]
    fn completion_times_come_from_the_last_change() {
        let completed = task(json!({
            "id": "t",
            "status": "completed",
            "completedDateTime": {"dateTime": "2026-10-28T00:00:00.0000000", "timeZone": "UTC"},
            "lastModifiedDateTime": "2026-10-28T17:45:12.3456789Z"
        }));
        assert_eq!(
            completed.completed_at().map(|t| t.to_rfc3339()),
            Some("2026-10-28T17:45:12.345678900+00:00".to_owned())
        );
        let edited_later = task(json!({
            "id": "t",
            "status": "completed",
            "completedDateTime": {"dateTime": "2026-10-28T00:00:00.0000000", "timeZone": "UTC"},
            "lastModifiedDateTime": "2026-10-30T09:00:00Z"
        }));
        assert_eq!(
            edited_later.completed_at().map(|t| t.to_rfc3339()),
            Some("2026-10-28T00:00:00+00:00".to_owned())
        );
        let open = task(json!({
            "id": "t",
            "status": "notStarted",
            "completedDateTime": {"dateTime": "2026-10-28T00:00:00", "timeZone": "UTC"}
        }));
        assert_eq!(open.completed_at(), None);
    }

    #[test]
    fn tasks_parse_leniently_and_round_trip() {
        let parsed = task(json!({
            "@odata.etag": "W/\"xzyPKP0BiUGgld+lMKXwbQAAgdhkVw==\"",
            "id": "AAMkADAwATM0MDAAMS0yMDkyLWVjMzYtMDACLTAwCgBGAAAD",
            "title": "Send the invoice",
            "importance": "high",
            "status": "inProgress",
            "isReminderOn": null,
            "categories": null,
            "body": {"content": "Line one\r\nLine two", "contentType": "text"},
            "recurrence": {"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["friday"]},
                           "range": {"type": "noEnd", "startDate": "2026-10-30"}},
            "startDateTime": {"dateTime": null},
            "createdDateTime": "2026-10-20T08:00:00.1234567Z",
            "lastModifiedDateTime": "2026-10-21T08:00:00",
            "checklistItems": "not a list"
        }));
        assert_eq!(
            parsed.etag.as_deref(),
            Some("W/\"xzyPKP0BiUGgld+lMKXwbQAAgdhkVw==\"")
        );
        assert_eq!(parsed.importance, Importance::High);
        assert_eq!(parsed.status, TaskStatus::InProgress);
        assert_eq!(parsed.notes(), "Line one\nLine two");
        assert!(parsed.repeats());
        assert_eq!(parsed.start_date_time, None, "malformed values are dropped");
        assert_eq!(parsed.checklist_items, None);
        assert_eq!(
            parsed.last_modified_date_time.map(|t| t.to_rfc3339()),
            Some("2026-10-21T08:00:00+00:00".to_owned())
        );
        let stored = serde_json::to_string(&parsed).unwrap_or_default();
        let back: TodoTask = serde_json::from_str(&stored).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, parsed);

        let html = task(
            json!({"id": "h", "body": {"content": "<p>Hello<br>world</p>", "contentType": "html"}}),
        );
        assert_eq!(html.notes(), "Hello\nworld");
        assert!(!html.repeats());
    }

    #[test]
    fn writes_carry_dates_in_utc_and_clear_with_null() {
        let new = NewTodoTask {
            title: "Call Maya".into(),
            notes: Some("About the venue".into()),
            importance: Importance::High,
            due_date: Some(date(2026, 10, 30)),
            completed: None,
            linked_resources: vec![NewLinkedResource::message(
                "Venue for Saturday",
                "<CAF=abc@mail.example.com>",
                None,
            )],
        };
        assert_eq!(
            new.to_json().unwrap_or_default(),
            json!({
                "title": "Call Maya",
                "body": {"content": "About the venue", "contentType": "text"},
                "importance": "high",
                "dueDateTime": {"dateTime": "2026-10-30T00:00:00", "timeZone": "UTC"},
                "status": "notStarted",
                "linkedResources": [{
                    "applicationName": "Envelope",
                    "displayName": "Venue for Saturday",
                    "externalId": "CAF=abc@mail.example.com"
                }]
            })
        );
        let reopen = TodoTaskPatch::new().due_date(None).completed(None);
        assert_eq!(
            reopen.to_json(),
            json!({"dueDateTime": null, "status": "notStarted", "completedDateTime": null})
        );
        let at = Utc
            .with_ymd_and_hms(2026, 10, 29, 8, 30, 0)
            .single()
            .unwrap_or_default();
        let complete = TodoTaskPatch::new().notes("").completed(Some(at));
        assert_eq!(
            complete.to_json(),
            json!({
                "body": {"content": "", "contentType": "text"},
                "status": "completed",
                "completedDateTime": {"dateTime": "2026-10-29T08:30:00", "timeZone": "UTC"}
            })
        );
        assert!(TodoTaskPatch::new().is_empty());
        assert_eq!(
            serde_json::to_value(ChecklistItemPatch {
                is_checked: Some(true),
                ..ChecklistItemPatch::default()
            })
            .unwrap_or_default(),
            json!({"isChecked": true})
        );
    }

    #[test]
    fn lists_know_the_built_in_ones() {
        let lists: Vec<TodoTaskList> = serde_json::from_value(json!([
            {"id": "a", "displayName": "Tasks", "isOwner": true, "isShared": false, "wellknownListName": "defaultList"},
            {"id": "b", "displayName": "Flagged email", "wellknownListName": "flaggedEmails"},
            {"id": "c", "displayName": "Groceries", "wellknownListName": "none"},
            {"id": "d", "displayName": null, "wellknownListName": "somethingNew"}
        ]))
        .unwrap_or_default();
        assert!(lists[0].is_default() && lists[0].is_owner);
        assert!(lists[1].is_flagged_emails() && !lists[1].is_default());
        assert_eq!(lists[2].wellknown_list_name, WellknownListName::None);
        assert_eq!(
            lists[3].wellknown_list_name,
            WellknownListName::UnknownFutureValue
        );
        assert_eq!(lists[3].display_name, "");
    }
}
