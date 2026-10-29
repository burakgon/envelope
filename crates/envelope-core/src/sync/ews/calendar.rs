//! Exchange calendars: every calendar folder of the mailbox, read through
//! calendar views (series expanded into their occurrences and stored as
//! instances, like Microsoft Graph calendars), and writes with meeting
//! semantics — invitations, updates, cancellations and answers go out
//! through Exchange.
//!
//! Steady-state syncs cost one calendar view per calendar: items whose
//! change key is unchanged are not read again (every item is re-read twice
//! a day as a safety net).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

use chrono::{Duration, Utc};
use envelope_ews::{
    self as ews, BodyType, CalendarItemUpdate, DeleteType, FolderId, GetCalendarItemsOptions,
    ItemId, ItemRef, SendCancellations, SendInvitations, SendUpdates,
};
use envelope_ical::{Event, PartStat};

use super::EwsMail;
use super::mapping::{self, EventContext};
use crate::calendar::{EditScope, EventInput, local_tz};
use crate::db::calendar::{self as cdb, CalendarRow, CalendarSyncState, CalendarUpsert, EventRow};
use crate::db::mail::AccountRow;
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{AccountKind, EngineEvent, SyncState};

/// Days before today the calendar view covers.
const PAST_DAYS: i64 = 180;
/// Days after today the calendar view covers.
const FUTURE_DAYS: i64 = 400;
/// Every item of a calendar is read again this often, whatever its change
/// key says.
const FULL_REFRESH_MS: i64 = 12 * 3_600_000;

/// Serializes calendar syncs and writes of an account: the scheduler,
/// notifications and the user's own changes all read and replace the same
/// rows.
fn calendar_lock(account_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = parking_lot::Mutex<HashMap<i64, Weak<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&account_id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(account_id, Arc::downgrade(&lock));
    lock
}

/// Synchronizes the calendars of an Exchange account.
pub(crate) async fn sync_account(engine: &Engine, account: &AccountRow) -> Result<()> {
    // A rejected password is not tried again behind the mail worker's back:
    // directory lockout policies count every failed sign-in.
    if matches!(engine.sync_state(account.id), SyncState::AuthError { .. }) {
        return Ok(());
    }
    let ews = EwsMail::for_account(engine, account).await?;
    let changed = sync_with(engine, account, &ews).await?;
    if !changed.is_empty() {
        engine.emit(EngineEvent::CalendarChanged {
            calendar_ids: changed,
        });
    }
    Ok(())
}

/// Updates the calendar list and every calendar; returns the calendars
/// whose events changed.
pub(super) async fn sync_with(
    engine: &Engine,
    account: &AccountRow,
    ews: &EwsMail,
) -> Result<Vec<i64>> {
    let lock = calendar_lock(account.id);
    let _guard = lock.lock().await;
    let tree = ews.client.folder_tree().await.map_err(|e| ews.err(e))?;
    let upserts = calendar_upserts(&tree, &account.color);
    let account_id = account.id;
    engine
        .db
        .write(|tx| cdb::sync_calendars(tx, account_id, "ews", &upserts).map(|_| ()))?;
    let rows = engine.db.read(|c| cdb::account_calendars(c, account_id))?;
    let mut changed = Vec::new();
    for row in rows {
        match sync_calendar(engine, ews, account, &row).await {
            Ok(true) => changed.push(row.id),
            Ok(false) => {}
            Err(e) if e.is_auth() || e.is_transient() => return Err(e),
            Err(e) => tracing::warn!("Exchange calendar {} sync failed: {e}", row.name),
        }
    }
    Ok(changed)
}

/// Calendar rows for the calendar folders of the mailbox tree.
pub(super) fn calendar_upserts(tree: &[ews::Folder], color: &str) -> Vec<CalendarUpsert> {
    tree.iter()
        .filter(|f| f.is_calendar() && !f.is_hidden)
        .enumerate()
        .map(|(i, f)| {
            let default = f.role == Some(ews::FolderRole::Calendar);
            CalendarUpsert {
                remote_id: f.id.id.clone(),
                name: Some(f.display_name.trim())
                    .filter(|n| !n.is_empty())
                    .unwrap_or("Calendar")
                    .to_string(),
                color: color.to_string(),
                // Exchange maintains the birthday calendar itself.
                read_only: f.folder_class.as_deref().is_some_and(|c| {
                    c.to_ascii_lowercase()
                        .starts_with("ipf.appointment.birthday")
                }),
                is_default: default,
                timezone: None,
                sort_order: if default { 0 } else { i as i64 + 1 },
                visible_default: true,
            }
        })
        .collect()
}

