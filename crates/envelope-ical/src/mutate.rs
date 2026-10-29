//! Building calendar data from [`Event`]s and editing existing calendar
//! text in place.
//!
//! Every edit operates on the raw text a server returned so that data the
//! model does not cover survives: unknown properties and parameters, vendor
//! `X-` extensions, `ATTACH`es, nested components, other events in the same
//! resource. Only properties whose value actually changed are rewritten;
//! everything else is kept byte-for-byte (modulo line folding).
//!
//! All functions writing timestamps (`DTSTAMP`, `LAST-MODIFIED`) have an
//! `_at` variant taking the current time explicitly, for reproducible output.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::color::hex_to_css_color_name;
use crate::component::{Component, Property};
use crate::error::{Error, Result};
use crate::events::{ExtractOptions, cal_address, event_from_vevent, parse_alarm, parse_attendee};
use crate::model::{
    Alarm, AlarmAction, Attendee, AttendeeRole, CalendarUserType, Event, EventClass, EventStatus,
    EventTime, Organizer, PartStat, Transparency,
};
use crate::parser::parse_components;
use crate::recur::rule::RecurRule;
use crate::text::escape_text;
use crate::tz::{TzContext, generate_vtimezone, resolve_tzid, wall_clock};
use crate::value::{
    format_duration, format_event_time, format_utc, parse_date_time, parse_event_time,
    property_time, property_times, time_property,
};
use crate::writer::write_components;

/// `PRODID` written into every calendar Envelope creates.
pub const PRODID: &str = "-//Envelope//Envelope Mail//EN";

/// Zone used for comparisons inside edits; only matters for floating and
/// all-day values, which are compared consistently against each other.
const EDIT_TZ: Tz = Tz::UTC;

/// A new globally unique identifier for an event or contact.
pub fn new_uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn new_calendar(method: Option<&str>) -> Component {
    let mut calendar = Component::new("VCALENDAR");
    calendar.push(Property::new("PRODID", PRODID));
    calendar.push(Property::new("VERSION", "2.0"));
    calendar.push(Property::new("CALSCALE", "GREGORIAN"));
    if let Some(method) = method {
        calendar.push(Property::new("METHOD", method));
    }
    calendar
}

pub(crate) fn partstat_value(partstat: PartStat) -> &'static str {
    match partstat {
        PartStat::NeedsAction => "NEEDS-ACTION",
        PartStat::Accepted => "ACCEPTED",
        PartStat::Declined => "DECLINED",
        PartStat::Tentative => "TENTATIVE",
        PartStat::Delegated => "DELEGATED",
    }
}

fn role_value(role: AttendeeRole) -> &'static str {
    match role {
        AttendeeRole::Chair => "CHAIR",
        AttendeeRole::Required => "REQ-PARTICIPANT",
        AttendeeRole::Optional => "OPT-PARTICIPANT",
        AttendeeRole::NonParticipant => "NON-PARTICIPANT",
    }
}

fn cutype_value(cutype: CalendarUserType) -> &'static str {
    match cutype {
        CalendarUserType::Individual => "INDIVIDUAL",
        CalendarUserType::Group => "GROUP",
        CalendarUserType::Resource => "RESOURCE",
        CalendarUserType::Room => "ROOM",
        CalendarUserType::Unknown => "UNKNOWN",
    }
}

fn status_value(status: EventStatus) -> &'static str {
    match status {
        EventStatus::Confirmed => "CONFIRMED",
        EventStatus::Tentative => "TENTATIVE",
        EventStatus::Cancelled => "CANCELLED",
    }
}

fn transparency_value(transparency: Transparency) -> &'static str {
    match transparency {
        Transparency::Opaque => "OPAQUE",
        Transparency::Transparent => "TRANSPARENT",
    }
}

fn class_value(class: EventClass) -> &'static str {
    match class {
        EventClass::Public => "PUBLIC",
        EventClass::Private => "PRIVATE",
        EventClass::Confidential => "CONFIDENTIAL",
    }
}

/// A calendar user address for an e-mail (`mailto:` unless already a URI).
pub(crate) fn mailto(email: &str) -> String {
    let email = email.trim();
    if email.contains(':') {
        email.to_owned()
    } else {
        format!("mailto:{email}")
    }
}

pub(crate) fn organizer_property(organizer: &Organizer) -> Property {
    let mut property = Property::new("ORGANIZER", mailto(&organizer.email));
    if let Some(name) = organizer.name.as_deref().filter(|n| !n.is_empty()) {
        property.set_param("CN", name);
    }
    property
}

pub(crate) fn attendee_property(attendee: &Attendee) -> Property {
    let mut property = Property::new("ATTENDEE", mailto(&attendee.email));
    if let Some(name) = attendee.name.as_deref().filter(|n| !n.is_empty()) {
        property.set_param("CN", name);
    }
    if attendee.cutype != CalendarUserType::Individual {
        property.set_param("CUTYPE", cutype_value(attendee.cutype));
    }
    property.set_param("ROLE", role_value(attendee.role));
    property.set_param("PARTSTAT", partstat_value(attendee.partstat));
    if attendee.rsvp {
        property.set_param("RSVP", "TRUE");
    }
    property
}

