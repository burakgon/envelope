//! Tasks (`VTODO`, RFC 5545 §3.6.2): reading them from calendar data,
//! writing new ones and editing stored ones in place.
//!
//! Like [`mutate`](crate::mutate) for events, edits work on the text the
//! server returned and rewrite only what changed, so alarms, categories,
//! vendor `X-` properties and everything else the model does not cover
//! survive.
//!
//! Repeating tasks follow the convention of Apple Reminders and most task
//! apps: completing one moves its `DTSTART` and `DUE` to the next occurrence
//! and leaves it open ([`Todo::next_occurrence`]).

use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::component::{Component, Property};
use crate::error::{Error, Result};
use crate::events::{ExtractOptions, content_uid, instant_of, non_empty};
use crate::model::EventTime;
use crate::mutate::{ensure_timezones, insert_after, new_calendar, set_text, touch};
use crate::parser::parse_components;
use crate::recur::{RecurRule, instances_after};
use crate::text::escape_text;
use crate::tz::{TzContext, wall_clock, with_wall_clock};
use crate::value::{format_utc, parse_duration, property_time, time_property};
use crate::writer::write_components;

/// Progress of a task (`STATUS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TodoStatus {
    /// `NEEDS-ACTION`
    NeedsAction,
    /// `IN-PROCESS`
    InProcess,
    /// `COMPLETED`
    Completed,
    /// `CANCELLED`
    Cancelled,
}

/// A task (a `VTODO`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Todo {
    /// iCalendar `UID`.
    pub uid: String,
    /// `SUMMARY`, the title.
    pub summary: String,
    /// `DESCRIPTION`, the notes.
    pub description: Option<String>,
    /// `DTSTART`: when work on the task starts. Anchors the recurrence when
    /// present.
    pub start: Option<EventTime>,
    /// `DUE` (or `DTSTART` plus `DURATION`).
    pub due: Option<EventTime>,
    /// `COMPLETED`: when the task was done.
    pub completed: Option<DateTime<Utc>>,
    /// `STATUS`, when the task has one.
    pub status: Option<TodoStatus>,
    /// `PERCENT-COMPLETE` (0–100).
    pub percent_complete: Option<u8>,
    /// `PRIORITY`: 1 (highest) to 9 (lowest), 0 when undefined.
    pub priority: u8,
    /// UID of the parent task (`RELATED-TO` with `RELTYPE=PARENT`, the
    /// default relationship).
    pub parent: Option<String>,
    /// `X-APPLE-SORT-ORDER`: position among the list's tasks (lower first),
    /// as Apple Reminders and Nextcloud Tasks keep it.
    pub sort_order: Option<i64>,
    /// `RRULE` value without the `RRULE:` prefix.
    pub rrule: Option<String>,
    /// `CREATED`.
    pub created: Option<DateTime<Utc>>,
    /// `LAST-MODIFIED`.
    pub last_modified: Option<DateTime<Utc>>,
}

/// Zone used to compare times inside edits (see [`crate::mutate`]).
const EDIT_TZ: Tz = Tz::UTC;

/// Occurrences looked at when deciding how far a rescheduled repeating task
/// moved through its series.
const MAX_PASSED: usize = 10_000;

impl Todo {
    /// True for completed and cancelled tasks. Without a `STATUS`, a
    /// `COMPLETED` time or `PERCENT-COMPLETE:100` mean done.
    pub fn is_done(&self) -> bool {
        match self.status {
            Some(TodoStatus::Completed | TodoStatus::Cancelled) => true,
            Some(TodoStatus::NeedsAction | TodoStatus::InProcess) => false,
            None => self.completed.is_some() || self.percent_complete == Some(100),
        }
    }

    /// Marks the task done at `at`: `STATUS:COMPLETED`, `COMPLETED` and
    /// `PERCENT-COMPLETE:100`.
    pub fn complete(&mut self, at: DateTime<Utc>) {
        self.status = Some(TodoStatus::Completed);
        self.completed = Some(at);
        self.percent_complete = Some(100);
    }

    /// Reverses [`complete`](Self::complete): `STATUS:NEEDS-ACTION` (when the
    /// task had a status), no `COMPLETED`, no `PERCENT-COMPLETE:100`.
    pub fn reopen(&mut self) {
        if self.status.is_some() {
            self.status = Some(TodoStatus::NeedsAction);
        }
        self.completed = None;
        if self.percent_complete == Some(100) {
            self.percent_complete = None;
        }
    }