/// The series a set of occurrences belong to: master id and repeat rule,
/// by UID.
struct Series {
    id: String,
    rule: Option<String>,
}

async fn series_masters<'a>(
    ews: &EwsMail,
    items: impl Iterator<Item = &'a ews::CalendarItem>,
) -> Result<HashMap<String, Series>> {
    let mut wanted: Vec<(String, ItemRef)> = Vec::new();
    for item in items {
        if !mapping::is_series_instance(item) {
            continue;
        }
        let Some(uid) = item.uid.clone() else {
            continue;
        };
        if wanted.iter().any(|(u, _)| *u == uid) {
            continue;
        }
        wanted.push((uid, ItemRef::MasterOf(ItemId::new(item.id.id.clone()))));
    }
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let refs: Vec<ItemRef> = wanted.iter().map(|(_, r)| r.clone()).collect();
    let masters = ews
        .client
        .get_calendar_items(&refs, &GetCalendarItemsOptions::default())
        .await
        .map_err(|e| ews.err(e))?;
    let mut out = HashMap::new();
    for ((uid, _), master) in wanted.into_iter().zip(masters) {
        match master {
            Ok(m) => {
                let rule = mapping::series_rule(&m);
                out.insert(
                    uid,
                    Series {
                        id: m.id.id.clone(),
                        rule,
                    },
                );
            }
            Err(e) => tracing::debug!("cannot read the series of {uid}: {e}"),
        }
    }
    Ok(out)
}

/// What the calendar view reports about an item, condensed. A change to
/// any of it, or to the change key, means the item is read again; the view
/// alone shows changes a series' occurrences do not carry in their change
/// keys.
pub(super) fn view_signature(item: &ews::CalendarItem) -> String {
    use sha2::{Digest, Sha256};
    let text = format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        item.id.change_key,
        item.subject,
        item.start,
        item.end,
        item.location,
        item.is_all_day,
        item.is_cancelled,
        item.my_response_type,
        item.free_busy,
        item.recurrence_id,
    );
    Sha256::digest(text.as_bytes())
        .iter()
        .take(12)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What later writes need to know about a stored item.
fn provider_data(item: &ews::CalendarItem, master: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": item.id.id,
        "itemType": item.item_type.map(|t| t.as_str()),
        "masterId": master,
        "isOrganizer": item.my_response_type == Some(ews::ResponseType::Organizer),
        "isMeeting": item.is_meeting.unwrap_or(false),
        "isCancelled": item.is_cancelled.unwrap_or(false),
    })
}

