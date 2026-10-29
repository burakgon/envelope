//! Task synchronization, shaped like the contacts sync: for every account
//! with task lists, local changes are uploaded first (conditionally where
//! the server allows it, with retries, backoff and `push_error`), then the
//! lists and the changes in them are pulled, every ten minutes and whenever
//! a local change or a new account wakes the scheduler. A second loop
//! announces tasks due at a time of day.
//!
//! # Services
//!
//! Each kind of server is a [`TaskService`] for one account, returned by
//! [`service`]. A service only talks to its server: it lists the lists,
//! pulls what changed in one of them (keeping its own bookkeeping, such as
//! a delta link or the time of the last pull, in the list's
//! [`ListSyncState::cursor`]), uploads one changed task and creates, renames
//! or deletes lists. Storing, retries, giving up on changes the server keeps
//! refusing and linking subtasks to their parents happen here.
//!
//! Tasks travel as [`RemoteTask`]s: the fields Envelope shows, plus `data`,
//! the server's own representation (iCalendar text, a JSON object), which
//! comes back in [`TaskRow::data`] when the task is pushed so the service
//! can update it without losing what Envelope does not edit. Subtasks name
//! their parent by its `uid` (`parent_uid`); a service whose server names
//! tasks by id uses that id as the `uid`. Pushes come parents first, so a
//! parent created here has its server id by the time its subtasks go up; a
//! subtask whose parent is not on the server yet waits for it (and goes to
//! the list on this computer with it, should the server never take it).
//!
//! [`caldav`] serves CalDAV, [`google`] Google Tasks, [`graph`] Microsoft To
//! Do and [`ews`] Exchange task folders. Google and Microsoft accounts get
//! task lists only when their sign-in granted access to tasks; accounts
//! signed in before Envelope asked for it are passed over quietly until they
//! sign in again.

pub(crate) mod caldav;
pub(crate) mod ews;
pub(crate) mod google;
pub(crate) mod graph;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::contacts::{Failure, backoff};
use crate::db::mail::{self, AccountRow};
use crate::db::now_ms;
use crate::db::tasks::{
    self as tdb, ListRow, ListSyncState, ListUpsert, RemoteIndex, RemoteTask, TaskRow,
};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{AccountKind, EngineEvent};
use crate::tasks::TaskListKind;

/// How often lists are checked for changes on the server.
const PULL_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// A change the server keeps refusing is given up after this many attempts.
const MAX_ATTEMPTS: i64 = 5;
/// Longest wait between two looks for tasks coming due.
const DUE_CHECK: Duration = Duration::from_secs(30);

/// What a task server offers Envelope, for one account.
#[async_trait::async_trait]
pub(crate) trait TaskService: Send + Sync {
    /// The kind of the lists this service keeps.
    fn kind(&self) -> TaskListKind;

    /// The account's lists as the server has them, in the server's order,
    /// with the one new tasks of the account go to marked as default.
    async fn lists(&self) -> Result<Vec<ListUpsert>>;

    /// What changed in `list` since its last pull, as told by the
    /// bookkeeping in `list.sync_state`. `local` has the etag and local
    /// state of every stored task of the list that is on the server.
    async fn pull(&self, list: &ListRow, local: &RemoteIndex) -> Result<Pulled>;

    /// Uploads one local change: `task.local_state` says whether the task
    /// was created, changed or deleted here, and `task.data` is the server
    /// copy the change started from (`None` for new tasks).
    async fn push(&self, list: &ListRow, task: &TaskRow) -> std::result::Result<Done, Failure>;

    /// Creates a list; returns it as [`lists`](Self::lists) would.
    async fn create_list(&self, name: &str) -> Result<ListUpsert>;

    async fn rename_list(&self, list: &ListRow, name: &str) -> Result<()>;

    async fn delete_list(&self, list: &ListRow) -> Result<()>;
}

/// The changes of one list since its last pull.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pulled {
    /// Tasks added or changed on the server.
    pub changed: Vec<RemoteTask>,
    /// Remote ids of tasks deleted on the server.
    pub removed: Vec<String>,
    /// When the pull listed every task of the list: their remote ids (any
    /// other stored task is gone from the server).
    pub listed: Option<HashSet<String>>,
    /// Bookkeeping for the next pull.
    pub state: ListSyncState,
}