fn alarm_component(alarm: &Alarm, summary: &str) -> Component {
    let mut valarm = Component::new("VALARM");
    let action = match alarm.action {
        AlarmAction::Display => "DISPLAY",
        AlarmAction::Audio => "AUDIO",
        AlarmAction::Email => "EMAIL",
    };
    valarm.push(Property::new("ACTION", action));
    let offset = alarm
        .minutes_before
        .checked_neg()
        .and_then(TimeDelta::try_minutes)
        .unwrap_or(TimeDelta::zero());
    valarm.push(Property::new("TRIGGER", format_duration(offset)));
    let description = alarm
        .description
        .clone()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| {
            if summary.is_empty() {
                "Reminder".to_owned()
            } else {
                summary.to_owned()
            }
        });
    if alarm.action != AlarmAction::Audio {
        valarm.push(Property::text("DESCRIPTION", &description));
    }
    if alarm.action == AlarmAction::Email {
        valarm.push(Property::text(
            "SUMMARY",
            if summary.is_empty() {
                "Reminder"
            } else {
                summary
            },
        ));
    }
    valarm
}

fn conference_mentioned(event: &Event, url: &str) -> bool {
    [
        &event.location,
        &event.description,
        &event.url,
        &event.description_html,
    ]
    .iter()
    .any(|field| field.as_deref().is_some_and(|text| text.contains(url)))
}

fn conference_property(url: &str) -> Property {
    Property::new("CONFERENCE", url)
        .with_param("VALUE", "URI")
        .with_param("FEATURE", "VIDEO")
}

fn color_property(hex: &str) -> Option<Property> {
    hex_to_css_color_name(hex).map(|name| Property::new("COLOR", name))
}

fn categories_property(categories: &[String]) -> Option<Property> {
    let values: Vec<String> = categories
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(escape_text)
        .collect();
    (!values.is_empty()).then(|| Property::new("CATEGORIES", values.join(",")))
}

/// Builds a complete `VEVENT` for an [`Event`] (every field of the model),
/// with `DTSTAMP` set to `dtstamp`.
pub fn event_to_component(event: &Event, dtstamp: DateTime<Utc>) -> Component {
    event_component(event, dtstamp, true)
}

pub(crate) fn event_component(
    event: &Event,
    dtstamp: DateTime<Utc>,
    include_alarms: bool,
) -> Component {
    let mut c = Component::new("VEVENT");
    c.push(Property::new("UID", event.uid.trim()));
    c.push(Property::new("DTSTAMP", format_utc(dtstamp)));
    if let Some(created) = event.created {
        c.push(Property::new("CREATED", format_utc(created)));
    }
    if let Some(modified) = event.last_modified {
        c.push(Property::new("LAST-MODIFIED", format_utc(modified)));
    }
    c.push(Property::new("SEQUENCE", event.sequence.max(0).to_string()));
    if let Some(rid) = &event.recurrence_id {
        c.push(time_property("RECURRENCE-ID", rid));
    }
    c.push(time_property("DTSTART", &event.start));
    c.push(time_property("DTEND", &event.end));
    if let Some(rule) = event
        .rrule
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        c.push(Property::new("RRULE", rule));
    }
    for rdate in &event.rdates {
        c.push(time_property("RDATE", rdate));
    }
    for exdate in &event.exdates {
        c.push(time_property("EXDATE", exdate));
    }
    if !event.summary.is_empty() {
        c.push(Property::text("SUMMARY", &event.summary));
    }
    if let Some(description) = event.description.as_deref().filter(|d| !d.is_empty()) {
        c.push(Property::text("DESCRIPTION", description));
    }
    if let Some(html) = event.description_html.as_deref().filter(|d| !d.is_empty()) {
        c.push(Property::text("X-ALT-DESC", html).with_param("FMTTYPE", "text/html"));
    }
    if let Some(location) = event.location.as_deref().filter(|l| !l.is_empty()) {
        c.push(Property::text("LOCATION", location));
    }
    if let Some(url) = event.url.as_deref().filter(|u| !u.is_empty()) {
        c.push(Property::new("URL", url.trim()));
    }
    c.push(Property::new("STATUS", status_value(event.status)));
    c.push(Property::new(
        "TRANSP",
        transparency_value(event.transparency),
    ));
    if event.class != EventClass::Public {
        c.push(Property::new("CLASS", class_value(event.class)));
    }
    if let Some(organizer) = &event.organizer {
        c.push(organizer_property(organizer));
    }
    for attendee in &event.attendees {
        c.push(attendee_property(attendee));
    }
    if let Some(categories) = categories_property(&event.categories) {
        c.push(categories);
    }
    if let Some(color) = event.color.as_deref().and_then(color_property) {
        c.push(color);
    }
    if let Some(url) = &event.conference_url
        && !conference_mentioned(event, url)
    {
        c.push(conference_property(url));
    }
    if include_alarms {
        for alarm in &event.alarms {
            c.components.push(alarm_component(alarm, &event.summary));
        }
    }
    c
}

/// Creates a new `VCALENDAR` (with `VTIMEZONE`s for every zoned time) for
/// one event. See [`create_calendar_at`].
pub fn create_calendar(event: &Event) -> Result<String> {
    create_calendar_at(event, Utc::now())
}

