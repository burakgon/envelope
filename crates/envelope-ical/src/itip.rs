//! iTIP (RFC 5546): reading invitations and building replies, requests,
//! cancellations and proposals for a new time (counters).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::component::{Component, Property};
use crate::error::{Error, Result};
use crate::events::{cal_address, parse_calendar};
use crate::model::{Attendee, Event, EventStatus, EventTime, PartStat};
use crate::mutate::{build_calendar, ensure_timezones, mailto, new_calendar, partstat_value};
use crate::recur::{ExpandOptions, expand};
use crate::tz::TzContext;
use crate::value::{format_utc, time_property};

/// An iTIP method (`METHOD` of the calendar object).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItipMethod {
    /// Publish a calendar entry; no interaction expected.
    Publish,
    /// Invitation or update from the organizer.
    Request,
    /// An attendee's answer.
    Reply,
    /// The organizer adds instances to a recurring event.
    Add,
    /// The organizer cancels the event or some instances.
    Cancel,
    /// An attendee asks for the latest version.
    Refresh,
    /// An attendee proposes a change.
    Counter,
    /// The organizer rejects a counter proposal.
    DeclineCounter,
    /// Any other (non-standard) method, upper-cased.
    Other(String),
}

impl ItipMethod {
    /// Parses a `METHOD` value (case-insensitive).
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "PUBLISH" => ItipMethod::Publish,
            "REQUEST" => ItipMethod::Request,
            "REPLY" => ItipMethod::Reply,
            "ADD" => ItipMethod::Add,
            "CANCEL" => ItipMethod::Cancel,
            "REFRESH" => ItipMethod::Refresh,
            "COUNTER" => ItipMethod::Counter,
            "DECLINECOUNTER" => ItipMethod::DeclineCounter,
            other => ItipMethod::Other(other.to_owned()),
        }
    }

    /// The `METHOD` keyword.
    pub fn as_str(&self) -> &str {
        match self {
            ItipMethod::Publish => "PUBLISH",
            ItipMethod::Request => "REQUEST",
            ItipMethod::Reply => "REPLY",
            ItipMethod::Add => "ADD",
            ItipMethod::Cancel => "CANCEL",
            ItipMethod::Refresh => "REFRESH",
            ItipMethod::Counter => "COUNTER",
            ItipMethod::DeclineCounter => "DECLINECOUNTER",
            ItipMethod::Other(other) => other,
        }
    }
}

impl fmt::Display for ItipMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed iTIP message (typically a `text/calendar` mail attachment).
#[derive(Debug, Clone)]
pub struct ItipMessage {
    /// The `METHOD`, or `None` for plain calendar data.
    pub method: Option<ItipMethod>,
    /// The events of the message (master and/or instances).
    pub events: Vec<Event>,
    /// The raw `VCALENDAR`, for building replies that preserve it.
    pub calendar: Component,
    /// Resolver for the message's `VTIMEZONE`s.
    pub tz: TzContext,
}

