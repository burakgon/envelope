//! iTIP (RFC 5546) messages.

mod common;

use chrono::{DateTime, Utc};
use common::{dt, text, utc};
use envelope_ical::{
    Component, EventStatus, EventTime, ItipMethod, PartStat, build_cancel_at, build_reply_at,
    build_request_at, my_partstat, parse_calendar, parse_components, parse_itip,
};

fn now() -> DateTime<Utc> {
    utc("2024-04-03 08:30")
}

fn calendar(text: &str) -> Component {
    parse_components(text).remove(0)
}

#[test]
fn reading_an_invitation() {
    let invite = text(include_bytes!("fixtures/outlook_invite.ics"));
    let message = parse_itip(invite).expect("message");
    assert_eq!(message.method, Some(ItipMethod::Request));
    assert_eq!(message.events.len(), 1);
    let event = &message.events[0];
    assert_eq!(
        my_partstat(event, &["someone@else.com", "BURAK@example.com"]),
        Some(PartStat::NeedsAction)
    );
    assert_eq!(
        my_partstat(event, &["hans.mueller@contoso.com"]),
        Some(PartStat::Accepted),
        "organizer"
    );
    assert_eq!(my_partstat(event, &["stranger@example.com"]), None);

    let teams = parse_itip(text(include_bytes!("fixtures/teams_invite.ics"))).expect("message");
    assert_eq!(teams.method, Some(ItipMethod::Request));
    let google = parse_itip(text(include_bytes!("fixtures/google_export.ics"))).expect("message");
    assert_eq!(google.method, Some(ItipMethod::Publish));
    let plain = parse_itip(text(include_bytes!("fixtures/recurring_berlin.ics"))).expect("message");
    assert_eq!(plain.method, None);

    for method in [
        "REPLY",
        "CANCEL",
        "ADD",
        "REFRESH",
        "COUNTER",
        "DECLINECOUNTER",
        "X-ODD",
    ] {
        let message = parse_itip(&format!(
            "BEGIN:VCALENDAR\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\nUID:m\r\nDTSTAMP:20240101T000000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        ))
        .expect("message");
        assert_eq!(
            message.method.as_ref().map(ItipMethod::as_str),
            Some(method)
        );
        assert_eq!(
            message.events.len(),
            1,
            "events without DTSTART are kept for iTIP"
        );
    }
}

#[test]
fn replying_to_an_exchange_invitation() {
    let invite = text(include_bytes!("fixtures/outlook_invite.ics"));
    let reply = build_reply_at(
        invite,
        "BURAK@example.com",
        PartStat::Accepted,
        Some("Gerne, bis dann!"),
        now(),
    )
    .expect("reply");
    assert!(reply.lines().all(|l| l.len() <= 76));
    let cal = calendar(&reply);
    assert_eq!(cal.value("METHOD"), Some("REPLY"));
    assert_eq!(cal.value("PRODID"), Some("-//Envelope//Envelope Mail//EN"));
    let vtimezones: Vec<&str> = cal
        .children("VTIMEZONE")
        .filter_map(|c| c.value("TZID"))
        .collect();
    assert_eq!(vtimezones, ["W. Europe Standard Time"]);
    let vevents: Vec<&Component> = cal.children("VEVENT").collect();
    assert_eq!(vevents.len(), 1);
    let vevent = vevents[0];
    let original = calendar(invite);
    let original_vevent = original.children("VEVENT").next().expect("vevent");
    for name in [
        "UID",
        "SEQUENCE",
        "ORGANIZER",
        "DTSTART",
        "DTEND",
        "SUMMARY",
    ] {
        assert_eq!(
            vevent.property(name),
            original_vevent.property(name),
            "{name} is copied"
        );
    }
    assert_eq!(vevent.value("DTSTAMP"), Some("20240403T083000Z"));
    assert_eq!(vevent.text("COMMENT").as_deref(), Some("Gerne, bis dann!"));
    let attendees: Vec<_> = vevent.properties_named("ATTENDEE").collect();
    assert_eq!(attendees.len(), 1, "only the replying attendee");
    assert_eq!(attendees[0].param("PARTSTAT"), Some("ACCEPTED"));
    assert_eq!(attendees[0].param("CN"), Some("Burak Gön"));
    assert_eq!(attendees[0].param("RSVP"), None);
    assert!(
        vevent.children("VALARM").next().is_none(),
        "no alarms in replies"
    );
    for name in [
        "DESCRIPTION",
        "X-MICROSOFT-CDO-BUSYSTATUS",
        "X-ALT-DESC",
        "PRIORITY",
    ] {
        assert!(
            vevent.property(name).is_none(),
            "{name} is not part of a reply"
        );
    }

    // The organizer side reads the reply.
    let parsed = parse_itip(&reply).expect("reply parses");
    assert_eq!(parsed.method, Some(ItipMethod::Reply));
    assert_eq!(parsed.events[0].attendees.len(), 1);
    assert_eq!(parsed.events[0].attendees[0].partstat, PartStat::Accepted);
    assert_eq!(parsed.events[0].attendees[0].email, "burak@example.com");
}

#[test]
fn replying_to_a_series_with_an_override() {
    let invite = text(include_bytes!("fixtures/google_export.ics"));
    let reply = build_reply_at(
        invite,
        "burak@example.com",
        PartStat::Tentative,
        None,
        now(),
    )
    .expect("reply");
    let message = parse_itip(&reply).expect("reply parses");
    assert_eq!(message.events.len(), 3);
    assert!(
        message.events[0].rrule.is_some(),
        "the series is answered with its rule"
    );
    assert_eq!(
        message.events[1].recurrence_id,
        Some(EventTime::Zoned {
            local: dt("2024-03-12 10:00"),
            tzid: "America/New_York".into()
        })
    );
    assert!(
        message
            .events
            .iter()
            .all(|e| e.attendees.len() == 1 && e.attendees[0].partstat == PartStat::Tentative)
    );
    assert!(!reply.contains("COMMENT"));
    // X-NUM-GUESTS is a parameter of the attendee and stays.
    assert!(reply.replace("\r\n ", "").contains("X-NUM-GUESTS=0"));
}

#[test]
fn replying_without_being_listed() {
    let invite = text(include_bytes!("fixtures/zoom_invite.ics"));
    let reply =
        build_reply_at(invite, "Me@Example.org", PartStat::Declined, None, now()).expect("reply");
    let cal = calendar(&reply);
    let vevent = cal.children("VEVENT").next().expect("vevent");
    let attendee = vevent.property("ATTENDEE").expect("attendee");
    assert_eq!(attendee.value, "mailto:me@example.org");
    assert_eq!(attendee.param("PARTSTAT"), Some("DECLINED"));

    assert!(matches!(
        build_reply_at(
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n",
            "a@b.c",
            PartStat::Accepted,
            None,
            now()
        ),
        Err(envelope_ical::Error::NoEvents)
    ));
    assert!(matches!(
        build_reply_at("garbage", "a@b.c", PartStat::Accepted, None, now()),
        Err(envelope_ical::Error::NoCalendar)
    ));
}

#[test]
fn sending_requests_and_cancellations() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/google_export.ics"))).expect("calendar");
    let series: Vec<_> = data
        .events
        .iter()
        .filter(|e| e.uid == data.events[0].uid)
        .cloned()
        .collect();
    let request = build_request_at(&series, now()).expect("request");
    let cal = calendar(&request);
    assert_eq!(cal.value("METHOD"), Some("REQUEST"));
    let vtimezones: Vec<&str> = cal
        .children("VTIMEZONE")
        .filter_map(|c| c.value("TZID"))
        .collect();
    assert_eq!(vtimezones, ["America/New_York"]);
    assert_eq!(cal.children("VEVENT").count(), 2);
    assert!(
        cal.children("VEVENT")
            .all(|v| v.children("VALARM").next().is_none())
    );
    let parsed = parse_itip(&request).expect("request parses");
    let mut expected = series.clone();
    for event in &mut expected {
        event.alarms.clear();
    }
    assert_eq!(parsed.events, expected);
    assert!(matches!(
        build_request_at(&[], now()),
        Err(envelope_ical::Error::NoEvents)
    ));

    // Cancel a single occurrence of the series.
    let master = &series[0];
    let rid = EventTime::Zoned {
        local: dt("2024-03-19 10:00"),
        tzid: "America/New_York".into(),
    };
    let cancel = build_cancel_at(master, Some(&rid), now()).expect("cancel");
    let message = parse_itip(&cancel).expect("cancel parses");
    assert_eq!(message.method, Some(ItipMethod::Cancel));
    let cancelled = &message.events[0];
    assert_eq!(cancelled.recurrence_id, Some(rid.clone()));
    assert_eq!(cancelled.start, rid);
    assert_eq!(
        cancelled.end,
        EventTime::Zoned {
            local: dt("2024-03-19 10:30"),
            tzid: "America/New_York".into()
        }
    );
    assert_eq!(cancelled.status, EventStatus::Cancelled);
    assert_eq!(cancelled.sequence, master.sequence + 1);
    assert_eq!(cancelled.rrule, None);
    assert_eq!(cancelled.attendees.len(), master.attendees.len());
    assert!(!cancel.contains("DESCRIPTION"));

    // Cancel the whole series; an instance given in another form is normalized.
    let cancel = build_cancel_at(master, None, now()).expect("cancel");
    let message = parse_itip(&cancel).expect("cancel parses");
    assert_eq!(message.events[0].recurrence_id, None);
    assert_eq!(message.events[0].rrule, master.rrule);
    let by_instant = build_cancel_at(
        master,
        Some(&EventTime::Utc {
            at: utc("2024-03-19 14:00"),
        }),
        now(),
    )
    .expect("cancel");
    assert!(by_instant.contains("RECURRENCE-ID;TZID=America/New_York:20240319T100000"));
}
