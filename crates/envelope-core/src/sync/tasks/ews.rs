//! Exchange task folders over EWS: the Tasks folder and the task folders
//! below it, their changes by `SyncFolderItems` (the first round lists every
//! task, which also clears out tasks no longer there) and the fields by
//! `GetItem`, and writes of the fields Envelope edits. Exchange keeps due
//! days in the user's time zone, which the requests name; tasks have no
//! subtasks, and Exchange works out the next occurrence of a repeating task
//! when it is completed.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use envelope_ews::{
    DeleteType, DistinguishedFolder, EwsClient, FOLDER_CLASS_TASKS, FolderId, FolderRole,
    Importance, ItemId, MAX_SYNC_CHANGES, NewTask, Task, TaskUpdate,
};
use serde::{Deserialize, Serialize};

use super::{Done, Failure, Pulled, TaskService};
use crate::calendar::local_tz;
use crate::db::mail::AccountRow;
use crate::db::now_ms;
use crate::db::tasks::{
    self as tdb, ListRow, ListSyncState, ListUpsert, RemoteIndex, RemoteTask, TaskRow,
};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::sync::ews::{EwsMail, ews_error};
use crate::tasks::TaskListKind;

/// Pull bookkeeping of a folder, kept in [`ListSyncState::cursor`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Cursor {
    /// `SyncFolderItems` state after the last complete pull.
    sync_state: Option<String>,
}

/// The task service of an Exchange account.
pub(crate) struct Ews {
    client: EwsClient,
}

impl Ews {
    /// The service for an Exchange account, signing in with its password.
    pub(crate) async fn connect(engine: &Engine, account: &AccountRow) -> Result<Ews> {
        let mail = EwsMail::for_account(engine, account).await?;
        Ok(Ews::new(mail.client().clone()))
    }

    pub(crate) fn new(client: EwsClient) -> Ews {
        Ews { client }
    }

    /// The task as the server has it now, after a write. The write is done,
    /// so a failure to read it back must not undo it: the task is then
    /// stored as written, without a server copy.
    async fn saved(&self, id: ItemId, row: &TaskRow) -> Done {
        match self.client.get_task(&id, local_tz()).await {
            Ok(task) => Done::Saved(Box::new(remote_task(&task))),
            Err(e) => {
                tracing::debug!(task = row.id, "could not read a saved task back: {e}");
                Done::Saved(Box::new(RemoteTask {
                    remote_id: id.id.clone(),
                    etag: id.change_key.clone(),
                    uid: id.id,
                    title: row.title.clone(),
                    notes: row.notes.clone(),
                    due_date: row.due_date.clone(),
                    due_at: None,
                    completed_at: row.completed_at,
                    important: row.important,
                    parent_uid: None,
                    position: None,
                    repeats: row.repeats,
                    created_at: Some(row.created_at),
                    updated_at: Some(now_ms()),
                    data: None,
                }))
            }
        }
    }

    async fn create(&self, list: &ListRow, row: &TaskRow) -> std::result::Result<Done, Failure> {
        let task = NewTask {
            subject: row.title.clone(),
            body: Some(row.notes.clone()).filter(|n| !n.is_empty()),
            importance: row.important.then_some(Importance::High),
            start_date: None,
            due_date: parse_day(row.due_date.as_deref()),
            completed: row.completed_at.map(instant),
        };
        let id = self
            .client
            .create_task(FolderId::new(&list.remote_id), &task, local_tz())
            .await?;
        Ok(self.saved(id, row).await)
    }
}

#[async_trait::async_trait]
impl TaskService for Ews {
    fn kind(&self) -> TaskListKind {
        TaskListKind::Ews
    }

