//! iTIP (RFC 5546): reading invitations and building replies, requests and
//! cancellations.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::component::{Component, Property};
use crate::error::{Error, Result};
use crate::events::{cal_address, parse_calendar};
use crate::model::{Event, EventStatus, EventTime, PartStat};
use crate::mutate::{build_calendar, ensure_timezones, mailto, new_calendar, partstat_value};
use crate::recur::{ExpandOptions, expand};
use crate::tz::TzContext;
use crate::value::format_utc;

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
        let mut attendee = vevent
            .properties_named("ATTENDEE")
            .find(|p| cal_address(p).as_deref() == Some(email.as_str()))
            .cloned()
            .unwrap_or_else(|| Property::new("ATTENDEE", mailto(&email)));
        attendee.set_param("PARTSTAT", partstat_value(partstat));
        attendee.remove_param("RSVP");
        attendee.remove_param("SCHEDULE-STATUS");
        out.push(attendee);
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
    let mut cancelled = event.clone();
    cancelled.status = EventStatus::Cancelled;
    cancelled.sequence = event.sequence + 1;
    cancelled.alarms.clear();
    if let Some(rid) = recurrence_id {
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
        match occurrence {
            Some(occurrence) => {
                cancelled.start = occurrence.start_time;
                cancelled.end = occurrence.end_time;
            }
            None => {
                cancelled.start = rid_value.clone();
                cancelled.end = rid_value.clone();
            }
        }
        cancelled.recurrence_id = Some(rid_value);
        cancelled.rrule = None;
        cancelled.rdates.clear();
        cancelled.exdates.clear();
    }
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
}