impl Pulled {
    /// Nothing changed since the pull that left `state`.
    pub fn unchanged(state: &ListSyncState) -> Pulled {
        Pulled {
            state: state.clone(),
            ..Pulled::default()
        }
    }
}

/// What an upload achieved.
#[derive(Debug)]
pub(crate) enum Done {
    /// Saved; the task as the server has it now.
    Saved(Box<RemoteTask>),
    /// Deleted (or already gone).
    Deleted,
}

/// The task service of an account, if it has one.
pub(crate) async fn service(
    engine: &Engine,
    account: &AccountRow,
) -> Result<Option<Box<dyn TaskService>>> {
    Ok(match account.kind {
        AccountKind::Imap => match &account.config.caldav_url {
            Some(url) => Some(Box::new(
                caldav::Caldav::connect(engine, account, url).await?,
            )),
            None => None,
        },
        AccountKind::Gmail if tasks_granted(engine, account).await => {
            Some(Box::new(google::Google::connect(engine, account)?))
        }
        AccountKind::Microsoft if tasks_granted(engine, account).await => {
            Some(Box::new(graph::Graph::connect(engine, account)?))
        }
        AccountKind::Gmail | AccountKind::Microsoft => None,
        AccountKind::Exchange => Some(Box::new(ews::Ews::connect(engine, account).await?)),
    })
}

/// Whether the OAuth grant of an account covers tasks. Grants from before
/// Envelope asked for that (and accounts signed in with a password) have no
/// task lists; signing in again asks for access.
async fn tasks_granted(engine: &Engine, account: &AccountRow) -> bool {
    let granted = engine.tokens.tasks_granted(account.id).await == Some(true);
    if !granted {
        tracing::debug!(
            account = account.id,
            "no task sync: the account's sign-in does not cover tasks"
        );
    }
    granted
}

/// Wakes the task sync after local changes or account changes.
pub(crate) fn wake(engine: &Engine) {
    engine.tasks_wake.notify_one();
}

/// Background loops: one uploads local changes and pulls server changes of
/// every account with task lists, the other announces tasks coming due.
pub(crate) fn spawn_scheduler(engine: Engine) {
    // Deletions on this computer can't be undone after a restart.
    if let Err(e) = engine.purge_deleted_tasks(i64::MAX) {
        tracing::debug!("deleted tasks: {e}");
    }
    spawn_due_notifier(engine.clone());
    tokio::spawn(async move {
        // Give mail a head start when the app launches.
        tokio::select! {
            _ = engine.shutdown.cancelled() => return,
            _ = engine.tasks_wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
        }
        let mut next_pull = Instant::now();
        loop {
            let pull = Instant::now() >= next_pull;
            run_cycle(&engine, pull).await;
            if pull {
                next_pull = Instant::now() + PULL_INTERVAL;
            }
            let retry = engine
                .db
                .read(tdb::next_push_at)
                .ok()
                .flatten()
                .map(|at| Duration::from_millis((at - now_ms()).clamp(1_000, i64::MAX) as u64));
            let wait = next_pull
                .saturating_duration_since(Instant::now())
                .min(retry.unwrap_or(PULL_INTERVAL));
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = engine.tasks_wake.notified() => {
                    // Let a burst of edits settle into one round.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    next_pull = Instant::now();
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }
    });
}

/// Announces tasks as they come due, looking again when the next one is
/// due or every half minute (edits can bring one forward). Each look also
/// follows a change of the time zone, which moves the days of tasks due at
/// a time of day, and removes tasks deleted on this computer that can't be
/// brought back any more.
fn spawn_due_notifier(engine: Engine) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = engine.follow_time_zone() {
                tracing::debug!("task days: {e}");
            }
            if let Err(e) = engine.purge_deleted_tasks(now_ms()) {
                tracing::debug!("deleted tasks: {e}");
            }
            let now = now_ms();
            let wait = match engine.notify_due_tasks(now) {
                Ok(Some(next)) => Duration::from_millis((next - now).clamp(200, i64::MAX) as u64),
                Ok(None) => DUE_CHECK,
                Err(e) => {
                    tracing::debug!("due tasks: {e}");
                    DUE_CHECK
                }
            };
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = tokio::time::sleep(wait.min(DUE_CHECK)) => {}
            }
        }
    });
}

