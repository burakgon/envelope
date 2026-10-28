//! iCalendar (RFC 5545) and vCard (RFC 6350) support for Envelope.
//!
//! The crate turns calendar and contact data from any source (CalDAV,
//! CardDAV, `.ics` mail attachments, exports) into the provider-neutral
//! [`model`] and back, without losing data it does not understand.
//!
//! * [`parse_components`] / [`Component`]: a tolerant parser and a folding
//!   serializer over a generic component tree that round-trips unknown
//!   properties, parameters and subcomponents.
//! * [`parse_calendar`]: `VCALENDAR` → [`Event`]s (series masters and
//!   overrides), with time zones normalized to IANA names where possible,
//!   attendees, alarms, colors and online meeting links.
//! * [`tz`]: TZID resolution (IANA, Windows, Outlook display names,
//!   embedded `VTIMEZONE`s) and `VTIMEZONE` generation.
//! * [`recur`]: recurrence expansion with overrides, `EXDATE`/`RDATE` and
//!   DST-correct wall-clock times.
//! * [`mutate`]: creating calendars and editing existing calendar text
//!   (upsert, delete an occurrence, "this and following").
//! * [`itip`]: RFC 5546 invitations, replies and cancellations.
//! * [`vcard`]: vCard 2.1/3.0/4.0 parsing and vCard 3.0 writing.
//!
//! # Example
//!
//! ```
//! use chrono::{TimeZone, Utc};
//! use envelope_ical::{ExpandOptions, PartStat, build_reply_at, expand_events, parse_calendar};
//!
//! let ics = "BEGIN:VCALENDAR\r\n\
//! VERSION:2.0\r\n\
//! METHOD:REQUEST\r\n\
//! BEGIN:VEVENT\r\n\
//! UID:standup-1\r\n\
//! DTSTAMP:20240101T000000Z\r\n\
//! DTSTART;TZID=Pacific Standard Time:20240304T090000\r\n\
//! DTEND;TZID=Pacific Standard Time:20240304T091500\r\n\
//! RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=4\r\n\
//! SUMMARY:Stand-up\r\n\
//! ORGANIZER;CN=Lead:mailto:lead@example.com\r\n\
//! ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@example.com\r\n\
//! DESCRIPTION:Join at https://meet.google.com/abc-defg-hij\r\n\
//! END:VEVENT\r\n\
//! END:VCALENDAR\r\n";
//!
//! let data = parse_calendar(ics)?;
//! let event = &data.events[0];
//! assert_eq!(event.conference_url.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
//!
//! // Expand the series for March 2024: the DST switch on March 10 keeps
//! // the meeting at 09:00 Los Angeles time.
//! let from = Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
//! let to = Utc.with_ymd_and_hms(2024, 4, 1, 0, 0, 0).unwrap();
//! let options = ExpandOptions::with_default_tz(chrono_tz::Europe::Istanbul);
//! let expansion = expand_events(&data.events, from, to, &data.tz, &options);
//! let starts: Vec<String> = expansion
//!     .occurrences
//!     .iter()
//!     .map(|o| o.occurrence.start.format("%d %H:%M").to_string())
//!     .collect();
//! assert_eq!(starts, ["04 17:00", "11 16:00", "18 16:00", "25 16:00"]);
//!
//! // Accept the invitation.
//! let now = Utc.with_ymd_and_hms(2024, 3, 1, 8, 0, 0).unwrap();
//! let reply = build_reply_at(ics, "me@example.com", PartStat::Accepted, None, now)?;
//! assert!(reply.contains("METHOD:REPLY"));
//! assert!(reply.contains("PARTSTAT=ACCEPTED"));
//! # Ok::<(), envelope_ical::Error>(())
//! ```

#![warn(missing_docs)]

mod charset;
pub mod color;
pub mod component;
pub mod conference;
pub mod error;
pub mod events;
pub mod itip;
#[allow(missing_docs)]
pub mod model;
pub mod mutate;
pub mod parser;
pub mod recur;
pub mod text;
pub mod tz;
pub mod value;
pub mod vcard;
mod writer;

pub use component::{Component, Param, Property};
pub use error::Error;
pub use events::{
    CalendarData, ExtractOptions, event_from_vevent, events_from_calendar, parse_calendar,
    parse_calendar_bytes, parse_calendar_with, parse_events,
};
pub use itip::{
    ItipMessage, ItipMethod, build_cancel, build_cancel_at, build_reply, build_reply_at,
    build_request, build_request_at, my_partstat, parse_itip,
};
pub use model::*;
pub use mutate::{
    PRODID, add_exdate, add_exdate_at, create_calendar, create_calendar_at, event_to_component,
    new_uid, remove_event, remove_override, truncate_series, truncate_series_at, upsert_event,
    upsert_event_at,
};
pub use parser::{decode_bytes, parse_components, parse_components_bytes};
pub use recur::{
    EventOccurrence, EventsExpansion, ExpandOptions, Expansion, Frequency, Occurrence, RecurRule,
    expand, expand_events,
};
pub use tz::{TzContext, Zone, generate_vtimezone, resolve_tzid, to_utc};
pub use vcard::{contact_to_vcard, parse_vcard, parse_vcards, upsert_contact};
pub use writer::write_components;
