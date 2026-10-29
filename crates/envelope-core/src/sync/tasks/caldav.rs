//! Tasks on CalDAV servers: every collection whose
//! `supported-calendar-component-set` includes `VTODO` is a list (one that
//! holds events too is a calendar as well). Lists without changes are
//! skipped by their `getctag`; changes come from `sync-collection` when the
//! server offers it, else from comparing entity tags. Uploads are
//! conditional (`If-Match`, `If-None-Match: *`) and patch the stored copy,
//! so what other clients keep in a task survives; when the server copy
//! changed meanwhile, the changes made here are applied to the new copy.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use envelope_dav::{
    CalendarPatch, ComponentSet, DavClient, DavError, NewCalendar, ObjectKind, Precondition,
};
use envelope_ical::{EventTime, Todo, TodoStatus, TzContext};
use parking_lot::Mutex;

use super::{Done, Failure, Pulled, TaskService};
use crate::calendar::{local_tz, to_ms};
use crate::db::mail::AccountRow;
use crate::db::now_ms;
use crate::db::tasks::{
    self as tdb, ListRow, ListSyncState, ListUpsert, RemoteIndex, RemoteTask, TaskRow,
};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::sync::calendar::dav_client_at;
use crate::sync::contacts::dav_err;
use crate::tasks::{TaskListKind, local_day};

/// Objects fetched per `calendar-multiget`.
const MULTIGET_CHUNK: usize = 100;
/// `X-APPLE-SORT-ORDER` counts seconds from 2001-01-01; tasks without one
/// are placed by when they were created, as Nextcloud Tasks does.
const APPLE_EPOCH: i64 = 978_307_200;

/// The CalDAV task service of an account.
pub(crate) struct Caldav {
    engine: Engine,
    account: AccountRow,
    client: DavClient,
    url: String,
    /// The collections seen by the last listing, by href.
    found: Mutex<HashMap<String, envelope_dav::Calendar>>,
}

impl Caldav {
    /// The service for an account's CalDAV server at `url`.
    pub(crate) async fn connect(
        engine: &Engine,
        account: &AccountRow,
        url: &str,
    ) -> Result<Caldav> {
        let client = dav_client_at(engine, account, url).await?;
        Ok(Caldav::new(engine, account, client, url))
    }

    pub(crate) fn new(
        engine: &Engine,
        account: &AccountRow,
        client: DavClient,
        url: &str,
    ) -> Caldav {
        Caldav {
            engine: engine.clone(),
            account: account.clone(),
            client,
            url: url.to_owned(),
            found: Mutex::new(HashMap::new()),
        }
    }

    /// The account's calendar home, as the calendar sync found it, else
    /// discovered (again when the CalDAV address changed) and remembered.
    async fn home(&self) -> Result<String> {
        let state = self
            .engine
            .account_row(self.account.id)
            .map(|a| a.state)
            .unwrap_or_else(|_| self.account.state.clone());
        let same_url = state
            .get("caldavUrl")
            .and_then(|v| v.as_str())
            .is_none_or(|url| url == self.url);
        if let Some(home) = state
            .get("caldavHome")
            .and_then(|v| v.as_str())
            .filter(|_| same_url)
        {
            return Ok(home.to_owned());
        }
        let found = self
            .client
            .discover_from(&self.url, envelope_dav::ServiceType::CalDav)
            .await
            .map_err(dav_err)?;
        let home = found
            .calendar_home_set
            .first()
            .cloned()
            .ok_or_else(|| Error::Other("the server has no calendars".into()))?;
        let account_id = self.account.id;
        self.engine.db.write(|tx| {
            crate::db::contacts::set_account_state_value(
                tx,
                account_id,
                "caldavHome",
                &home.clone().into(),
            )?;
            crate::db::contacts::set_account_state_value(
                tx,
                account_id,
                "caldavUrl",
                &self.url.clone().into(),
            )
        })?;
        Ok(home)
    }

    /// A collection, from the last listing or the server.
    async fn collection(&self, href: &str) -> Result<envelope_dav::Calendar> {
        if let Some(found) = self.found.lock().get(href) {
            return Ok(found.clone());
        }
        self.client.get_calendar(href).await.map_err(dav_err)
    }
}

#[async_trait::async_trait]
impl TaskService for Caldav {
    fn kind(&self) -> TaskListKind {
        TaskListKind::Caldav
    }

