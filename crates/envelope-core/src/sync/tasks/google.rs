//! Google Tasks: the account's task lists, pulled in full the first time
//! and then by what changed since the last pull (`updatedMin`, deletions
//! included), and written back field by field. Google keeps due days only,
//! no importance, and subtasks one level deep (`parent`); the order among
//! siblings is Google's `position`, changed by moving a task after the one
//! it follows here (`previous`).

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, Utc};
use envelope_google::{
    GoogleClient, GoogleTask, NewTask, TaskCursor, TaskPatch, TaskPosition, TaskStatus,
};

use super::{Done, Failure, Pulled, TaskService};
use crate::db::mail::AccountRow;
use crate::db::now_ms;
use crate::db::tasks::{
    self as tdb, ListRow, ListSyncState, ListUpsert, RemoteIndex, RemoteTask, TaskRow,
};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::sync::calendar::google_client;
use crate::tasks::TaskListKind;

/// Why an account's tasks are left alone.
const UNAVAILABLE: &str = "Google Tasks isn't available for this account: the account was \
     signed in without access to tasks, or the Google Tasks API is not enabled for Envelope's \
     Google client. Signing in again asks for access.";

/// The Google Tasks service of an account.
pub(crate) struct Google {
    engine: Engine,
    client: GoogleClient,
}

impl Google {
    /// The service for a Google account, signing in with its OAuth grant.
    pub(crate) fn connect(engine: &Engine, account: &AccountRow) -> Result<Google> {
        Ok(Google::new(engine, google_client(engine, account.id)?))
    }

    pub(crate) fn new(engine: &Engine, client: GoogleClient) -> Google {
        Google {
            engine: engine.clone(),
            client,
        }
    }

    /// The server id of the task a subtask belongs to, which the parent
    /// has once it is on the server (parents are uploaded first).
    fn parent_id(&self, row: &TaskRow) -> std::result::Result<Option<String>, Failure> {
        let Some(parent) = row.parent_id else {
            return Ok(None);
        };
        let parent = self.engine.db.read(|c| tdb::get_task(c, parent))?;
        match parent.remote_id {
            Some(id) => Ok(Some(id)),
            None => Err(Failure::Rejected(
                "the task it belongs to is not on the server yet".into(),
            )),
        }
    }

    /// Where `row` goes on the server: under its parent, after the nearest
    /// task before it here that Google can place others after (one that is
    /// on the server and not hidden).
    fn position(
        &self,
        list: &ListRow,
        row: &TaskRow,
    ) -> std::result::Result<TaskPosition, Failure> {
        let parent = self.parent_id(row)?;
        let previous = self.engine.db.read(|c| {
            let siblings = tdb::siblings(c, list.id, row.parent_id)?;
            let before = match siblings.iter().position(|(id, _)| *id == row.id) {
                Some(index) => &siblings[..index],
                None => &siblings[..],
            };
            for (id, _) in before.iter().rev() {
                let sibling = tdb::get_task(c, *id)?;
                let hidden = sibling
                    .data
                    .as_deref()
                    .and_then(|d| serde_json::from_str::<GoogleTask>(d).ok())
                    .is_some_and(|t| t.hidden);
                if let Some(remote) = sibling.remote_id.filter(|_| !hidden) {
                    return Ok(Some(remote));
                }
            }
            Ok(None)
        })?;
        Ok(TaskPosition { parent, previous })
    }

    /// Creates `row` on the server, after the task before it here.
    async fn insert(&self, list: &ListRow, row: &TaskRow) -> std::result::Result<Done, Failure> {
        let task = NewTask {
            title: row.title.clone(),
            notes: Some(row.notes.clone()).filter(|n| !n.is_empty()),
            due: parse_day(row.due_date.as_deref()),
            completed: row.completed_at.map(instant),
        };
        let position = self.position(list, row)?;
        let saved = self
            .client
            .insert_task(&list.remote_id, &task, &position)
            .await
            .map_err(failure)?;
        Ok(placed_task(saved))
    }
}

#[async_trait::async_trait]
impl TaskService for Google {
    fn kind(&self) -> TaskListKind {
        TaskListKind::Google
    }