/// What a sync round changed.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    /// Lists whose tasks changed.
    pub tasks: Vec<i64>,
    /// Lists were added, changed or removed.
    pub lists: bool,
}

async fn run_cycle(engine: &Engine, pull: bool) {
    let accounts = engine.db.read(mail::list_accounts).unwrap_or_default();
    let mut changes = Changes::default();
    for account in accounts.into_iter().filter(|a| a.enabled) {
        let service = match service(engine, &account).await {
            Ok(Some(service)) => service,
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!(account = account.id, "no task sync: {e}");
                continue;
            }
        };
        match sync_account(engine, &account, service.as_ref(), pull).await {
            Ok(done) => {
                changes.tasks.extend(done.tasks);
                changes.lists |= done.lists;
            }
            Err(e) if e.is_transient() => {
                tracing::debug!(account = account.id, "task sync postponed: {e}")
            }
            // Tasks the server does not offer this account (no access to
            // them, or the service turned off): nothing to report.
            Err(e @ Error::Invalid(_)) => {
                tracing::debug!(account = account.id, "no task sync: {e}")
            }
            Err(e) => tracing::warn!(account = account.id, "task sync failed: {e}"),
        }
    }
    emit_changed(engine, changes);
}

pub(crate) fn emit_changed(engine: &Engine, changes: Changes) {
    if changes.lists {
        // An empty list tells the UI to reload everything.
        engine.emit(EngineEvent::TasksChanged {
            list_ids: Vec::new(),
        });
        return;
    }
    let mut list_ids = changes.tasks;
    if list_ids.is_empty() {
        return;
    }
    list_ids.sort_unstable();
    list_ids.dedup();
    engine.emit(EngineEvent::TasksChanged { list_ids });
}

/// Synchronizes the task lists of one account.
pub(crate) async fn sync_account(
    engine: &Engine,
    account: &AccountRow,
    service: &dyn TaskService,
    pull: bool,
) -> Result<Changes> {
    let mut changes = Changes::default();
    let account_id = account.id;
    for list in engine.db.read(|c| tdb::account_lists(c, account_id))? {
        if push_list(engine, service, &list).await? {
            changes.tasks.push(list.id);
        }
    }
    if !pull {
        return Ok(changes);
    }
    let lists = service.lists().await?;
    let changed = engine
        .db
        .write(|tx| tdb::sync_lists(tx, account_id, service.kind(), &lists))?;
    changes.lists = !changed.is_empty();
    // Pushing may have reset a list's sync state (a refused change is undone
    // by syncing it from scratch), so the lists are read again.
    for list in engine.db.read(|c| tdb::account_lists(c, account_id))? {
        match pull_list(engine, service, &list).await {
            Ok(true) => changes.tasks.push(list.id),
            Ok(false) => {}
            Err(e) if e.is_transient() || e.is_auth() => return Err(e),
            Err(e) => tracing::warn!("task list {} sync failed: {e}", list.name),
        }
    }
    Ok(changes)
}

/// Pulls the changes of one list. Returns whether any task changed.
pub(crate) async fn pull_list(
    engine: &Engine,
    service: &dyn TaskService,
    list: &ListRow,
) -> Result<bool> {
    let list_id = list.id;
    let local = engine
        .db
        .read_async(move |c| tdb::remote_index(c, list_id))
        .await?;
    let pulled = service.pull(list, &local).await?;
    store_pulled(engine, list_id, pulled).await
}

/// Stores what a pull found.
pub(crate) async fn store_pulled(engine: &Engine, list_id: i64, pulled: Pulled) -> Result<bool> {
    engine
        .db
        .write_async(move |tx| {
            let mut any = false;
            for task in &pulled.changed {
                any |= tdb::upsert_synced(tx, list_id, task)?.is_some();
            }
            for remote_id in &pulled.removed {
                any |= tdb::delete_synced(tx, list_id, remote_id)?;
            }
            if let Some(listed) = &pulled.listed {
                any |= tdb::sweep(tx, list_id, listed)? > 0;
            }
            if any {
                tdb::resolve_parents(tx, list_id)?;
            }
            let state = ListSyncState {
                last_sync: Some(now_ms()),
                ..pulled.state
            };
            tdb::set_list_sync_state(tx, list_id, &state)?;
            Ok(any)
        })
        .await
}