    async fn lists(&self) -> Result<Vec<ListUpsert>> {
        let folders = self.client.task_folders().await.map_err(ews_error)?;
        Ok(folders
            .into_iter()
            .enumerate()
            .map(|(i, folder)| ListUpsert {
                is_default: folder.role == Some(FolderRole::Tasks),
                remote_id: folder.id.id,
                name: Some(folder.display_name)
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
        let folder = FolderId::new(&list.remote_id);
        let mut state = stored.sync_state;
        let mut full = state.is_none();
        let mut restarted = false;
        // The latest version of each changed task, by id.
        let mut changed: HashMap<String, ItemId> = HashMap::new();
        let mut removed: Vec<String> = Vec::new();
        loop {
            match self
                .client
                .sync_tasks(folder.clone(), state.as_deref(), MAX_SYNC_CHANGES)
                .await
            {
                Ok(page) => {
                    for id in page.created.into_iter().chain(page.updated) {
                        removed.retain(|r| *r != id.id);
                        changed.insert(id.id.clone(), id);
                    }
                    for id in page.deleted {
                        changed.remove(&id.id);
                        removed.push(id.id);
                    }
                    state = Some(page.sync_state);
                    if page.includes_last_item_in_range {
                        break;
                    }
                }
                // The state expired: the folder is listed from scratch.
                Err(e) if e.requires_resync() && !restarted => {
                    restarted = true;
                    full = true;
                    state = None;
                    changed.clear();
                    removed.clear();
                }
                Err(e) => return Err(ews_error(e)),
            }
        }
        let mut listed: HashSet<String> = changed.keys().cloned().collect();
        // Fetched unless stored already in this version; tasks changed here
        // are reconciled when they are uploaded.
        let wanted: Vec<ItemId> = changed
            .into_values()
            .filter(|id| match local.get(&id.id) {
                None => true,
                Some((_, state)) if *state != tdb::SYNCED => false,
                Some((etag, _)) => etag.is_none() || *etag != id.change_key,
            })
            .collect();
        let mut pulled = Pulled {
            removed,
            ..Pulled::default()
        };
        let fetched = self
            .client
            .get_tasks(&wanted, local_tz())
            .await
            .map_err(ews_error)?;
        for (id, result) in wanted.iter().zip(fetched) {
            match result {
                Ok(task) => pulled.changed.push(remote_task(&task)),
                Err(e) if e.code.is_not_found() => {
                    listed.remove(&id.id);
                    pulled.removed.push(id.id.clone());
                }
                Err(e) => tracing::warn!("task {} could not be read: {e}", id.id),
            }
        }
        pulled.listed = full.then_some(listed);
        pulled.state = ListSyncState {
            cursor: serde_json::to_value(Cursor { sync_state: state }).ok(),
            ..ListSyncState::default()
        };
        Ok(pulled)
    }

    async fn push(&self, list: &ListRow, row: &TaskRow) -> std::result::Result<Done, Failure> {
        let Some(remote_id) = row.remote_id.clone() else {
            if row.local_state == tdb::DELETED {
                return Ok(Done::Deleted);
            }
            return self.create(list, row).await;
        };
        let id = ItemId::new(remote_id);
        if row.local_state == tdb::DELETED {
            return match self
                .client
                .delete_task(&id, DeleteType::MoveToDeletedItems)
                .await
            {
                Ok(()) => Ok(Done::Deleted),
                Err(e) if e.is_not_found() => Ok(Done::Deleted),
                Err(e) => Err(e.into()),
            };
        }
        let base = row
            .data
            .as_deref()
            .and_then(|d| serde_json::from_str::<Task>(d).ok());
        let update = update_for(row, base.as_ref().map(remote_task).as_ref());
        match self.client.update_task(&id, &update, local_tz()).await {
            Ok(saved) => Ok(self.saved(saved, row).await),
            // Deleted there while edited here: the edit wins.
            Err(e) if e.is_not_found() => self.create(list, row).await,
            Err(e) => Err(e.into()),
        }
    }

    async fn create_list(&self, name: &str) -> Result<ListUpsert> {
        let id = self
            .client
            .create_folder(DistinguishedFolder::Tasks, name, FOLDER_CLASS_TASKS)
            .await
            .map_err(ews_error)?;
        Ok(ListUpsert {
            remote_id: id.id,
            name: name.to_owned(),
            color: None,
            read_only: false,
            is_default: false,
            sort_order: i64::from(i32::MAX),
        })
    }

    async fn rename_list(&self, list: &ListRow, name: &str) -> Result<()> {
        if list.is_default {
            return Err(Error::Invalid(
                "Exchange doesn't let the Tasks folder be renamed.".into(),
            ));
        }
        self.client
            .rename_folder(&FolderId::new(&list.remote_id), name)
            .await
            .map(drop)
            .map_err(ews_error)
    }

    async fn delete_list(&self, list: &ListRow) -> Result<()> {
        match self
            .client
            .delete_folder(
                FolderId::new(&list.remote_id),
                DeleteType::MoveToDeletedItems,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if e.is_not_found() => Ok(()),
            Err(e) => Err(ews_error(e)),
        }
    }
}

fn instant(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

fn parse_day(day: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(day?, "%Y-%m-%d").ok()
}

/// What Envelope stores of an Exchange task.
fn remote_task(task: &Task) -> RemoteTask {
    RemoteTask {
        remote_id: task.id.id.clone(),
        etag: task.id.change_key.clone(),
        uid: task.id.id.clone(),
        title: task.subject.clone().unwrap_or_default(),
        notes: task.body.as_deref().unwrap_or("").trim().to_owned(),
        due_date: task.due_date.map(|d| d.format("%Y-%m-%d").to_string()),
        due_at: None,
        completed_at: task.is_completed().then(|| {
            task.completed_at()
                .map_or_else(now_ms, |at| at.timestamp_millis())
        }),
        important: task.importance == Some(Importance::High),
        parent_uid: None,
        position: None,
        repeats: task.is_recurring.unwrap_or(false),
        created_at: task.created.map(|at| at.timestamp_millis()),
        updated_at: task.last_modified.map(|at| at.timestamp_millis()),
        data: serde_json::to_string(task).ok(),
    }
}

/// The fields of `row` that differ from `base`, the copy the changes made
/// here started from (`None`: all of them).
fn update_for(row: &TaskRow, base: Option<&RemoteTask>) -> TaskUpdate {
    let changed = |same: &dyn Fn(&RemoteTask) -> bool| base.is_none_or(|b| !same(b));
    TaskUpdate {
        subject: changed(&|b| b.title == row.title).then(|| row.title.clone()),
        body: changed(&|b| b.notes == row.notes).then(|| row.notes.clone()),
        importance: changed(&|b| b.important == row.important).then_some(if row.important {
            Importance::High
        } else {
            Importance::Normal
        }),
        start_date: None,
        due_date: changed(&|b| b.due_date == row.due_date)
            .then(|| parse_day(row.due_date.as_deref())),
        completed: changed(&|b| b.completed_at.is_some() == row.completed_at.is_some())
            .then(|| row.completed_at.map(instant)),
    }
}