/// [`create_calendar`] with an explicit current time.
pub fn create_calendar_at(event: &Event, now: DateTime<Utc>) -> Result<String> {
    let calendar = build_calendar(std::slice::from_ref(event), None, now, true)?;
    Ok(calendar.to_ical())
}

/// Builds a calendar holding `events` (a master and its overrides, or
/// unrelated events), with `VTIMEZONE`s and an optional `METHOD`.
pub(crate) fn build_calendar(
    events: &[Event],
    method: Option<&str>,
    now: DateTime<Utc>,
    include_alarms: bool,
) -> Result<Component> {
    if events.is_empty() {
        return Err(Error::NoEvents);
    }
    let mut calendar = new_calendar(method);
    for event in events {
        if event.uid.trim().is_empty() {
            return Err(Error::MissingUid);
        }
        let mut vevent = event_component(event, now, include_alarms);
        if method.is_none() {
            if event.created.is_none() {
                insert_after(
                    &mut vevent,
                    Property::new("CREATED", format_utc(now)),
                    &["DTSTAMP"],
                );
            }
            if event.last_modified.is_none() {
                insert_after(
                    &mut vevent,
                    Property::new("LAST-MODIFIED", format_utc(now)),
                    &["CREATED", "DTSTAMP"],
                );
            }
        }
        calendar.components.push(vevent);
    }
    ensure_timezones(&mut calendar);
    Ok(calendar)
}

/// Inserts `property` after the last property named in `anchors` (or at
/// the end).
pub(crate) fn insert_after(component: &mut Component, property: Property, anchors: &[&str]) {
    let position = component
        .properties
        .iter()
        .rposition(|p| anchors.iter().any(|a| p.name.eq_ignore_ascii_case(a)));
    match position {
        Some(index) => component.properties.insert(index + 1, property),
        None => component.properties.push(property),
    }
}

/// Parsed text plus the index of the `VCALENDAR` to edit.
struct Document {
    components: Vec<Component>,
    calendar: usize,
}

impl Document {
    fn load(existing: &str, uid: Option<&str>) -> Result<Self> {
        let mut components = parse_components(existing);
        if !components.iter().any(|c| c.is("VCALENDAR")) {
            let loose: Vec<Component> = components
                .into_iter()
                .filter(|c| {
                    ["VEVENT", "VTODO", "VJOURNAL", "VTIMEZONE", "VFREEBUSY"]
                        .iter()
                        .any(|n| c.is(n))
                })
                .collect();
            if loose.is_empty() {
                return Err(Error::NoCalendar);
            }
            let mut calendar = new_calendar(None);
            calendar.components = loose;
            components = vec![calendar];
        }
        let calendar = uid
            .and_then(|uid| {
                components.iter().position(|c| {
                    c.is("VCALENDAR") && c.children("VEVENT").any(|e| uid_of(e) == Some(uid.trim()))
                })
            })
            .or_else(|| components.iter().position(|c| c.is("VCALENDAR")))
            .ok_or(Error::NoCalendar)?;
        Ok(Document {
            components,
            calendar,
        })
    }

    fn calendar(&mut self) -> &mut Component {
        &mut self.components[self.calendar]
    }

    fn finish(mut self) -> String {
        ensure_timezones(self.calendar());
        write_components(&self.components)
    }
}

fn uid_of(vevent: &Component) -> Option<&str> {
    vevent.value("UID").map(str::trim)
}

fn raw_time(vevent: &Component, name: &str) -> Option<EventTime> {
    vevent.property(name).and_then(property_time)
}

/// Index of the `VEVENT` with `uid` and `RECURRENCE-ID` matching `rid`
/// (`None` = the master / single event).
fn find_vevent(
    calendar: &Component,
    uid: &str,
    rid: Option<&EventTime>,
    ctx: &TzContext,
) -> Option<usize> {
    let uid = uid.trim();
    calendar.components.iter().position(|c| {
        c.is("VEVENT")
            && uid_of(c) == Some(uid)
            && match (raw_time(c, "RECURRENCE-ID"), rid) {
                (None, None) => true,
                (Some(own), Some(wanted)) => ctx.same_instant(&own, wanted, EDIT_TZ),
                _ => false,
            }
    })
}

fn rid_label(rid: Option<&EventTime>) -> Option<String> {
    rid.map(format_event_time)
}

/// Sets `DTSTAMP` and `LAST-MODIFIED` to `now` and optionally increments `SEQUENCE`.
pub(crate) fn touch(vevent: &mut Component, now: DateTime<Utc>, bump_sequence: bool) {
    if bump_sequence {
        let sequence = vevent
            .value("SEQUENCE")
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        if vevent.property("SEQUENCE").is_some() {
            vevent.set(Property::new("SEQUENCE", (sequence + 1).to_string()));
        } else {
            insert_after(
                vevent,
                Property::new("SEQUENCE", (sequence + 1).to_string()),
                &["UID", "DTSTAMP"],
            );
        }
    }
    let stamp = format_utc(now);
    if vevent.property("DTSTAMP").is_some() {
        vevent.set(Property::new("DTSTAMP", stamp.clone()));
    } else {
        insert_after(vevent, Property::new("DTSTAMP", stamp.clone()), &["UID"]);
    }
    if vevent.property("LAST-MODIFIED").is_some() {
        vevent.set(Property::new("LAST-MODIFIED", stamp));
    } else {
        insert_after(vevent, Property::new("LAST-MODIFIED", stamp), &["DTSTAMP"]);
    }
}

