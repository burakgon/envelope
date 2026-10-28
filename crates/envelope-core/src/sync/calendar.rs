//! Calendar synchronization and writes for CalDAV, Google Calendar,
//! Microsoft Graph and local calendars.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use envelope_ical::{EventStatus, EventTime, PartStat};

use super::AccountTokens;
use crate::calendar::{EditScope, EventInput, local_tz, to_ms};
use crate::db::calendar::{self as cdb, CalendarRow, CalendarSyncState, CalendarUpsert, EventRow};
use crate::db::mail::{self, AccountRow};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{AccountKind, AuthMethod, EngineEvent};

fn dav_err(e: envelope_dav::DavError) -> Error {
    if e.is_transient() {
        Error::Network(e.to_string())
    } else if matches!(e, envelope_dav::DavError::Unauthorized) {
        Error::Auth(e.to_string())
    } else {
        Error::Other(e.to_string())
    }
}

fn google_err(e: envelope_google::Error) -> Error {
    if e.is_unauthorized() {
        Error::Auth(e.to_string())
    } else {
        Error::Other(e.to_string())
    }
}

fn graph_err(e: envelope_graph::GraphError) -> Error {
    if e.is_transient() {
        Error::Network(e.to_string())
    } else {
        Error::Other(e.to_string())
    }
}

pub(crate) fn google_client(
    engine: &Engine,
    account_id: i64,
) -> Result<envelope_google::GoogleClient> {
    envelope_google::GoogleClient::new(AccountTokens::new(engine, account_id)).map_err(google_err)
}

pub(crate) fn graph_client(
    engine: &Engine,
    account_id: i64,
) -> Result<envelope_graph::GraphClient> {
    envelope_graph::GraphClient::builder(AccountTokens::new(engine, account_id))
        .time_zone_resolver(envelope_ical::resolve_tzid)
        .build()
        .map_err(graph_err)
}

async fn dav_client(engine: &Engine, account: &AccountRow) -> Result<envelope_dav::DavClient> {
    let url = account
        .config
        .caldav_url
        .clone()
        .ok_or_else(|| Error::Invalid("no CalDAV server configured".into()))?;
    let auth = match account.config.auth {
        AuthMethod::Password => {
            let user = account
                .config
                .imap
                .as_ref()
                .map(|s| s.username.clone())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| account.email.clone());
            let password = engine
                .secrets
                .get(&crate::secrets::password_key(account.id))
                .await?
                .ok_or_else(|| Error::Auth("password missing".into()))?;
            envelope_dav::Auth::basic(user, password)
        }
        _ => envelope_dav::Auth::Bearer(AccountTokens::new(engine, account.id)),
    };
    envelope_dav::DavClient::builder(&url)
        .auth(auth)
        .accept_invalid_certs(account.config.accept_invalid_certs)
        .build()
        .map_err(dav_err)
}

/// Synchronizes all calendars of an account.
pub(crate) async fn sync_account(engine: &Engine, account: &AccountRow) -> Result<()> {
    match account.kind {
        AccountKind::Gmail => google_sync(engine, account).await,
        AccountKind::Microsoft => graph_sync(engine, account).await,
        AccountKind::Imap if account.config.caldav_url.is_some() => {
            caldav_sync(engine, account).await
        }
        AccountKind::Imap => Ok(()),
    }
}