    async fn lists(&self) -> Result<Vec<ListUpsert>> {
        let lists = self.client.list_task_lists().await.map_err(err)?;
        let default = self.client.default_task_list().await.map_err(err)?.id;
        Ok(lists
            .into_iter()
            .enumerate()
            .map(|(i, list)| ListUpsert {
                is_default: list.id == default,
                remote_id: list.id,
                name: Some(list.title)
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| "Tasks".into()),
                color: None,
                read_only: false,
                sort_order: i as i64,
            })
            .collect())
    }

    async fn pull(&self, list: &ListRow, local: &RemoteIndex) -> Result<Pulled> {
        let stored: Option<TaskCursor> = list
            .sync_state
            .cursor
            .clone()
            .and_then(|v| serde_json::from_value(v).ok());
        // Every page of one run, so that what is stored is never half a run.
        let mut cursor = stored;
        let mut tasks: Vec<GoogleTask> = Vec::new();
        let full = loop {
            match self
                .client
                .task_changes(&list.remote_id, cursor.as_ref())
                .await
            {
                Ok(page) => {
                    tasks.extend(page.tasks);
                    let done = page.next.is_done();
                    cursor = Some(page.next);
                    if done {
                        break page.full;
                    }
                }
                // Too long ago to list the changes: list everything again.
                Err(envelope_google::Error::SyncTokenExpired) if cursor.is_some() => {
                    cursor = None;
                    tasks.clear();
                }
                Err(e) => return Err(err(e)),
            }
        };
        let mut pulled = Pulled {
            state: ListSyncState {
                cursor: cursor.and_then(|c| serde_json::to_value(c).ok()),
                ..ListSyncState::default()
            },
            ..Pulled::default()
        };
        let mut listed = HashSet::new();
        for task in tasks {
            if task.deleted {
                pulled.removed.push(task.id);
                continue;
            }
            listed.insert(task.id.clone());
            let unchanged = local
                .get(&task.id)
                .is_some_and(|(etag, state)| *state == tdb::SYNCED && *etag == task.etag);
            if !unchanged {
                pulled.changed.push(remote_task(&task));
            }
        }
        pulled.listed = full.then_some(listed);
        Ok(pulled)
    }

    async fn push(&self, list: &ListRow, row: &TaskRow) -> std::result::Result<Done, Failure> {
        let Some(id) = row.remote_id.clone() else {
            if row.local_state == tdb::DELETED {
                return Ok(Done::Deleted);
            }
            return self.insert(list, row).await;
        };
        if row.local_state == tdb::DELETED {
            return match self.client.delete_task(&list.remote_id, &id).await {
                Ok(()) => Ok(Done::Deleted),
                Err(e) if e.is_not_found() => Ok(Done::Deleted),
                Err(e) => Err(failure(e)),
            };
        }
        // The copy the changes made here started from (every field, when
        // there is none).
        let base = row
            .data
            .as_deref()
            .and_then(|d| serde_json::from_str::<GoogleTask>(d).ok());
        let before = base.as_ref().map(remote_task);
        let patch = patch_for(row, before.as_ref());
        let position = self.position(list, row)?;
        let moved = before
            .as_ref()
            .is_none_or(|b| b.parent_uid != position.parent || b.position != Some(row.position));
        let mut saved = if patch.is_empty() {
            None
        } else {
            match self.client.patch_task(&list.remote_id, &id, &patch).await {
                Ok(task) => Some(task),
                // Deleted there while edited here: the edit wins.
                Err(e) if e.is_not_found() || e.is_gone() => {
                    return self.insert(list, row).await;
                }
                Err(e) => return Err(failure(e)),
            }
        };
        if moved {
            match self
                .client
                .move_task(&list.remote_id, &id, &position, None)
                .await
            {
                Ok(task) => saved = Some(task),
                Err(e) if saved.is_none() && (e.is_not_found() || e.is_gone()) => {
                    return self.insert(list, row).await;
                }
                Err(e) => return Err(failure(e)),
            }
        }
        let saved = match saved {
            Some(task) => task,
            None => self
                .client
                .get_task(&list.remote_id, &id)
                .await
                .map_err(failure)?,
        };
        Ok(if moved {
            placed_task(saved)
        } else {
            saved_task(saved)
        })
    }

    fn server_copy(&self, task: &TaskRow) -> Option<RemoteTask> {
        let copy = serde_json::from_str::<GoogleTask>(task.data.as_deref()?).ok()?;
        Some(remote_task(&copy))
    }

    async fn create_list(&self, name: &str) -> Result<ListUpsert> {
        let created = self.client.insert_task_list(name).await.map_err(err)?;
        Ok(ListUpsert {
            remote_id: created.id,
            name: Some(created.title)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| name.to_owned()),
            color: None,
            read_only: false,
            is_default: false,
            sort_order: i64::from(i32::MAX),
        })
    }

    async fn rename_list(&self, list: &ListRow, name: &str) -> Result<()> {
        self.client
            .rename_task_list(&list.remote_id, name)
            .await
            .map(drop)
            .map_err(err)
    }

    async fn delete_list(&self, list: &ListRow) -> Result<()> {
        match self.client.delete_task_list(&list.remote_id).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_not_found() => Ok(()),
            Err(e) => Err(err(e)),
        }
    }
}