/// Collects every TZID referenced below `component` with the earliest
/// wall-clock time it is used with.
fn collect_tzids(component: &Component, out: &mut BTreeMap<String, Option<NaiveDateTime>>) {
    for property in &component.properties {
        let Some(tzid) = property
            .param("TZID")
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        let earliest = property
            .value
            .split(',')
            .filter_map(|v| parse_date_time(v.split('/').next().unwrap_or("")).map(|(dt, _)| dt))
            .min();
        let entry = out.entry(tzid.to_owned()).or_insert(None);
        if let Some(earliest) = earliest {
            *entry = Some(entry.map_or(earliest, |e| e.min(earliest)));
        }
    }
    for child in &component.components {
        collect_tzids(child, out);
    }
}

/// Makes the `VTIMEZONE`s of a calendar match its TZID references: adds
/// generated definitions for referenced zones that lack one (when the TZID
/// resolves to an IANA zone) and drops definitions nothing refers to.
pub(crate) fn ensure_timezones(calendar: &mut Component) {
    let mut referenced: BTreeMap<String, Option<NaiveDateTime>> = BTreeMap::new();
    for child in calendar.components.iter().filter(|c| !c.is("VTIMEZONE")) {
        collect_tzids(child, &mut referenced);
    }
    calendar.components.retain(|c| {
        !c.is("VTIMEZONE")
            || c.value("TZID")
                .is_some_and(|tzid| referenced.contains_key(tzid.trim()))
    });
    let present: HashSet<String> = calendar
        .children("VTIMEZONE")
        .filter_map(|c| c.value("TZID").map(|t| t.trim().to_owned()))
        .collect();
    let mut added = Vec::new();
    for (tzid, earliest) in referenced {
        if present.contains(&tzid) {
            continue;
        }
        let Some(tz) = resolve_tzid(&tzid) else {
            continue;
        };
        let around = earliest
            .map(|local| local.and_utc())
            .unwrap_or_else(Utc::now);
        let mut vtimezone = generate_vtimezone(tz, around);
        vtimezone.set(Property::new("TZID", tzid));
        added.push(vtimezone);
    }
    if added.is_empty() {
        return;
    }
    let position = calendar
        .components
        .iter()
        .position(|c| !c.is("VTIMEZONE"))
        .unwrap_or(calendar.components.len());
    for (offset, vtimezone) in added.into_iter().enumerate() {
        calendar.components.insert(position + offset, vtimezone);
    }
}

/// Inserts or updates `event` in existing calendar text. See
/// [`upsert_event_at`].
pub fn upsert_event(existing: &str, event: &Event) -> Result<String> {
    upsert_event_at(existing, event, Utc::now())
}

/// Inserts or updates `event` in existing calendar text.
///
/// The `VEVENT` with the same UID and `RECURRENCE-ID` is updated in place:
/// only properties whose value differs from what the existing data
/// expresses are rewritten, so unknown properties, parameters and
/// subcomponents survive. `DTSTAMP` and `LAST-MODIFIED` are set to `now`;
/// `SEQUENCE` is incremented when the time, recurrence, location or
/// cancellation changed (RFC 5546 §2.1.4), and never decreases. Events not
/// present yet are appended. `VTIMEZONE`s are added and pruned as needed.
/// Empty `existing` text creates a new calendar.
pub fn upsert_event_at(existing: &str, event: &Event, now: DateTime<Utc>) -> Result<String> {
    if event.uid.trim().is_empty() {
        return Err(Error::MissingUid);
    }
    if existing.trim().is_empty() {
        return create_calendar_at(event, now);
    }
    let mut document = Document::load(existing, Some(&event.uid))?;
    let calendar = document.calendar();
    let ctx = TzContext::from_calendar(calendar);
    match find_vevent(calendar, &event.uid, event.recurrence_id.as_ref(), &ctx) {
        Some(index) => merge_event(&mut calendar.components[index], event, &ctx, now),
        None => {
            let mut vevent = event_component(event, now, true);
            if event.created.is_none() {
                insert_after(
                    &mut vevent,
                    Property::new("CREATED", format_utc(now)),
                    &["DTSTAMP"],
                );
            }
            vevent.set(Property::new("LAST-MODIFIED", format_utc(now)));
            let position = calendar
                .components
                .iter()
                .rposition(|c| c.is("VEVENT") && uid_of(c) == Some(event.uid.trim()))
                .map_or(calendar.components.len(), |i| i + 1);
            calendar.components.insert(position, vevent);
        }
    }
    Ok(document.finish())
}

/// Replaces all properties called `name` by those of `fresh`.
fn replace_from(vevent: &mut Component, fresh: &Component, name: &str) {
    let properties: Vec<Property> = fresh.properties_named(name).cloned().collect();
    vevent.set_all(name, properties);
}

