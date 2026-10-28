//! Provider-neutral calendar and contact model.
//!
//! Every calendar backend (CalDAV, Google Calendar, Microsoft Graph, EWS) maps its
//! wire format into these types, so the rest of Envelope only ever deals with one
//! shape of event. The model intentionally mirrors RFC 5545 semantics.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// A point in time as written in the source calendar data.
///
/// The original form is preserved (all-day date, zoned wall-clock time, UTC
/// instant or floating time) because recurrence expansion and round-tripping
/// edits back to the server both depend on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EventTime {
    /// An all-day date (`DTSTART;VALUE=DATE:20240101`).
    Date { date: NaiveDate },
    /// Wall-clock time in a named time zone. `tzid` is ideally an IANA name
    /// (`Europe/Istanbul`), but may be any TZID found in the source data
    /// (for example a Windows zone name such as `W. Europe Standard Time`).
    Zoned { local: NaiveDateTime, tzid: String },
    /// An absolute instant (`DTSTART:20240101T090000Z`).
    Utc { at: DateTime<Utc> },
    /// Floating wall-clock time that follows whatever zone the viewer is in.
    Floating { local: NaiveDateTime },
}

impl EventTime {
    /// True for all-day (`VALUE=DATE`) values.
    pub fn is_date(&self) -> bool {
        matches!(self, EventTime::Date { .. })
    }
}

/// RSVP state of an attendee (`PARTSTAT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PartStat {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Delegated,
}

/// Attendee role (`ROLE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttendeeRole {
    Chair,
    #[default]
    Required,
    Optional,
    NonParticipant,
}

/// Kind of calendar user (`CUTYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CalendarUserType {
    #[default]
    Individual,
    Group,
    Resource,
    Room,
    Unknown,
}

/// An event attendee.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    /// Lower-cased e-mail address (without the `mailto:` prefix).
    pub email: String,
    pub name: Option<String>,
    pub partstat: PartStat,
    pub role: AttendeeRole,
    pub cutype: CalendarUserType,
    /// Whether the organizer asked for a reply.
    pub rsvp: bool,
}

/// The event organizer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Organizer {
    pub email: String,
    pub name: Option<String>,
}

/// `STATUS` of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventStatus {
    #[default]
    Confirmed,
    Tentative,
    Cancelled,
}

/// `TRANSP` — whether the event blocks time on free/busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Transparency {
    /// Busy.
    #[default]
    Opaque,
    /// Free.
    Transparent,
}

/// Access classification (`CLASS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventClass {
    #[default]
    Public,
    Private,
    Confidential,
}

/// What an alarm does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AlarmAction {
    #[default]
    Display,
    Audio,
    Email,
}

/// A reminder attached to an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alarm {
    pub action: AlarmAction,
    /// Minutes before the event start the alarm fires (negative = after start).
    /// Absolute triggers are converted relative to the event start.
    pub minutes_before: i64,
    pub description: Option<String>,
}

/// A calendar event (a `VEVENT`), either a single event, a recurring series
/// master, or an overridden occurrence of a series (`recurrence_id` set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// iCalendar `UID`. Shared between a series master and its overrides.
    pub uid: String,
    /// Set on overridden occurrences: the original start of the occurrence.
    pub recurrence_id: Option<EventTime>,
    pub summary: String,
    pub description: Option<String>,
    /// Optional HTML version of the description when the source provides one.
    pub description_html: Option<String>,
    pub location: Option<String>,
    pub url: Option<String>,
    pub start: EventTime,
    /// Exclusive end. Always present: computed from `DURATION` or defaults
    /// (one day for all-day events, zero length otherwise) when missing.
    pub end: EventTime,
    /// `RRULE` value without the `RRULE:` prefix, e.g. `FREQ=WEEKLY;BYDAY=MO,WE`.
    pub rrule: Option<String>,
    pub rdates: Vec<EventTime>,
    pub exdates: Vec<EventTime>,
    pub status: EventStatus,
    pub transparency: Transparency,
    pub class: EventClass,
    pub organizer: Option<Organizer>,
    pub attendees: Vec<Attendee>,
    pub alarms: Vec<Alarm>,
    pub categories: Vec<String>,
    /// CSS color (`#rrggbb`) if the event carries its own color.
    pub color: Option<String>,
    /// Online meeting link (Google Meet, Teams, Zoom, …) if one was found.
    pub conference_url: Option<String>,
    pub sequence: i64,
    pub created: Option<DateTime<Utc>>,
    pub last_modified: Option<DateTime<Utc>>,
}