    /// For a repeating task: the task moved on to its next occurrence, open.
    /// `DTSTART` (which anchors the series; `DUE` when there is none) moves
    /// to the next instance of the `RRULE`, `DUE` by the same amount, and a
    /// `COUNT` shrinks by one. `None` when the task does not repeat, has no
    /// date, or its rule ends with this occurrence.
    pub fn next_occurrence(&self, default_tz: Tz) -> Option<Todo> {
        let rrule = self.rrule.as_deref()?;
        let rule = RecurRule::parse(rrule)?;
        let anchor = self.start.as_ref().or(self.due.as_ref())?;
        let next = instances_after(&rule, anchor, &TzContext::new(), default_tz).next()?;
        let delta = wall_clock(&next) - wall_clock(anchor);
        let mut todo = self.clone();
        todo.start = self.start.as_ref().map(|t| shifted(t, delta));
        todo.due = self.due.as_ref().map(|t| shifted(t, delta));
        if let Some(count) = rule.count {
            todo.rrule = Some(set_count(rrule, count.saturating_sub(1).max(1)));
        }
        todo.reopen();
        Some(todo)
    }

    /// The task due at `due` instead (`None`: without a due date).
    ///
    /// A repeating task's `DTSTART` moves along with `DUE`, keeping the
    /// series' rhythm, and when the task moved exactly onto a later
    /// occurrence a `COUNT` shrinks by the occurrences passed. Otherwise
    /// `DTSTART` stays, unless it would contradict the new due date (another
    /// value type, or after it), which servers reject: then it takes the due
    /// date's form, and the due date when later.
    pub fn reschedule(&self, due: Option<EventTime>, default_tz: Tz) -> Todo {
        let mut todo = self.clone();
        let Some(due) = due else {
            todo.due = None;
            return todo;
        };
        if let (Some(old), Some(rrule)) = (&self.due, self.rrule.as_deref())
            && let Some(rule) = RecurRule::parse(rrule)
        {
            let delta = wall_clock(&due) - wall_clock(old);
            let anchor = self.start.as_ref().unwrap_or(old);
            todo.start = self.start.as_ref().map(|t| shifted(t, delta));
            if let Some(count) = rule.count
                && delta > TimeDelta::zero()
            {
                let target = wall_clock(anchor) + delta;
                let instances = instances_after(&rule, anchor, &TzContext::new(), default_tz);
                for (passed, instance) in (1..).zip(instances.take(MAX_PASSED)) {
                    let at = wall_clock(&instance);
                    if at == target {
                        todo.rrule = Some(set_count(rrule, count.saturating_sub(passed).max(1)));
                    }
                    if at >= target {
                        break;
                    }
                }
            }
        }
        todo.start = todo
            .start
            .take()
            .map(|start| start_before(start, &due, default_tz));
        todo.due = Some(due);
        todo
    }
}

/// `time` moved by `delta` in wall-clock time, in the same form.
fn shifted(time: &EventTime, delta: TimeDelta) -> EventTime {
    let local = wall_clock(time);
    with_wall_clock(local.checked_add_signed(delta).unwrap_or(local), time)
}

/// A `DTSTART` that agrees with `due`: the same value type, not after it.
fn start_before(start: EventTime, due: &EventTime, tz: Tz) -> EventTime {
    let start = match (start.is_date(), due.is_date()) {
        (true, false) | (false, true) => with_wall_clock(wall_clock(&start), due),
        _ => start,
    };
    let ctx = TzContext::new();
    let after = if start.is_date() {
        wall_clock(&start) > wall_clock(due)
    } else {
        ctx.to_utc(&start, tz) > ctx.to_utc(due, tz)
    };
    if after { due.clone() } else { start }
}

