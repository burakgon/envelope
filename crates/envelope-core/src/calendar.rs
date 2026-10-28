//! Calendar API used by the UI: calendars, occurrences in a range, editing,
//! invitations and reminders.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use envelope_ical::{
    Alarm, AlarmAction, Attendee, AttendeeRole, Event, EventStatus, EventTime, ExpandOptions, Organizer, PartStat,
    Transparency, TzContext,
};
use serde::{Deserialize, Serialize};

use crate::db::calendar::{self as cdb, CalendarRow, EventRow, EventWrite};
use crate::db::{mail, now_ms};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{Address, EngineEvent};

/// The user's time zone.
pub fn local_tz() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|n| n.parse::<Tz>().ok())
        .unwrap_or(Tz::UTC)
}

pub(crate) fn to_ms(t: &EventTime) -> i64 {
    envelope_ical::to_utc(t, &TzContext::new(), local_tz()).timestamp_millis()
}

/// End of the recurrence for range queries (None = unbounded or COUNT-limited).
pub(crate) fn until_ms(event: &Event) -> Option<i64> {
    let rule = event.rrule.as_deref()?;
    let until = rule
        .split(';')
        .find_map(|p| p.strip_prefix("UNTIL=").or_else(|| p.strip_prefix("until=")))?;
    let duration = to_ms(&event.end) - to_ms(&event.start);
    let ts = if until.len() == 8 {
        NaiveDate::parse_from_str(until, "%Y%m%d").ok()?.and_hms_opt(23, 59, 59)?.and_utc().timestamp_millis()
    } else {
        let s = until.trim_end_matches('Z');
        let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").ok()?;
        if until.ends_with('Z') {
            naive.and_utc().timestamp_millis()
        } else {
            local_tz().from_local_datetime(&naive).earliest()?.timestamp_millis()
        }
    };
    Some(ts + duration.max(0) + 86_400_000)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarView {
    pub id: i64,
    pub account_id: Option<i64>,
    pub kind: String,
    pub name: String,
    pub color: String,
    pub read_only: bool,
    pub is_default: bool,
    pub visible: bool,
}

impl From<&CalendarRow> for CalendarView {
    fn from(c: &CalendarRow) -> Self {
        CalendarView {
            id: c.id,
            account_id: c.account_id,
            kind: c.kind.clone(),
            name: c.name.clone(),
            color: c.color.clone(),
            read_only: c.read_only,
            is_default: c.is_default,
            visible: c.visible,
        }
    }
}

/// One occurrence of an event, ready to draw.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    /// Unique per occurrence: `<event row>:<start ms>`.
    pub key: String,
    pub event_id: i64,
    pub calendar_id: i64,
    pub uid: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// All-day events: first day and exclusive end day (`YYYY-MM-DD`).
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub location: Option<String>,
    pub color: String,
    pub status: EventStatus,
    pub transparent: bool,
    pub is_recurring: bool,
    pub recurrence_id: Option<EventTime>,
    pub my_status: Option<PartStat>,
    pub has_attendees: bool,
    pub conference_url: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDetail {
    pub occurrence: Occurrence,
    pub event: Event,
    pub calendar: CalendarView,
    pub organizer_is_me: bool,
    pub my_email: Option<String>,
}

/// Event fields edited in the UI.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventInput {
    pub calendar_id: i64,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    pub all_day: bool,
    /// Timed events: unix ms.
    #[serde(default)]
    pub start: i64,
    #[serde(default)]
    pub end: i64,
    /// All-day events: `YYYY-MM-DD`, end exclusive.
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub rrule: Option<String>,
    #[serde(default)]
    pub attendees: Vec<Address>,
    #[serde(default)]
    pub alarms: Vec<i64>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub transparent: bool,
    #[serde(default)]
    pub add_conference: bool,
}

/// Which occurrences of a series an edit applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditScope {
    This,
    Following,
    All,
}

fn date_str(t: &EventTime) -> Option<String> {
    match t {
        EventTime::Date { date } => Some(date.format("%Y-%m-%d").to_string()),
        _ => None,
    }
}

impl Engine {
    fn my_emails(&self) -> Result<Vec<String>> {
        Ok(self
            .identities()?
            .into_iter()
            .map(|i| i.email.to_lowercase())
            .chain(self.db.read(mail::list_accounts)?.into_iter().map(|a| a.email.to_lowercase()))
            .collect())
    }

