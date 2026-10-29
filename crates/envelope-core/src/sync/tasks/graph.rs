//! Microsoft To Do through Microsoft Graph: the account's lists (all but
//! "Flagged email", whose tasks are flagged messages Envelope already shows
//! in mail), the tasks of each by their delta, and their steps
//! (`checklistItems`) as subtasks.
//!
//! A step has a title and whether it is done, nothing more, so subtasks of
//! these lists keep only that. A step's id is only unique within its task,
//! so a subtask's remote id is the task id and the step id joined by
//! [`SEPARATOR`]; that also finds the steps of a task the server removed.
//! Delta pages do not carry steps: they are fetched (batched) for the tasks
//! a pull reports, and once a day the list is pulled from scratch, which
//! also brings in steps changed without their task changing.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use envelope_graph::{
    ChecklistItem, ChecklistItemPatch, DeltaCursor, GraphClient, GraphError, Importance,
    NewLinkedResource, NewTodoTask, TaskStatus, TodoTask, TodoTaskPatch,
};
use serde::{Deserialize, Serialize};

use super::{Done, Failure, Pulled, TaskService};
use crate::calendar::local_tz;
use crate::db::mail::AccountRow;
use crate::db::now_ms;
use crate::db::tasks::{
    self as tdb, ListRow, ListSyncState, ListUpsert, POSITION_STEP, RemoteIndex, RemoteTask,
    TaskRow,
};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::sync::calendar::graph_client;
use crate::tasks::TaskListKind;

/// Joins a task id and a step id into a subtask's remote id; neither kind
/// of id contains it.
pub(crate) const SEPARATOR: char = '|';
/// Tasks per delta page.
const DELTA_PAGE: u32 = 100;
/// How often a list is pulled from scratch.
const FULL_EVERY_MS: i64 = 24 * 60 * 60_000;

/// Why an account's tasks are left alone.
const UNAVAILABLE: &str = "Microsoft To Do isn't available for this account: Microsoft refused \
     access to its tasks, or its mailbox can't be reached through Microsoft Graph. Signing in \
     again asks for access.";

/// Pull bookkeeping of a list, kept in [`ListSyncState::cursor`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Cursor {
    /// Where the next delta round starts.
    delta: Option<DeltaCursor>,
    /// When the list was last pulled from scratch.
    full_at: Option<i64>,
}

/// The server copy of a step, kept in its row's `data`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    task_id: String,
    checklist_item: ChecklistItem,
}

/// The To Do service of a Microsoft account.
pub(crate) struct Graph {
    engine: Engine,
    client: GraphClient,
}

impl Graph {
    /// The service for a Microsoft account, signing in with its OAuth grant.
    pub(crate) fn connect(engine: &Engine, account: &AccountRow) -> Result<Graph> {
        Ok(Graph::new(engine, graph_client(engine, account.id)?))
    }

    pub(crate) fn new(engine: &Engine, client: GraphClient) -> Graph {
        Graph {
            engine: engine.clone(),
            client,
        }
    }

    /// The To Do task a subtask belongs to, which the parent has once it is
    /// on the server (parents are uploaded first).
    fn parent_task(&self, row: &TaskRow) -> std::result::Result<String, Failure> {
        let parent = row
            .parent_id
            .ok_or_else(|| Failure::Rejected("a step needs its task".into()))?;
        let parent = self.engine.db.read(|c| tdb::get_task(c, parent))?;
        parent
            .remote_id
            .filter(|id| !id.contains(SEPARATOR))
            .ok_or_else(|| {
                Failure::Rejected("the task it belongs to is not on the server yet".into())
            })
    }