/// A Google error as the engine reports it. Tasks that are not available
/// for the account (no access in its grant, or the API turned off for
/// Envelope's client) are `Invalid`, which the scheduler passes over
/// quietly.
fn err(e: envelope_google::Error) -> Error {
    if e.is_insufficient_scope() || e.is_api_disabled() {
        return Error::Invalid(UNAVAILABLE.into());
    }
    match Failure::from(e) {
        Failure::Transient(m) => Error::Network(m),
        Failure::Auth(m) => Error::Auth(m),
        Failure::Forbidden(m) | Failure::Rejected(m) => Error::Other(m),
    }
}

/// Why an upload failed. When tasks are not available for the account the
/// change waits (it is kept and tried again later) instead of being undone.
fn failure(e: envelope_google::Error) -> Failure {
    if e.is_insufficient_scope() || e.is_api_disabled() {
        return Failure::Transient(UNAVAILABLE.into());
    }
    Failure::from(e)
}

fn instant(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

fn parse_day(day: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(day?, "%Y-%m-%d").ok()
}

/// Google's `position`, a zero-padded number that sorts as text, as a
/// number that sorts the same way.
fn position_value(position: &str) -> Option<i64> {
    let digits = position.trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(
        digits
            .parse::<u128>()
            .ok()
            .and_then(|n| i64::try_from(n).ok())
            .unwrap_or(i64::MAX),
    )
}

/// What Envelope stores of a Google task.
fn remote_task(task: &GoogleTask) -> RemoteTask {
    RemoteTask {
        remote_id: task.id.clone(),
        etag: task.etag.clone(),
        uid: task.id.clone(),
        title: task.title.clone(),
        notes: task
            .notes
            .as_deref()
            .unwrap_or("")
            .replace("\r\n", "\n")
            .trim()
            .to_owned(),
        due_date: task.due.map(|d| d.format("%Y-%m-%d").to_string()),
        due_at: None,
        completed_at: (task.status == TaskStatus::Completed).then(|| {
            task.completed
                .or(task.updated)
                .map_or_else(now_ms, |at| at.timestamp_millis())
        }),
        important: false,
        parent_uid: task.parent.clone(),
        position: task.position.as_deref().and_then(position_value),
        repeats: false,
        created_at: None,
        updated_at: task.updated.map(|at| at.timestamp_millis()),
        data: serde_json::to_string(task).ok(),
    }
}

fn saved_task(task: GoogleTask) -> Done {
    Done::Saved(Box::new(remote_task(&task)))
}

/// A task created or moved on the server. Google's position for it is not
/// comparable with the positions given here to tasks still waiting to be
/// uploaded (which are placed after the task before them here), so it
/// keeps its place here; leaving out the entity tag has the next pull,
/// which lists it as changed, bring Google's position.
fn placed_task(task: GoogleTask) -> Done {
    let mut saved = remote_task(&task);
    saved.position = None;
    saved.etag = None;
    Done::Saved(Box::new(saved))
}

/// The fields of `row` that differ from `base`, the copy the changes made
/// here started from (`None`: all of them).
fn patch_for(row: &TaskRow, base: Option<&RemoteTask>) -> TaskPatch {
    let changed = |same: &dyn Fn(&RemoteTask) -> bool| base.is_none_or(|b| !same(b));
    let mut patch = TaskPatch::new();
    if changed(&|b| b.title == row.title) {
        patch = patch.title(row.title.clone());
    }
    if changed(&|b| b.notes == row.notes) {
        patch = patch.notes(Some(&row.notes));
    }
    if changed(&|b| b.due_date == row.due_date) {
        patch = patch.due(parse_day(row.due_date.as_deref()));
    }
    if changed(&|b| b.completed_at.is_some() == row.completed_at.is_some()) {
        patch = patch.completed(row.completed_at.map(instant));
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_sort_like_googles() {
        assert_eq!(position_value("00000000000000000000"), Some(0));
        assert_eq!(position_value("00000000002147483647"), Some(2_147_483_647));
        assert!(
            position_value("00000000000000000002") < position_value("00000000000000000010"),
            "as numbers, the same order as the text"
        );
        assert_eq!(position_value("99999999999999999999"), Some(i64::MAX));
        assert_eq!(position_value(""), None);
        assert_eq!(position_value("12ab"), None);
    }
}