    /// Makes sure there is somewhere to put events: a local calendar when no
    /// account provides calendars.
    pub fn ensure_local_calendar(&self) -> Result<()> {
        let has_remote = self.accounts()?.iter().any(|a| a.has_calendar && a.enabled);
        if has_remote {
            // Drop the placeholder local calendar once a real one exists and it is unused.
            self.db.write(|tx| {
                tx.execute(
                    "DELETE FROM calendars WHERE kind = 'local' AND account_id IS NULL
                     AND NOT EXISTS (SELECT 1 FROM events e WHERE e.calendar_id = calendars.id)
                     AND EXISTS (SELECT 1 FROM calendars r WHERE r.account_id IS NOT NULL)",
                    [],
                )?;
                Ok(())
            })?;
            return Ok(());
        }
        if self.db.read(cdb::calendar_count)? == 0 {
            self.db.write(|tx| cdb::create_local_calendar(tx, "Personal", "#6d5efc"))?;
        }
        Ok(())
    }

    pub fn calendars(&self) -> Result<Vec<CalendarView>> {
        self.ensure_local_calendar()?;
        Ok(self.db.read(cdb::list_calendars)?.iter().map(Into::into).collect())
    }

    pub fn set_calendar_visible(&self, id: i64, visible: bool) -> Result<()> {
        self.db.write(|tx| {
            tx.execute("UPDATE calendars SET visible = ? WHERE id = ?", rusqlite::params![visible as i64, id])?;
            Ok(())
        })?;
        self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![id] });
        Ok(())
    }

    pub fn set_calendar_color(&self, id: i64, color: &str) -> Result<()> {
        self.db.write(|tx| {
            tx.execute(
                "UPDATE calendars SET color = ?, sync_state = json_set(sync_state, '$.colorOverride', 1) WHERE id = ?",
                rusqlite::params![color, id],
            )?;
            Ok(())
        })?;
        self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![id] });
        Ok(())
    }

    fn occurrence(&self, row: &EventRow, event: &Event, cal: &CalendarRow, start: i64, end: i64, rid: Option<EventTime>, me: &[String]) -> Occurrence {
        let my_status = event
            .attendees
            .iter()
            .find(|a| me.contains(&a.email.to_lowercase()))
            .map(|a| a.partstat);
        Occurrence {
            key: format!("{}:{}", row.id, start),
            event_id: row.id,
            calendar_id: row.calendar_id,
            uid: event.uid.clone(),
            title: if event.summary.trim().is_empty() { "(No title)".into() } else { event.summary.clone() },
            start,
            end,
            all_day: event.is_all_day(),
            start_date: None,
            end_date: None,
            location: event.location.clone().filter(|l| !l.trim().is_empty()),
            color: event.color.clone().unwrap_or_else(|| cal.color.clone()),
            status: event.status,
            transparent: event.transparency == Transparency::Transparent,
            is_recurring: row.is_recurring || event.recurrence_id.is_some(),
            recurrence_id: rid,
            my_status,
            has_attendees: !event.attendees.is_empty(),
            conference_url: event.conference_url.clone(),
            read_only: cal.read_only,
        }
    }

    /// Occurrences of visible calendars overlapping `[from, to)`.
    pub fn occurrences(&self, from: i64, to: i64) -> Result<Vec<Occurrence>> {
        let rows = self.db.read(|c| cdb::range_rows(c, from, to))?;
        let calendars: HashMap<i64, CalendarRow> =
            self.db.read(cdb::list_calendars)?.into_iter().map(|c| (c.id, c)).collect();
        let me = self.my_emails()?;
        let tz = local_tz();
        let ctx = TzContext::new();
        let opts = ExpandOptions { default_tz: tz, ..Default::default() };
        let window_from = Utc.timestamp_millis_opt(from).single().unwrap_or_else(Utc::now);
        let window_to = Utc.timestamp_millis_opt(to).single().unwrap_or_else(Utc::now);

        let mut out = Vec::new();
        let mut masters: Vec<&EventRow> = Vec::new();
        let mut overrides: HashMap<(i64, &str), Vec<&EventRow>> = HashMap::new();
        for r in &rows {
            let Some(cal) = calendars.get(&r.calendar_id) else { continue };
            if r.is_recurring {
                masters.push(r);
            } else if r.recurrence_id.is_some() && cal.kind != "graph" {
                overrides.entry((r.calendar_id, r.uid.as_str())).or_default().push(r);
            } else {
                if r.event.status == EventStatus::Cancelled && r.recurrence_id.is_some() {
                    continue;
                }
                let mut o = self.occurrence(r, &r.event, cal, r.start_ts, r.end_ts, r.recurrence_id.clone(), &me);
                o.start_date = date_str(&r.event.start);
                o.end_date = date_str(&r.event.end);
                out.push(o);
            }
        }
        for m in masters {
            let Some(cal) = calendars.get(&m.calendar_id) else { continue };
            let ovs: Vec<&EventRow> = overrides.get(&(m.calendar_id, m.uid.as_str())).cloned().unwrap_or_default();
            let ov_events: Vec<Event> = ovs.iter().map(|r| r.event.clone()).collect();
            let expansion = envelope_ical::expand(&m.event, &ov_events, window_from, window_to, &ctx, &opts);
            for occ in expansion.occurrences {
                let (row, event) = match occ.override_index {
                    Some(i) => (ovs[i], &ov_events[i]),
                    None => (m, &m.event),
                };
                let mut o = self.occurrence(row, event, cal, occ.start.timestamp_millis(), occ.end.timestamp_millis(), occ.recurrence_id.clone(), &me);
                o.event_id = m.id;
                o.is_recurring = true;
                o.start_date = date_str(&occ.start_time);
                o.end_date = date_str(&occ.end_time);
                out.push(o);
            }
        }
        out.sort_by_key(|o| (o.start, -(o.end - o.start)));
        Ok(out)
    }

    pub fn event_detail(&self, event_id: i64, recurrence_id: Option<EventTime>) -> Result<EventDetail> {
        let row = self.db.read(|c| cdb::get_event(c, event_id))?;
        let cal = self.db.read(|c| cdb::get_calendar(c, row.calendar_id))?;
        let me = self.my_emails()?;
        // An occurrence of a series may have its own override row.
        let (event, start, end, src_row) = match &recurrence_id {
            Some(rid) if row.is_recurring => {
                let series = self.db.read(|c| cdb::series_rows(c, row.calendar_id, &row.uid))?;
                match series.iter().find(|r| r.recurrence_id.as_ref().map(to_ms) == Some(to_ms(rid))) {
                    Some(o) => (o.event.clone(), o.start_ts, o.end_ts, o.clone()),
                    None => {
                        let dur = row.end_ts - row.start_ts;
                        let start = to_ms(rid);
                        let mut e = row.event.clone();
                        e.recurrence_id = Some(rid.clone());
                        (e, start, start + dur, row.clone())
                    }
                }
            }
            _ => (row.event.clone(), row.start_ts, row.end_ts, row.clone()),
        };
        let mut occurrence = self.occurrence(&src_row, &event, &cal, start, end, recurrence_id.clone(), &me);
        occurrence.event_id = row.id;
        occurrence.start_date = date_str(&event.start);
        occurrence.end_date = date_str(&event.end);
        let organizer_is_me = event
            .organizer
            .as_ref()
            .is_none_or(|o| me.contains(&o.email.to_lowercase()));
        let my_email = cal
            .account_id
            .and_then(|a| self.db.read(|c| mail::get_account(c, a)).ok())
            .map(|a| a.email);
        Ok(EventDetail { occurrence, event, calendar: (&cal).into(), organizer_is_me, my_email })
    }

    /// Builds the canonical event from UI input.
    pub(crate) fn event_from_input(&self, input: &EventInput, base: Option<&Event>, cal: &CalendarRow) -> Result<Event> {
        let tz_name = input.timezone.clone().unwrap_or_else(|| local_tz().name().to_string());
        let tz: Tz = tz_name.parse().unwrap_or(local_tz());
        let (start, end) = if input.all_day {
            let s = NaiveDate::parse_from_str(input.start_date.as_deref().unwrap_or(""), "%Y-%m-%d")
                .map_err(|_| Error::Invalid("Invalid start date".into()))?;
            let e = input
                .end_date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .filter(|e| *e > s)
                .unwrap_or_else(|| s.succ_opt().unwrap_or(s));
            (EventTime::Date { date: s }, EventTime::Date { date: e })
        } else {
            let to_local = |ms: i64| -> Result<chrono::NaiveDateTime> {
                Ok(DateTime::from_timestamp_millis(ms)
                    .ok_or_else(|| Error::Invalid("Invalid time".into()))?
                    .with_timezone(&tz)
                    .naive_local())
            };
            if input.end < input.start {
                return Err(Error::Invalid("The event ends before it starts".into()));
            }
            (
                EventTime::Zoned { local: to_local(input.start)?, tzid: tz_name.clone() },
                EventTime::Zoned { local: to_local(input.end)?, tzid: tz_name.clone() },
            )
        };
        let mut e = base.cloned().unwrap_or_else(|| Event::new(envelope_ical::new_uid(), start.clone(), end.clone()));
        e.summary = input.summary.trim().to_string();
        e.description = input.description.clone().filter(|d| !d.trim().is_empty());
        e.location = input.location.clone().filter(|l| !l.trim().is_empty());
        e.start = start;
        e.end = end;
        e.rrule = input.rrule.clone().filter(|r| !r.trim().is_empty());
        e.color = input.color.clone();
        e.transparency = if input.transparent { Transparency::Transparent } else { Transparency::Opaque };
        e.alarms = input
            .alarms
            .iter()
            .map(|m| Alarm { action: AlarmAction::Display, minutes_before: *m, description: None })
            .collect();
        e.last_modified = Some(Utc::now());
        if e.created.is_none() {
            e.created = Some(Utc::now());
        }
        // Attendees: keep existing statuses, add new people as needs-action.
        let mut attendees: Vec<Attendee> = Vec::new();
        for a in &input.attendees {
            let email = a.email.trim().to_lowercase();
            if email.is_empty() {
                continue;
            }
            match e.attendees.iter().find(|x| x.email == email) {
                Some(existing) => attendees.push(existing.clone()),
                None => attendees.push(Attendee {
                    email,
                    name: a.name.clone(),
                    partstat: PartStat::NeedsAction,
                    role: AttendeeRole::Required,
                    rsvp: true,
                    ..Default::default()
                }),
            }
        }
        if !attendees.is_empty() && e.organizer.is_none() {
            let account = cal.account_id.and_then(|id| self.db.read(|c| mail::get_account(c, id)).ok());
            if let Some(acc) = account {
                e.organizer = Some(Organizer { email: acc.email.clone(), name: Some(acc.name.clone()) });
                if !attendees.iter().any(|a| a.email == acc.email.to_lowercase()) {
                    attendees.insert(0, Attendee {
                        email: acc.email.to_lowercase(),
                        name: Some(acc.name.clone()),
                        partstat: PartStat::Accepted,
                        role: AttendeeRole::Chair,
                        ..Default::default()
                    });
                }
            }
        }
        e.attendees = attendees;
        if base.is_some() {
            e.sequence += 1;
        }
        Ok(e)
    }

    /// Stores an event row locally (after a successful server write, or for local calendars).
    pub(crate) fn store_event(
        &self,
        calendar_id: i64,
        event: &Event,
        remote_id: Option<&str>,
        etag: Option<&str>,
        raw: Option<&str>,
        provider_data: Option<&serde_json::Value>,
        expanded: bool,
    ) -> Result<i64> {
        let start_ts = to_ms(&event.start);
        let end_ts = to_ms(&event.end).max(start_ts);
        let until_ts = if event.is_recurring() { until_ms(event) } else { None };
        self.db.write(|tx| {
            cdb::upsert_event(
                tx,
                &EventWrite {
                    calendar_id,
                    remote_id,
                    etag,
                    event,
                    raw,
                    provider_data,
                    start_ts,
                    end_ts,
                    until_ts,
                    expanded,
                },
            )
        })
    }

    /// Creates an event and returns its id.
    pub async fn create_event(&self, input: EventInput) -> Result<i64> {
        let cal = self.db.read(|c| cdb::get_calendar(c, input.calendar_id))?;
        if cal.read_only {
            return Err(Error::Invalid("This calendar is read-only".into()));
        }
        let event = self.event_from_input(&input, None, &cal)?;
        let id = crate::sync::calendar::write_create(self, &cal, &event, input.add_conference).await?;
        self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![cal.id] });
        Ok(id)
    }

    /// Updates an event (or occurrence/series according to `scope`).
    pub async fn update_event(&self, event_id: i64, recurrence_id: Option<EventTime>, scope: EditScope, input: EventInput) -> Result<()> {
        let row = self.db.read(|c| cdb::get_event(c, event_id))?;
        let cal = self.db.read(|c| cdb::get_calendar(c, row.calendar_id))?;
        if cal.read_only {
            return Err(Error::Invalid("This calendar is read-only".into()));
        }
        if input.calendar_id != row.calendar_id {
            // Moving between calendars: create in the new one, delete the old.
            let mut moved = input.clone();
            moved.rrule = moved.rrule.or(row.event.rrule.clone());
            self.create_event(moved).await?;
            return self.delete_event(event_id, None, EditScope::All).await;
        }
        crate::sync::calendar::write_update(self, &cal, &row, recurrence_id, scope, &input).await?;
        self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![cal.id] });
        Ok(())
    }

    /// Moves/resizes an occurrence (drag and drop).
    pub async fn move_event(&self, event_id: i64, recurrence_id: Option<EventTime>, scope: EditScope, start: i64, end: i64) -> Result<()> {
        let detail = self.event_detail(event_id, recurrence_id.clone())?;
        let e = &detail.event;
        let input = EventInput {
            calendar_id: detail.calendar.id,
            summary: e.summary.clone(),
            description: e.description.clone(),
            location: e.location.clone(),
            all_day: e.is_all_day(),
            start,
            end,
            start_date: if e.is_all_day() {
                DateTime::from_timestamp_millis(start).map(|d| d.with_timezone(&local_tz()).date_naive().format("%Y-%m-%d").to_string())
            } else {
                None
            },
            end_date: if e.is_all_day() {
                DateTime::from_timestamp_millis(end).map(|d| d.with_timezone(&local_tz()).date_naive().format("%Y-%m-%d").to_string())
            } else {
                None
            },
            timezone: match &e.start {
                EventTime::Zoned { tzid, .. } => Some(tzid.clone()),
                _ => None,
            },
            rrule: if scope == EditScope::This { None } else { e.rrule.clone() },
            attendees: e.attendees.iter().map(|a| Address::new(a.name.clone(), a.email.clone())).collect(),
            alarms: e.alarms.iter().map(|a| a.minutes_before).collect(),
            color: e.color.clone(),
            transparent: e.transparency == Transparency::Transparent,
            add_conference: false,
        };
        self.update_event(event_id, recurrence_id, scope, input).await
    }

    pub async fn delete_event(&self, event_id: i64, recurrence_id: Option<EventTime>, scope: EditScope) -> Result<()> {
        let row = self.db.read(|c| cdb::get_event(c, event_id))?;
        let cal = self.db.read(|c| cdb::get_calendar(c, row.calendar_id))?;
        if cal.read_only {
            return Err(Error::Invalid("This calendar is read-only".into()));
        }
        crate::sync::calendar::write_delete(self, &cal, &row, recurrence_id, scope).await?;
        self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![cal.id] });
        Ok(())
    }

    /// RSVP to an event in one of the user's calendars.
    pub async fn respond_event(&self, event_id: i64, status: PartStat, comment: Option<String>) -> Result<()> {
        let row = self.db.read(|c| cdb::get_event(c, event_id))?;
        let cal = self.db.read(|c| cdb::get_calendar(c, row.calendar_id))?;
        crate::sync::calendar::write_respond(self, &cal, &row, status, comment.as_deref()).await?;
        self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![cal.id] });
        Ok(())
    }

    /// Fires due reminders (called periodically).
    pub(crate) fn check_reminders(&self, since: i64, now: i64) -> Result<()> {
        let horizon = now + 7 * 86_400_000;
        let occurrences = self.occurrences(since - 86_400_000, horizon)?;
        for o in occurrences {
            if o.status == EventStatus::Cancelled || o.my_status == Some(PartStat::Declined) {
                continue;
            }
            let Ok(row) = self.db.read(|c| cdb::get_event(c, o.event_id)) else { continue };
            for alarm in &row.event.alarms {
                if alarm.action == AlarmAction::Email {
                    continue;
                }
                let fire = o.start - alarm.minutes_before * 60_000;
                if fire > since && fire <= now && o.start > now - 3_600_000 {
                    self.emit(EngineEvent::Reminder {
                        event_id: o.event_id,
                        title: o.title.clone(),
                        start: o.start,
                        location: o.location.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// An invitation found in a message.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteInfo {
    pub method: String,
    pub uid: String,
    pub summary: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub location: Option<String>,
    pub description: Option<String>,
    pub organizer: Option<Address>,
    pub attendees: Vec<Address>,
    pub my_status: Option<PartStat>,
    pub event_id: Option<i64>,
    pub conference_url: Option<String>,
    pub recurring: bool,
}

impl Engine {
    async fn invite_ics(&self, message_id: i64) -> Result<(String, i64)> {
        let message = self.db.read(|c| mail::get_message(c, message_id))?;
        let att = message
            .attachments
            .iter()
            .find(|a| a.mime_type == "text/calendar" || a.mime_type == "application/ics" || a.filename.ends_with(".ics"))
            .ok_or_else(|| Error::NotFound("invitation".into()))?;
        let path = self.attachment_file(att.id).await?;
        let bytes = tokio::fs::read(path).await?;
        Ok((String::from_utf8_lossy(&bytes).into_owned(), message.account_id))
    }

    /// Parses the invitation carried by a message.
    pub async fn invite_info(&self, message_id: i64) -> Result<InviteInfo> {
        let (ics, _) = self.invite_ics(message_id).await?;
        let itip = envelope_ical::parse_itip(&ics).map_err(|e| Error::Other(e.to_string()))?;
        let event = itip
            .events
            .iter()
            .find(|e| e.recurrence_id.is_none())
            .or(itip.events.first())
            .ok_or_else(|| Error::NotFound("event".into()))?;
        let me = self.my_emails()?;
        let existing = self.db.read(|c| cdb::rows_by_uid(c, &event.uid))?;
        let my_status = existing
            .first()
            .map(|r| &r.event)
            .unwrap_or(event)
            .attendees
            .iter()
            .find(|a| me.contains(&a.email.to_lowercase()))
            .map(|a| a.partstat);
        let start = envelope_ical::to_utc(&event.start, &itip.tz, local_tz()).timestamp_millis();
        let end = envelope_ical::to_utc(&event.end, &itip.tz, local_tz()).timestamp_millis();
        Ok(InviteInfo {
            method: itip.method.map(|m| format!("{m:?}").to_uppercase()).unwrap_or_else(|| "REQUEST".into()),
            uid: event.uid.clone(),
            summary: event.summary.clone(),
            start,
            end,
            all_day: event.is_all_day(),
            location: event.location.clone(),
            description: event.description.clone(),
            organizer: event.organizer.as_ref().map(|o| Address::new(o.name.clone(), o.email.clone())),
            attendees: event.attendees.iter().map(|a| Address::new(a.name.clone(), a.email.clone())).collect(),
            my_status,
            event_id: existing.first().map(|r| r.id),
            conference_url: event.conference_url.clone(),
            recurring: event.is_recurring(),
        })
    }

    /// Accepts, declines or tentatively accepts an invitation from a message.
    pub async fn respond_invite(&self, message_id: i64, status: PartStat, comment: Option<String>) -> Result<()> {
        let info = self.invite_info(message_id).await?;
        if let Some(event_id) = info.event_id {
            return self.respond_event(event_id, status, comment).await;
        }
        let (ics, account_id) = self.invite_ics(message_id).await?;
        let me = self.my_emails()?;
        let account = self.account_row(account_id)?;
        let itip = envelope_ical::parse_itip(&ics).map_err(|e| Error::Other(e.to_string()))?;
        let my_email = itip
            .events
            .iter()
            .flat_map(|e| e.attendees.iter())
            .find(|a| me.contains(&a.email.to_lowercase()))
            .map(|a| a.email.clone())
            .unwrap_or_else(|| account.email.clone());
        if let Some(organizer) = &info.organizer {
            let reply = envelope_ical::build_reply(&ics, &my_email, status, comment.as_deref())
                .map_err(|e| Error::Other(e.to_string()))?;
            crate::sync::calendar::send_reply(self, account_id, &info.summary, &organizer.email, status, reply).await?;
        }
        if status != PartStat::Declined {
            // Put it on the user's calendar.
            let calendars = self.db.read(cdb::list_calendars)?;
            let target = calendars
                .iter()
                .find(|c| c.account_id == Some(account_id) && c.is_default && !c.read_only)
                .or_else(|| calendars.iter().find(|c| c.account_id == Some(account_id) && !c.read_only))
                .or_else(|| calendars.iter().find(|c| !c.read_only));
            if let Some(cal) = target {
                for mut event in itip.events.clone() {
                    for a in event.attendees.iter_mut() {
                        if a.email.eq_ignore_ascii_case(&my_email) {
                            a.partstat = status;
                        }
                    }
                    crate::sync::calendar::write_create(self, cal, &event, false).await?;
                }
                self.emit(EngineEvent::CalendarChanged { calendar_ids: vec![cal.id] });
            }
        }
        Ok(())
    }
}

/// Last time reminders were checked (unix ms), shared by the scheduler.
pub(crate) fn reminder_cursor() -> &'static parking_lot::Mutex<i64> {
    static CURSOR: std::sync::OnceLock<parking_lot::Mutex<i64>> = std::sync::OnceLock::new();
    CURSOR.get_or_init(|| parking_lot::Mutex::new(now_ms()))
}