/// Sets a TEXT property, keeping its parameters except `ALTREP` (which
/// would point at the old text); removes it for empty values.
pub(crate) fn set_text(vevent: &mut Component, name: &str, value: Option<&str>) {
    match value.filter(|v| !v.is_empty()) {
        None => {
            vevent.remove_all(name);
        }
        Some(text) => {
            let mut property = vevent
                .property(name)
                .cloned()
                .unwrap_or_else(|| Property::new(name, ""));
            property.value = escape_text(text);
            property.remove_param("ALTREP");
            vevent.set(property);
        }
    }
}

fn merge_event(vevent: &mut Component, event: &Event, ctx: &TzContext, now: DateTime<Utc>) {
    let options = ExtractOptions {
        default_tz: EDIT_TZ,
        normalize_tzids: true,
    };
    let Some(old) = event_from_vevent(vevent, ctx, &options) else {
        *vevent = event_component(event, now, true);
        return;
    };
    let fresh = event_component(event, now, true);

    if old.summary != event.summary {
        set_text(vevent, "SUMMARY", Some(&event.summary));
    }
    if old.description != event.description {
        set_text(vevent, "DESCRIPTION", event.description.as_deref());
    }
    if old.description_html != event.description_html {
        replace_from(vevent, &fresh, "X-ALT-DESC");
    }
    if old.location != event.location {
        set_text(vevent, "LOCATION", event.location.as_deref());
    }
    if old.url != event.url {
        replace_from(vevent, &fresh, "URL");
    }
    let start_changed = old.start != event.start;
    let end_changed = old.end != event.end;
    if start_changed {
        replace_from(vevent, &fresh, "DTSTART");
    }
    let has_duration = vevent.property("DURATION").is_some();
    if end_changed || (start_changed && has_duration) {
        vevent.remove_all("DURATION");
        if vevent.property("DTEND").is_some() {
            replace_from(vevent, &fresh, "DTEND");
        } else {
            insert_after(vevent, time_property("DTEND", &event.end), &["DTSTART"]);
        }
    }
    if old.rrule != event.rrule {
        replace_from(vevent, &fresh, "RRULE");
    }
    if old.rdates != event.rdates {
        replace_from(vevent, &fresh, "RDATE");
    }
    if old.exdates != event.exdates {
        replace_from(vevent, &fresh, "EXDATE");
    }
    if old.status != event.status {
        replace_from(vevent, &fresh, "STATUS");
    }
    if old.transparency != event.transparency {
        replace_from(vevent, &fresh, "TRANSP");
        vevent.remove_all("X-MICROSOFT-CDO-BUSYSTATUS");
    }
    if old.class != event.class {
        replace_from(vevent, &fresh, "CLASS");
    }
    if old.organizer != event.organizer {
        merge_organizer(vevent, event.organizer.as_ref());
    }
    if old.attendees != event.attendees {
        merge_attendees(vevent, &event.attendees);
    }
    if old.alarms != event.alarms || start_changed || end_changed {
        merge_alarms(vevent, event, ctx);
    }
    if old.categories != event.categories {
        replace_from(vevent, &fresh, "CATEGORIES");
    }
    if old.color != event.color {
        replace_from(vevent, &fresh, "COLOR");
    }
    if old.conference_url != event.conference_url {
        if let Some(previous) = &old.conference_url {
            for name in [
                "CONFERENCE",
                "X-GOOGLE-CONFERENCE",
                "X-MICROSOFT-SKYPETEAMSMEETINGURL",
                "X-MICROSOFT-ONLINEMEETINGCONFLINK",
            ] {
                vevent
                    .properties
                    .retain(|p| !(p.name == name && p.value.trim() == previous.as_str()));
            }
        }
        if let Some(url) = &event.conference_url
            && !conference_mentioned(event, url)
        {
            vevent.push(conference_property(url));
        }
    }

    let significant = start_changed
        || end_changed
        || old.rrule != event.rrule
        || old.rdates != event.rdates
        || old.exdates != event.exdates
        || old.location != event.location
        || (event.status == EventStatus::Cancelled && old.status != EventStatus::Cancelled);
    let sequence = if significant {
        (old.sequence + 1).max(event.sequence)
    } else {
        old.sequence.max(event.sequence)
    };
    if sequence != old.sequence || vevent.property("SEQUENCE").is_some() {
        if vevent.property("SEQUENCE").is_some() {
            vevent.set(Property::new("SEQUENCE", sequence.to_string()));
        } else {
            insert_after(
                vevent,
                Property::new("SEQUENCE", sequence.to_string()),
                &["UID", "DTSTAMP"],
            );
        }
    }
    touch(vevent, now, false);
    if vevent.property("CREATED").is_none()
        && let Some(created) = event.created
    {
        insert_after(
            vevent,
            Property::new("CREATED", format_utc(created)),
            &["DTSTAMP"],
        );
    }
}

fn merge_organizer(vevent: &mut Component, organizer: Option<&Organizer>) {
    let Some(organizer) = organizer else {
        vevent.remove_all("ORGANIZER");
        return;
    };
    let property = match vevent.property("ORGANIZER").cloned() {
        Some(mut existing)
            if cal_address(&existing).as_deref() == Some(organizer.email.as_str()) =>
        {
            match &organizer.name {
                Some(name) => existing.set_param("CN", name.clone()),
                None => existing.remove_param("CN"),
            }
            existing
        }
        _ => organizer_property(organizer),
    };
    vevent.set(property);
}

