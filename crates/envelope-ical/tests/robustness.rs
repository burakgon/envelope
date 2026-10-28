//! Malformed and adversarial input must never panic or hang.

mod common;

use chrono::{DateTime, Utc};
use common::{ICS_FIXTURES, VCF_FIXTURES, dt, utc};
use envelope_ical::vcard::parse_vcards_bytes;
use envelope_ical::{
    Contact, EventTime, ExpandOptions, PartStat, add_exdate_at, build_cancel_at, build_reply_at,
    build_request_at, create_calendar_at, expand_events, parse_calendar, parse_calendar_bytes,
    parse_components_bytes, remove_override, truncate_series_at, upsert_contact, upsert_event_at,
    write_components,
};

fn now() -> DateTime<Utc> {
    utc("2026-01-01 00:00")
}

/// Runs every public entry point on `bytes`; only panics are failures.
fn exercise(bytes: &[u8]) {
    let components = parse_components_bytes(bytes);
    let written = write_components(&components);
    let _ = parse_calendar(&written);
    let text = String::from_utf8_lossy(bytes);
    let options = ExpandOptions {
        max_occurrences: 500,
        ..ExpandOptions::default()
    };
    if let Ok(data) = parse_calendar_bytes(bytes) {
        let _ = expand_events(
            &data.events,
            utc("1990-01-01 00:00"),
            utc("2040-01-01 00:00"),
            &data.tz,
            &options,
        );
        for event in data.events.iter().take(4) {
            let _ = create_calendar_at(event, now());
            let _ = upsert_event_at(&text, event, now());
            let _ = build_request_at(std::slice::from_ref(event), now());
            let _ = build_cancel_at(event, Some(&event.start), now());
            let _ = add_exdate_at(&text, &event.uid, &event.start, now());
            let _ = truncate_series_at(&text, &event.uid, &event.end, now());
            let _ = remove_override(&text, &event.uid, &event.start);
        }
    }
    let _ = build_reply_at(
        &text,
        "someone@example.com",
        PartStat::Accepted,
        Some("ok"),
        now(),
    );
    for contact in parse_vcards_bytes(bytes).iter().take(4) {
        let _ = upsert_contact(&text, contact);
        let _ = envelope_ical::contact_to_vcard(contact);
    }
    let _ = upsert_contact(&text, &Contact::default());
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[test]
fn truncated_fixtures() {
    for (_, bytes) in ICS_FIXTURES.iter().chain(VCF_FIXTURES) {
        for cut in (0..bytes.len()).step_by(29) {
            exercise(&bytes[..cut]);
        }
    }
}

#[test]
fn mutated_fixtures() {
    const NOISE: &[u8] = b":;,\"=\\\r\n \t^-+0123456789TZ\xc3\xa7\xff\x80";
    let mut rng = Lcg(42);
    for (_, bytes) in ICS_FIXTURES.iter().chain(VCF_FIXTURES) {
        for _ in 0..40 {
            let mut mutated = bytes.to_vec();
            for _ in 0..1 + rng.below(8) {
                let position = rng.below(mutated.len() as u64) as usize;
                let noise = NOISE[rng.below(NOISE.len() as u64) as usize];
                match rng.below(3) {
                    0 => mutated[position] = noise,
                    1 => mutated.insert(position, noise),
                    _ => {
                        mutated.remove(position);
                    }
                }
                if mutated.is_empty() {
                    break;
                }
            }
            exercise(&mutated);
        }
    }
}

#[test]
fn hostile_calendars() {
    const LINES: &[&str] = &[
        "BEGIN:VEVENT",
        "END:VEVENT",
        "BEGIN:VALARM",
        "END:VALARM",
        "BEGIN:VTIMEZONE",
        "END:VTIMEZONE",
        "BEGIN:STANDARD",
        "BEGIN:DAYLIGHT",
        "END:STANDARD",
        "TZID:Hostile",
        "TZOFFSETFROM:+9999",
        "TZOFFSETTO:-2359",
        "TZOFFSETTO:+0100",
        "DTSTART:16010101T000000",
        "RRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30",
        "RRULE:FREQ=SECONDLY;INTERVAL=1",
        "RRULE:FREQ=MINUTELY;BYHOUR=1;BYMINUTE=1;BYSECOND=1",
        "RRULE:FREQ=YEARLY;BYWEEKNO=-53,53,1;BYDAY=-1MO,53SU;BYSETPOS=-366,366",
        "RRULE:FREQ=MONTHLY;INTERVAL=4294967295;COUNT=4294967295",
        "RRULE:FREQ=WEEKLY;UNTIL=00000101T000000Z",
        "RRULE:FREQ=DAILY;UNTIL=99991231",
        "RRULE:",
        "UID:hostile",
        "UID:",
        "RECURRENCE-ID;TZID=Hostile:20240101T000000",
        "RECURRENCE-ID:garbage",
        "DTSTART;TZID=Hostile:20240101T090000",
        "DTSTART;VALUE=DATE:99991231",
        "DTSTART:00010101T000000Z",
        "DTSTART:20240101T240000",
        "DTEND:20231231T000000Z",
        "DURATION:P99999999W",
        "DURATION:-P1D",
        "TRIGGER:-P9999999999999D",
        "TRIGGER;RELATED=END:PT0S",
        "TRIGGER;VALUE=DATE-TIME:garbage",
        "ACTION:EMAIL",
        "RDATE;VALUE=PERIOD:garbage/also,20240101T000000Z/PT1H",
        "EXDATE:,,,",
        "EXDATE;VALUE=DATE:20240101",
        "ATTENDEE;CN=\"unterminated:mailto:x@y",
        "ATTENDEE:",
        "ORGANIZER;SENT-BY=mailto:a@b:mailto:c@d",
        "COLOR:#",
        "CATEGORIES:,,,\\",
        "SUMMARY:\\",
        "X-;;;=:",
        ";;;:::",
        "SEQUENCE:99999999999999999999",
        "CONFERENCE;VALUE=URI:https://",
        "DESCRIPTION:http://http://https:// zoom.us/j/ https://zoom.us/j/",
    ];
    let mut rng = Lcg(7);
    for _ in 0..1_500 {
        let mut text = String::from("BEGIN:VCALENDAR\r\n");
        for _ in 0..rng.below(40) {
            text.push_str(LINES[rng.below(LINES.len() as u64) as usize]);
            text.push_str("\r\n");
        }
        if rng.below(2) == 0 {
            text.push_str("END:VCALENDAR\r\n");
        }
        exercise(text.as_bytes());
    }
}

#[test]
fn extreme_times_in_the_model() {
    let extremes = [
        EventTime::Utc {
            at: DateTime::<Utc>::MIN_UTC,
        },
        EventTime::Utc {
            at: DateTime::<Utc>::MAX_UTC,
        },
        EventTime::Floating {
            local: chrono::NaiveDateTime::MIN,
        },
        EventTime::Floating {
            local: chrono::NaiveDateTime::MAX,
        },
        EventTime::Date {
            date: chrono::NaiveDate::MIN,
        },
        EventTime::Date {
            date: chrono::NaiveDate::MAX,
        },
        EventTime::Zoned {
            local: chrono::NaiveDateTime::MIN,
            tzid: "Europe/Berlin".into(),
        },
        EventTime::Zoned {
            local: chrono::NaiveDateTime::MAX,
            tzid: "Nowhere".into(),
        },
        EventTime::Zoned {
            local: dt("2024-01-01 00:00"),
            tzid: "Europe/Berlin".into(),
        },
    ];
    for start in &extremes {
        for end in &extremes {
            let mut event = envelope_ical::Event::new("x", start.clone(), end.clone());
            event.rrule = Some("FREQ=DAILY".into());
            event.alarms.push(envelope_ical::Alarm {
                action: envelope_ical::AlarmAction::Display,
                minutes_before: i64::MAX,
                description: None,
            });
            let ctx = envelope_ical::TzContext::new();
            let options = ExpandOptions {
                max_occurrences: 10,
                ..ExpandOptions::default()
            };
            let _ = envelope_ical::expand(
                &event,
                &[],
                DateTime::<Utc>::MIN_UTC,
                DateTime::<Utc>::MAX_UTC,
                &ctx,
                &options,
            );
            if let Ok(created) = create_calendar_at(&event, now()) {
                exercise(created.as_bytes());
                let _ = truncate_series_at(&created, "x", end, now());
                let _ = add_exdate_at(&created, "x", end, now());
            }
            let _ = build_cancel_at(&event, Some(end), now());
        }
    }
}