/// Replaces `COUNT` in an RRULE value, keeping every other part as written.
fn set_count(rule: &str, count: u32) -> String {
    rule.split(';')
        .map(|part| {
            let key = part.split('=').next().unwrap_or("").trim();
            if key.eq_ignore_ascii_case("COUNT") {
                format!("COUNT={count}")
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn status_value(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::NeedsAction => "NEEDS-ACTION",
        TodoStatus::InProcess => "IN-PROCESS",
        TodoStatus::Completed => "COMPLETED",
        TodoStatus::Cancelled => "CANCELLED",
    }
}

fn parse_status(value: Option<&str>) -> Option<TodoStatus> {
    Some(match value?.trim().to_ascii_uppercase().as_str() {
        "NEEDS-ACTION" => TodoStatus::NeedsAction,
        "IN-PROCESS" => TodoStatus::InProcess,
        "COMPLETED" => TodoStatus::Completed,
        "CANCELLED" | "CANCELED" => TodoStatus::Cancelled,
        _ => return None,
    })
}

/// A `RELATED-TO` naming the parent: `RELTYPE=PARENT`, or no `RELTYPE`.
fn is_parent_link(property: &Property) -> bool {
    property.name == "RELATED-TO"
        && property
            .param("RELTYPE")
            .is_none_or(|t| t.trim().eq_ignore_ascii_case("PARENT"))
}

/// Reads one `VTODO`. `ctx` resolves the TZIDs of the calendar it is in.
pub fn todo_from_vtodo(vtodo: &Component, ctx: &TzContext, options: &ExtractOptions) -> Todo {
    let normalize = |time: EventTime| {
        if options.normalize_tzids {
            ctx.normalize(time)
        } else {
            time
        }
    };
    let time_of = |name: &str| vtodo.property(name).and_then(property_time).map(normalize);
    let tz = options.default_tz;
    let start = time_of("DTSTART");
    let due = time_of("DUE").or_else(|| {
        let start = start.as_ref()?;
        let duration = vtodo.value("DURATION").and_then(parse_duration)?;
        Some(shifted(start, duration.max(TimeDelta::zero())))
    });
    let uid = vtodo
        .value("UID")
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map_or_else(
            || {
                content_uid(
                    vtodo,
                    &["DTSTART", "DUE", "SUMMARY", "DESCRIPTION", "CREATED"],
                )
            },
            str::to_owned,
        );
    Todo {
        uid,
        summary: vtodo.text("SUMMARY").unwrap_or_default().trim().to_owned(),
        description: non_empty(vtodo.text("DESCRIPTION")),
        start,
        due,
        completed: instant_of(vtodo.property("COMPLETED"), ctx, tz),
        status: parse_status(vtodo.value("STATUS")),
        percent_complete: vtodo
            .value("PERCENT-COMPLETE")
            .and_then(|p| p.trim().parse::<u8>().ok())
            .map(|p| p.min(100)),
        priority: vtodo
            .value("PRIORITY")
            .and_then(|p| p.trim().parse::<u8>().ok())
            .filter(|p| *p <= 9)
            .unwrap_or(0),
        parent: vtodo
            .properties
            .iter()
            .filter(|p| is_parent_link(p))
            .map(|p| p.text_value().trim().to_owned())
            .find(|uid| !uid.is_empty()),
        sort_order: vtodo
            .value("X-APPLE-SORT-ORDER")
            .and_then(|o| o.trim().parse::<i64>().ok()),
        rrule: vtodo
            .value("RRULE")
            .map(|r| {
                let r = r.trim();
                match r.get(..6) {
                    Some(prefix) if prefix.eq_ignore_ascii_case("RRULE:") => r[6..].trim(),
                    _ => r,
                }
                .to_owned()
            })
            .filter(|r| !r.is_empty()),
        created: instant_of(vtodo.property("CREATED"), ctx, tz),
        last_modified: instant_of(vtodo.property("LAST-MODIFIED"), ctx, tz),
    }
}

/// Parses iCalendar text and returns its tasks: one per UID, the series
/// itself when other clients stored overrides of single occurrences
/// (`RECURRENCE-ID`) next to it.
///
/// Fails only with [`Error::NoCalendar`] when the text holds no calendar data
/// at all; a calendar without tasks gives an empty list.
///
/// ```
/// let tasks = envelope_ical::parse_todos(
///     "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t1\r\nSUMMARY:Call Maya\r\nDUE;VALUE=DATE:20241030\r\n\
///      PRIORITY:1\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
/// )
/// .unwrap();
/// assert_eq!(tasks[0].summary, "Call Maya");
/// assert_eq!(tasks[0].priority, 1);
/// assert!(!tasks[0].is_done());
/// ```
pub fn parse_todos(text: &str) -> Result<Vec<Todo>> {
    let components = parse_components(text);
    if !components
        .iter()
        .any(|c| c.is("VCALENDAR") || c.is("VTODO") || c.is("VEVENT"))
    {
        return Err(Error::NoCalendar);
    }
    let mut ctx = TzContext::new();
    for component in &components {
        if component.is("VCALENDAR") {
            ctx.add_calendar(component);
        } else if component.is("VTIMEZONE") {
            ctx.add_vtimezone(component);
        }
    }
    let vtodos: Vec<&Component> = components
        .iter()
        .flat_map(|c| {
            if c.is("VCALENDAR") {
                c.children("VTODO").collect()
            } else if c.is("VTODO") {
                vec![c]
            } else {
                Vec::new()
            }
        })
        .collect();
    let options = ExtractOptions::default();
    let mut todos: Vec<(Todo, bool)> = Vec::new();
    for vtodo in vtodos {
        let master = vtodo.property("RECURRENCE-ID").is_none();
        let todo = todo_from_vtodo(vtodo, &ctx, &options);
        match todos.iter_mut().find(|(t, _)| t.uid == todo.uid) {
            Some(entry) if master && !entry.1 => *entry = (todo, true),
            Some(_) => {}
            None => todos.push((todo, master)),
        }
    }
    Ok(todos.into_iter().map(|(todo, _)| todo).collect())
}

/// Builds a complete `VTODO` (every field of the model), with `DTSTAMP` set
/// to `now`; `CREATED` and `LAST-MODIFIED` default to `now` as well.
fn todo_component(todo: &Todo, now: DateTime<Utc>) -> Component {
    let mut c = Component::new("VTODO");
    c.push(Property::new("UID", todo.uid.trim()));
    c.push(Property::new("DTSTAMP", format_utc(now)));
    c.push(Property::new(
        "CREATED",
        format_utc(todo.created.unwrap_or(now)),
    ));
    c.push(Property::new(
        "LAST-MODIFIED",
        format_utc(todo.last_modified.unwrap_or(now)),
    ));
    if !todo.summary.is_empty() {
        c.push(Property::text("SUMMARY", &todo.summary));
    }
    if let Some(description) = todo.description.as_deref().filter(|d| !d.is_empty()) {
        c.push(Property::text("DESCRIPTION", description));
    }
    if let Some(start) = &todo.start {
        c.push(time_property("DTSTART", start));
    }
    if let Some(due) = &todo.due {
        c.push(time_property("DUE", due));
    }
    if let Some(rule) = todo
        .rrule
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        c.push(Property::new("RRULE", rule));
    }
    if let Some(status) = todo.status {
        c.push(Property::new("STATUS", status_value(status)));
    }
    if let Some(completed) = todo.completed {
        c.push(Property::new("COMPLETED", format_utc(completed)));
    }
    if let Some(percent) = todo.percent_complete {
        c.push(Property::new("PERCENT-COMPLETE", percent.to_string()));
    }
    if todo.priority > 0 {
        c.push(Property::new("PRIORITY", todo.priority.min(9).to_string()));
    }
    if let Some(parent) = todo.parent.as_deref().filter(|p| !p.trim().is_empty()) {
        c.push(
            Property::new("RELATED-TO", escape_text(parent.trim())).with_param("RELTYPE", "PARENT"),
        );
    }
    if let Some(order) = todo.sort_order {
        c.push(Property::new("X-APPLE-SORT-ORDER", order.to_string()));
    }
    c
}

/// Creates a new `VCALENDAR` (with `VTIMEZONE`s for every zoned time) for
/// one task. See [`create_todo_at`].
pub fn create_todo(todo: &Todo) -> Result<String> {
    create_todo_at(todo, Utc::now())
}

/// [`create_todo`] with an explicit current time.
pub fn create_todo_at(todo: &Todo, now: DateTime<Utc>) -> Result<String> {
    if todo.uid.trim().is_empty() {
        return Err(Error::MissingUid);
    }
    let mut calendar = new_calendar(None);
    calendar.components.push(todo_component(todo, now));
    ensure_timezones(&mut calendar);
    Ok(calendar.to_ical())
}

/// Writes `todo` into existing calendar text. See [`upsert_todo_at`].
pub fn upsert_todo(existing: &str, todo: &Todo) -> Result<String> {
    upsert_todo_at(existing, todo, Utc::now())
}

/// Writes `todo` into existing calendar text (as stored on a server).
///
/// The `VTODO` with the task's UID is updated in place, or else the
/// resource's task, which takes the new UID (a task moved to another list
/// keeps what other clients stored with it). Only properties whose value
/// differs from what the text expresses are rewritten, so `VALARM`s,
/// `CATEGORIES`, `ATTACH`es and vendor properties survive; `DTSTAMP` and
/// `LAST-MODIFIED` are set to `now`. A new `DUE` replaces a `DURATION`.
/// `VTIMEZONE`s are added and pruned as needed. Empty `existing` text
/// creates a new calendar.
pub fn upsert_todo_at(existing: &str, todo: &Todo, now: DateTime<Utc>) -> Result<String> {
    let uid = todo.uid.trim();
    if uid.is_empty() {
        return Err(Error::MissingUid);
    }
    if existing.trim().is_empty() {
        return create_todo_at(todo, now);
    }
    let mut components = parse_components(existing);
    if !components.iter().any(|c| c.is("VCALENDAR")) {
        let loose: Vec<Component> = components
            .into_iter()
            .filter(|c| c.is("VTODO") || c.is("VTIMEZONE"))
            .collect();
        if loose.is_empty() {
            return Err(Error::NoCalendar);
        }
        let mut calendar = new_calendar(None);
        calendar.components = loose;
        components = vec![calendar];
    }
    let holds = |c: &Component, uid: Option<&str>| {
        c.is("VCALENDAR")
            && c.children("VTODO")
                .any(|t| uid.is_none_or(|uid| t.value("UID").map(str::trim) == Some(uid)))
    };
    let index = components
        .iter()
        .position(|c| holds(c, Some(uid)))
        .or_else(|| components.iter().position(|c| holds(c, None)))
        .or_else(|| components.iter().position(|c| c.is("VCALENDAR")))
        .ok_or(Error::NoCalendar)?;
    let calendar = &mut components[index];
    let ctx = TzContext::from_calendar(calendar);
    let is_master = |c: &Component| c.is("VTODO") && c.property("RECURRENCE-ID").is_none();
    let target = calendar
        .components
        .iter()
        .position(|c| is_master(c) && c.value("UID").map(str::trim) == Some(uid))
        .or_else(|| calendar.components.iter().position(is_master));
    match target {
        Some(target) => merge_todo(&mut calendar.components[target], todo, &ctx, now),
        None => calendar.components.push(todo_component(todo, now)),
    }
    ensure_timezones(calendar);
    Ok(write_components(&components))
}

/// Sets or removes a date property.
fn set_time(vtodo: &mut Component, name: &str, time: Option<&EventTime>, anchors: &[&str]) {
    match time {
        None => {
            vtodo.remove_all(name);
        }
        Some(time) if vtodo.property(name).is_some() => vtodo.set(time_property(name, time)),
        Some(time) => insert_after(vtodo, time_property(name, time), anchors),
    }
}

/// Sets a property with a raw value, or removes it.
fn set_value(vtodo: &mut Component, name: &str, value: Option<String>, anchors: &[&str]) {
    match value {
        None => {
            vtodo.remove_all(name);
        }
        Some(value) if vtodo.property(name).is_some() => vtodo.set(Property::new(name, value)),
        Some(value) => insert_after(vtodo, Property::new(name, value), anchors),
    }
}

/// Points the task at another parent, keeping the form of the existing link
/// (with or without `RELTYPE`) and links of other kinds.
fn set_parent(vtodo: &mut Component, parent: Option<&str>) {
    let existing = vtodo.properties.iter().position(is_parent_link);
    let mut link = existing
        .map(|i| vtodo.properties[i].clone())
        .unwrap_or_else(|| Property::new("RELATED-TO", "").with_param("RELTYPE", "PARENT"));
    vtodo.properties.retain(|p| !is_parent_link(p));
    let Some(parent) = parent.map(str::trim).filter(|p| !p.is_empty()) else {
        return;
    };
    link.value = escape_text(parent);
    match existing {
        Some(index) => vtodo
            .properties
            .insert(index.min(vtodo.properties.len()), link),
        None => vtodo.properties.push(link),
    }
}

fn merge_todo(vtodo: &mut Component, todo: &Todo, ctx: &TzContext, now: DateTime<Utc>) {
    let options = ExtractOptions {
        default_tz: EDIT_TZ,
        normalize_tzids: true,
    };
    let old = todo_from_vtodo(vtodo, ctx, &options);
    let uid = todo.uid.trim();
    if old.uid != uid {
        vtodo.set(Property::new("UID", uid));
    }
    if old.summary != todo.summary {
        set_text(vtodo, "SUMMARY", Some(&todo.summary));
    }
    if old.description != todo.description {
        set_text(vtodo, "DESCRIPTION", todo.description.as_deref());
    }
    let start_changed = old.start != todo.start;
    if start_changed {
        set_time(
            vtodo,
            "DTSTART",
            todo.start.as_ref(),
            &["UID", "DTSTAMP", "SUMMARY"],
        );
    }
    let has_duration = vtodo.property("DURATION").is_some();
    if old.due != todo.due || (start_changed && has_duration) {
        vtodo.remove_all("DURATION");
        set_time(
            vtodo,
            "DUE",
            todo.due.as_ref(),
            &["UID", "DTSTAMP", "SUMMARY", "DTSTART"],
        );
    }
    if old.rrule != todo.rrule {
        set_value(
            vtodo,
            "RRULE",
            todo.rrule
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned),
            &["DTSTART", "DUE"],
        );
    }
    if old.status != todo.status {
        set_value(
            vtodo,
            "STATUS",
            todo.status.map(|s| status_value(s).to_owned()),
            &["UID", "DTSTAMP", "SUMMARY"],
        );
    }
    if old.completed != todo.completed {
        set_value(
            vtodo,
            "COMPLETED",
            todo.completed.map(format_utc),
            &["STATUS", "UID", "DTSTAMP"],
        );
    }
    if old.percent_complete != todo.percent_complete {
        set_value(
            vtodo,
            "PERCENT-COMPLETE",
            todo.percent_complete.map(|p| p.min(100).to_string()),
            &["COMPLETED", "STATUS", "UID", "DTSTAMP"],
        );
    }
    if old.priority != todo.priority {
        set_value(
            vtodo,
            "PRIORITY",
            (todo.priority > 0).then(|| todo.priority.min(9).to_string()),
            &["STATUS", "UID", "DTSTAMP", "SUMMARY"],
        );
    }
    if old.parent != todo.parent {
        set_parent(vtodo, todo.parent.as_deref());
    }
    if old.sort_order != todo.sort_order {
        set_value(
            vtodo,
            "X-APPLE-SORT-ORDER",
            todo.sort_order.map(|o| o.to_string()),
            &[],
        );
    }
    if vtodo.property("CREATED").is_none()
        && let Some(created) = todo.created
    {
        insert_after(
            vtodo,
            Property::new("CREATED", format_utc(created)),
            &["UID", "DTSTAMP"],
        );
    }
    touch(vtodo, now, false);
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone};

    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").expect("valid test datetime")
    }

    fn date(s: &str) -> EventTime {
        EventTime::Date {
            date: NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid test date"),
        }
    }

    fn zoned(s: &str, tzid: &str) -> EventTime {
        EventTime::Zoned {
            local: dt(s),
            tzid: tzid.into(),
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .unwrap_or_default()
    }

    fn one(body: &str) -> Todo {
        let text = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\n{body}END:VTODO\r\nEND:VCALENDAR\r\n"
        );
        let mut todos = parse_todos(&text).expect("calendar");
        assert_eq!(todos.len(), 1);
        todos.remove(0)
    }

    #[test]
    fn reads_every_field() {
        let t = one(
            "UID:t1\r\nSUMMARY:Call Maya\\, today\r\nDESCRIPTION:About the\\noffer\r\n\
             DTSTART;TZID=W. Europe Standard Time:20240301T090000\r\n\
             DUE;TZID=W. Europe Standard Time:20240301T150000\r\nSTATUS:IN-PROCESS\r\n\
             PERCENT-COMPLETE:40\r\nPRIORITY:3\r\nRELATED-TO;RELTYPE=SIBLING:s1\r\n\
             RELATED-TO:p1\r\nX-APPLE-SORT-ORDER:717930000\r\nRRULE:RRULE:FREQ=WEEKLY\r\n\
             CREATED:20240201T100000Z\r\nLAST-MODIFIED:20240202T100000Z\r\n",
        );
        assert_eq!(t.uid, "t1");
        assert_eq!(t.summary, "Call Maya, today");
        assert_eq!(t.description.as_deref(), Some("About the\noffer"));
        assert_eq!(t.start, Some(zoned("2024-03-01 09:00", "Europe/Berlin")));
        assert_eq!(t.due, Some(zoned("2024-03-01 15:00", "Europe/Berlin")));
        assert_eq!(t.status, Some(TodoStatus::InProcess));
        assert_eq!(t.percent_complete, Some(40));
        assert_eq!(t.priority, 3);
        assert_eq!(
            t.parent.as_deref(),
            Some("p1"),
            "RELTYPE defaults to PARENT"
        );
        assert_eq!(t.sort_order, Some(717_930_000));
        assert_eq!(t.rrule.as_deref(), Some("FREQ=WEEKLY"));
        assert_eq!(t.created, Some(dt("2024-02-01 10:00").and_utc()));
        assert_eq!(t.last_modified, Some(dt("2024-02-02 10:00").and_utc()));
        assert!(!t.is_done());
    }

    #[test]
    fn due_forms_and_duration() {
        assert_eq!(
            one("UID:a\r\nDUE;VALUE=DATE:20240301\r\n").due,
            Some(date("2024-03-01"))
        );
        assert_eq!(
            one("UID:a\r\nDUE:20240301T090000Z\r\n").due,
            Some(EventTime::Utc {
                at: dt("2024-03-01 09:00").and_utc()
            })
        );
        assert_eq!(
            one("UID:a\r\nDUE:20240301T090000\r\n").due,
            Some(EventTime::Floating {
                local: dt("2024-03-01 09:00")
            })
        );
        assert_eq!(
            one("UID:a\r\nDTSTART;TZID=Europe/Istanbul:20240301T090000\r\nDURATION:PT2H\r\n").due,
            Some(zoned("2024-03-01 11:00", "Europe/Istanbul"))
        );
        let t = one("UID:a\r\nSUMMARY:x\r\n");
        assert_eq!((t.start, t.due), (None, None));
    }

    #[test]
    fn done_states() {
        assert!(one("UID:a\r\nSTATUS:COMPLETED\r\n").is_done());
        assert!(one("UID:a\r\nSTATUS:CANCELLED\r\n").is_done());
        assert!(one("UID:a\r\nCOMPLETED:20240301T090000Z\r\n").is_done());
        assert!(one("UID:a\r\nPERCENT-COMPLETE:100\r\n").is_done());
        assert!(
            !one("UID:a\r\nSTATUS:NEEDS-ACTION\r\nCOMPLETED:20240301T090000Z\r\n").is_done(),
            "the status wins"
        );
        assert!(!one("UID:a\r\nPRIORITY:12\r\n").is_done());
        assert_eq!(one("UID:a\r\nPRIORITY:12\r\n").priority, 0);
    }

    #[test]
    fn series_overrides_and_events_are_left_out() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e\r\nDTSTART:20240301T090000Z\r\nEND:VEVENT\r\n\
                    BEGIN:VTODO\r\nUID:r\r\nRECURRENCE-ID:20240308T090000Z\r\nSUMMARY:Once\r\nEND:VTODO\r\n\
                    BEGIN:VTODO\r\nUID:r\r\nDTSTART:20240301T090000Z\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Series\r\nEND:VTODO\r\n\
                    BEGIN:VTODO\r\nSUMMARY:No UID\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let todos = parse_todos(text).expect("calendar");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].summary, "Series");
        assert!(todos[1].uid.starts_with("envelope-generated-"));
        assert_eq!(
            parse_todos("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").map(|t| t.len()),
            Ok(0)
        );
        assert_eq!(parse_todos("hello"), Err(Error::NoCalendar));
    }

    #[test]
    fn created_task_round_trips() {
        let todo = Todo {
            uid: "new-1".into(),
            summary: "Pay rent; today, please".into(),
            description: Some("Line 1\nLine 2".into()),
            start: Some(zoned("2026-01-05 09:00", "Europe/Istanbul")),
            due: Some(zoned("2026-01-05 15:00", "Europe/Istanbul")),
            completed: None,
            status: Some(TodoStatus::NeedsAction),
            percent_complete: None,
            priority: 1,
            parent: Some("parent-1".into()),
            sort_order: Some(2048),
            rrule: Some("FREQ=MONTHLY;BYMONTHDAY=5".into()),
            created: Some(now()),
            last_modified: Some(now()),
        };
        let text = create_todo_at(&todo, now()).expect("calendar");
        assert!(text.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Istanbul\r\n"));
        assert!(text.contains("RELATED-TO;RELTYPE=PARENT:parent-1\r\n"));
        assert_eq!(parse_todos(&text).expect("tasks"), vec![todo.clone()]);
        let mut done = todo.clone();
        done.complete(now());
        let text = create_todo_at(&done, now()).expect("calendar");
        assert!(text.contains(
            "STATUS:COMPLETED\r\nCOMPLETED:20260102T030405Z\r\nPERCENT-COMPLETE:100\r\n"
        ));
        assert_eq!(parse_todos(&text).expect("tasks"), vec![done]);
        let nameless = Todo::default();
        assert_eq!(create_todo_at(&nameless, now()), Err(Error::MissingUid));
    }

    #[test]
    fn upsert_rewrites_only_what_changed() {
        let original = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//iOS 17.0//EN\r\n\
            BEGIN:VTODO\r\nUID:t1\r\nDTSTAMP:20240101T000000Z\r\nSUMMARY:Call the bank\r\n\
            DUE;TZID=Europe/Istanbul:20240301T150000\r\nRELATED-TO:p1\r\nCATEGORIES:Money\r\n\
            X-APPLE-SORT-ORDER:5\r\nX-VENDOR;X-A=1:keep me\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\n\
            TRIGGER;RELATED=END:-PT15M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VTODO\r\n\
            BEGIN:VTIMEZONE\r\nTZID:Europe/Istanbul\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\n\
            TZOFFSETFROM:+0300\r\nTZOFFSETTO:+0300\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n";
        let todo = parse_todos(original).expect("tasks").remove(0);
        let same = upsert_todo_at(original, &todo, now()).expect("upsert");
        let lines = |text: &str| -> Vec<String> {
            text.split("\r\n")
                .filter(|l| !l.starts_with("DTSTAMP") && !l.starts_with("LAST-MODIFIED"))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(
            lines(&same),
            lines(original),
            "nothing but timestamps changes"
        );
        assert!(same.contains("LAST-MODIFIED:20260102T030405Z\r\n"));

        let mut edited = todo.clone();
        edited.summary = "Call the bank again".into();
        edited.parent = Some("p2".into());
        edited.priority = 1;
        edited.sort_order = None;
        edited.complete(now());
        let text = upsert_todo_at(original, &edited, now()).expect("upsert");
        assert!(text.contains("SUMMARY:Call the bank again\r\n"));
        assert!(
            text.contains("RELATED-TO:p2\r\n"),
            "the link keeps its form"
        );
        assert!(text.contains("STATUS:COMPLETED\r\n"));
        assert!(text.contains("PERCENT-COMPLETE:100\r\n"));
        assert!(text.contains("PRIORITY:1\r\n"));
        assert!(!text.contains("X-APPLE-SORT-ORDER"));
        assert!(text.contains("X-VENDOR;X-A=1:keep me\r\n"));
        assert!(text.contains("CATEGORIES:Money\r\n"));
        assert!(text.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT15M\r\n"));
        assert!(text.contains("DUE;TZID=Europe/Istanbul:20240301T150000\r\n"));
        assert_eq!(
            parse_todos(&text).expect("tasks"),
            vec![{
                let mut expected = edited.clone();
                expected.last_modified = Some(now());
                expected
            }]
        );

        let mut reopened = parse_todos(&text).expect("tasks").remove(0);
        reopened.reopen();
        reopened.due = Some(date("2024-03-02"));
        reopened.parent = None;
        let text = upsert_todo_at(&text, &reopened, now()).expect("upsert");
        assert!(text.contains("STATUS:NEEDS-ACTION\r\n"));
        assert!(!text.contains("COMPLETED:"));
        assert!(!text.contains("PERCENT-COMPLETE"));
        assert!(!text.contains("RELATED-TO"));
        assert!(text.contains("DUE;VALUE=DATE:20240302\r\n"));
        assert!(!text.contains("VTIMEZONE"), "the unused zone goes");
    }

    #[test]
    fn upsert_moves_the_task_to_a_new_uid_and_replaces_durations() {
        let original = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:old\r\nSUMMARY:Draft\r\n\
            DTSTART:20240301T090000Z\r\nDURATION:PT1H\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let mut todo = parse_todos(original).expect("tasks").remove(0);
        assert_eq!(
            todo.due,
            Some(EventTime::Utc {
                at: dt("2024-03-01 10:00").and_utc()
            })
        );
        todo.uid = "new".into();
        todo.due = Some(EventTime::Utc {
            at: dt("2024-03-01 12:00").and_utc(),
        });
        let text = upsert_todo_at(original, &todo, now()).expect("upsert");
        assert!(text.contains("UID:new\r\n"));
        assert!(!text.contains("UID:old"));
        assert!(!text.contains("DURATION"));
        assert!(text.contains("DUE:20240301T120000Z\r\n"));
        assert_eq!(text.matches("BEGIN:VTODO").count(), 1);
        // Bare components are wrapped in a calendar.
        let bare =
            upsert_todo_at("BEGIN:VTODO\r\nUID:x\r\nEND:VTODO\r\n", &todo, now()).expect("upsert");
        assert!(bare.starts_with("BEGIN:VCALENDAR\r\n"));
        assert_eq!(parse_todos(&bare).expect("tasks")[0].uid, "new");
        assert_eq!(
            upsert_todo_at("", &todo, now()),
            create_todo_at(&todo, now())
        );
    }

    #[test]
    fn completing_a_repeating_task_moves_it_to_the_next_occurrence() {
        let mut todo = Todo {
            uid: "r".into(),
            start: Some(zoned("2024-03-29 09:00", "Europe/Berlin")),
            due: Some(zoned("2024-03-29 17:00", "Europe/Berlin")),
            rrule: Some("FREQ=WEEKLY;BYDAY=FR;COUNT=3;X-KEEP=1".into()),
            status: Some(TodoStatus::Completed),
            completed: Some(now()),
            percent_complete: Some(100),
            ..Todo::default()
        };
        let next = todo.next_occurrence(Tz::UTC).expect("next");
        // Across the switch to summer time the wall-clock times stay.
        assert_eq!(next.start, Some(zoned("2024-04-05 09:00", "Europe/Berlin")));
        assert_eq!(next.due, Some(zoned("2024-04-05 17:00", "Europe/Berlin")));
        assert_eq!(
            next.rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=FR;COUNT=2;X-KEEP=1")
        );
        assert_eq!(next.status, Some(TodoStatus::NeedsAction));
        assert_eq!((next.completed, next.percent_complete), (None, None));
        let last = next.next_occurrence(Tz::UTC).expect("last");
        assert_eq!(
            last.rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=FR;COUNT=1;X-KEEP=1")
        );
        assert_eq!(last.next_occurrence(Tz::UTC), None, "the series ends");

        // Without DTSTART the due date anchors the series.
        todo.start = None;
        todo.due = Some(date("2024-01-31"));
        todo.rrule = Some("FREQ=MONTHLY;BYMONTHDAY=-1;UNTIL=20240301".into());
        let next = todo.next_occurrence(Tz::UTC).expect("next");
        assert_eq!(next.due, Some(date("2024-02-29")));
        assert_eq!(next.next_occurrence(Tz::UTC), None, "past UNTIL");
        todo.rrule = None;
        assert_eq!(todo.next_occurrence(Tz::UTC), None);
        todo.rrule = Some("FREQ=DAILY".into());
        todo.due = None;
        assert_eq!(todo.next_occurrence(Tz::UTC), None, "no date to move");
    }

    #[test]
    fn rescheduling_keeps_dtstart_valid() {
        let base = Todo {
            uid: "a".into(),
            start: Some(date("2024-03-01")),
            due: Some(date("2024-03-04")),
            ..Todo::default()
        };
        let later = base.reschedule(Some(date("2024-03-10")), Tz::UTC);
        assert_eq!(later.start, Some(date("2024-03-01")), "kept");
        let earlier = base.reschedule(Some(date("2024-02-20")), Tz::UTC);
        assert_eq!(earlier.start, Some(date("2024-02-20")), "not after DUE");
        let timed = base.reschedule(Some(zoned("2024-03-05 10:00", "Europe/Paris")), Tz::UTC);
        assert_eq!(timed.start, Some(zoned("2024-03-01 00:00", "Europe/Paris")));
        let cleared = base.reschedule(None, Tz::UTC);
        assert_eq!(
            (cleared.start, cleared.due),
            (Some(date("2024-03-01")), None)
        );

        // A repeating task keeps the distance from DTSTART, and moving it two
        // occurrences on uses up two of its COUNT.
        let series = Todo {
            rrule: Some("FREQ=DAILY;COUNT=10".into()),
            start: Some(zoned("2024-03-01 09:00", "Europe/Paris")),
            due: Some(zoned("2024-03-01 12:00", "Europe/Paris")),
            ..base.clone()
        };
        let moved = series.reschedule(Some(zoned("2024-03-03 12:00", "Europe/Paris")), Tz::UTC);
        assert_eq!(moved.start, Some(zoned("2024-03-03 09:00", "Europe/Paris")));
        assert_eq!(moved.rrule.as_deref(), Some("FREQ=DAILY;COUNT=8"));
        let off_series =
            series.reschedule(Some(zoned("2024-03-03 13:00", "Europe/Paris")), Tz::UTC);
        assert_eq!(off_series.rrule.as_deref(), Some("FREQ=DAILY;COUNT=10"));
    }
}