fn merge_attendees(vevent: &mut Component, attendees: &[Attendee]) {
    let existing: Vec<Property> = vevent.properties_named("ATTENDEE").cloned().collect();
    let merged: Vec<Property> = attendees
        .iter()
        .map(|attendee| {
            existing
                .iter()
                .find(|p| cal_address(p).as_deref() == Some(attendee.email.as_str()))
                .map_or_else(
                    || attendee_property(attendee),
                    |p| update_attendee(p.clone(), attendee),
                )
        })
        .collect();
    vevent.set_all("ATTENDEE", merged);
}

/// Updates the parameters of an existing `ATTENDEE` that differ from the
/// model, keeping all others (`X-NUM-GUESTS`, `DELEGATED-FROM`, …).
fn update_attendee(mut property: Property, attendee: &Attendee) -> Property {
    let Some(current) = parse_attendee(&property) else {
        return attendee_property(attendee);
    };
    if current == *attendee {
        return property;
    }
    if current.name != attendee.name {
        match &attendee.name {
            Some(name) => property.set_param("CN", name.clone()),
            None => property.remove_param("CN"),
        }
    }
    if current.partstat != attendee.partstat {
        property.set_param("PARTSTAT", partstat_value(attendee.partstat));
        // A new participation status invalidates the server's delivery status.
        property.remove_param("SCHEDULE-STATUS");
    }
    if current.role != attendee.role {
        property.set_param("ROLE", role_value(attendee.role));
    }
    if current.cutype != attendee.cutype {
        property.set_param("CUTYPE", cutype_value(attendee.cutype));
    }
    if current.rsvp != attendee.rsvp {
        property.set_param("RSVP", if attendee.rsvp { "TRUE" } else { "FALSE" });
    }
    property
}

/// Keeps existing `VALARM`s that still express one of the event's alarms
/// (with all their extra properties), drops the others and appends new
/// ones. Alarms the model cannot represent (e.g. `ACTION:NONE`) are kept.
fn merge_alarms(vevent: &mut Component, event: &Event, ctx: &TzContext) {
    let mut remaining: Vec<&Alarm> = event.alarms.iter().collect();
    let children = std::mem::take(&mut vevent.components);
    for child in children {
        if !child.is("VALARM") {
            vevent.components.push(child);
            continue;
        }
        match parse_alarm(
            &child,
            &event.start,
            &event.end,
            &event.summary,
            ctx,
            EDIT_TZ,
        ) {
            None => vevent.components.push(child),
            Some(parsed) => {
                if let Some(position) = remaining.iter().position(|a| **a == parsed) {
                    remaining.remove(position);
                    vevent.components.push(child);
                }
            }
        }
    }
    for alarm in remaining {
        vevent
            .components
            .push(alarm_component(alarm, &event.summary));
    }
}

/// Excludes one occurrence from a series by adding an `EXDATE`. See
/// [`add_exdate_at`].
pub fn add_exdate(existing: &str, uid: &str, occurrence: &EventTime) -> Result<String> {
    add_exdate_at(existing, uid, occurrence, Utc::now())
}

/// Deletes one occurrence of the series `uid`: adds an `EXDATE` (written in
/// the same form as the series' `DTSTART`, e.g. with its TZID), removes an
/// override for that occurrence if there is one, and bumps `SEQUENCE`.
/// `occurrence` may be given in any form denoting the same instant.
pub fn add_exdate_at(
    existing: &str,
    uid: &str,
    occurrence: &EventTime,
    now: DateTime<Utc>,
) -> Result<String> {
    let mut document = Document::load(existing, Some(uid))?;
    let calendar = document.calendar();
    let ctx = TzContext::from_calendar(calendar);
    let master_index =
        find_vevent(calendar, uid, None, &ctx).ok_or_else(|| Error::EventNotFound {
            uid: uid.to_owned(),
            recurrence_id: None,
        })?;
    let master = &mut calendar.components[master_index];
    let like = raw_time(master, "DTSTART");
    let recurring = master.property("RRULE").is_some() || master.property("RDATE").is_some();
    let Some(like) = like.filter(|_| recurring) else {
        return Err(Error::NotRecurring {
            uid: uid.to_owned(),
        });
    };
    let exdate = ctx.express_like(occurrence, &like, EDIT_TZ);
    let already = master
        .properties_named("EXDATE")
        .flat_map(property_times)
        .any(|t| ctx.same_instant(&t, &exdate, EDIT_TZ));
    if !already {
        insert_after(
            master,
            time_property("EXDATE", &exdate),
            &["EXDATE", "RDATE", "RRULE", "DTEND", "DTSTART"],
        );
    }
    touch(master, now, true);
    calendar.components.retain(|c| {
        !(c.is("VEVENT")
            && uid_of(c) == Some(uid.trim())
            && raw_time(c, "RECURRENCE-ID")
                .is_some_and(|rid| ctx.same_instant(&rid, occurrence, EDIT_TZ)))
    });
    Ok(document.finish())
}

/// Ends a series before `from`. See [`truncate_series_at`].
pub fn truncate_series(existing: &str, uid: &str, from: &EventTime) -> Result<String> {
    truncate_series_at(existing, uid, from, Utc::now())
}