/// Background loop keeping calendars fresh and firing reminders.
pub(crate) fn spawn_scheduler(engine: Engine) {
    tokio::spawn(async move {
        let mut next_sync = std::time::Instant::now();
        loop {
            if std::time::Instant::now() >= next_sync {
                let accounts = engine.db.read(mail::list_accounts).unwrap_or_default();
                tracing::debug!("calendar sync for {} accounts", accounts.len());
                for a in accounts.into_iter().filter(|a| a.enabled) {
                    if let Err(e) = sync_account(&engine, &a).await {
                        tracing::warn!(account = a.id, "calendar sync failed: {e}");
                    }
                }
                next_sync = std::time::Instant::now() + Duration::from_secs(5 * 60);
            }
            let now = now_ms();
            let since = std::mem::replace(&mut *crate::calendar::reminder_cursor().lock(), now);
            if let Err(e) = engine.check_reminders(since, now) {
                tracing::debug!("reminders: {e}");
            }
            tokio::select! {
                _ = engine.shutdown.cancelled() => return,
                _ = engine.calendar_wake.notified() => { next_sync = std::time::Instant::now(); }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    });
}

// ---------------------------------------------------------------------------
// CalDAV

async fn caldav_sync(engine: &Engine, account: &AccountRow) -> Result<()> {
    let client = dav_client(engine, account).await?;
    let home = match account.state.get("caldavHome").and_then(|v| v.as_str()) {
        Some(h) => h.to_string(),
        None => {
            let url = account.config.caldav_url.clone().unwrap_or_default();
            let d = client
                .discover_from(&url, envelope_dav::ServiceType::CalDav)
                .await
                .map_err(dav_err)?;
            let home = d
                .calendar_home_set
                .first()
                .cloned()
                .ok_or_else(|| Error::Other("the server has no calendars".into()))?;
            let mut state = account.state.clone();
            if !state.is_object() {
                state = serde_json::json!({});
            }
            state["caldavHome"] = serde_json::Value::String(home.clone());
            engine
                .db
                .write(|tx| mail::set_account_state(tx, account.id, &state))?;
            home
        }
    };
    let remote = client.list_calendars(&home).await.map_err(dav_err)?;
    let remote: Vec<_> = remote.into_iter().filter(|c| c.components.vevent).collect();
    let upserts: Vec<CalendarUpsert> = remote
        .iter()
        .enumerate()
        .map(|(i, c)| CalendarUpsert {
            remote_id: c.href.clone(),
            name: c
                .display_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "Calendar".into()),
            color: c.color.clone().unwrap_or_else(|| account.color.clone()),
            read_only: c.read_only,
            is_default: i == 0,
            timezone: None,
            sort_order: c.order.map(i64::from).unwrap_or(i as i64),
            visible_default: true,
        })
        .collect();
    let account_id = account.id;
    engine
        .db
        .write(|tx| cdb::sync_calendars(tx, account_id, "caldav", &upserts).map(|_| ()))?;
    let rows = engine.db.read(|c| cdb::account_calendars(c, account_id))?;
    let mut changed = Vec::new();
    for dav in &remote {
        let Some(row) = rows.iter().find(|r| r.remote_id == dav.href) else {
            continue;
        };
        match caldav_sync_calendar(engine, &client, row, dav).await {
            Ok(true) => changed.push(row.id),
            Ok(false) => {}
            Err(e) => tracing::warn!("calendar {} sync failed: {e}", row.name),
        }
    }
    if !changed.is_empty() {
        engine.emit(EngineEvent::CalendarChanged {
            calendar_ids: changed,
        });
    }
    Ok(())
}

async fn caldav_sync_calendar(
    engine: &Engine,
    client: &envelope_dav::DavClient,
    row: &CalendarRow,
    dav: &envelope_dav::Calendar,
) -> Result<bool> {
    let state = &row.sync_state;
    if state.last_sync.is_some() && dav.ctag.is_some() && state.ctag == dav.ctag {
        return Ok(false);
    }
    let local: HashMap<String, Option<String>> = engine
        .db
        .read(|c| cdb::remote_etags(c, row.id))?
        .into_iter()
        .collect();
    let mut sync_token = None;
    let (changed, removed): (Vec<String>, Vec<String>) = {
        let delta = if dav.supports_sync {
            match client
                .sync_collection(&row.remote_id, state.sync_token.as_deref())
                .await
            {
                Ok(d) => Some(d),
                Err(e) if e.requires_full_resync() => None,
                Err(e) => return Err(dav_err(e)),
            }
        } else {
            None
        };
        match delta {
            Some(d) => {
                sync_token = d.sync_token.clone();
                let changed = d
                    .changed
                    .into_iter()
                    .filter(|o| {
                        local
                            .get(&o.href)
                            .is_none_or(|etag| etag.is_none() || *etag != o.etag)
                    })
                    .map(|o| o.href)
                    .collect();
                (changed, d.removed)
            }
            None => {
                let server = client.list_etags(&row.remote_id).await.map_err(dav_err)?;
                let server_map: HashMap<&str, &Option<String>> =
                    server.iter().map(|o| (o.href.as_str(), &o.etag)).collect();
                let changed = server
                    .iter()
                    .filter(|o| {
                        local
                            .get(&o.href)
                            .is_none_or(|etag| etag.is_none() || *etag != o.etag)
                    })
                    .map(|o| o.href.clone())
                    .collect();
                let removed = local
                    .keys()
                    .filter(|h| !server_map.contains_key(h.as_str()))
                    .cloned()
                    .collect();
                (changed, removed)
            }
        }
    };
    let any = !changed.is_empty() || !removed.is_empty();
    if !changed.is_empty() {
        let result = client
            .multiget(&row.remote_id, &changed)
            .await
            .map_err(dav_err)?;
        for obj in result.objects {
            store_ics(engine, row.id, &obj.href, obj.etag.as_deref(), &obj.data)?;
        }
        for gone in result.missing {
            engine
                .db
                .write(|tx| cdb::delete_by_remote(tx, row.id, &gone).map(|_| ()))?;
        }
    }
    for href in &removed {
        engine
            .db
            .write(|tx| cdb::delete_by_remote(tx, row.id, href).map(|_| ()))?;
    }
    let new_state = CalendarSyncState {
        ctag: dav.ctag.clone(),
        sync_token: sync_token.or_else(|| dav.sync_token.clone()),
        cursor: None,
        last_sync: Some(now_ms()),
    };
    engine
        .db
        .write(|tx| cdb::set_calendar_sync_state(tx, row.id, &new_state))?;
    Ok(any)
}

/// Replaces the rows of one CalDAV resource with the events it contains.
fn store_ics(
    engine: &Engine,
    calendar_id: i64,
    href: &str,
    etag: Option<&str>,
    data: &str,
) -> Result<()> {
    let parsed = match envelope_ical::parse_calendar(data) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("unparseable calendar object {href}: {e}");
            return Ok(());
        }
    };
    engine
        .db
        .write(|tx| cdb::delete_by_remote(tx, calendar_id, href).map(|_| ()))?;
    for event in &parsed.events {
        engine.store_event(
            calendar_id,
            event,
            Some(href),
            etag,
            Some(data),
            None,
            false,
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Google Calendar

async fn google_sync(engine: &Engine, account: &AccountRow) -> Result<()> {
    let client = google_client(engine, account.id)?;
    let cals = client.list_calendars().await.map_err(google_err)?;
    let upserts: Vec<CalendarUpsert> = cals
        .iter()
        .filter(|c| !c.deleted)
        .enumerate()
        .map(|(i, c)| CalendarUpsert {
            remote_id: c.id.clone(),
            name: c.display_name().to_string(),
            color: c
                .background_color
                .clone()
                .unwrap_or_else(|| account.color.clone()),
            read_only: c.read_only,
            is_default: c.primary,
            timezone: c.time_zone.clone(),
            sort_order: if c.primary { 0 } else { i as i64 + 1 },
            visible_default: c.selected && !c.hidden,
        })
        .collect();
    let account_id = account.id;
    engine
        .db
        .write(|tx| cdb::sync_calendars(tx, account_id, "google", &upserts).map(|_| ()))?;
    let rows = engine.db.read(|c| cdb::account_calendars(c, account_id))?;
    let mut changed = Vec::new();
    for row in rows {
        match google_sync_calendar(engine, &client, &row).await {
            Ok(true) => changed.push(row.id),
            Ok(false) => {}
            Err(e) => tracing::warn!("google calendar {} sync failed: {e}", row.name),
        }
    }
    if !changed.is_empty() {
        engine.emit(EngineEvent::CalendarChanged {
            calendar_ids: changed,
        });
    }
    Ok(())
}

async fn google_sync_calendar(
    engine: &Engine,
    client: &envelope_google::GoogleClient,
    row: &CalendarRow,
) -> Result<bool> {
    use envelope_google::EventSyncOptions;
    let mut full = row.sync_state.cursor.is_none();
    let mut any = false;
    loop {
        let opts = match (&row.sync_state.cursor, full) {
            (Some(token), false) => EventSyncOptions::incremental(token.clone()),
            _ => EventSyncOptions::full().time_min(Utc::now() - chrono::Duration::days(365)),
        };
        if full {
            engine.db.write(|tx| cdb::clear_calendar(tx, row.id))?;
        }
        let mut sync = client.event_sync(row.remote_id.clone(), opts);
        let mut token = None;
        let mut expired = false;
        loop {
            match sync.next_page().await {
                Ok(Some(page)) => {
                    any |= !page.events.is_empty()
                        || !page.deleted_event_ids.is_empty()
                        || !page.cancelled_occurrences.is_empty();
                    for ge in &page.events {
                        let pd = serde_json::json!({ "id": ge.id, "recurringEventId": ge.recurring_event_id, "htmlLink": ge.html_link });
                        engine.store_event(
                            row.id,
                            &ge.event,
                            Some(&ge.id),
                            ge.etag.as_deref(),
                            None,
                            Some(&pd),
                            false,
                        )?;
                    }
                    for co in &page.cancelled_occurrences {
                        let master = engine
                            .db
                            .read(|c| cdb::rows_by_remote(c, row.id, &co.recurring_event_id))?;
                        if let Some(m) = master.into_iter().find(|r| r.is_recurring) {
                            let mut e = envelope_ical::Event::new(
                                m.uid.clone(),
                                co.original_start.clone(),
                                co.original_start.clone(),
                            );
                            e.recurrence_id = Some(co.original_start.clone());
                            e.status = EventStatus::Cancelled;
                            engine.store_event(
                                row.id,
                                &e,
                                Some(&co.id),
                                None,
                                None,
                                None,
                                false,
                            )?;
                        }
                    }
                    for id in &page.deleted_event_ids {
                        engine
                            .db
                            .write(|tx| cdb::delete_by_remote(tx, row.id, id).map(|_| ()))?;
                    }
                    if page.next_sync_token.is_some() {
                        token = page.next_sync_token.clone();
                    }
                }
                Ok(None) => break,
                Err(envelope_google::Error::SyncTokenExpired) => {
                    expired = true;
                    break;
                }
                Err(e) => return Err(google_err(e)),
            }
        }
        if expired && !full {
            full = true;
            continue;
        }
        let state = CalendarSyncState {
            cursor: token,
            last_sync: Some(now_ms()),
            ..Default::default()
        };
        engine
            .db
            .write(|tx| cdb::set_calendar_sync_state(tx, row.id, &state))?;
        return Ok(any);
    }
}

// ---------------------------------------------------------------------------
// Microsoft Graph

fn graph_window() -> Result<envelope_graph::TimeWindow> {
    envelope_graph::TimeWindow::new(
        Utc::now() - chrono::Duration::days(180),
        Utc::now() + chrono::Duration::days(400),
    )
    .map_err(graph_err)
}

async fn graph_sync(engine: &Engine, account: &AccountRow) -> Result<()> {
    let client = graph_client(engine, account.id)?;
    let cals = client.list_calendars().await.map_err(graph_err)?;
    let upserts: Vec<CalendarUpsert> = cals
        .iter()
        .enumerate()
        .map(|(i, c)| CalendarUpsert {
            remote_id: c.id.clone(),
            name: c.name.clone(),
            color: c.css_color().unwrap_or_else(|| account.color.clone()),
            read_only: !c.can_edit,
            is_default: c.is_default_calendar,
            timezone: None,
            sort_order: if c.is_default_calendar {
                0
            } else {
                i as i64 + 1
            },
            visible_default: true,
        })
        .collect();
    let account_id = account.id;
    engine
        .db
        .write(|tx| cdb::sync_calendars(tx, account_id, "graph", &upserts).map(|_| ()))?;
    let rows = engine.db.read(|c| cdb::account_calendars(c, account_id))?;
    let mut changed = Vec::new();
    for row in rows {
        match graph_sync_calendar(engine, &client, &row).await {
            Ok(()) => changed.push(row.id),
            Err(e) => tracing::warn!("graph calendar {} sync failed: {e}", row.name),
        }
    }
    if !changed.is_empty() {
        engine.emit(EngineEvent::CalendarChanged {
            calendar_ids: changed,
        });
    }
    Ok(())
}

async fn graph_sync_calendar(
    engine: &Engine,
    client: &envelope_graph::GraphClient,
    row: &CalendarRow,
) -> Result<()> {
    let window = graph_window()?;
    let events = client
        .calendar_view(Some(&row.remote_id), &window)
        .await
        .map_err(graph_err)?;
    engine.db.write(|tx| cdb::clear_calendar(tx, row.id))?;
    for ge in &events {
        let pd = serde_json::json!({ "id": ge.id, "seriesMasterId": ge.series_master_id, "isOrganizer": ge.is_organizer });
        engine.store_event(
            row.id,
            &ge.event,
            Some(&ge.id),
            ge.change_key.as_deref(),
            None,
            Some(&pd),
            true,
        )?;
    }
    let state = CalendarSyncState {
        last_sync: Some(now_ms()),
        ..Default::default()
    };
    engine
        .db
        .write(|tx| cdb::set_calendar_sync_state(tx, row.id, &state))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Writes

fn provider_id(row: &EventRow) -> Option<String> {
    row.provider_data
        .as_ref()
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| row.remote_id.clone())
}

fn account_of(engine: &Engine, cal: &CalendarRow) -> Result<AccountRow> {
    let id = cal
        .account_id
        .ok_or_else(|| Error::Invalid("local calendar".into()))?;
    engine.account_row(id)
}

/// Creates an event on the server (or locally) and stores it.
pub(crate) async fn write_create(
    engine: &Engine,
    cal: &CalendarRow,
    event: &envelope_ical::Event,
    conference: bool,
) -> Result<i64> {
    match cal.kind.as_str() {
        "local" | "subscription" => {
            engine.store_event(cal.id, event, None, None, None, None, false)
        }
        "caldav" => {
            let account = account_of(engine, cal)?;
            let client = dav_client(engine, &account).await?;
            let ics =
                envelope_ical::create_calendar(event).map_err(|e| Error::Other(e.to_string()))?;
            let href = envelope_dav::object_href(
                &cal.remote_id,
                &event.uid,
                envelope_dav::ObjectKind::Calendar,
            )
            .map_err(dav_err)?;
            let etag = client
                .put(&href, &ics, envelope_dav::Precondition::IfNoneMatch)
                .await
                .map_err(dav_err)?;
            if !event.attendees.is_empty() {
                send_itip(engine, &account, event, "REQUEST").await?;
            }
            engine.store_event(
                cal.id,
                event,
                Some(&href),
                etag.as_deref(),
                Some(&ics),
                None,
                false,
            )
        }
        "google" => {
            let account = account_of(engine, cal)?;
            let client = google_client(engine, account.id)?;
            let opts = envelope_google::WriteOptions {
                send_updates: Some(if event.attendees.is_empty() {
                    envelope_google::SendUpdates::None
                } else {
                    envelope_google::SendUpdates::All
                }),
                add_google_meet: conference,
                ..Default::default()
            };
            let ge = client
                .insert_event(
                    &cal.remote_id,
                    &envelope_google::GoogleEvent::new(event.clone()),
                    &opts,
                )
                .await
                .map_err(google_err)?;
            let pd = serde_json::json!({ "id": ge.id, "recurringEventId": ge.recurring_event_id, "htmlLink": ge.html_link });
            engine.store_event(
                cal.id,
                &ge.event,
                Some(&ge.id),
                ge.etag.as_deref(),
                None,
                Some(&pd),
                false,
            )
        }
        "graph" => {
            let account = account_of(engine, cal)?;
            let client = graph_client(engine, account.id)?;
            let opts = envelope_graph::EventWriteOptions {
                online_meeting: conference
                    .then_some(envelope_graph::OnlineMeetingProvider::TeamsForBusiness),
                ..Default::default()
            };
            let ge = client
                .create_event(Some(&cal.remote_id), event, &opts)
                .await
                .map_err(graph_err)?;
            if event.is_recurring() {
                // Graph calendars store expanded instances; fetch them.
                graph_sync_calendar(engine, &client, cal).await?;
                return Ok(0);
            }
            let pd = serde_json::json!({ "id": ge.id, "seriesMasterId": ge.series_master_id, "isOrganizer": ge.is_organizer });
            engine.store_event(
                cal.id,
                &ge.event,
                Some(&ge.id),
                ge.change_key.as_deref(),
                None,
                Some(&pd),
                true,
            )
        }
        other => Err(Error::Invalid(format!("unsupported calendar type {other}"))),
    }
}

/// UNTIL value that ends a series just before `rid`.
fn until_before(rid: &EventTime) -> String {
    match rid {
        EventTime::Date { date } => date
            .pred_opt()
            .unwrap_or(*date)
            .format("%Y%m%d")
            .to_string(),
        other => {
            let ms = to_ms(other) - 1000;
            chrono::DateTime::from_timestamp_millis(ms)
                .unwrap_or_else(Utc::now)
                .format("%Y%m%dT%H%M%SZ")
                .to_string()
        }
    }
}

fn rrule_with_until(rrule: &str, until: &str) -> String {
    let parts: Vec<&str> = rrule
        .split(';')
        .filter(|p| {
            !p.to_ascii_uppercase().starts_with("UNTIL=")
                && !p.to_ascii_uppercase().starts_with("COUNT=")
        })
        .collect();
    format!("{};UNTIL={until}", parts.join(";"))
}

pub(crate) async fn write_update(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    rid: Option<EventTime>,
    scope: EditScope,
    input: &EventInput,
) -> Result<()> {
    let series = row.is_recurring && rid.is_some();
    let scope = if series { scope } else { EditScope::All };

    if scope == EditScope::Following {
        let rid = rid.clone().unwrap_or_else(|| row.event.start.clone());
        // End the old series before this occurrence and start a new one.
        let mut master = row.event.clone();
        if let Some(rule) = &master.rrule {
            master.rrule = Some(rrule_with_until(rule, &until_before(&rid)));
        }
        replace_master(engine, cal, row, &master).await?;
        let mut fresh = input.clone();
        fresh.rrule = input.rrule.clone().or(row.event.rrule.clone());
        let mut event = engine.event_from_input(&fresh, None, cal)?;
        event.attendees = row.event.attendees.clone();
        event.organizer = row.event.organizer.clone();
        write_create(engine, cal, &event, false).await?;
        return Ok(());
    }

    match scope {
        EditScope::This => {
            let rid = rid.expect("series edits have a recurrence id");
            let existing = engine
                .db
                .read(|c| cdb::series_rows(c, row.calendar_id, &row.uid))?
                .into_iter()
                .find(|r| r.recurrence_id.as_ref().map(to_ms) == Some(to_ms(&rid)));
            let mut base = match &existing {
                Some(o) => o.event.clone(),
                None => {
                    let mut e = row.event.clone();
                    e.rrule = None;
                    e.rdates.clear();
                    e.exdates.clear();
                    e
                }
            };
            base.recurrence_id = Some(rid.clone());
            let mut single = input.clone();
            single.rrule = None;
            let mut event = engine.event_from_input(&single, Some(&base), cal)?;
            event.recurrence_id = Some(rid.clone());
            event.uid = row.uid.clone();
            write_override(engine, cal, row, existing.as_ref(), &event).await
        }
        _ => {
            let mut event = engine.event_from_input(input, Some(&row.event), cal)?;
            event.uid = row.uid.clone();
            if row.is_recurring && input.rrule.is_none() && cal.kind != "graph" {
                // Keep the recurrence when the form did not touch it.
                event.rrule = row.event.rrule.clone();
            }
            replace_master(engine, cal, row, &event).await
        }
    }
}

/// Writes a changed series master / single event.
async fn replace_master(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    event: &envelope_ical::Event,
) -> Result<()> {
    match cal.kind.as_str() {
        "local" | "subscription" => {
            engine.store_event(cal.id, event, None, None, None, None, false)?;
        }
        "caldav" => {
            let account = account_of(engine, cal)?;
            let client = dav_client(engine, &account).await?;
            let href = row
                .remote_id
                .clone()
                .ok_or_else(|| Error::Invalid("event has no server copy".into()))?;
            let raw = row.raw.clone().unwrap_or_default();
            let ics = if raw.is_empty() {
                envelope_ical::create_calendar(event)
            } else {
                envelope_ical::upsert_event(&raw, event)
            }
            .map_err(|e| Error::Other(e.to_string()))?;
            let cond = row
                .etag
                .clone()
                .map(envelope_dav::Precondition::IfMatch)
                .unwrap_or_default();
            let etag = client.put(&href, &ics, cond).await.map_err(dav_err)?;
            if !event.attendees.is_empty()
                && event
                    .organizer
                    .as_ref()
                    .is_some_and(|o| o.email.eq_ignore_ascii_case(&account.email))
            {
                send_itip(engine, &account, event, "REQUEST").await?;
            }
            store_ics(engine, cal.id, &href, etag.as_deref(), &ics)?;
        }
        "google" => {
            let account = account_of(engine, cal)?;
            let client = google_client(engine, account.id)?;
            let mut ge = envelope_google::GoogleEvent::new(event.clone());
            ge.id =
                provider_id(row).ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            let opts = envelope_google::WriteOptions {
                send_updates: Some(if event.attendees.is_empty() {
                    envelope_google::SendUpdates::None
                } else {
                    envelope_google::SendUpdates::All
                }),
                ..Default::default()
            };
            let saved = client
                .update_event(&cal.remote_id, &ge, &opts)
                .await
                .map_err(google_err)?;
            let pd = serde_json::json!({ "id": saved.id, "recurringEventId": saved.recurring_event_id, "htmlLink": saved.html_link });
            engine.store_event(
                cal.id,
                &saved.event,
                Some(&saved.id),
                saved.etag.as_deref(),
                None,
                Some(&pd),
                false,
            )?;
        }
        "graph" => {
            let account = account_of(engine, cal)?;
            let client = graph_client(engine, account.id)?;
            let master_id = row
                .provider_data
                .as_ref()
                .and_then(|p| p.get("seriesMasterId"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            match master_id {
                Some(mid) => {
                    let master = client.get_series_master(&mid).await.map_err(graph_err)?;
                    let mut new = master.event.clone();
                    new.summary = event.summary.clone();
                    new.description = event.description.clone();
                    new.location = event.location.clone();
                    new.attendees = event.attendees.clone();
                    new.alarms = event.alarms.clone();
                    if let Some(rule) = &event.rrule {
                        new.rrule = Some(rule.clone());
                    }
                    client
                        .update_event_diff(&mid, &master.event, &new, &Default::default())
                        .await
                        .map_err(graph_err)?;
                }
                None => {
                    let id = provider_id(row)
                        .ok_or_else(|| Error::Invalid("event has no server id".into()))?;
                    client
                        .update_event_diff(&id, &row.event, event, &Default::default())
                        .await
                        .map_err(graph_err)?;
                }
            }
            graph_sync_calendar(engine, &client, cal).await?;
        }
        other => return Err(Error::Invalid(format!("unsupported calendar type {other}"))),
    }
    Ok(())
}

/// Writes one changed occurrence of a series.
async fn write_override(
    engine: &Engine,
    cal: &CalendarRow,
    master: &EventRow,
    existing: Option<&EventRow>,
    event: &envelope_ical::Event,
) -> Result<()> {
    match cal.kind.as_str() {
        "local" | "subscription" => {
            engine.store_event(cal.id, event, None, None, None, None, false)?;
        }
        "caldav" => replace_master(engine, cal, master, event).await?,
        "google" => {
            let account = account_of(engine, cal)?;
            let client = google_client(engine, account.id)?;
            let master_id = provider_id(master)
                .ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            let rid = event.recurrence_id.clone().expect("override");
            let mut instance = match existing.and_then(provider_id) {
                Some(id) => match client
                    .get_event(&cal.remote_id, &id)
                    .await
                    .map_err(google_err)?
                {
                    envelope_google::EventChange::Upsert(ge) => Some(*ge),
                    _ => None,
                },
                None => client
                    .find_instance(&cal.remote_id, &master_id, &rid)
                    .await
                    .map_err(google_err)?,
            }
            .ok_or_else(|| Error::NotFound("occurrence".into()))?;
            instance.event = event.clone();
            let saved = client
                .update_event(
                    &cal.remote_id,
                    &instance,
                    &envelope_google::WriteOptions::default(),
                )
                .await
                .map_err(google_err)?;
            let pd = serde_json::json!({ "id": saved.id, "recurringEventId": saved.recurring_event_id, "htmlLink": saved.html_link });
            engine.store_event(
                cal.id,
                &saved.event,
                Some(&saved.id),
                saved.etag.as_deref(),
                None,
                Some(&pd),
                false,
            )?;
        }
        "graph" => {
            let account = account_of(engine, cal)?;
            let client = graph_client(engine, account.id)?;
            let row = existing.unwrap_or(master);
            let id =
                provider_id(row).ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            client
                .update_event_diff(&id, &row.event, event, &Default::default())
                .await
                .map_err(graph_err)?;
            graph_sync_calendar(engine, &client, cal).await?;
        }
        other => return Err(Error::Invalid(format!("unsupported calendar type {other}"))),
    }
    Ok(())
}

pub(crate) async fn write_delete(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    rid: Option<EventTime>,
    scope: EditScope,
) -> Result<()> {
    let series = row.is_recurring && rid.is_some();
    let scope = if series { scope } else { EditScope::All };
    match (cal.kind.as_str(), scope) {
        (_, EditScope::Following) => {
            let rid = rid.expect("series");
            let mut master = row.event.clone();
            if let Some(rule) = &master.rrule {
                master.rrule = Some(rrule_with_until(rule, &until_before(&rid)));
            }
            replace_master(engine, cal, row, &master).await?;
        }
        ("local" | "subscription", EditScope::This) => {
            let rid = rid.expect("series");
            let mut master = row.event.clone();
            master.exdates.push(rid.clone());
            engine.store_event(cal.id, &master, None, None, None, None, false)?;
            let overrides = engine.db.read(|c| cdb::series_rows(c, cal.id, &row.uid))?;
            for o in overrides
                .into_iter()
                .filter(|o| o.recurrence_id.as_ref().map(to_ms) == Some(to_ms(&rid)))
            {
                engine.db.write(|tx| cdb::delete_event_row(tx, o.id))?;
            }
        }
        ("local" | "subscription", _) => {
            let rows = engine.db.read(|c| cdb::series_rows(c, cal.id, &row.uid))?;
            engine.db.write(|tx| {
                for r in rows {
                    cdb::delete_event_row(tx, r.id)?;
                }
                Ok(())
            })?;
        }
        ("caldav", EditScope::This) => {
            let rid = rid.expect("series");
            let account = account_of(engine, cal)?;
            let client = dav_client(engine, &account).await?;
            let href = row
                .remote_id
                .clone()
                .ok_or_else(|| Error::Invalid("event has no server copy".into()))?;
            let raw = row.raw.clone().unwrap_or_default();
            let ics = envelope_ical::add_exdate(&raw, &row.uid, &rid)
                .map_err(|e| Error::Other(e.to_string()))?;
            let cond = row
                .etag
                .clone()
                .map(envelope_dav::Precondition::IfMatch)
                .unwrap_or_default();
            let etag = client.put(&href, &ics, cond).await.map_err(dav_err)?;
            store_ics(engine, cal.id, &href, etag.as_deref(), &ics)?;
        }
        ("caldav", _) => {
            let account = account_of(engine, cal)?;
            let client = dav_client(engine, &account).await?;
            let href = row
                .remote_id
                .clone()
                .ok_or_else(|| Error::Invalid("event has no server copy".into()))?;
            let organizer_is_me = row
                .event
                .organizer
                .as_ref()
                .is_some_and(|o| o.email.eq_ignore_ascii_case(&account.email));
            client
                .delete(&href, row.etag.as_deref())
                .await
                .map_err(dav_err)?;
            if organizer_is_me && !row.event.attendees.is_empty() {
                send_itip(engine, &account, &row.event, "CANCEL").await?;
            }
            engine
                .db
                .write(|tx| cdb::delete_by_remote(tx, cal.id, &href).map(|_| ()))?;
        }
        ("google", EditScope::This) => {
            let rid = rid.expect("series");
            let account = account_of(engine, cal)?;
            let client = google_client(engine, account.id)?;
            let master_id =
                provider_id(row).ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            if let Some(instance) = client
                .find_instance(&cal.remote_id, &master_id, &rid)
                .await
                .map_err(google_err)?
            {
                client
                    .delete_event(
                        &cal.remote_id,
                        &instance.id,
                        &envelope_google::WriteOptions::default(),
                    )
                    .await
                    .map_err(google_err)?;
            }
            let mut e = envelope_ical::Event::new(row.uid.clone(), rid.clone(), rid.clone());
            e.recurrence_id = Some(rid);
            e.status = EventStatus::Cancelled;
            engine.store_event(cal.id, &e, None, None, None, None, false)?;
        }
        ("google", _) => {
            let account = account_of(engine, cal)?;
            let client = google_client(engine, account.id)?;
            let id =
                provider_id(row).ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            let opts = envelope_google::WriteOptions {
                send_updates: Some(envelope_google::SendUpdates::All),
                ..Default::default()
            };
            client
                .delete_event(&cal.remote_id, &id, &opts)
                .await
                .map_err(google_err)?;
            let rows = engine.db.read(|c| cdb::series_rows(c, cal.id, &row.uid))?;
            engine.db.write(|tx| {
                for r in rows {
                    cdb::delete_event_row(tx, r.id)?;
                }
                Ok(())
            })?;
        }
        ("graph", scope) => {
            let account = account_of(engine, cal)?;
            let client = graph_client(engine, account.id)?;
            let id = match scope {
                EditScope::All => row
                    .provider_data
                    .as_ref()
                    .and_then(|p| p.get("seriesMasterId"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| provider_id(row)),
                _ => provider_id(row),
            }
            .ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            client.delete_event(&id).await.map_err(graph_err)?;
            graph_sync_calendar(engine, &client, cal).await?;
        }
        (other, _) => return Err(Error::Invalid(format!("unsupported calendar type {other}"))),
    }
    Ok(())
}

pub(crate) async fn write_respond(
    engine: &Engine,
    cal: &CalendarRow,
    row: &EventRow,
    status: PartStat,
    comment: Option<&str>,
) -> Result<()> {
    match cal.kind.as_str() {
        "google" => {
            let account = account_of(engine, cal)?;
            let client = google_client(engine, account.id)?;
            let id =
                provider_id(row).ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            let opts = envelope_google::WriteOptions {
                send_updates: Some(envelope_google::SendUpdates::All),
                ..Default::default()
            };
            let saved = client
                .respond_to_event(&cal.remote_id, &id, status, comment, &opts)
                .await
                .map_err(google_err)?;
            let pd = serde_json::json!({ "id": saved.id, "recurringEventId": saved.recurring_event_id, "htmlLink": saved.html_link });
            engine.store_event(
                cal.id,
                &saved.event,
                Some(&saved.id),
                saved.etag.as_deref(),
                None,
                Some(&pd),
                false,
            )?;
        }
        "graph" => {
            let account = account_of(engine, cal)?;
            let client = graph_client(engine, account.id)?;
            let id =
                provider_id(row).ok_or_else(|| Error::Invalid("event has no server id".into()))?;
            let response = match status {
                PartStat::Accepted => envelope_graph::EventResponse::Accept,
                PartStat::Declined => envelope_graph::EventResponse::Decline,
                _ => envelope_graph::EventResponse::TentativelyAccept,
            };
            client
                .respond_to_event(&id, response, comment, true)
                .await
                .map_err(graph_err)?;
            graph_sync_calendar(engine, &client, cal).await?;
        }
        _ => {
            // CalDAV and local: update our copy and e-mail an iTIP reply to the organizer.
            let mut event = row.event.clone();
            let me: Vec<String> = engine
                .identities()?
                .into_iter()
                .map(|i| i.email.to_lowercase())
                .collect();
            let mut my_email = None;
            for a in event.attendees.iter_mut() {
                if me.contains(&a.email.to_lowercase()) {
                    a.partstat = status;
                    my_email = Some(a.email.clone());
                }
            }
            let source = match &row.raw {
                Some(r) => r.clone(),
                None => envelope_ical::create_calendar(&row.event)
                    .map_err(|e| Error::Other(e.to_string()))?,
            };
            if let (Some(email), Some(organizer)) = (my_email, row.event.organizer.clone()) {
                let reply = envelope_ical::build_reply(&source, &email, status, comment)
                    .map_err(|e| Error::Other(e.to_string()))?;
                let account_id = cal
                    .account_id
                    .or_else(|| {
                        engine
                            .db
                            .read(mail::list_accounts)
                            .ok()
                            .and_then(|a| a.first().map(|a| a.id))
                    })
                    .ok_or_else(|| Error::Invalid("no account to send the reply from".into()))?;
                send_reply(
                    engine,
                    account_id,
                    &row.event.summary,
                    &organizer.email,
                    status,
                    reply,
                )
                .await?;
            }
            replace_master(engine, cal, row, &event).await?;
        }
    }
    Ok(())
}

/// E-mails an iTIP message (invitation / cancellation) to the attendees.
async fn send_itip(
    engine: &Engine,
    account: &AccountRow,
    event: &envelope_ical::Event,
    method: &str,
) -> Result<()> {
    let ics = match method {
        "CANCEL" => envelope_ical::build_cancel(event, None),
        _ => envelope_ical::build_request(std::slice::from_ref(event)),
    }
    .map_err(|e| Error::Other(e.to_string()))?;
    let recipients: Vec<crate::model::Address> = event
        .attendees
        .iter()
        .filter(|a| !a.email.eq_ignore_ascii_case(&account.email))
        .map(|a| crate::model::Address::new(a.name.clone(), a.email.clone()))
        .collect();
    if recipients.is_empty() {
        return Ok(());
    }
    let when = chrono::DateTime::from_timestamp_millis(to_ms(&event.start))
        .map(|d| {
            d.with_timezone(&local_tz())
                .format("%A, %B %-d, %Y %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    let verb = if method == "CANCEL" {
        "Cancelled"
    } else {
        "Invitation"
    };
    let out = crate::compose::Outgoing {
        from: crate::model::Address::new(Some(account.name.clone()), account.email.clone()),
        to: recipients,
        subject: format!("{verb}: {} @ {when}", event.summary),
        html: format!(
            "<p><b>{}</b></p><p>{when}{}</p><p>{}</p>",
            html_escape::encode_text(&event.summary),
            event
                .location
                .as_deref()
                .map(|l| format!(" · {}", html_escape::encode_text(l)))
                .unwrap_or_default(),
            html_escape::encode_text(event.description.as_deref().unwrap_or(""))
        ),
        calendar: Some((method.to_string(), ics)),
        ..Default::default()
    };
    queue_mail(engine, account.id, out).await
}

pub(crate) async fn send_reply(
    engine: &Engine,
    account_id: i64,
    summary: &str,
    organizer: &str,
    status: PartStat,
    ics: String,
) -> Result<()> {
    let account = engine.account_row(account_id)?;
    let verb = match status {
        PartStat::Accepted => "Accepted",
        PartStat::Declined => "Declined",
        _ => "Tentative",
    };
    let out = crate::compose::Outgoing {
        from: crate::model::Address::new(Some(account.name.clone()), account.email.clone()),
        to: vec![crate::model::Address::new(None, organizer.to_string())],
        subject: format!("{verb}: {summary}"),
        html: format!(
            "<p>{} has {} this invitation.</p>",
            html_escape::encode_text(&account.name),
            verb.to_lowercase()
        ),
        calendar: Some(("REPLY".into(), ics)),
        ..Default::default()
    };
    queue_mail(engine, account_id, out).await
}

async fn queue_mail(engine: &Engine, account_id: i64, out: crate::compose::Outgoing) -> Result<()> {
    let rendered = crate::compose::render(&out)?;
    let meta = crate::db::misc::OutboxMeta {
        from: out.from.email.clone(),
        recipients: rendered.recipients.clone(),
        to: out.to.clone(),
        subject: out.subject.clone(),
        message_id: rendered.message_id.clone(),
        ..Default::default()
    };
    engine.db.write(|tx| {
        crate::db::misc::outbox_add(tx, account_id, None, &rendered.mime, &meta, now_ms())
    })?;
    engine.outbox_wake.notify_one();
    Ok(())
}
