//! Calendars and events.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use envelope_ical::{Event, EventTime};

/// Per-calendar sync bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CalendarSyncState {
    pub ctag: Option<String>,
    pub sync_token: Option<String>,
    /// Graph delta link / Google sync token.
    pub cursor: Option<String>,
    pub last_sync: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct CalendarRow {
    pub id: i64,
    pub account_id: Option<i64>,
    pub kind: String,
    pub remote_id: String,
    pub name: String,
    pub color: String,
    pub read_only: bool,
    pub is_default: bool,
    pub visible: bool,
    pub timezone: Option<String>,
    pub sync_state: CalendarSyncState,
}

fn calendar_from_row(r: &Row<'_>) -> rusqlite::Result<CalendarRow> {
    let state: String = r.get("sync_state")?;
    Ok(CalendarRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        kind: r.get("kind")?,
        remote_id: r.get("remote_id")?,
        name: r.get("name")?,
        color: r.get("color")?,
        read_only: r.get::<_, i64>("read_only")? != 0,
        is_default: r.get::<_, i64>("is_default")? != 0,
        visible: r.get::<_, i64>("visible")? != 0,
        timezone: r.get("timezone")?,
        sync_state: serde_json::from_str(&state).unwrap_or_default(),
    })
}

pub fn list_calendars(c: &Connection) -> Result<Vec<CalendarRow>> {
    let mut st = c.prepare_cached(
        "SELECT c.* FROM calendars c LEFT JOIN accounts a ON a.id = c.account_id
         WHERE c.account_id IS NULL OR a.enabled = 1
         ORDER BY coalesce(a.sort_order, 999), c.account_id, c.is_default DESC, c.sort_order, c.name COLLATE NOCASE",
    )?;
    let rows = st.query_map([], calendar_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn account_calendars(c: &Connection, account_id: i64) -> Result<Vec<CalendarRow>> {
    let mut st =
        c.prepare_cached("SELECT * FROM calendars WHERE account_id = ? ORDER BY sort_order, name")?;
    let rows = st.query_map([account_id], calendar_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_calendar(c: &Connection, id: i64) -> Result<CalendarRow> {
    c.query_row(
        "SELECT * FROM calendars WHERE id = ?",
        [id],
        calendar_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("calendar {id}")))
}

/// Fields of a calendar as reported by the server.
#[derive(Debug, Clone)]
pub struct CalendarUpsert {
    pub remote_id: String,
    pub name: String,
    pub color: String,
    pub read_only: bool,
    pub is_default: bool,
    pub timezone: Option<String>,
    pub sort_order: i64,
    pub visible_default: bool,
}

/// Creates/updates calendars of an account and removes the ones that vanished.
pub fn sync_calendars(
    tx: &Transaction<'_>,
    account_id: i64,
    kind: &str,
    list: &[CalendarUpsert],
) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    for c in list {
        tx.execute(
            "INSERT INTO calendars (account_id, kind, remote_id, name, color, read_only, is_default, timezone, sort_order, visible)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (account_id, remote_id) DO UPDATE SET
               name = excluded.name, read_only = excluded.read_only, is_default = excluded.is_default,
               timezone = excluded.timezone, sort_order = excluded.sort_order, kind = excluded.kind,
               color = CASE WHEN json_extract(calendars.sync_state, '$.colorOverride') = 1 THEN calendars.color ELSE excluded.color END",
            params![
                account_id,
                kind,
                c.remote_id,
                c.name,
                c.color,
                c.read_only as i64,
                c.is_default as i64,
                c.timezone,
                c.sort_order,
                c.visible_default as i64
            ],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM calendars WHERE account_id = ? AND remote_id = ?",
            params![account_id, c.remote_id],
            |r| r.get(0),
        )?;
        ids.push(id);
    }
    let keep: std::collections::HashSet<&str> = list.iter().map(|c| c.remote_id.as_str()).collect();
    let existing: Vec<(i64, String)> = {
        let mut st = tx.prepare("SELECT id, remote_id FROM calendars WHERE account_id = ?")?;
        st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, remote) in existing {
        if !keep.contains(remote.as_str()) {
            tx.execute("DELETE FROM calendars WHERE id = ?", [id])?;
        }
    }
    Ok(ids)
}

pub fn set_calendar_sync_state(
    tx: &Transaction<'_>,
    id: i64,
    state: &CalendarSyncState,
) -> Result<()> {
    tx.execute(
        "UPDATE calendars SET sync_state = json_patch(sync_state, ?) WHERE id = ?",
        params![serde_json::to_string(state)?, id],
    )?;
    Ok(())
}

/// A stored event row.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub calendar_id: i64,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub uid: String,
    pub recurrence_id: Option<EventTime>,
    pub start_ts: i64,
    pub end_ts: i64,
    pub is_recurring: bool,
    pub event: Event,
    pub raw: Option<String>,
    pub provider_data: Option<serde_json::Value>,
}

fn event_from_row(r: &Row<'_>) -> rusqlite::Result<EventRow> {
    let data: String = r.get("data")?;
    let rid: Option<String> = r.get("recurrence_id")?;
    let pd: Option<String> = r.get("provider_data")?;
    let event: Event = serde_json::from_str(&data).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(EventRow {
        id: r.get("id")?,
        calendar_id: r.get("calendar_id")?,
        remote_id: r.get("remote_id")?,
        etag: r.get("etag")?,
        uid: r.get("uid")?,
        recurrence_id: rid.and_then(|s| serde_json::from_str(&s).ok()),
        start_ts: r.get("start_ts")?,
        end_ts: r.get("end_ts")?,
        is_recurring: r.get::<_, i64>("is_recurring")? != 0,
        event,
        raw: r.get("raw")?,
        provider_data: pd.and_then(|s| serde_json::from_str(&s).ok()),
    })
}

pub fn get_event(c: &Connection, id: i64) -> Result<EventRow> {
    c.query_row("SELECT * FROM events WHERE id = ?", [id], event_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("event {id}")))
}