/// Ends the series `uid` just before the occurrence `from`, for "this and
/// following" edits (the caller then creates a new series starting at
/// `from`).
///
/// The `RRULE` gets an inclusive `UNTIL` one second before `from` (in UTC
/// for zoned and UTC series, as wall-clock time for floating series) or
/// the previous day as a `DATE` for all-day series; `COUNT` is removed.
/// An earlier existing `UNTIL` is kept. `RDATE`s and `EXDATE`s at or after
/// `from` are dropped, as are overrides of occurrences at or after `from`.
/// Fails with [`Error::TruncateAtSeriesStart`] if `from` is not after the
/// first occurrence.
pub fn truncate_series_at(
    existing: &str,
    uid: &str,
    from: &EventTime,
    now: DateTime<Utc>,
) -> Result<String> {
    let mut document = Document::load(existing, Some(uid))?;
    let calendar = document.calendar();
    let ctx = TzContext::from_calendar(calendar);
    let master_index =
        find_vevent(calendar, uid, None, &ctx).ok_or_else(|| Error::EventNotFound {
            uid: uid.to_owned(),
            recurrence_id: None,
        })?;
    let master = &mut calendar.components[master_index];
    let recurring = master.property("RRULE").is_some() || master.property("RDATE").is_some();
    let Some(like) = raw_time(master, "DTSTART").filter(|_| recurring) else {
        return Err(Error::NotRecurring {
            uid: uid.to_owned(),
        });
    };
    let at_or_after = |time: &EventTime| -> bool {
        if time.is_date() || from.is_date() || like.is_date() {
            wall_clock(&ctx.express_like(time, &like, EDIT_TZ)).date()
                >= wall_clock(&ctx.express_like(from, &like, EDIT_TZ)).date()
        } else {
            ctx.to_utc(time, EDIT_TZ) >= ctx.to_utc(from, EDIT_TZ)
        }
    };
    if at_or_after(&like) {
        return Err(Error::TruncateAtSeriesStart {
            uid: uid.to_owned(),
        });
    }

    if let Some(rule_value) = master.value("RRULE").map(str::to_owned) {
        let new_until = match &like {
            EventTime::Date { .. } => {
                let date = wall_clock(&ctx.express_like(from, &like, EDIT_TZ)).date();
                EventTime::Date {
                    date: date.pred_opt().unwrap_or(date),
                }
            }
            EventTime::Floating { .. } => {
                let local = wall_clock(&ctx.express_like(from, &like, EDIT_TZ));
                EventTime::Floating {
                    local: local
                        .checked_sub_signed(TimeDelta::seconds(1))
                        .unwrap_or(local),
                }
            }
            EventTime::Zoned { .. } | EventTime::Utc { .. } => {
                let at = ctx.to_utc(from, EDIT_TZ);
                EventTime::Utc {
                    at: at.checked_sub_signed(TimeDelta::seconds(1)).unwrap_or(at),
                }
            }
        };
        let existing_until = RecurRule::parse(&rule_value).and_then(|r| r.until);
        let keep = existing_until.is_some_and(|until| until_not_after(&until, &new_until, &ctx));
        if !keep {
            master.set(Property::new(
                "RRULE",
                set_until(&rule_value, &format_event_time(&new_until)),
            ));
        }
    }
    for name in ["RDATE", "EXDATE"] {
        let mut kept = Vec::new();
        for mut property in master.properties_named(name).cloned().collect::<Vec<_>>() {
            let value_type = property
                .param("VALUE")
                .filter(|v| !v.eq_ignore_ascii_case("PERIOD"))
                .map(str::to_owned);
            let tzid = property.param("TZID").map(str::to_owned);
            let items: Vec<&str> = property
                .value
                .split(',')
                .filter(|item| {
                    let start = item.split('/').next().unwrap_or("");
                    parse_event_time(start, value_type.as_deref(), tzid.as_deref())
                        .is_none_or(|t| !at_or_after(&t))
                })
                .collect();
            if !items.is_empty() {
                property.value = items.join(",");
                kept.push(property);
            }
        }
        master.set_all(name, kept);
    }
    touch(master, now, true);
    calendar.components.retain(|c| {
        !(c.is("VEVENT")
            && uid_of(c) == Some(uid.trim())
            && raw_time(c, "RECURRENCE-ID").is_some_and(|rid| at_or_after(&rid)))
    });
    Ok(document.finish())
}

fn until_not_after(existing: &EventTime, new: &EventTime, ctx: &TzContext) -> bool {
    match (existing, new) {
        (EventTime::Date { date: a }, EventTime::Date { date: b }) => a <= b,
        (EventTime::Date { date }, other) => end_of_day(*date) <= wall_clock(other),
        (other, EventTime::Date { date }) => wall_clock(other).date() <= *date,
        (a, b) => ctx.to_utc(a, EDIT_TZ) <= ctx.to_utc(b, EDIT_TZ),
    }
}

fn end_of_day(date: NaiveDate) -> NaiveDateTime {
    date.and_hms_opt(23, 59, 59)
        .unwrap_or_else(|| date.and_time(chrono::NaiveTime::MIN))
}