impl ItipMessage {
    /// The note the sender wrote to the message (`COMMENT`), such as an
    /// attendee's words with a reply or a proposal: that of the first
    /// `VEVENT` having one, several joined by line breaks.
    pub fn comment(&self) -> Option<String> {
        self.calendar
            .children("VEVENT")
            .map(|vevent| {
                vevent
                    .properties_named("COMMENT")
                    .map(|p| p.text_value().trim().to_owned())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .find(|c| !c.is_empty())
    }

    /// Whether the organizer asked not to be sent new time proposals
    /// (Outlook's `X-MICROSOFT-DISALLOW-COUNTER:TRUE`).
    pub fn disallows_counter(&self) -> bool {
        self.calendar.children("VEVENT").any(|vevent| {
            vevent
                .value("X-MICROSOFT-DISALLOW-COUNTER")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("TRUE"))
        })
    }
}

/// Parses an iTIP message.
///
/// ```
/// use envelope_ical::itip::{ItipMethod, parse_itip};
/// let msg = parse_itip(
///     "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20240101T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
/// )
/// .unwrap();
/// assert_eq!(msg.method, Some(ItipMethod::Request));
/// assert_eq!(msg.events[0].uid, "x");
/// ```
pub fn parse_itip(text: &str) -> Result<ItipMessage> {
    let data = parse_calendar(text)?;
    let calendar = data
        .components
        .iter()
        .find(|c| c.is("VCALENDAR"))
        .cloned()
        .unwrap_or_else(|| {
            let mut calendar = new_calendar(None);
            calendar.components = data.components.clone();
            calendar
        });
    Ok(ItipMessage {
        method: data.method.as_deref().map(ItipMethod::parse),
        events: data.events,
        calendar,
        tz: data.tz,
    })
}

/// The participation status of the user in `event`, given all of the
/// user's addresses (case-insensitive). The organizer is considered to
/// have accepted unless listed as an attendee. `None` if the user is not
/// involved.
pub fn my_partstat<S: AsRef<str>>(event: &Event, my_emails: &[S]) -> Option<PartStat> {
    let mine = |email: &str| {
        my_emails
            .iter()
            .any(|m| m.as_ref().trim().eq_ignore_ascii_case(email.trim()))
    };
    if let Some(attendee) = event.attendees.iter().find(|a| mine(&a.email)) {
        return Some(attendee.partstat);
    }
    event
        .organizer
        .as_ref()
        .filter(|o| mine(&o.email))
        .map(|_| PartStat::Accepted)
}

/// Properties copied from the invitation into a `REPLY` (RFC 5546 §3.2.3).
const REPLY_PROPERTIES: [&str; 12] = [
    "UID",
    "RECURRENCE-ID",
    "SEQUENCE",
    "ORGANIZER",
    "SUMMARY",
    "DTSTART",
    "DTEND",
    "DURATION",
    "RRULE",
    "RDATE",
    "EXDATE",
    "LOCATION",
];

/// Builds a `METHOD:REPLY` for one attendee. See [`build_reply_at`].
pub fn build_reply(
    invitation: &str,
    attendee_email: &str,
    partstat: PartStat,
    comment: Option<&str>,
) -> Result<String> {
    build_reply_at(invitation, attendee_email, partstat, comment, Utc::now())
}

/// Builds a `METHOD:REPLY` answering `invitation` for `attendee_email`.
///
/// Every `VEVENT` of the invitation is answered with the same `UID`,
/// `RECURRENCE-ID`, `SEQUENCE`, `ORGANIZER` and times, a fresh `DTSTAMP`,
/// only the replying `ATTENDEE` (keeping its parameters, with the new
/// `PARTSTAT` and without `RSVP`) and the optional `COMMENT`. Alarms and
/// other attendees are not included. An attendee missing from the
/// invitation (invited through a list or alias) is added.
pub fn build_reply_at(
    invitation: &str,
    attendee_email: &str,
    partstat: PartStat,
    comment: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String> {
    let message = parse_itip(invitation)?;
    let email = attendee_email.trim().to_ascii_lowercase();
    let mut reply = new_calendar(Some("REPLY"));
    for vtimezone in message.calendar.children("VTIMEZONE") {
        reply.components.push(vtimezone.clone());
    }
    let mut answered = 0;
    for vevent in message.calendar.children("VEVENT") {
        let mut out = Component::new("VEVENT");
        for name in REPLY_PROPERTIES {
            for property in vevent.properties_named(name) {
                out.push(property.clone());
            }
        }
        out.push(Property::new("DTSTAMP", format_utc(now)));
        out.push(answering_attendee(vevent, &email, partstat));
        if let Some(comment) = comment.map(str::trim).filter(|c| !c.is_empty()) {
            out.push(Property::text("COMMENT", comment));
        }
        reply.components.push(out);
        answered += 1;
    }
    if answered == 0 {
        return Err(Error::NoEvents);
    }
    ensure_timezones(&mut reply);
    Ok(reply.to_ical())
}

/// The `ATTENDEE` answering `vevent` for `email` (lower-cased): theirs from
/// the invitation, keeping its parameters, or a new one for someone invited
/// through a list or an alias, with `partstat` and without `RSVP`.
fn answering_attendee(vevent: &Component, email: &str, partstat: PartStat) -> Property {
    let mut attendee = vevent
        .properties_named("ATTENDEE")
        .find(|p| cal_address(p).as_deref() == Some(email))
        .cloned()
        .unwrap_or_else(|| Property::new("ATTENDEE", mailto(email)));
    attendee.set_param("PARTSTAT", partstat_value(partstat));
    attendee.remove_param("RSVP");
    attendee.remove_param("SCHEDULE-STATUS");
    attendee
}

/// Properties copied from the invitation into a `COUNTER`, besides the
/// proposed times (RFC 5546 §3.2.7).
const COUNTER_PROPERTIES: [&str; 9] = [
    "UID",
    "RECURRENCE-ID",
    "SEQUENCE",
    "ORGANIZER",
    "SUMMARY",
    "LOCATION",
    "RRULE",
    "RDATE",
    "EXDATE",
];

/// Builds a `METHOD:COUNTER`. See [`build_counter_at`].
pub fn build_counter(
    invitation: &str,
    attendee_email: &str,
    partstat: PartStat,
    start: &EventTime,
    end: &EventTime,
    comment: Option<&str>,
) -> Result<String> {
    build_counter_at(
        invitation,
        attendee_email,
        partstat,
        start,
        end,
        comment,
        Utc::now(),
    )
}

/// Builds a `METHOD:COUNTER`: `attendee_email` proposes that the event of
/// `invitation` moves to `start`–`end`, answering it with `partstat` at the
/// same time (Outlook answers `TENTATIVE`, or `DECLINED` when the attendee
/// had declined).
///
/// The proposal is for what the invitation is about: its event or series
/// (the `VEVENT` without `RECURRENCE-ID`), or else the occurrence it
/// carries. It keeps the invitation's `UID`, `RECURRENCE-ID`, `SEQUENCE`,
/// `ORGANIZER`, summary, location and recurrence, with a fresh `DTSTAMP`,
/// the proposed `DTSTART` and `DTEND`, only the proposing `ATTENDEE` (as in
/// a reply) and the optional `COMMENT`. Alarms are left out.
pub fn build_counter_at(
    invitation: &str,
    attendee_email: &str,
    partstat: PartStat,
    start: &EventTime,
    end: &EventTime,
    comment: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String> {
    let message = parse_itip(invitation)?;
    let email = attendee_email.trim().to_ascii_lowercase();
    let vevent = message
        .calendar
        .children("VEVENT")
        .find(|v| v.property("RECURRENCE-ID").is_none())
        .or_else(|| message.calendar.children("VEVENT").next())
        .ok_or(Error::NoEvents)?;
    let mut counter = new_calendar(Some("COUNTER"));
    for vtimezone in message.calendar.children("VTIMEZONE") {
        counter.components.push(vtimezone.clone());
    }
    let mut out = Component::new("VEVENT");
    for name in COUNTER_PROPERTIES {
        for property in vevent.properties_named(name) {
            out.push(property.clone());
        }
    }
    out.push(Property::new("DTSTAMP", format_utc(now)));
    out.push(time_property("DTSTART", start));
    out.push(time_property("DTEND", end));
    out.push(answering_attendee(vevent, &email, partstat));
    if let Some(comment) = comment.map(str::trim).filter(|c| !c.is_empty()) {
        out.push(Property::text("COMMENT", comment));
    }
    counter.components.push(out);
    ensure_timezones(&mut counter);
    Ok(counter.to_ical())
}

/// Builds a `METHOD:DECLINECOUNTER`. See [`build_decline_counter_at`].
pub fn build_decline_counter(
    event: &Event,
    recurrence_id: Option<&EventTime>,
    attendee_email: &str,
    comment: Option<&str>,
) -> Result<String> {
    build_decline_counter_at(event, recurrence_id, attendee_email, comment, Utc::now())
}

/// Builds a `METHOD:DECLINECOUNTER` for an event the user organizes,
/// telling `attendee_email`, who proposed another time, that `event` (the
/// whole event, or its occurrence `recurrence_id`) stays as it is.
///
/// It carries the event's `UID`, `RECURRENCE-ID`, `SEQUENCE` (which the
/// proposal echoed), `ORGANIZER`, summary, location and times, with a fresh
/// `DTSTAMP`, only the proposing `ATTENDEE` and the optional `COMMENT`.
pub fn build_decline_counter_at(
    event: &Event,
    recurrence_id: Option<&EventTime>,
    attendee_email: &str,
    comment: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String> {
    let email = attendee_email.trim().to_ascii_lowercase();
    let mut kept = match recurrence_id {
        Some(rid) => occurrence_of(event, rid),
        None => event.clone(),
    };
    let mut attendee = event
        .attendees
        .iter()
        .find(|a| a.email.trim().eq_ignore_ascii_case(&email))
        .cloned()
        .unwrap_or_else(|| Attendee {
            email: email.clone(),
            ..Attendee::default()
        });
    attendee.rsvp = false;
    kept.attendees = vec![attendee];
    kept.alarms.clear();
    let mut calendar = build_calendar(
        std::slice::from_ref(&kept),
        Some("DECLINECOUNTER"),
        now,
        false,
    )?;
    for vevent in calendar.components.iter_mut().filter(|c| c.is("VEVENT")) {
        for name in [
            "CREATED",
            "LAST-MODIFIED",
            "DESCRIPTION",
            "X-ALT-DESC",
            "URL",
            "STATUS",
            "TRANSP",
            "CLASS",
            "CATEGORIES",
            "COLOR",
            "CONFERENCE",
        ] {
            vevent.remove_all(name);
        }
        if let Some(comment) = comment.map(str::trim).filter(|c| !c.is_empty()) {
            vevent.push(Property::text("COMMENT", comment));
        }
    }
    ensure_timezones(&mut calendar);
    Ok(calendar.to_ical())
}

/// The occurrence `rid` of the series `event` as an event of its own: the
/// occurrence's times when the series produces it (else `rid` itself), its
/// `RECURRENCE-ID` written like the series' start, and no recurrence.
fn occurrence_of(event: &Event, rid: &EventTime) -> Event {
    let ctx = TzContext::new();
    let options = ExpandOptions::default();
    let rid_utc = ctx.to_utc(rid, options.default_tz);
    let window_end = rid_utc
        .checked_add_signed(chrono::TimeDelta::seconds(1))
        .unwrap_or(rid_utc);
    // Use the occurrence's own times when the series produces it.
    let occurrence = expand(event, &[], rid_utc, window_end, &ctx, &options)
        .occurrences
        .into_iter()
        .find(|o| {
            o.recurrence_id
                .as_ref()
                .is_some_and(|r| ctx.same_instant(r, rid, options.default_tz))
        });
    let rid_value = ctx.express_like(rid, &event.start, options.default_tz);
    let mut instance = event.clone();
    match occurrence {
        Some(occurrence) => {
            instance.start = occurrence.start_time;
            instance.end = occurrence.end_time;
        }
        None => {
            instance.start = rid_value.clone();
            instance.end = rid_value.clone();
        }
    }
    instance.recurrence_id = Some(rid_value);
    instance.rrule = None;
    instance.rdates.clear();
    instance.exdates.clear();
    instance
}

/// Builds a `METHOD:REQUEST` for events the user organizes. See
/// [`build_request_at`].
pub fn build_request(events: &[Event]) -> Result<String> {
    build_request_at(events, Utc::now())
}

/// Builds a `METHOD:REQUEST` (invitation or update) carrying `events` (a
/// series master and its overrides, or a single event) with `VTIMEZONE`s.
/// Alarms are left out, as they are personal.
pub fn build_request_at(events: &[Event], now: DateTime<Utc>) -> Result<String> {
    build_calendar(events, Some("REQUEST"), now, false).map(|c| c.to_ical())
}

/// Builds a `METHOD:CANCEL`. See [`build_cancel_at`].
pub fn build_cancel(event: &Event, recurrence_id: Option<&EventTime>) -> Result<String> {
    build_cancel_at(event, recurrence_id, Utc::now())
}

/// Builds a `METHOD:CANCEL` for an event the user organizes: the whole
/// event (or series), or the single occurrence `recurrence_id` of a
/// series. `SEQUENCE` is incremented and `STATUS` set to `CANCELLED`; the
/// attendees of `event` are the recipients.
pub fn build_cancel_at(
    event: &Event,
    recurrence_id: Option<&EventTime>,
    now: DateTime<Utc>,
) -> Result<String> {
    let mut cancelled = match recurrence_id {
        Some(rid) => occurrence_of(event, rid),
        None => event.clone(),
    };
    cancelled.status = EventStatus::Cancelled;
    cancelled.sequence = event.sequence + 1;
    cancelled.alarms.clear();
    let mut calendar =
        build_calendar(std::slice::from_ref(&cancelled), Some("CANCEL"), now, false)?;
    // Keep the cancellation minimal: no descriptive extras.
    for vevent in calendar.components.iter_mut().filter(|c| c.is("VEVENT")) {
        for name in [
            "DESCRIPTION",
            "X-ALT-DESC",
            "CATEGORIES",
            "COLOR",
            "TRANSP",
            "CONFERENCE",
        ] {
            vevent.remove_all(name);
        }
    }
    ensure_timezones(&mut calendar);
    Ok(calendar.to_ical())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attendee, Organizer};

    #[test]
    fn methods() {
        for method in [
            "PUBLISH",
            "REQUEST",
            "REPLY",
            "ADD",
            "CANCEL",
            "REFRESH",
            "COUNTER",
            "DECLINECOUNTER",
        ] {
            let parsed = ItipMethod::parse(&method.to_ascii_lowercase());
            assert_eq!(parsed.as_str(), method);
            assert!(!matches!(parsed, ItipMethod::Other(_)));
        }
        assert_eq!(
            ItipMethod::parse("x-foo"),
            ItipMethod::Other("X-FOO".into())
        );
    }

    #[test]
    fn partstat_for_user() {
        let mut event = Event::new(
            "u",
            EventTime::Utc { at: Utc::now() },
            EventTime::Utc { at: Utc::now() },
        );
        event.organizer = Some(Organizer {
            email: "boss@x.com".into(),
            name: None,
        });
        event.attendees.push(Attendee {
            email: "me@x.com".into(),
            partstat: PartStat::Tentative,
            ..Attendee::default()
        });
        assert_eq!(
            my_partstat(&event, &["ME@x.com"]),
            Some(PartStat::Tentative)
        );
        assert_eq!(
            my_partstat(&event, &["boss@x.com".to_string()]),
            Some(PartStat::Accepted)
        );
        assert_eq!(my_partstat(&event, &["other@x.com"]), None);
        assert_eq!(my_partstat::<&str>(&event, &[]), None);
    }

    const INVITATION: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//Example Corp.//Calendar 3.1//EN\r\n\
METHOD:REQUEST\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:Europe/Istanbul\r\n\
BEGIN:STANDARD\r\n\
DTSTART:19700101T000000\r\n\
TZOFFSETFROM:+0300\r\n\
TZOFFSETTO:+0300\r\n\
TZNAME:+03\r\n\
END:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
UID:review-1@example.com\r\n\
SEQUENCE:2\r\n\
DTSTAMP:20261028T090000Z\r\n\
DTSTART;TZID=Europe/Istanbul:20261105T100000\r\n\
DTEND;TZID=Europe/Istanbul:20261105T104500\r\n\
SUMMARY:Roadmap review\r\n\
LOCATION:Room Galata\r\n\
DESCRIPTION:A last look at the roadmap.\r\n\
ORGANIZER;CN=Priya Raman:mailto:priya@example.com\r\n\
ATTENDEE;CN=Priya Raman;PARTSTAT=ACCEPTED;ROLE=CHAIR:mailto:priya@example.com\r\n\
ATTENDEE;CN=Alex Morgan;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:alex@example.com\r\n\
X-MICROSOFT-DISALLOW-COUNTER:FALSE\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
TRIGGER:-PT15M\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|day| day.and_hms_opt(h, mi, 0))
            .unwrap()
    }

    fn istanbul(h: u32, mi: u32) -> EventTime {
        EventTime::Zoned {
            local: at(2026, 11, 5, h, mi),
            tzid: "Europe/Istanbul".into(),
        }
    }

    fn now() -> DateTime<Utc> {
        at(2026, 10, 30, 8, 0).and_utc()
    }

    #[test]
    fn counter_round_trip() {
        let (start, end) = (istanbul(11, 0), istanbul(11, 45));
        let counter = build_counter_at(
            INVITATION,
            "Alex@Example.com",
            PartStat::Tentative,
            &start,
            &end,
            Some("  Could we meet an hour later?  "),
            now(),
        )
        .unwrap();
        assert!(counter.contains("DTSTAMP:20261030T080000Z"));
        assert!(counter.contains("BEGIN:VTIMEZONE"));
        assert!(!counter.contains("VALARM"));
        assert!(!counter.contains("RSVP"));
        let parsed = parse_itip(&counter).unwrap();
        assert_eq!(parsed.method, Some(ItipMethod::Counter));
        assert_eq!(parsed.events.len(), 1);
        let proposal = &parsed.events[0];
        assert_eq!(proposal.uid, "review-1@example.com");
        assert_eq!(proposal.recurrence_id, None);
        assert_eq!(proposal.sequence, 2);
        assert_eq!(proposal.start, start);
        assert_eq!(proposal.end, end);
        assert_eq!(proposal.summary, "Roadmap review");
        assert_eq!(proposal.location.as_deref(), Some("Room Galata"));
        assert_eq!(proposal.description, None);
        assert_eq!(
            proposal.organizer.as_ref().map(|o| o.email.as_str()),
            Some("priya@example.com")
        );
        // Only the attendee who proposes, with their answer.
        assert_eq!(proposal.attendees.len(), 1);
        let alex = &proposal.attendees[0];
        assert_eq!(alex.email, "alex@example.com");
        assert_eq!(alex.name.as_deref(), Some("Alex Morgan"));
        assert_eq!(alex.partstat, PartStat::Tentative);
        assert!(!alex.rsvp);
        assert!(proposal.alarms.is_empty());
        assert_eq!(
            parsed.comment().as_deref(),
            Some("Could we meet an hour later?")
        );
        assert!(!parsed.disallows_counter());
    }

    #[test]
    fn counter_for_one_occurrence() {
        // An update of one occurrence of a series, to someone invited
        // through a list.
        let invitation = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
UID:standup@example.com\r\nRECURRENCE-ID:20261104T063000Z\r\nSEQUENCE:1\r\n\
DTSTAMP:20261028T090000Z\r\nDTSTART:20261104T070000Z\r\nDTEND:20261104T071500Z\r\n\
SUMMARY:Standup\r\nORGANIZER:mailto:lead@example.com\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:team@example.com\r\n\
X-MICROSOFT-DISALLOW-COUNTER:TRUE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = EventTime::Utc {
            at: at(2026, 11, 4, 8, 0).and_utc(),
        };
        let end = EventTime::Utc {
            at: at(2026, 11, 4, 8, 15).and_utc(),
        };
        let counter = build_counter_at(
            invitation,
            "me@example.com",
            PartStat::Declined,
            &start,
            &end,
            Some(" "),
            now(),
        )
        .unwrap();
        assert!(!counter.contains("BEGIN:VTIMEZONE"));
        assert!(!counter.contains("COMMENT"));
        let parsed = parse_itip(&counter).unwrap();
        let proposal = &parsed.events[0];
        assert_eq!(
            proposal.recurrence_id,
            Some(EventTime::Utc {
                at: at(2026, 11, 4, 6, 30).and_utc()
            })
        );
        assert_eq!(proposal.rrule, None);
        assert_eq!((&proposal.start, &proposal.end), (&start, &end));
        assert_eq!(proposal.attendees.len(), 1);
        assert_eq!(proposal.attendees[0].email, "me@example.com");
        assert_eq!(proposal.attendees[0].partstat, PartStat::Declined);
        assert_eq!(parsed.comment(), None);
        // The organizer's wish travels with the invitation, not the proposal.
        assert!(parse_itip(invitation).unwrap().disallows_counter());
        assert!(!parsed.disallows_counter());
        assert!(
            build_counter_at(
                "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
                "me@example.com",
                PartStat::Tentative,
                &start,
                &end,
                None,
                now(),
            )
            .is_err()
        );
    }

    fn design_review() -> Event {
        let mut event = Event::new("design-1@example.com", istanbul(15, 0), istanbul(15, 30));
        event.summary = "Design review".into();
        event.description = Some("The new onboarding.".into());
        event.location = Some("Figtree board".into());
        event.sequence = 3;
        event.organizer = Some(Organizer {
            email: "alex@example.com".into(),
            name: Some("Alex Morgan".into()),
        });
        event.attendees = vec![
            Attendee {
                email: "alex@example.com".into(),
                name: Some("Alex Morgan".into()),
                partstat: PartStat::Accepted,
                ..Attendee::default()
            },
            Attendee {
                email: "maya@example.com".into(),
                name: Some("Maya Chen".into()),
                partstat: PartStat::Tentative,
                rsvp: true,
                ..Attendee::default()
            },
        ];
        event.alarms.push(crate::model::Alarm {
            action: crate::model::AlarmAction::Display,
            minutes_before: 10,
            description: None,
        });
        event
    }

    #[test]
    fn decline_counter_round_trip() {
        let event = design_review();
        let text = build_decline_counter_at(
            &event,
            None,
            "MAYA@example.com",
            Some("The others can only make it at three."),
            now(),
        )
        .unwrap();
        assert!(text.contains("DTSTAMP:20261030T080000Z"));
        assert!(!text.contains("VALARM"));
        assert!(!text.contains("RSVP"));
        let parsed = parse_itip(&text).unwrap();
        assert_eq!(parsed.method, Some(ItipMethod::DeclineCounter));
        let kept = &parsed.events[0];
        assert_eq!(kept.uid, "design-1@example.com");
        assert_eq!(kept.recurrence_id, None);
        assert_eq!(kept.sequence, 3);
        // The event stays at its time.
        assert_eq!((&kept.start, &kept.end), (&event.start, &event.end));
        assert_eq!(kept.summary, "Design review");
        assert_eq!(kept.description, None);
        assert_eq!(kept.organizer, event.organizer);
        assert_eq!(kept.attendees.len(), 1);
        assert_eq!(kept.attendees[0].email, "maya@example.com");
        assert_eq!(kept.attendees[0].name.as_deref(), Some("Maya Chen"));
        assert!(kept.alarms.is_empty());
        assert_eq!(
            parsed.comment().as_deref(),
            Some("The others can only make it at three.")
        );
        // Someone the event does not list is named all the same.
        let stranger = build_decline_counter_at(&event, None, "pat@example.com", None, now())
            .and_then(|t| parse_itip(&t))
            .unwrap();
        assert_eq!(stranger.events[0].attendees[0].email, "pat@example.com");
        assert_eq!(stranger.comment(), None);
    }

    #[test]
    fn decline_counter_for_an_occurrence() {
        // Mondays at 09:00 in Berlin; the proposal was for November 9.
        let berlin = |d: u32, h: u32| EventTime::Zoned {
            local: at(2026, 11, d, h, 0),
            tzid: "Europe/Berlin".into(),
        };
        let mut series = design_review();
        series.start = berlin(2, 9);
        series.end = berlin(2, 10);
        series.rrule = Some("FREQ=WEEKLY;COUNT=5".into());
        let rid = EventTime::Utc {
            at: at(2026, 11, 9, 8, 0).and_utc(),
        };
        let text =
            build_decline_counter_at(&series, Some(&rid), "maya@example.com", None, now()).unwrap();
        assert!(text.contains("BEGIN:VTIMEZONE"));
        let kept = &parse_itip(&text).unwrap().events[0];
        assert_eq!(kept.rrule, None);
        assert_eq!(kept.recurrence_id, Some(berlin(9, 9)));
        assert_eq!((&kept.start, &kept.end), (&berlin(9, 9), &berlin(9, 10)));
        assert_eq!(kept.sequence, 3);
    }
}