    /// Uploads a subtask as a step of its task.
    async fn push_step(
        &self,
        list: &ListRow,
        row: &TaskRow,
        base: Option<Step>,
    ) -> std::result::Result<Done, Failure> {
        let task_id = self.parent_task(row)?;
        let done = row.completed_at.is_some();
        // Already a step of this task: changed in place.
        let current = row
            .remote_id
            .as_deref()
            .and_then(|id| id.split_once(SEPARATOR))
            .filter(|(task, _)| *task == task_id)
            .map(|(_, item)| item.to_owned());
        if let Some(item_id) = current {
            let before = base
                .as_ref()
                .filter(|b| b.task_id == task_id)
                .map(|b| &b.checklist_item);
            let patch = ChecklistItemPatch {
                display_name: before
                    .is_none_or(|i| i.display_name != row.title)
                    .then(|| row.title.clone()),
                is_checked: before.is_none_or(|i| i.is_checked != done).then_some(done),
            };
            if let Some(item) = before.filter(|_| patch == ChecklistItemPatch::default()) {
                return Ok(saved_step(&task_id, item.clone()));
            }
            match self
                .client
                .update_checklist_item(&list.remote_id, &task_id, &item_id, &patch)
                .await
            {
                Ok(item) => return Ok(saved_step(&task_id, item)),
                // Deleted there while edited here: the edit wins.
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Pulls do not always bring the steps of a task: a step whose create
        // got no answer is looked for among its task's.
        if let Some(item) = self.landed_step(list, row, &task_id).await? {
            return Ok(saved_step(&task_id, item));
        }
        let item = self
            .client
            .create_checklist_item(&list.remote_id, &task_id, &row.title, done)
            .await?;
        // It was something else on the server before (a task, or a step of
        // another task): that goes.
        self.retire_old(list, row, &task_id, &item.id).await;
        Ok(saved_step(&task_id, item))
    }

    /// The step of task `task_id` that a create of `row` made though its
    /// answer got lost, if there is one: the same title, made since, and not
    /// stored here as another step.
    async fn landed_step(
        &self,
        list: &ListRow,
        row: &TaskRow,
        task_id: &str,
    ) -> std::result::Result<Option<ChecklistItem>, Failure> {
        let Some(sent) = row.sent_at.filter(|_| row.remote_id.is_none()) else {
            return Ok(None);
        };
        let items = self
            .client
            .todo_checklist_items(&list.remote_id, task_id)
            .await?;
        let list_id = list.id;
        let known = self.engine.db.read(|c| tdb::remote_index(c, list_id))?;
        Ok(items.into_iter().find(|item| {
            item.display_name == row.title
                && !known.contains_key(&format!("{task_id}{SEPARATOR}{}", item.id))
                && item
                    .created_date_time
                    .is_none_or(|at| ms(at) >= sent - tdb::UNANSWERED_SLACK_MS)
        }))
    }

    /// Uploads a top-level task.
    async fn push_task(
        &self,
        list: &ListRow,
        row: &TaskRow,
        base: Option<TodoTask>,
    ) -> std::result::Result<Done, Failure> {
        // Already a task on the server: changed in place (every property when
        // its copy is missing).
        if let Some(id) = row.remote_id.as_ref().filter(|id| !id.contains(SEPARATOR)) {
            let before = base.as_ref().map(remote_task);
            let patch = patch_for(row, before.as_ref());
            match self
                .client
                .update_todo_task(&list.remote_id, id, &patch)
                .await
            {
                Ok(task) => return Ok(saved_task(task)),
                // Deleted there while edited here: the edit wins.
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        let linked_resources = row
            .source
            .as_ref()
            .filter(|s| !s.message_id.is_empty())
            .map(|s| NewLinkedResource::message(&s.subject, &s.message_id, None))
            .into_iter()
            .collect();
        let task = NewTodoTask {
            title: row.title.clone(),
            notes: Some(row.notes.clone()).filter(|n| !n.is_empty()),
            importance: importance(row.important),
            due_date: parse_day(row.due_date.as_deref()),
            completed: row.completed_at.map(instant),
            linked_resources,
        };
        let created = self
            .client
            .create_todo_task(&list.remote_id, &task, local_tz())
            .await?;
        self.retire_old(list, row, &created.id, "").await;
        Ok(saved_task(created))
    }

    /// Removes what a row was on the server before it became something else
    /// there (a step turned task, a task turned step, a step moved to
    /// another task). The new copy is saved already, so a failure here only
    /// leaves the old one behind, which the next pull brings back.
    async fn retire_old(&self, list: &ListRow, row: &TaskRow, new_task: &str, new_item: &str) {
        let Some(old) = row.remote_id.as_deref() else {
            return;
        };
        let result = match old.split_once(SEPARATOR) {
            Some((task, item)) if task != new_task || item != new_item => {
                self.client
                    .delete_checklist_item(&list.remote_id, task, item)
                    .await
            }
            None if old != new_task => self.client.delete_todo_task(&list.remote_id, old).await,
            _ => Ok(()),
        };
        if let Err(e) = result.or_else(|e| if e.is_not_found() { Ok(()) } else { Err(e) }) {
            tracing::warn!(
                task = row.id,
                "the old copy of a moved task stays on the server: {e}"
            );
        }
    }
}

#[async_trait::async_trait]
impl TaskService for Graph {
    fn kind(&self) -> TaskListKind {
        TaskListKind::Graph
    }

    async fn lists(&self) -> Result<Vec<ListUpsert>> {
        let lists = self.client.todo_lists().await.map_err(|e| {
            if unavailable(&e) {
                Error::Invalid(UNAVAILABLE.into())
            } else {
                err(e)
            }
        })?;
        Ok(lists
            .into_iter()
            .filter(|l| !l.is_flagged_emails())
            .enumerate()
            .map(|(i, l)| ListUpsert {
                is_default: l.is_default(),
                remote_id: l.id,
                name: Some(l.display_name)
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "Tasks".into()),
                color: None,
                read_only: false,
                sort_order: i as i64,
            })
            .collect())
    }

    async fn pull(&self, list: &ListRow, local: &RemoteIndex) -> Result<Pulled> {
        let stored: Cursor = list
            .sync_state
            .cursor
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let now = now_ms();
        let stale = stored.full_at.is_none_or(|at| now - at >= FULL_EVERY_MS);
        let mut cursor = stored.delta.clone().filter(|_| !stale);
        let mut full = cursor.is_none();
        let mut restarted = false;
        let mut tasks: Vec<TodoTask> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        loop {
            match self
                .client
                .todo_tasks_delta(&list.remote_id, cursor.as_ref(), Some(DELTA_PAGE))
                .await
            {
                Ok(page) => {
                    tasks.extend(page.items);
                    removed.extend(page.removed_ids);
                    let done = page.next.is_done();
                    cursor = Some(page.next);
                    if done {
                        break;
                    }
                }
                // The delta link expired: the list is pulled from scratch.
                Err(e) if e.is_resync_required() && !restarted => {
                    restarted = true;
                    full = true;
                    cursor = None;
                    tasks.clear();
                    removed.clear();
                }
                Err(e) => return Err(err(e)),
            }
        }

        // Stored steps by task, to find the ones that went.
        let mut stored_steps: HashMap<&str, Vec<&str>> = HashMap::new();
        for remote_id in local.keys() {
            if let Some((task, _)) = remote_id.split_once(SEPARATOR) {
                stored_steps.entry(task).or_default().push(remote_id);
            }
        }
        let pairs: Vec<(&str, &str)> = tasks
            .iter()
            .map(|t| (list.remote_id.as_str(), t.id.as_str()))
            .collect();
        let steps = self.client.todo_checklists(&pairs).await.map_err(err)?;

        let mut pulled = Pulled::default();
        let mut listed: HashSet<String> = HashSet::new();
        let unchanged = |task: &RemoteTask| {
            local
                .get(&task.remote_id)
                .is_some_and(|(etag, state)| *state == tdb::SYNCED && *etag == task.etag)
        };
        for (task, steps) in tasks.iter().zip(steps) {
            let items = match steps {
                Ok(items) => items,
                // Deleted since the delta was read; the next delta says so.
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(err(e)),
            };
            let remote = remote_task(task);
            listed.insert(remote.remote_id.clone());
            if !unchanged(&remote) {
                pulled.changed.push(remote);
            }
            let mut present = HashSet::new();
            for (index, item) in items.into_iter().enumerate() {
                let position = (index as i64 + 1) * POSITION_STEP;
                let step = remote_step(&task.id, item, Some(position));
                present.insert(step.remote_id.clone());
                listed.insert(step.remote_id.clone());
                if !unchanged(&step) {
                    pulled.changed.push(step);
                }
            }
            for gone in stored_steps.get(task.id.as_str()).into_iter().flatten() {
                if !present.contains(*gone) {
                    pulled.removed.push((*gone).to_owned());
                }
            }
        }
        for task in removed {
            for step in stored_steps.get(task.as_str()).into_iter().flatten() {
                pulled.removed.push((*step).to_owned());
            }
            pulled.removed.push(task);
        }
        pulled.listed = full.then_some(listed);
        let next = Cursor {
            delta: cursor,
            full_at: if full { Some(now) } else { stored.full_at },
        };
        pulled.state = ListSyncState {
            cursor: serde_json::to_value(next).ok(),
            ..ListSyncState::default()
        };
        Ok(pulled)
    }

    async fn push(&self, list: &ListRow, row: &TaskRow) -> std::result::Result<Done, Failure> {
        if row.local_state == tdb::DELETED {
            let Some(id) = &row.remote_id else {
                // A step made though its create got no answer goes too.
                if let Ok(task_id) = self.parent_task(row)
                    && let Some(item) = self.landed_step(list, row, &task_id).await?
                {
                    match self
                        .client
                        .delete_checklist_item(&list.remote_id, &task_id, &item.id)
                        .await
                    {
                        Ok(()) => {}
                        Err(e) if e.is_not_found() => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                return Ok(Done::Deleted);
            };
            let result = match id.split_once(SEPARATOR) {
                Some((task, item)) => {
                    self.client
                        .delete_checklist_item(&list.remote_id, task, item)
                        .await
                }
                None => self.client.delete_todo_task(&list.remote_id, id).await,
            };
            return match result {
                Ok(()) => Ok(Done::Deleted),
                Err(e) if e.is_not_found() => Ok(Done::Deleted),
                Err(e) => Err(e.into()),
            };
        }
        let data = row.data.as_deref().unwrap_or_default();
        let step = serde_json::from_str::<Step>(data).ok();
        let task = match step {
            Some(_) => None,
            None => serde_json::from_str::<TodoTask>(data).ok(),
        };
        if row.parent_id.is_some() {
            self.push_step(list, row, step).await
        } else {
            self.push_task(list, row, task).await
        }
    }

    fn server_copy(&self, task: &TaskRow) -> Option<RemoteTask> {
        let data = task.data.as_deref()?;
        if let Ok(step) = serde_json::from_str::<Step>(data) {
            return Some(remote_step(&step.task_id, step.checklist_item, None));
        }
        let copy = serde_json::from_str::<TodoTask>(data).ok()?;
        Some(remote_task(&copy))
    }

    async fn create_list(&self, name: &str) -> Result<ListUpsert> {
        let created = self.client.create_todo_list(name).await.map_err(err)?;
        Ok(ListUpsert {
            remote_id: created.id,
            name: Some(created.display_name)
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| name.to_owned()),
            color: None,
            read_only: false,
            is_default: false,
            sort_order: i64::from(i32::MAX),
        })
    }

    async fn rename_list(&self, list: &ListRow, name: &str) -> Result<()> {
        if list.is_default {
            return Err(Error::Invalid(
                "Microsoft To Do doesn't let its Tasks list be renamed.".into(),
            ));
        }
        self.client
            .rename_todo_list(&list.remote_id, name)
            .await
            .map(drop)
            .map_err(err)
    }

    async fn delete_list(&self, list: &ListRow) -> Result<()> {
        match self.client.delete_todo_list(&list.remote_id).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_not_found() => Ok(()),
            Err(e) => Err(err(e)),
        }
    }
}

/// To Do refused for the whole account: the grant has no access to tasks
/// (`403`), or the mailbox is one Graph can't reach (on-premises Exchange).
/// The scheduler passes such accounts over quietly.
fn unavailable(e: &GraphError) -> bool {
    e.is_forbidden()
        || e.code().is_some_and(|code| {
            [
                "MailboxNotEnabledForRESTAPI",
                "MailboxNotSupportedForRESTAPI",
            ]
            .iter()
            .any(|c| code.eq_ignore_ascii_case(c))
        })
}

fn err(e: GraphError) -> Error {
    match Failure::from(e) {
        Failure::Transient(m) | Failure::Unanswered(m) => Error::Network(m),
        Failure::Auth(m) => Error::Auth(m),
        Failure::Forbidden(m) | Failure::Rejected(m) => Error::Other(m),
    }
}

fn ms(at: DateTime<Utc>) -> i64 {
    at.timestamp_millis()
}

fn instant(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

fn parse_day(day: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(day?, "%Y-%m-%d").ok()
}

fn importance(important: bool) -> Importance {
    if important {
        Importance::High
    } else {
        Importance::Normal
    }
}

/// What Envelope stores of a To Do task.
fn remote_task(task: &TodoTask) -> RemoteTask {
    let completed = task.status == TaskStatus::Completed;
    RemoteTask {
        remote_id: task.id.clone(),
        etag: task.etag.clone(),
        uid: task.id.clone(),
        title: task.title.clone(),
        notes: task.notes().trim().to_owned(),
        due_date: task
            .due_date(local_tz())
            .map(|d| d.format("%Y-%m-%d").to_string()),
        due_at: None,
        completed_at: completed.then(|| task.completed_at().map_or_else(now_ms, ms)),
        important: task.importance == Importance::High,
        parent_uid: None,
        position: None,
        repeats: task.repeats(),
        created_at: task.created_date_time.map(ms),
        updated_at: task.last_modified_date_time.map(ms),
        data: serde_json::to_string(task).ok(),
    }
}

/// What Envelope stores of a step: a subtask of its task, with a stand-in
/// entity tag (steps have none) that changes with what Envelope shows.
fn remote_step(task_id: &str, item: ChecklistItem, position: Option<i64>) -> RemoteTask {
    let remote_id = format!("{task_id}{SEPARATOR}{}", item.id);
    let etag = format!(
        "{}:{}:{}",
        position.unwrap_or_default(),
        u8::from(item.is_checked),
        item.display_name
    );
    let step = Step {
        task_id: task_id.to_owned(),
        checklist_item: item,
    };
    let item = &step.checklist_item;
    RemoteTask {
        uid: remote_id.clone(),
        remote_id,
        etag: Some(etag),
        title: item.display_name.clone(),
        notes: String::new(),
        due_date: None,
        due_at: None,
        completed_at: item
            .is_checked
            .then(|| item.checked_date_time.map_or_else(now_ms, ms)),
        important: false,
        parent_uid: Some(task_id.to_owned()),
        position,
        repeats: false,
        created_at: item.created_date_time.map(ms),
        updated_at: item.checked_date_time.or(item.created_date_time).map(ms),
        data: serde_json::to_string(&step).ok(),
    }
}

fn saved_task(task: TodoTask) -> Done {
    Done::Saved(Box::new(remote_task(&task)))
}

/// A step as uploaded; it keeps its place here (`position: None`).
fn saved_step(task_id: &str, item: ChecklistItem) -> Done {
    Done::Saved(Box::new(remote_step(task_id, item, None)))
}

/// The properties of `row` that differ from `base`, the copy the changes
/// made here started from (`None`: all of them).
fn patch_for(row: &TaskRow, base: Option<&RemoteTask>) -> TodoTaskPatch {
    let changed = |same: &dyn Fn(&RemoteTask) -> bool| base.is_none_or(|b| !same(b));
    let mut patch = TodoTaskPatch::new();
    if changed(&|b| b.title == row.title) {
        patch = patch.title(row.title.clone());
    }
    if changed(&|b| b.notes == row.notes) {
        patch = patch.notes(&row.notes);
    }
    if changed(&|b| b.important == row.important) {
        patch = patch.importance(importance(row.important));
    }
    if changed(&|b| b.due_date == row.due_date) {
        patch = patch.due_date(parse_day(row.due_date.as_deref()), local_tz());
    }
    if changed(&|b| b.completed_at.is_some() == row.completed_at.is_some()) {
        patch = patch.completed(row.completed_at.map(instant));
    }
    patch
}