/// Replaces `UNTIL`/`COUNT` in an RRULE value by `UNTIL=until`, keeping
/// every other part (including unknown ones) as written.
fn set_until(rule: &str, until: &str) -> String {
    let rule = rule.trim();
    let rule = match rule.get(..6) {
        Some(prefix) if prefix.eq_ignore_ascii_case("RRULE:") => &rule[6..],
        _ => rule,
    };
    let key = |part: &str| {
        part.split('=')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase()
    };
    let mut parts: Vec<String> = rule
        .split(';')
        .filter(|p| !p.trim().is_empty())
        .filter(|p| !matches!(key(p).as_str(), "UNTIL" | "COUNT"))
        .map(str::to_owned)
        .collect();
    let position = parts
        .iter()
        .position(|p| key(p) == "FREQ")
        .map_or(0, |i| i + 1);
    parts.insert(position, format!("UNTIL={until}"));
    parts.join(";")
}

/// Removes the override of one occurrence, restoring the regular
/// occurrence. Fails with [`Error::EventNotFound`] if there is no such
/// override.
pub fn remove_override(existing: &str, uid: &str, recurrence_id: &EventTime) -> Result<String> {
    let mut document = Document::load(existing, Some(uid))?;
    let calendar = document.calendar();
    let ctx = TzContext::from_calendar(calendar);
    let index = find_vevent(calendar, uid, Some(recurrence_id), &ctx).ok_or_else(|| {
        Error::EventNotFound {
            uid: uid.to_owned(),
            recurrence_id: rid_label(Some(recurrence_id)),
        }
    })?;
    calendar.components.remove(index);
    Ok(document.finish())
}

/// Removes every `VEVENT` with `uid` (the series and all its overrides).
/// Returns `Ok(None)` when the calendar holds no other component than
/// time zones afterwards, i.e. the whole resource should be deleted.
pub fn remove_event(existing: &str, uid: &str) -> Result<Option<String>> {
    let mut document = Document::load(existing, Some(uid))?;
    let calendar = document.calendar();
    let before = calendar.components.len();
    calendar
        .components
        .retain(|c| !(c.is("VEVENT") && uid_of(c) == Some(uid.trim())));
    if calendar.components.len() == before {
        return Err(Error::EventNotFound {
            uid: uid.to_owned(),
            recurrence_id: None,
        });
    }
    if calendar.components.iter().all(|c| c.is("VTIMEZONE")) && document.components.len() == 1 {
        return Ok(None);
    }
    Ok(Some(document.finish()))
}

/// A UTC timestamp formatted for tests and fixtures.
#[cfg(test)]
pub(crate) fn fixed_now() -> DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
        .single()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::parse_events;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").expect("valid test datetime")
    }

    #[test]
    fn set_until_keeps_other_parts() {
        assert_eq!(
            set_until("FREQ=WEEKLY;COUNT=10;BYDAY=MO;X-FOO=1", "20240101T000000Z"),
            "FREQ=WEEKLY;UNTIL=20240101T000000Z;BYDAY=MO;X-FOO=1"
        );
        assert_eq!(
            set_until("RRULE:BYDAY=MO;FREQ=WEEKLY;UNTIL=20300101", "20240101"),
            "BYDAY=MO;FREQ=WEEKLY;UNTIL=20240101"
        );
    }

    #[test]
    fn created_calendar_round_trips() {
        let mut event = Event::new(
            "abc-123",
            EventTime::Zoned {
                local: dt("2024-03-01 09:00"),
                tzid: "Europe/Istanbul".into(),
            },
            EventTime::Zoned {
                local: dt("2024-03-01 10:00"),
                tzid: "Europe/Istanbul".into(),
            },
        );
        event.summary = "Kahvaltı, toplantı; plan".into();
        event.rrule = Some("FREQ=WEEKLY;BYDAY=FR".into());
        event.alarms.push(Alarm {
            action: AlarmAction::Display,
            minutes_before: 15,
            description: None,
        });
        event.categories = vec!["Work".into()];
        event.color = Some("#ff0000".into());
        event.conference_url = Some("https://meet.google.com/abc-defg-hij".into());
        event.organizer = Some(Organizer {
            email: "me@example.com".into(),
            name: Some("Me".into()),
        });
        event.attendees.push(Attendee {
            email: "you@example.com".into(),
            rsvp: true,
            ..Attendee::default()
        });
        event.created = Some(fixed_now());
        event.last_modified = Some(fixed_now());
        let text = create_calendar_at(&event, fixed_now()).expect("calendar");
        assert!(text.starts_with(
            "BEGIN:VCALENDAR\r\nPRODID:-//Envelope//Envelope Mail//EN\r\nVERSION:2.0\r\n"
        ));
        assert!(text.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Istanbul\r\n"));
        let parsed = parse_events(&text).expect("events");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], event);
    }

    #[test]
    fn missing_uid_is_rejected() {
        let event = Event::new(
            "",
            EventTime::Utc { at: fixed_now() },
            EventTime::Utc { at: fixed_now() },
        );
        assert_eq!(
            create_calendar_at(&event, fixed_now()).unwrap_err(),
            Error::MissingUid
        );
        assert_eq!(
            upsert_event_at("BEGIN:VCALENDAR\nEND:VCALENDAR\n", &event, fixed_now()).unwrap_err(),
            Error::MissingUid
        );
    }
}