/// All rows sharing a UID in a calendar (series master + overrides).
pub fn series_rows(c: &Connection, calendar_id: i64, uid: &str) -> Result<Vec<EventRow>> {
    let mut st = c.prepare_cached("SELECT * FROM events WHERE calendar_id = ? AND uid = ? ORDER BY recurrence_id IS NOT NULL, id")?;
    let rows = st.query_map(params![calendar_id, uid], event_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn rows_by_remote(c: &Connection, calendar_id: i64, remote_id: &str) -> Result<Vec<EventRow>> {
    let mut st =
        c.prepare_cached("SELECT * FROM events WHERE calendar_id = ? AND remote_id = ?")?;
    let rows = st.query_map(params![calendar_id, remote_id], event_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Rows whose UID matches in any calendar (invitations).
pub fn rows_by_uid(c: &Connection, uid: &str) -> Result<Vec<EventRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM events WHERE uid = ? ORDER BY recurrence_id IS NOT NULL, id",
    )?;
    let rows = st.query_map([uid], event_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `(remote_id, etag)` of every resource in a calendar.
pub fn remote_etags(c: &Connection, calendar_id: i64) -> Result<Vec<(String, Option<String>)>> {
    let mut st = c.prepare_cached(
        "SELECT remote_id, max(etag) FROM events WHERE calendar_id = ? AND remote_id IS NOT NULL GROUP BY remote_id",
    )?;
    let rows = st.query_map([calendar_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Data to store for one event.
pub struct EventWrite<'a> {
    pub calendar_id: i64,
    pub remote_id: Option<&'a str>,
    pub etag: Option<&'a str>,
    pub event: &'a Event,
    pub raw: Option<&'a str>,
    pub provider_data: Option<&'a serde_json::Value>,
    pub start_ts: i64,
    pub end_ts: i64,
    pub until_ts: Option<i64>,
    /// The row is a concrete instance even if it has a recurrence id (Graph).
    pub expanded: bool,
}

/// Inserts or replaces the row for (calendar, uid, recurrence-id).
pub fn upsert_event(tx: &Transaction<'_>, w: &EventWrite<'_>) -> Result<i64> {
    let rid = w
        .event
        .recurrence_id
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM events WHERE calendar_id = ? AND uid = ? AND recurrence_id IS ?",
            params![w.calendar_id, w.event.uid, rid],
            |r| r.get(0),
        )
        .optional()?;
    let is_recurring = w.event.is_recurring() && !w.expanded;
    let data = serde_json::to_string(w.event)?;
    let pd = w.provider_data.map(serde_json::to_string).transpose()?;
    match existing {
        Some(id) => {
            tx.execute(
                "UPDATE events SET remote_id = ?, etag = ?, start_ts = ?, end_ts = ?, until_ts = ?, all_day = ?,
                   is_recurring = ?, summary = ?, data = ?, raw = ?, provider_data = ?, local_state = 0 WHERE id = ?",
                params![
                    w.remote_id,
                    w.etag,
                    w.start_ts,
                    w.end_ts,
                    w.until_ts,
                    w.event.is_all_day() as i64,
                    is_recurring as i64,
                    w.event.summary,
                    data,
                    w.raw,
                    pd,
                    id
                ],
            )?;
            Ok(id)
        }
        None => {
            tx.execute(
                "INSERT INTO events (calendar_id, remote_id, etag, uid, recurrence_id, start_ts, end_ts, until_ts, all_day,
                   is_recurring, summary, data, raw, provider_data)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    w.calendar_id,
                    w.remote_id,
                    w.etag,
                    w.event.uid,
                    rid,
                    w.start_ts,
                    w.end_ts,
                    w.until_ts,
                    w.event.is_all_day() as i64,
                    is_recurring as i64,
                    w.event.summary,
                    data,
                    w.raw,
                    pd
                ],
            )?;
            Ok(tx.last_insert_rowid())
        }
    }
}

pub fn delete_by_remote(tx: &Transaction<'_>, calendar_id: i64, remote_id: &str) -> Result<usize> {
    Ok(tx.execute(
        "DELETE FROM events WHERE calendar_id = ? AND remote_id = ?",
        params![calendar_id, remote_id],
    )?)
}

pub fn delete_event_row(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("DELETE FROM events WHERE id = ?", [id])?;
    Ok(())
}

pub fn clear_calendar(tx: &Transaction<'_>, calendar_id: i64) -> Result<()> {
    tx.execute("DELETE FROM events WHERE calendar_id = ?", [calendar_id])?;
    Ok(())
}

/// Whether calendars of this kind store concrete occurrences (Microsoft
/// Graph and Exchange calendar views) instead of series masters plus
/// overrides: their rows with a recurrence id stand on their own.
pub fn stores_instances(kind: &str) -> bool {
    matches!(kind, "graph" | "ews")
}

/// Candidate rows for a time range: concrete events overlapping it, recurring
/// masters that may produce occurrences in it, and their overrides.
pub fn range_rows(c: &Connection, from: i64, to: i64) -> Result<Vec<EventRow>> {
    let mut st = c.prepare_cached(
        "SELECT e.* FROM events e JOIN calendars c ON c.id = e.calendar_id
         LEFT JOIN accounts a ON a.id = c.account_id
         WHERE c.visible = 1 AND (c.account_id IS NULL OR a.enabled = 1) AND e.local_state != 3 AND (
           (e.is_recurring = 0 AND (e.recurrence_id IS NULL OR c.kind IN ('graph', 'ews')) AND e.start_ts < ?2 AND e.end_ts > ?1)
           OR (e.is_recurring = 1 AND e.start_ts < ?2 AND (e.until_ts IS NULL OR e.until_ts > ?1))
           OR (e.recurrence_id IS NOT NULL AND c.kind NOT IN ('graph', 'ews') AND EXISTS (
                SELECT 1 FROM events m WHERE m.calendar_id = e.calendar_id AND m.uid = e.uid AND m.is_recurring = 1
                  AND m.start_ts < ?2 AND (m.until_ts IS NULL OR m.until_ts > ?1)))
         )",
    )?;
    let rows = st.query_map(params![from, to], event_from_row)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Rows with alarms that may fire soon (for reminders).
pub fn calendar_count(c: &Connection) -> Result<i64> {
    Ok(c.query_row("SELECT count(*) FROM calendars", [], |r| r.get(0))?)
}

pub fn create_local_calendar(tx: &Transaction<'_>, name: &str, color: &str) -> Result<i64> {
    let remote = format!("local-{}", uuid::Uuid::new_v4());
    tx.execute(
        "INSERT INTO calendars (account_id, kind, remote_id, name, color, is_default) VALUES (NULL, 'local', ?, ?, ?, 1)",
        params![remote, name, color],
    )?;
    Ok(tx.last_insert_rowid())
}