    async fn lists(&self) -> Result<Vec<ListUpsert>> {
        let home = self.home().await?;
        let lists: Vec<envelope_dav::Calendar> = self
            .client
            .list_calendars(&home)
            .await
            .map_err(dav_err)?
            .into_iter()
            .filter(|c| c.components.vtodo)
            .collect();
        // New tasks go to a list of tasks only, when there is one.
        let default = lists
            .iter()
            .position(|c| !c.components.vevent && !c.read_only)
            .or_else(|| lists.iter().position(|c| !c.read_only))
            .unwrap_or(0);
        let upserts = lists
            .iter()
            .enumerate()
            .map(|(i, c)| ListUpsert {
                remote_id: c.href.clone(),
                name: c
                    .display_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "Tasks".into()),
                color: c.color.clone(),
                read_only: c.read_only,
                is_default: i == default,
                sort_order: c.order.map(i64::from).unwrap_or(i as i64),
            })
            .collect();
        *self.found.lock() = lists.into_iter().map(|c| (c.href.clone(), c)).collect();
        Ok(upserts)
    }

    async fn pull(&self, list: &ListRow, local: &RemoteIndex) -> Result<Pulled> {
        let dav = self.collection(&list.remote_id).await?;
        let state = &list.sync_state;
        if state.last_sync.is_some() && dav.ctag.is_some() && state.ctag == dav.ctag {
            return Ok(Pulled::unchanged(state));
        }
        // Either the changes since the last sync token, or the full listing.
        // Collections that hold events too are listed by their tasks only.
        let mut token = state.sync_token.clone();
        let (refs, removed, full, sync_token) = loop {
            if dav.components.vevent {
                let refs = self
                    .client
                    .list_todo_etags(&list.remote_id)
                    .await
                    .map_err(dav_err)?;
                break (refs, Vec::new(), true, None);
            }
            if !dav.supports_sync {
                let refs = self
                    .client
                    .list_etags(&list.remote_id)
                    .await
                    .map_err(dav_err)?;
                break (refs, Vec::new(), true, None);
            }
            match self
                .client
                .sync_collection(&list.remote_id, token.as_deref())
                .await
            {
                // A truncated listing is not complete enough to infer removals.
                Ok(delta) => {
                    break (
                        delta.changed,
                        delta.removed,
                        token.is_none() && !delta.truncated,
                        delta.sync_token,
                    );
                }
                Err(e) if e.requires_full_resync() && token.is_some() => token = None,
                Err(e) => return Err(dav_err(e)),
            }
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut wanted: Vec<String> = Vec::new();
        for r in &refs {
            if !seen.insert(r.href.clone()) {
                continue;
            }
            let fetch = match local.get(&r.href) {
                None => true,
                // Tasks with local changes are reconciled when pushed.
                Some((_, state)) if *state != tdb::SYNCED => false,
                Some((etag, _)) => etag.is_none() || r.etag.is_none() || *etag != r.etag,
            };
            if fetch {
                wanted.push(r.href.clone());
            }
        }
        let mut pulled = Pulled {
            removed,
            listed: full.then_some(seen),
            state: ListSyncState {
                ctag: dav.ctag.clone(),
                sync_token: sync_token.or_else(|| dav.sync_token.clone()),
                ..ListSyncState::default()
            },
            ..Pulled::default()
        };
        for chunk in wanted.chunks(MULTIGET_CHUNK) {
            let result = self
                .client
                .multiget(&list.remote_id, chunk)
                .await
                .map_err(dav_err)?;
            for object in result.objects {
                if let Some(task) = remote_task(&object.href, object.etag, &object.data) {
                    pulled.changed.push(task);
                }
            }
            pulled.removed.extend(result.missing);
        }
        Ok(pulled)
    }

    async fn push(&self, list: &ListRow, row: &TaskRow) -> std::result::Result<Done, Failure> {
        let client = &self.client;
        if row.local_state == tdb::DELETED {
            if let Some(href) = &row.remote_id {
                match client.delete(href, None).await {
                    Ok(()) | Err(DavError::NotFound { .. }) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            return Ok(Done::Deleted);
        }
        let href = match &row.remote_id {
            Some(href) => href.clone(),
            None => envelope_dav::object_href(&list.remote_id, &row.uid, ObjectKind::Calendar)?,
        };
        let mut server_raw = row.data.clone().unwrap_or_default();
        let mut server = first_todo(&server_raw);
        // What the changes made here started from; none for a new task,
        // where all of it is new.
        let mut base = server.clone();
        let mut etag = row.etag.clone();

        if row.local_state == tdb::CREATED || server.is_none() {
            base = None;
            let todo = outgoing(server.as_ref(), None, row);
            let text = write(&server_raw, &todo)?;
            match client.put(&href, &text, Precondition::IfNoneMatch).await {
                Ok(etag) => return Ok(saved(&href, etag, &text)),
                Err(DavError::PreconditionFailed { .. }) => {
                    // Already there: an earlier upload whose answer was lost,
                    // or a clash with another task.
                    let existing = client.get(&href).await?;
                    let theirs = first_todo(&existing.data);
                    if theirs.as_ref().map(|t| t.uid.as_str()) != Some(row.uid.as_str()) {
                        return Err(Failure::Rejected(format!(
                            "{href} already holds another task"
                        )));
                    }
                    server = theirs;
                    server_raw = existing.data;
                    etag = existing.etag;
                }
                Err(e) => return Err(e.into()),
            }
        }

        for _ in 0..3 {
            let todo = outgoing(server.as_ref(), base.as_ref(), row);
            let text = write(&server_raw, &todo)?;
            let condition = etag.clone().map(Precondition::IfMatch).unwrap_or_default();
            match client.put(&href, &text, condition).await {
                Ok(etag) => return Ok(saved(&href, etag, &text)),
                Err(DavError::PreconditionFailed { .. } | DavError::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
            // Changed (or deleted) on the server since we last saw it.
            match client.get(&href).await {
                Ok(fresh) => {
                    tracing::info!(%href, "merging a task that also changed on the server");
                    server = first_todo(&fresh.data);
                    server_raw = fresh.data;
                    etag = fresh.etag;
                }
                Err(DavError::NotFound { .. }) => {
                    // Deleted there while edited here: the edit wins.
                    return match client.put(&href, &text, Precondition::IfNoneMatch).await {
                        Ok(etag) => Ok(saved(&href, etag, &text)),
                        Err(e) => Err(e.into()),
                    };
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(Failure::Transient(format!(
            "{href} keeps changing on the server"
        )))
    }

    async fn create_list(&self, name: &str) -> Result<ListUpsert> {
        let home = self.home().await?;
        let href = self
            .client
            .create_calendar(
                &home,
                &NewCalendar::new(name).with_components(ComponentSet::TASKS),
            )
            .await
            .map_err(dav_err)?;
        let href = envelope_dav::normalize_href(&href).unwrap_or(href);
        // Read back: servers may change the name or pick a color.
        let created = self.client.get_calendar(&href).await.ok();
        Ok(ListUpsert {
            remote_id: created.as_ref().map_or(href, |c| c.href.clone()),
            name: created
                .as_ref()
                .and_then(|c| c.display_name.clone())
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| name.to_owned()),
            color: created.as_ref().and_then(|c| c.color.clone()),
            read_only: false,
            is_default: false,
            sort_order: created
                .and_then(|c| c.order)
                .map_or(i64::from(i32::MAX), i64::from),
        })
    }

    async fn rename_list(&self, list: &ListRow, name: &str) -> Result<()> {
        let patch = CalendarPatch {
            display_name: Some(name.to_owned()),
            ..CalendarPatch::default()
        };
        self.client
            .update_calendar(&list.remote_id, &patch)
            .await
            .map_err(dav_err)
    }

    async fn delete_list(&self, list: &ListRow) -> Result<()> {
        let collection = self
            .client
            .get_calendar(&list.remote_id)
            .await
            .map_err(dav_err)?;
        if collection.components.vevent {
            return Err(Error::Invalid(
                "This list is a calendar too. Delete it with the calendar instead.".into(),
            ));
        }
        match self.client.delete_collection(&list.remote_id).await {
            Ok(()) | Err(DavError::NotFound { .. }) => Ok(()),
            Err(e) => Err(dav_err(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// VTODO ⇄ stored task

/// The task of a calendar object, if it holds one.
fn first_todo(data: &str) -> Option<Todo> {
    if data.trim().is_empty() {
        return None;
    }
    envelope_ical::parse_todos(data).ok()?.into_iter().next()
}

fn ms(at: DateTime<Utc>) -> i64 {
    at.timestamp_millis()
}

/// Priorities 1–4 are the high ones (RFC 5545).
fn is_important(priority: u8) -> bool {
    (1..=4).contains(&priority)
}

/// The due date as Envelope keeps it: the day, and the time when there is
/// one.
fn due_parts(due: Option<&EventTime>) -> (Option<String>, Option<i64>) {
    match due {
        None => (None, None),
        Some(EventTime::Date { date }) => (Some(date.format("%Y-%m-%d").to_string()), None),
        Some(time) => {
            let at = to_ms(time);
            (Some(local_day(at)), Some(at))
        }
    }
}

/// A due date as iCalendar writes it: a date, or a time in the zone of the
/// task's current due time (`like`), else in the user's zone.
fn due_time(day: Option<&str>, at: Option<i64>, like: Option<&EventTime>) -> Option<EventTime> {
    let tz = local_tz();
    match (at, day) {
        (Some(at), _) => {
            let instant = DateTime::from_timestamp_millis(at)?;
            match like.filter(|l| !l.is_date()) {
                Some(like) => {
                    Some(TzContext::new().express_like(&EventTime::Utc { at: instant }, like, tz))
                }
                None => Some(EventTime::Zoned {
                    local: instant.with_timezone(&tz).naive_local(),
                    tzid: tz.name().to_owned(),
                }),
            }
        }
        (None, Some(day)) => NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .ok()
            .map(|date| EventTime::Date { date }),
        (None, None) => None,
    }
}

/// What Envelope stores of a task from the server.
fn remote_from_todo(href: &str, etag: Option<String>, data: &str, todo: &Todo) -> RemoteTask {
    let (due_date, due_at) = due_parts(todo.due.as_ref());
    RemoteTask {
        remote_id: href.to_owned(),
        etag,
        uid: todo.uid.clone(),
        title: todo.summary.clone(),
        notes: todo.description.clone().unwrap_or_default(),
        due_date,
        due_at,
        completed_at: todo.is_done().then(|| {
            todo.completed
                .or(todo.last_modified)
                .or(todo.created)
                .map_or_else(now_ms, ms)
        }),
        important: is_important(todo.priority),
        parent_uid: todo.parent.clone(),
        position: Some(todo.sort_order.unwrap_or_else(|| {
            todo.created
                .map_or(0, |created| created.timestamp() - APPLE_EPOCH)
        })),
        repeats: todo.rrule.is_some(),
        created_at: todo.created.map(ms),
        updated_at: todo.last_modified.map(ms),
        data: Some(data.to_owned()),
    }
}

/// The task stored in a calendar object from the server (`None`: there is
/// none, or it cannot be read).
pub(super) fn remote_task(href: &str, etag: Option<String>, data: &str) -> Option<RemoteTask> {
    match envelope_ical::parse_todos(data) {
        Ok(todos) => todos
            .first()
            .map(|todo| remote_from_todo(href, etag, data, todo)),
        Err(e) => {
            tracing::warn!("unreadable task {href}: {e}");
            None
        }
    }
}

/// The task to write: the server's copy (`server`; `None` for a new task)
/// with the changes made here, that is the fields where `row` differs from
/// `base`, the copy the changes started from (`None`: every field).
fn outgoing(server: Option<&Todo>, base: Option<&Todo>, row: &TaskRow) -> Todo {
    let before = base.map(|b| remote_from_todo("", None, "", b));
    let changed = |same: &dyn Fn(&RemoteTask) -> bool| before.as_ref().is_none_or(|b| !same(b));
    let mut todo = server.cloned().unwrap_or_default();
    todo.uid = row.uid.clone();
    if changed(&|b| b.title == row.title) {
        todo.summary = row.title.clone();
    }
    if changed(&|b| b.notes == row.notes) {
        todo.description = Some(row.notes.clone()).filter(|n| !n.is_empty());
    }
    if changed(&|b| b.due_date == row.due_date && b.due_at == row.due_at) {
        let due = due_time(row.due_date.as_deref(), row.due_at, todo.due.as_ref());
        todo = todo.reschedule(due, local_tz());
    }
    if changed(&|b| b.completed_at.is_some() == row.completed_at.is_some()) {
        match row.completed_at {
            Some(at) => todo.complete(DateTime::from_timestamp_millis(at).unwrap_or_else(Utc::now)),
            None if server.is_none() => todo.status = Some(TodoStatus::NeedsAction),
            None => todo.reopen(),
        }
    }
    if changed(&|b| b.important == row.important) {
        todo.priority = if row.important { 1 } else { 0 };
    }
    if changed(&|b| b.parent_uid == row.parent_uid) {
        todo.parent = row.parent_uid.clone();
    }
    if changed(&|b| b.position == Some(row.position)) {
        todo.sort_order = Some(row.position);
    }
    todo
}

/// The calendar object for `todo`: the stored copy patched, or a new one.
fn write(existing: &str, todo: &Todo) -> std::result::Result<String, Failure> {
    envelope_ical::upsert_todo(existing, todo).map_err(|e| Failure::Rejected(e.to_string()))
}

/// The task as the server stores it after an upload of `text`. Reading the
/// text back gives the same form a later pull would.
fn saved(href: &str, etag: Option<String>, text: &str) -> Done {
    let task = remote_task(href, etag.clone(), text).unwrap_or_else(|| RemoteTask {
        remote_id: href.to_owned(),
        etag,
        data: Some(text.to_owned()),
        ..RemoteTask::default()
    });
    Done::Saved(Box::new(task))
}

/// For a repeating task of a CalDAV list: its next due date, `(day, time)`,
/// which completing it moves it to. `None` when it does not repeat, has no
/// due date, or its series ends with this occurrence.
pub(crate) fn next_due(list: &ListRow, row: &TaskRow) -> Option<(Option<String>, Option<i64>)> {
    if list.kind != TaskListKind::Caldav || !row.repeats {
        return None;
    }
    let base = first_todo(row.data.as_deref()?)?;
    // The task as it is here, with the edits not uploaded yet.
    let current = outgoing(Some(&base), Some(&base), row);
    current.due.as_ref()?;
    let next = current.next_occurrence(local_tz())?;
    Some(due_parts(next.due.as_ref()))
}