/// Reads one calendar's events in the sync window. Returns whether any
/// changed.
pub(super) async fn sync_calendar(
    engine: &Engine,
    ews: &EwsMail,
    account: &AccountRow,
    cal: &CalendarRow,
) -> Result<bool> {
    let now = Utc::now();
    let view = ews
        .client
        .calendar_view(
            FolderId::new(cal.remote_id.clone()),
            now - Duration::days(PAST_DAYS),
            now + Duration::days(FUTURE_DAYS),
        )
        .await
        .map_err(|e| ews.err(e))?;
    let full_refresh = cal
        .sync_state
        .cursor
        .as_deref()
        .and_then(|c| c.parse::<i64>().ok())
        .is_none_or(|at| now_ms() - at >= FULL_REFRESH_MS);
    let stored: HashMap<String, Option<String>> = engine
        .db
        .read(|c| cdb::remote_etags(c, cal.id))?
        .into_iter()
        .collect();
    // One item per event (UID and occurrence); should two items claim the
    // same, the first wins.
    let mut keys = HashSet::new();
    let mut current: HashSet<&str> = HashSet::new();
    let mut stale: Vec<&ews::CalendarItem> = Vec::new();
    for item in &view {
        current.insert(item.id.id.as_str());
        let uid = item.uid.clone().unwrap_or_else(|| item.id.id.clone());
        if !keys.insert((uid, item.recurrence_id)) {
            continue;
        }
        let unchanged = !full_refresh
            && stored
                .get(&item.id.id)
                .is_some_and(|etag| etag.as_deref() == Some(view_signature(item).as_str()));
        if !unchanged {
            stale.push(item);
        }
    }
    let mut changed = false;
    if !stale.is_empty() {
        let refs: Vec<ItemRef> = stale
            .iter()
            .map(|i| ItemRef::Id(ItemId::new(i.id.id.clone())))
            .collect();
        let options = GetCalendarItemsOptions {
            include_mime_content: false,
            body_type: Some(BodyType::Text),
        };
        let details = ews
            .client
            .get_calendar_items(&refs, &options)
            .await
            .map_err(|e| ews.err(e))?;
        let mut items = Vec::new();
        for (summary, detail) in stale.iter().zip(details) {
            match detail {
                Ok(item) => items.push((*summary, item)),
                Err(e) if e.code.is_not_found() => {}
                Err(e) if e.code.is_transient() => return Err(ews.err(e.into())),
                Err(e) => tracing::debug!("cannot read calendar item {}: {e}", summary.id.id),
            }
        }
        let series = series_masters(ews, items.iter().map(|(_, item)| item)).await?;
        let me = account.email.to_lowercase();
        let default_tz = local_tz();
        for (summary, item) in &items {
            let master = item.uid.as_deref().and_then(|uid| series.get(uid));
            let cx = EventContext {
                me: &me,
                default_tz,
                series_rule: master.and_then(|m| m.rule.as_deref()),
            };
            let event = mapping::calendar_event(item, &cx);
            let pd = provider_data(item, master.map(|m| m.id.as_str()));
            let etag = view_signature(summary);
            engine.store_event(
                cal.id,
                &event,
                Some(&item.id.id),
                Some(&etag),
                None,
                Some(&pd),
                true,
            )?;
            changed = true;
        }
    }
    let gone: Vec<String> = stored
        .keys()
        .filter(|id| !current.contains(id.as_str()))
        .cloned()
        .collect();
    let cal_id = cal.id;
    if !gone.is_empty() {
        engine.db.write(|tx| {
            for id in &gone {
                cdb::delete_by_remote(tx, cal_id, id)?;
            }
            Ok(())
        })?;
        changed = true;
    }
    let state = CalendarSyncState {
        last_sync: Some(now_ms()),
        cursor: if full_refresh {
            Some(now_ms().to_string())
        } else {
            cal.sync_state.cursor.clone()
        },
        ..CalendarSyncState::default()
    };
    engine
        .db
        .write(|tx| cdb::set_calendar_sync_state(tx, cal_id, &state))?;
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Writes

fn account_of(engine: &Engine, cal: &CalendarRow) -> Result<AccountRow> {
    let id = cal
        .account_id
        .ok_or_else(|| Error::Invalid("local calendar".into()))?;
    engine.account_row(id)
}

/// A stored item as the server knows it.
#[derive(Debug, Clone)]
struct Instance {
    id: String,
    /// An occurrence (or modified occurrence) of a series.
    series: bool,
    /// The mailbox owner organizes it (or it is a plain appointment).
    organizer: bool,
    meeting: bool,
    cancelled: bool,
}

impl Instance {
    fn of(row: &EventRow) -> Result<Instance> {
        let get = |key: &str| row.provider_data.as_ref().and_then(|p| p.get(key));
        let id = get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| row.remote_id.clone())
            .ok_or_else(|| Error::Invalid("event has no server id".into()))?;
        let item_type = get("itemType").and_then(|v| v.as_str()).unwrap_or("Single");
        Ok(Instance {
            id,
            series: matches!(item_type, "Occurrence" | "Exception"),
            organizer: get("isOrganizer").and_then(|v| v.as_bool()).unwrap_or(true),
            meeting: get("isMeeting")
                .and_then(|v| v.as_bool())
                .unwrap_or(!row.event.attendees.is_empty()),
            cancelled: get("isCancelled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    /// Updates and cancellations go out when the mailbox owner organizes a
    /// meeting.
    fn notifies(&self) -> bool {
        self.organizer && self.meeting && !self.cancelled
    }

    /// Someone else's meeting: removing it means declining it.
    fn declines(&self) -> bool {
        self.meeting && !self.organizer && !self.cancelled
    }

    fn send_updates(&self) -> SendUpdates {
        if self.notifies() {
            SendUpdates::SendToAllAndSaveCopy
        } else {
            SendUpdates::SendToNone
        }
    }
}

/// The series master of an occurrence.
async fn master_of(ews: &EwsMail, instance: &Instance) -> Result<ews::CalendarItem> {
    ews.client
        .get_calendar_item(
            ItemRef::MasterOf(ItemId::new(instance.id.clone())),
            &GetCalendarItemsOptions::default(),
        )
        .await
        .map_err(|e| ews.err(e))
}

async fn apply_update(
    ews: &EwsMail,
    id: &str,
    update: &CalendarItemUpdate,
    send: SendUpdates,
) -> Result<()> {
    if *update == CalendarItemUpdate::default() {
        return Ok(());
    }
    ews.client
        .update_calendar_item(ItemId::new(id.to_string()), update, send)
        .await
        .map(drop)
        .map_err(|e| ews.err(e))
}

/// Moves one occurrence to new times. Only the instants change, which is
/// all an occurrence takes.
async fn move_occurrence(ews: &EwsMail, id: &str, event: &Event, notify: bool) -> Result<()> {
    let tz = local_tz();
    let timing = mapping::event_timing(&event.start, &event.end, tz);
    let (start, end) = mapping::instants(&timing);
    let request = mapping::occurrence_times_request(id, start, end, notify);
    let response = ews
        .client
        .execute_raw(&request, false)
        .await
        .map_err(|e| ews.err(e))?;
    match mapping::raw_error(&response) {
        Some(code) => Err(Error::Other(format!(
            "Exchange could not move the occurrence ({code})"
        ))),
        None => Ok(()),
    }
}

/// Ends a series on the day before an occurrence.
async fn end_series_before(
    ews: &EwsMail,
    master: &ews::CalendarItem,
    occurrence: &EventRow,
    send: SendUpdates,
) -> Result<()> {
    let mut recurrence = master
        .recurrence
        .clone()
        .ok_or_else(|| Error::Invalid("Exchange does not know this series".into()))?;
    let zone = master.start_tz().unwrap_or_else(local_tz);
    let original = occurrence
        .event
        .recurrence_id
        .as_ref()
        .unwrap_or(&occurrence.event.start);
    let day = mapping::local_date(original, zone);
    let start = recurrence.range.start();
    let last = day.pred_opt().unwrap_or(day).max(start);
    recurrence.range = ews::RecurrenceRange::EndDate { start, end: last };
    let update = CalendarItemUpdate {
        recurrence: Some(Some(recurrence)),
        ..CalendarItemUpdate::default()
    };
    apply_update(ews, &master.id.id, &update, send).await
}

/// Whether an occurrence is the first of its series.
fn is_first(master: &ews::CalendarItem, occurrence: &EventRow) -> bool {
    master
        .start
        .is_none_or(|first| first.timestamp_millis() >= occurrence.start_ts)
}

/// Creates an event: a meeting with invitations when the mailbox owner
/// organizes it. Returns the stored row (`0` when it is outside the synced
/// window).
pub(crate) async fn write_create(engine: &Engine, cal: &CalendarRow, event: &Event) -> Result<i64> {
    let account = account_of(engine, cal)?;
    let lock = calendar_lock(account.id);
    let _guard = lock.lock().await;
    let ews = EwsMail::for_account(engine, &account).await?;
    let me = account.email.to_lowercase();
    // Someone else's meeting being filed stays a plain appointment: creating
    // it with attendees would invite them all again.
    let mine = event
        .organizer
        .as_ref()
        .is_none_or(|o| o.email.eq_ignore_ascii_case(&me));
    let item = mapping::new_calendar_item(event, &me, mine, local_tz())?;
    let invites = !item.required_attendees.is_empty()
        || !item.optional_attendees.is_empty()
        || !item.resources.is_empty();
    let send = if invites {
        SendInvitations::SendToAllAndSaveCopy
    } else {
        SendInvitations::SendToNone
    };
    let id = ews
        .client
        .create_calendar_item(FolderId::new(cal.remote_id.clone()), &item, send)
        .await
        .map_err(|e| ews.err(e))?;
    sync_calendar(engine, &ews, &account, cal).await?;
    let cal_id = cal.id;
    let row = engine.db.read(|c| {
        let mut rows = cdb::rows_by_remote(c, cal_id, &id.id)?;
        if rows.is_empty() {
            rows = cdb::series_rows(c, cal_id, &event.uid)?;
        }
        Ok(rows.into_iter().map(|r| r.id).min())
    })?;
    Ok(row.unwrap_or(0))
}

/// Changes an event, one occurrence, this and the following occurrences or
/// a whole series.
pub(crate) async fn write_update(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    scope: EditScope,
    input: &EventInput,
) -> Result<()> {
    let account = account_of(engine, cal)?;
    let lock = calendar_lock(account.id);
    let _guard = lock.lock().await;
    let ews = EwsMail::for_account(engine, &account).await?;
    let me = account.email.to_lowercase();
    let tz = local_tz();
    let instance = Instance::of(row)?;
    let mut event = engine.event_from_input(input, Some(&row.event), cal)?;
    event.uid = row.uid.clone();
    let send = instance.send_updates();
    let scope = if instance.series {
        scope
    } else {
        EditScope::All
    };
    match scope {
        _ if !instance.series => {
            let mut update = mapping::calendar_update(&row.event, &event, &me);
            if mapping::times_differ(&row.event, &event, tz) {
                update.timing = Some(mapping::event_timing(&event.start, &event.end, tz));
            }
            let start = mapping::start_date(&event, tz);
            if !mapping::same_rule(event.rrule.as_deref(), row.event.rrule.as_deref(), start) {
                update.recurrence = Some(
                    event
                        .rrule
                        .as_deref()
                        .map(|rule| mapping::recurrence(rule, start, &event, tz))
                        .transpose()?,
                );
            }
            apply_update(&ews, &instance.id, &update, send).await?;
        }
        EditScope::This => {
            let update = mapping::calendar_update(&row.event, &event, &me);
            apply_update(&ews, &instance.id, &update, send).await?;
            if mapping::times_differ(&row.event, &event, tz) {
                move_occurrence(&ews, &instance.id, &event, instance.notifies()).await?;
            }
        }
        EditScope::All => {
            let master = master_of(&ews, &instance).await?;
            update_series(&ews, &master, row, &event, &me, send).await?;
        }
        EditScope::Following => {
            let master = master_of(&ews, &instance).await?;
            if is_first(&master, row) {
                update_series(&ews, &master, row, &event, &me, send).await?;
            } else {
                if instance.declines() {
                    return Err(Error::Invalid(
                        "Only the organizer can change later occurrences of a meeting".into(),
                    ));
                }
                end_series_before(&ews, &master, row, send).await?;
                // A new series from this occurrence on, with the changes.
                let mut fresh = event.clone();
                fresh.uid = envelope_ical::new_uid();
                fresh.recurrence_id = None;
                fresh.rrule = input
                    .rrule
                    .clone()
                    .filter(|r| !r.trim().is_empty())
                    .or_else(|| mapping::series_rule(&master));
                let item = mapping::new_calendar_item(&fresh, &me, instance.organizer, tz)?;
                let invites = instance.notifies()
                    && (!item.required_attendees.is_empty()
                        || !item.optional_attendees.is_empty()
                        || !item.resources.is_empty());
                let send = if invites {
                    SendInvitations::SendToAllAndSaveCopy
                } else {
                    SendInvitations::SendToNone
                };
                ews.client
                    .create_calendar_item(FolderId::new(cal.remote_id.clone()), &item, send)
                    .await
                    .map_err(|e| ews.err(e))?;
            }
        }
    }
    sync_calendar(engine, &ews, &account, cal).await?;
    Ok(())
}

/// Applies changes made to one occurrence to its whole series; a changed
/// time shifts the series by as much.
async fn update_series(
    ews: &EwsMail,
    master: &ews::CalendarItem,
    occurrence: &EventRow,
    event: &Event,
    me: &str,
    send: SendUpdates,
) -> Result<()> {
    let tz = local_tz();
    let mut update = mapping::calendar_update(&occurrence.event, event, me);
    if mapping::times_differ(&occurrence.event, event, tz) {
        update.timing = Some(mapping::shifted_timing(
            master,
            &occurrence.event,
            event,
            tz,
        ));
    }
    let first = master
        .recurrence
        .as_ref()
        .map(|r| r.range.start())
        .unwrap_or_else(|| mapping::start_date(event, tz));
    if !mapping::same_rule(
        event.rrule.as_deref(),
        occurrence.event.rrule.as_deref(),
        first,
    ) {
        update.recurrence = Some(
            event
                .rrule
                .as_deref()
                .map(|rule| mapping::recurrence(rule, first, event, tz))
                .transpose()?,
        );
    }
    apply_update(ews, &master.id.id, &update, send).await
}

/// Removes an event, one occurrence, this and the following occurrences or
/// a whole series. Someone else's meeting is declined instead, which tells
/// the organizer and removes it.
pub(crate) async fn write_delete(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    scope: EditScope,
) -> Result<()> {
    let account = account_of(engine, cal)?;
    let lock = calendar_lock(account.id);
    let _guard = lock.lock().await;
    let ews = EwsMail::for_account(engine, &account).await?;
    let instance = Instance::of(row)?;
    let scope = if instance.series {
        scope
    } else {
        EditScope::All
    };
    let cancellations = if instance.notifies() {
        SendCancellations::SendToAllAndSaveCopy
    } else {
        SendCancellations::SendToNone
    };
    match scope {
        EditScope::Following if instance.declines() => {
            return Err(Error::Invalid(
                "Only the organizer can remove later occurrences of a meeting; decline it instead"
                    .into(),
            ));
        }
        EditScope::Following => {
            let master = master_of(&ews, &instance).await?;
            if is_first(&master, row) {
                delete_item(&ews, &master.id.id, cancellations).await?;
            } else {
                end_series_before(&ews, &master, row, instance.send_updates()).await?;
            }
        }
        EditScope::This => {
            if instance.declines() {
                decline(&ews, &instance.id).await?;
            } else {
                delete_item(&ews, &instance.id, cancellations).await?;
            }
        }
        EditScope::All => {
            let target = if instance.series {
                master_of(&ews, &instance).await?.id.id
            } else {
                instance.id.clone()
            };
            if instance.declines() {
                decline(&ews, &target).await?;
            } else {
                delete_item(&ews, &target, cancellations).await?;
            }
        }
    }
    sync_calendar(engine, &ews, &account, cal).await?;
    Ok(())
}

async fn delete_item(ews: &EwsMail, id: &str, cancellations: SendCancellations) -> Result<()> {
    match ews
        .client
        .delete_calendar_item(
            ItemId::new(id.to_string()),
            DeleteType::MoveToDeletedItems,
            cancellations,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(e) if e.is_not_found() => Ok(()),
        Err(e) => Err(ews.err(e)),
    }
}

async fn decline(ews: &EwsMail, id: &str) -> Result<()> {
    ews.client
        .respond_to_meeting(
            &ItemId::new(id.to_string()),
            ews::MeetingResponse::Decline,
            None,
            true,
        )
        .await
        .map(drop)
        .map_err(|e| ews.err(e))
}

/// Answers a meeting in the calendar; a series is answered as a whole.
pub(crate) async fn write_respond(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    status: PartStat,
    comment: Option<&str>,
) -> Result<()> {
    let account = account_of(engine, cal)?;
    let lock = calendar_lock(account.id);
    let _guard = lock.lock().await;
    let ews = EwsMail::for_account(engine, &account).await?;
    let instance = Instance::of(row)?;
    let target = if instance.series {
        master_of(&ews, &instance).await?.id.id
    } else {
        instance.id.clone()
    };
    ews.client
        .respond_to_meeting(
            &ItemId::new(target),
            mapping::meeting_response(status),
            comment,
            true,
        )
        .await
        .map_err(|e| ews.err(e))?;
    sync_calendar(engine, &ews, &account, cal).await?;
    Ok(())
}

/// Answers a meeting request received by an Exchange account through
/// Exchange, which also files the meeting in the calendar. Returns false for
/// messages Exchange cannot answer, so the iCalendar reply path takes over.
pub(crate) async fn respond_to_request(
    engine: &Engine,
    message_id: i64,
    status: PartStat,
    comment: Option<&str>,
) -> Result<bool> {
    let (account_id, remote, method): (i64, Option<String>, Option<String>) =
        engine.db.read(|c| {
            Ok(c.query_row(
                "SELECT account_id, remote_id, calendar_method FROM messages WHERE id = ?",
                [message_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })?;
    let account = engine.account_row(account_id)?;
    let Some(remote) = remote else {
        return Ok(false);
    };
    if account.kind != AccountKind::Exchange
        || method
            .as_deref()
            .is_some_and(|m| !m.eq_ignore_ascii_case("REQUEST"))
    {
        return Ok(false);
    }
    let ews = EwsMail::for_account(engine, &account).await?;
    if let Err(e) = ews
        .client
        .respond_to_meeting(
            &ItemId::new(remote),
            mapping::meeting_response(status),
            comment,
            true,
        )
        .await
    {
        let error = ews.err(e);
        if error.is_auth() || error.is_transient() {
            return Err(error);
        }
        tracing::info!(
            message_id,
            "Exchange could not answer the meeting request: {error}"
        );
        return Ok(false);
    }
    // The request leaves the inbox and the calendar changes.
    engine.wake(account_id);
    match sync_with(engine, &account, &ews).await {
        Ok(changed) if !changed.is_empty() => engine.emit(EngineEvent::CalendarChanged {
            calendar_ids: changed,
        }),
        Ok(_) => {}
        Err(e) => tracing::warn!(account_id, "calendar refresh after answering failed: {e}"),
    }
    Ok(true)
}