impl Event {
    /// An empty event with the given UID and times; convenient for builders/tests.
    pub fn new(uid: impl Into<String>, start: EventTime, end: EventTime) -> Self {
        Event {
            uid: uid.into(),
            recurrence_id: None,
            summary: String::new(),
            description: None,
            description_html: None,
            location: None,
            url: None,
            start,
            end,
            rrule: None,
            rdates: Vec::new(),
            exdates: Vec::new(),
            status: EventStatus::Confirmed,
            transparency: Transparency::Opaque,
            class: EventClass::Public,
            organizer: None,
            attendees: Vec::new(),
            alarms: Vec::new(),
            categories: Vec::new(),
            color: None,
            conference_url: None,
            sequence: 0,
            created: None,
            last_modified: None,
        }
    }

    /// True if this is a recurring series master.
    pub fn is_recurring(&self) -> bool {
        self.recurrence_id.is_none() && (self.rrule.is_some() || !self.rdates.is_empty())
    }

    /// True for all-day events.
    pub fn is_all_day(&self) -> bool {
        self.start.is_date()
    }
}

/// A contact (subset of vCard that a mail client needs).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// vCard `UID` or provider id.
    pub uid: String,
    /// Formatted display name (`FN`).
    pub name: String,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub nickname: Option<String>,
    pub emails: Vec<ContactField>,
    pub phones: Vec<ContactField>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub note: Option<String>,
    pub birthday: Option<NaiveDate>,
    /// Photo as a `data:` URI or remote URL.
    pub photo: Option<String>,
    /// Postal addresses (`ADR`).
    #[serde(default)]
    pub addresses: Vec<ContactAddress>,
    /// Web sites (`URL`), typed like e-mail addresses.
    #[serde(default)]
    pub urls: Vec<ContactField>,
}

/// A typed value of a contact (an e-mail address or phone number).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactField {
    pub value: String,
    /// Lower-cased type label such as `work`, `home`, `cell`.
    pub kind: Option<String>,
    pub preferred: bool,
}

/// A postal address with the components of a vCard `ADR`. Absent components
/// are empty strings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContactAddress {
    /// Lower-cased type label such as `home` or `work`.
    pub kind: Option<String>,
    pub preferred: bool,
    pub po_box: String,
    /// Apartment, suite, building.
    pub extended: String,
    /// Street address; may span several lines.
    pub street: String,
    /// City.
    pub locality: String,
    /// State or province.
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

impl ContactAddress {
    /// An address typed as free text: every line goes into the street, which
    /// keeps the text exactly as written ([`formatted`](Self::formatted)
    /// gives it back) and is how most clients store unstructured addresses.
    pub fn from_text(text: &str, kind: Option<String>) -> Self {
        ContactAddress {
            kind,
            street: clean_lines(text),
            ..ContactAddress::default()
        }
    }

    /// True when every component is blank.
    pub fn is_empty(&self) -> bool {
        [
            &self.po_box,
            &self.extended,
            &self.street,
            &self.locality,
            &self.region,
            &self.postal_code,
            &self.country,
        ]
        .iter()
        .all(|c| c.trim().is_empty())
    }

    /// The address as multi-line text: street lines, then the city line
    /// (`Cupertino, CA 95014` when there is a region, `34710 Kadıköy`
    /// otherwise), then the country.
    pub fn formatted(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for part in [&self.po_box, &self.extended, &self.street] {
            let part = clean_lines(part);
            if !part.is_empty() {
                lines.push(part);
            }
        }
        let join = |parts: &[&str], sep: &str| {
            parts
                .iter()
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(sep)
        };
        let city = if self.region.trim().is_empty() {
            join(&[&self.postal_code, &self.locality], " ")
        } else {
            let place = join(&[&self.locality, &self.region], ", ");
            join(&[&place, &self.postal_code], " ")
        };
        if !city.is_empty() {
            lines.push(city);
        }
        let country = clean_lines(&self.country);
        if !country.is_empty() {
            lines.push(country);
        }
        lines.join("\n")
    }
}

/// Trims every line, drops blank ones and normalizes line breaks to `\n`.
fn clean_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