/// Uploads the pending changes of a list. Returns whether any task changed.
pub(crate) async fn push_list(
    engine: &Engine,
    service: &dyn TaskService,
    list: &ListRow,
) -> Result<bool> {
    let list_id = list.id;
    let pending = engine
        .db
        .read_async(move |c| tdb::dirty_rows(c, list_id))
        .await?;
    let mut changed = false;
    for queued in pending {
        // An earlier upload in this round may have moved or settled it (a
        // refused parent takes its new subtasks along).
        let id = queued.id;
        // A deletion that can still be undone waits.
        let Some(row) = engine
            .db
            .read(|c| tdb::get_task(c, id))
            .ok()
            .filter(|r| r.list_id == list.id && r.local_state != tdb::SYNCED)
            .filter(|r| r.push_after <= now_ms())
        else {
            continue;
        };
        // The server names the task of a subtask by its id there: a subtask
        // waits for its task, without counting attempts.
        if row.local_state != tdb::DELETED && engine.db.read(|c| tdb::parent_unsent(c, &row))? {
            continue;
        }
        let outcome = if list.read_only {
            Err(Failure::Forbidden("the list is read-only".into()))
        } else {
            service.push(list, &row).await
        };
        changed |= settle(engine, list, &row, outcome)?;
    }
    Ok(changed)
}

/// Gives up on a local change: a task the server would not create is kept
/// in the list on this computer, anything else returns to the server's
/// copy. A task deleted while its change was being uploaded is left to its
/// deletion.
fn give_up(engine: &Engine, list: &ListRow, row: &TaskRow) -> Result<()> {
    let local = engine.ensure_local_task_list()?;
    engine.db.write(|tx| {
        let Ok(current) = tdb::get_task(tx, row.id) else {
            return Ok(());
        };
        if current.local_state == tdb::DELETED && row.local_state != tdb::DELETED {
            return Ok(());
        }
        if current.local_state == tdb::CREATED {
            tdb::keep_locally(tx, current.id, local)
        } else {
            tdb::discard_local_change(tx, current.id, list.id)
        }
    })
}

/// Records the outcome of one upload. Returns whether the task changed
/// locally; fails (stopping the account's round) when retrying later makes
/// more sense than going on.
fn settle(
    engine: &Engine,
    list: &ListRow,
    row: &TaskRow,
    outcome: std::result::Result<Done, Failure>,
) -> Result<bool> {
    match outcome {
        Ok(Done::Saved(task)) => {
            engine
                .db
                .write(|tx| tdb::finish_push(tx, row.id, row.version, &task))?;
            Ok(true)
        }
        Ok(Done::Deleted) => {
            engine.db.write(|tx| tdb::delete_row(tx, row.id))?;
            Ok(true)
        }
        Err(Failure::Transient(message)) => {
            let retry = now_ms() + backoff(row.push_attempts, true);
            engine
                .db
                .write(|tx| tdb::push_failed(tx, row.id, &message, retry))?;
            Err(Error::Network(message))
        }
        Err(Failure::Auth(message)) => Err(Error::Auth(message)),
        Err(Failure::Forbidden(message)) => {
            tracing::warn!(
                task = row.id,
                "the server does not allow this change: {message}"
            );
            engine.db.write(|tx| {
                tx.execute(
                    "UPDATE task_lists SET read_only = 1 WHERE id = ?",
                    [list.id],
                )?;
                Ok(())
            })?;
            give_up(engine, list, row)?;
            Ok(true)
        }
        Err(Failure::Rejected(message)) if row.push_attempts + 1 >= MAX_ATTEMPTS => {
            tracing::warn!(
                task = row.id,
                "giving up on a change the server refuses: {message}"
            );
            give_up(engine, list, row)?;
            Ok(true)
        }
        Err(Failure::Rejected(message)) => {
            tracing::warn!(task = row.id, "the server refused a change: {message}");
            let retry = now_ms() + backoff(row.push_attempts, false);
            engine
                .db
                .write(|tx| tdb::push_failed(tx, row.id, &message, retry))?;
            // The task now shows why it is not saved.
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests;
