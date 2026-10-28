//! Editing raw calendar text: create, upsert, delete occurrences, split series.

mod common;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use common::{date, dt, fmt, text, utc};
use envelope_ical::{
    Alarm, AlarmAction, Component, Error, Event, EventTime, ExpandOptions, ExtractOptions,
    PartStat, add_exdate_at, create_calendar_at, event_from_vevent, expand, parse_calendar,
    parse_components, remove_event, remove_override, truncate_series_at, upsert_event_at,
};

fn now() -> DateTime<Utc> {
    utc("2026-01-02 03:04")
}

const NOW_STAMP: &str = "20260102T030400Z";

fn vevents(text: &str) -> Vec<Component> {
    parse_components(text)
        .into_iter()
        .filter(|c| c.is("VCALENDAR"))
        .flat_map(|c| c.components.into_iter().filter(|e| e.is("VEVENT")))
        .collect()
}

fn vtimezone_ids(text: &str) -> Vec<String> {
    parse_components(text)
        .into_iter()
        .flat_map(|c| c.components)
        .filter(|c| c.is("VTIMEZONE"))
        .filter_map(|c| c.value("TZID").map(str::to_owned))
        .collect()
}

/// Properties of a component except the ones every edit refreshes.
fn stable_properties(component: &Component) -> Vec<String> {
    component
        .properties
        .iter()
        .filter(|p| !matches!(p.name.as_str(), "DTSTAMP" | "LAST-MODIFIED"))
        .map(|p| p.to_content_line())
        .collect()
}

fn zoned(local: &str, tzid: &str) -> EventTime {
    EventTime::Zoned {
        local: dt(local),
        tzid: tzid.to_owned(),
    }
}

#[test]
fn unchanged_upsert_only_refreshes_timestamps() {
    for (name, bytes) in common::ICS_FIXTURES {
        let original = text(bytes);
        let data = parse_calendar(original).expect("calendar");
        for event in &data.events {
            let updated = upsert_event_at(original, event, now()).expect("upsert");
            let before = vevents(original);
            let after = vevents(&updated);
            assert_eq!(before.len(), after.len(), "{name}");
            for (b, a) in before.iter().zip(&after) {
                let target = event_from_vevent(b, &data.tz, &ExtractOptions::default()).as_ref()
                    == Some(event);
                if target {
                    assert_eq!(a.value("DTSTAMP"), Some(NOW_STAMP), "{name}");
                    assert_eq!(a.value("LAST-MODIFIED"), Some(NOW_STAMP), "{name}");
                } else {
                    assert_eq!(a, b, "{name}: an unrelated event changed");
                }
                assert_eq!(
                    stable_properties(b),
                    stable_properties(a),
                    "{name}: {}",
                    event.uid
                );
                assert_eq!(b.components, a.components, "{name}: subcomponents changed");
            }
            // Time zones that are still referenced are kept as they are.
            assert_eq!(
                parse_calendar(&updated).expect("calendar").events.len(),
                data.events.len()
            );
        }
    }
}

#[test]
fn summary_change_keeps_vendor_data_and_parameters() {
    let original = text(include_bytes!("fixtures/outlook_invite.ics"));
    let mut event = parse_calendar(original).expect("calendar").events.remove(0);
    event.summary = "Quartalsplanung Q2 (verschoben)".into();
    event.alarms.push(Alarm {
        action: AlarmAction::Display,
        minutes_before: 60,
        description: None,
    });
    let updated = upsert_event_at(original, &event, now()).expect("upsert");
    let vevent = &vevents(&updated)[0];
    let summary = vevent.property("SUMMARY").expect("summary");
    assert_eq!(summary.text_value(), "Quartalsplanung Q2 (verschoben)");
    assert_eq!(
        summary.param("LANGUAGE"),
        Some("de-DE"),
        "parameters survive edits"
    );
    for name in [
        "X-MICROSOFT-CDO-OWNERAPPTID",
        "X-MICROSOFT-SKYPETEAMSPROPERTIES",
        "X-MICROSOFT-LOCATIONS",
        "PRIORITY",
    ] {
        assert!(vevent.property(name).is_some(), "lost {name}");
    }
    assert_eq!(
        vevent.value("SEQUENCE"),
        Some("0"),
        "a title change is not significant"
    );
    assert_eq!(
        vevent
            .property("DESCRIPTION")
            .and_then(|d| d.param("LANGUAGE")),
        Some("de-DE")
    );
    let alarms: Vec<&Component> = vevent.children("VALARM").collect();
    assert_eq!(alarms.len(), 2);
    assert_eq!(
        alarms[0].value("DESCRIPTION"),
        Some("REMINDER"),
        "the existing alarm is kept verbatim"
    );
    assert_eq!(alarms[1].value("TRIGGER"), Some("-PT1H"));
    let expected = Event {
        last_modified: Some(now()),
        ..event
    };
    assert_eq!(
        parse_calendar(&updated).expect("calendar").events[0],
        expected
    );
}

#[test]
fn moving_an_event_bumps_sequence_and_rewrites_time_zones() {
    let original = text(include_bytes!("fixtures/outlook_invite.ics"));
    let mut event = parse_calendar(original).expect("calendar").events.remove(0);
    event.start = zoned("2024-04-11 15:00", "Europe/Berlin");
    event.end = zoned("2024-04-11 16:30", "Europe/Berlin");
    let updated = upsert_event_at(original, &event, now()).expect("upsert");
    let vevent = &vevents(&updated)[0];
    assert_eq!(
        vevent.property("DTSTART").and_then(|p| p.param("TZID")),
        Some("Europe/Berlin")
    );
    assert_eq!(vevent.value("SEQUENCE"), Some("1"));
    // The Windows definition is no longer referenced; Berlin's is generated.
    assert_eq!(vtimezone_ids(&updated), ["Europe/Berlin"]);
    let reparsed = parse_calendar(&updated).expect("calendar");
    assert_eq!(
        reparsed.tz.to_utc(&reparsed.events[0].start, Tz::UTC),
        utc("2024-04-11 13:00")
    );
    assert_eq!(reparsed.events[0].sequence, 1);

    // Moving only the start of a DURATION-based event rewrites the end too.
    let original = text(include_bytes!("fixtures/recurring_dst_ny.ics"));
    let data = parse_calendar(original).expect("calendar");
    let mut standup = data
        .events
        .iter()
        .find(|e| e.uid == "standup@example.com")
        .cloned()
        .expect("standup");
    standup.start = zoned("2024-03-07 09:00", "America/New_York");
    standup.end = zoned("2024-03-07 09:20", "America/New_York");
    let updated = upsert_event_at(original, &standup, now()).expect("upsert");
    let reparsed = parse_calendar(&updated).expect("calendar");
    let after = reparsed
        .events
        .iter()
        .find(|e| e.uid == "standup@example.com")
        .expect("standup");
    assert_eq!(after.end, zoned("2024-03-07 09:20", "America/New_York"));
    assert!(!updated.contains("DURATION:PT15M"));
}

#[test]
fn partstat_change_keeps_unknown_attendee_parameters() {
    let original = text(include_bytes!("fixtures/google_export.ics"));
    let mut master = parse_calendar(original).expect("calendar").events.remove(0);
    master.attendees[1].partstat = PartStat::Accepted;
    master.attendees[1].rsvp = false;
    let updated = upsert_event_at(original, &master, now()).expect("upsert");
    let vevent = &vevents(&updated)[0];
    let me = vevent
        .properties_named("ATTENDEE")
        .find(|p| p.value == "mailto:burak@example.com")
        .expect("attendee");
    assert_eq!(me.param("PARTSTAT"), Some("ACCEPTED"));
    assert_eq!(me.param("X-NUM-GUESTS"), Some("0"));
    assert_eq!(me.param("RSVP"), Some("FALSE"));
    assert_eq!(me.param("CUTYPE"), Some("INDIVIDUAL"));
    assert_eq!(
        vevent.value("SEQUENCE"),
        Some("2"),
        "replies do not bump SEQUENCE"
    );
    // The other VEVENTs are untouched.
    assert_eq!(
        stable_properties(&vevents(original)[1]),
        stable_properties(&vevents(&updated)[1])
    );
}

#[test]
fn new_override_and_new_event_are_appended() {
    let original = text(include_bytes!("fixtures/google_export.ics"));
    let data = parse_calendar(original).expect("calendar");
    let mut moved = data.events[0].clone();
    moved.rrule = None;
    moved.exdates.clear();
    moved.recurrence_id = Some(zoned("2024-03-19 10:00", "America/New_York"));
    moved.start = zoned("2024-03-19 11:00", "America/New_York");
    moved.end = zoned("2024-03-19 11:30", "America/New_York");
    moved.summary = "Team sync (one hour later)".into();
    let updated = upsert_event_at(original, &moved, now()).expect("upsert");
    let uids: Vec<Option<String>> = vevents(&updated)
        .iter()
        .map(|v| v.value("UID").map(str::to_owned))
        .collect();
    assert_eq!(uids.len(), 4);
    assert_eq!(
        uids[2], uids[0],
        "the override is placed after the series' other components"
    );

    let reparsed = parse_calendar(&updated).expect("calendar");
    let options = ExpandOptions::with_default_tz(Tz::America__New_York);
    let expansion = envelope_ical::expand_events(
        &reparsed.events,
        utc("2024-03-18 00:00"),
        utc("2024-03-20 00:00"),
        &reparsed.tz,
        &options,
    );
    assert_eq!(expansion.occurrences.len(), 1);
    assert_eq!(
        fmt(&expansion.occurrences[0].occurrence.start),
        "2024-03-19 15:00"
    );
    assert_eq!(
        reparsed.events[expansion.occurrences[0].event_index].summary,
        "Team sync (one hour later)"
    );

    let mut fresh = Event::new(
        "new-event-1",
        zoned("2024-05-01 18:00", "Asia/Tokyo"),
        zoned("2024-05-01 19:00", "Asia/Tokyo"),
    );
    fresh.summary = "Dinner".into();
    let updated = upsert_event_at(&updated, &fresh, now()).expect("upsert");
    assert_eq!(vtimezone_ids(&updated), ["America/New_York", "Asia/Tokyo"]);
    let last = vevents(&updated).pop().expect("vevent");
    assert_eq!(last.value("UID"), Some("new-event-1"));
    assert_eq!(last.value("CREATED"), Some(NOW_STAMP));
}

#[test]
fn icloud_alarms_keep_their_extra_properties() {
    let original = text(include_bytes!("fixtures/icloud_event.ics"));
    let mut event = parse_calendar(original).expect("calendar").events.remove(0);
    event.alarms[0].minutes_before = 15;
    let updated = upsert_event_at(original, &event, now()).expect("upsert");
    let vevent = &vevents(&updated)[0];
    let alarms: Vec<&Component> = vevent.children("VALARM").collect();
    assert_eq!(alarms.len(), 2);
    assert_eq!(
        alarms[0].value("ACTION"),
        Some("NONE"),
        "Apple's marker alarm is kept"
    );
    assert_eq!(alarms[1].value("TRIGGER"), Some("-PT15M"));
    assert_eq!(
        parse_calendar(&updated).expect("calendar").events[0].alarms,
        event.alarms
    );
    // Unchanged alarms keep X-WR-ALARMUID.
    let event = parse_calendar(original).expect("calendar").events.remove(0);
    let updated = upsert_event_at(original, &event, now()).expect("upsert");
    assert!(updated.contains("X-WR-ALARMUID:A1B2C3D4-E5F6-7890-ABCD-EF0123456789"));
    // SCHEDULE-STATUS and EMAIL parameters survive.
    assert!(updated.replace("\r\n ", "").contains("SCHEDULE-STATUS=1.2"));
}

#[test]
fn deleting_one_occurrence() {
    let original = text(include_bytes!("fixtures/recurring_dst_ny.ics"));
    let uid = "one-on-one@example.com";
    let updated = add_exdate_at(
        original,
        uid,
        &zoned("2024-03-04 09:00", "America/New_York"),
        now(),
    )
    .expect("exdate");
    // An occurrence given as an instant is written like DTSTART.
    let updated = add_exdate_at(
        &updated,
        uid,
        &EventTime::Utc {
            at: utc("2024-03-11 13:00"),
        },
        now(),
    )
    .expect("exdate");
    // Deleting an overridden occurrence removes the override as well.
    let updated = add_exdate_at(
        &updated,
        uid,
        &zoned("2024-03-27 09:00", "America/New_York"),
        now(),
    )
    .expect("exdate");
    let master = &vevents(&updated)[0];
    let exdates: Vec<String> = master
        .properties_named("EXDATE")
        .map(|p| p.to_content_line())
        .collect();
    assert_eq!(
        exdates,
        [
            "EXDATE;TZID=America/New_York:20240320T090000\r\n",
            "EXDATE;TZID=America/New_York:20240304T090000\r\n",
            "EXDATE;TZID=America/New_York:20240311T090000\r\n",
            "EXDATE;TZID=America/New_York:20240327T090000\r\n",
        ]
    );
    assert_eq!(master.value("SEQUENCE"), Some("3"));
    assert_eq!(master.value("X-ENVELOPE-TEST"), Some("keep me"));
    assert!(!updated.contains("moved to April"));

    let data = parse_calendar(&updated).expect("calendar");
    let all = envelope_ical::expand_events(
        &data.events,
        utc("2024-03-01 05:00"),
        utc("2024-05-01 04:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    let one_on_one: Vec<String> = all
        .occurrences
        .iter()
        .filter(|o| data.events[o.event_index].uid == uid)
        .map(|o| fmt(&o.occurrence.start))
        .collect();
    assert!(!one_on_one.contains(&"2024-03-04 14:00".to_owned()));
    assert!(!one_on_one.contains(&"2024-03-11 13:00".to_owned()));
    assert!(
        !one_on_one.contains(&"2024-04-02 14:00".to_owned()),
        "the moved occurrence is gone"
    );

    // Adding the same EXDATE twice is idempotent (apart from SEQUENCE).
    let again = add_exdate_at(
        &updated,
        uid,
        &zoned("2024-03-04 09:00", "America/New_York"),
        now(),
    )
    .expect("exdate");
    assert_eq!(vevents(&again)[0].properties_named("EXDATE").count(), 4);

    assert!(matches!(
        add_exdate_at(
            original,
            "missing",
            &zoned("2024-03-04 09:00", "America/New_York"),
            now()
        ),
        Err(Error::EventNotFound { .. })
    ));
    let berlin = text(include_bytes!("fixtures/recurring_berlin.ics"));
    assert!(matches!(
        add_exdate_at(
            berlin,
            "floating@example.com",
            &EventTime::Floating {
                local: dt("2024-10-25 18:00")
            },
            now()
        ),
        Err(Error::NotRecurring { .. })
    ));
}

#[test]
fn exdates_use_the_series_own_tzid() {
    let original = text(include_bytes!("fixtures/outlook_display_tzid.ics"));
    let uid = parse_calendar(original).expect("calendar").events[0]
        .uid
        .clone();
    let updated = add_exdate_at(
        original,
        &uid,
        &zoned("2024-02-05 10:00", "Europe/Berlin"),
        now(),
    )
    .expect("exdate");
    assert!(updated.replace("\r\n ", "").contains(
        "EXDATE;TZID=\"(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna\":20240205T100000\r\n"
    ));
    assert_eq!(
        vtimezone_ids(&updated),
        [
            "(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
            "(UTC+03:00) Istanbul"
        ]
    );
}

#[test]
fn this_and_following() {
    let original = text(include_bytes!("fixtures/recurring_dst_ny.ics"));
    let uid = "one-on-one@example.com";
    let updated = truncate_series_at(
        original,
        uid,
        &zoned("2024-03-18 09:00", "America/New_York"),
        now(),
    )
    .expect("truncate");
    let events = vevents(&updated);
    let master = &events[0];
    assert_eq!(
        master.value("RRULE"),
        Some("FREQ=WEEKLY;UNTIL=20240318T125959Z;BYDAY=MO,WE")
    );
    assert_eq!(
        master.properties_named("EXDATE").count(),
        0,
        "EXDATEs after the split are dropped"
    );
    let remaining_rids: Vec<&str> = events
        .iter()
        .filter(|e| e.value("UID") == Some(uid))
        .filter_map(|e| e.value("RECURRENCE-ID"))
        .collect();
    assert_eq!(remaining_rids, ["20240228T090000", "20240313T090000"]);
    let data = parse_calendar(&updated).expect("calendar");
    let (master, overrides): (Vec<Event>, Vec<Event>) = data
        .events
        .iter()
        .filter(|e| e.uid == uid)
        .cloned()
        .partition(|e| e.recurrence_id.is_none());
    let expansion = expand(
        &master[0],
        &overrides,
        utc("2024-03-01 05:00"),
        utc("2024-05-01 04:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    let starts: Vec<String> = expansion
        .occurrences
        .iter()
        .map(|o| fmt(&o.start))
        .collect();
    assert_eq!(
        starts,
        [
            "2024-03-01 16:00",
            "2024-03-04 14:00",
            "2024-03-06 14:00",
            "2024-03-11 13:00"
        ]
    );

    // All-day series: COUNT becomes a DATE UNTIL on the previous day.
    let updated = truncate_series_at(
        original,
        "trash-day@example.com",
        &EventTime::Date {
            date: date("2024-03-14"),
        },
        now(),
    )
    .expect("truncate");
    let trash = vevents(&updated)
        .into_iter()
        .find(|e| e.value("UID") == Some("trash-day@example.com"))
        .expect("series");
    assert_eq!(
        trash.value("RRULE"),
        Some("FREQ=WEEKLY;UNTIL=20240313;BYDAY=TH")
    );

    // UTC series.
    let berlin = text(include_bytes!("fixtures/recurring_berlin.ics"));
    let updated = truncate_series_at(
        berlin,
        "utc-weekly@example.com",
        &EventTime::Utc {
            at: utc("2024-10-28 09:00"),
        },
        now(),
    )
    .expect("truncate");
    let series = vevents(&updated)
        .into_iter()
        .find(|e| e.value("UID") == Some("utc-weekly@example.com"))
        .expect("series");
    assert_eq!(
        series.value("RRULE"),
        Some("FREQ=WEEKLY;UNTIL=20241028T085959Z")
    );

    // Floating series get a floating UNTIL.
    let mut floating = Event::new(
        "float-1",
        EventTime::Floating {
            local: dt("2024-03-01 09:00"),
        },
        EventTime::Floating {
            local: dt("2024-03-01 10:00"),
        },
    );
    floating.rrule = Some("FREQ=DAILY".into());
    let created = create_calendar_at(&floating, now()).expect("create");
    let updated = truncate_series_at(
        &created,
        "float-1",
        &EventTime::Floating {
            local: dt("2024-03-05 09:00"),
        },
        now(),
    )
    .expect("truncate");
    assert_eq!(
        vevents(&updated)[0].value("RRULE"),
        Some("FREQ=DAILY;UNTIL=20240305T085959")
    );

    // An earlier UNTIL is kept; the first occurrence cannot be split off.
    let updated = truncate_series_at(
        original,
        uid,
        &zoned("2024-06-03 09:00", "America/New_York"),
        now(),
    )
    .expect("truncate");
    assert_eq!(
        vevents(&updated)[0].value("RRULE"),
        Some("FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20240417T130000Z")
    );
    assert!(matches!(
        truncate_series_at(
            original,
            uid,
            &zoned("2024-02-05 09:00", "America/New_York"),
            now()
        ),
        Err(Error::TruncateAtSeriesStart { .. })
    ));
}

#[test]
fn removing_overrides_and_events() {
    let original = text(include_bytes!("fixtures/recurring_berlin.ics"));
    let uid = "daily-berlin@example.com";
    let updated = remove_override(original, uid, &zoned("2024-10-24 10:00", "Europe/Berlin"))
        .expect("remove");
    let data = parse_calendar(&updated).expect("calendar");
    let all = envelope_ical::expand_events(
        &data.events,
        utc("2024-10-24 00:00"),
        utc("2024-10-25 00:00"),
        &data.tz,
        &ExpandOptions::default(),
    );
    let starts: Vec<String> = all
        .occurrences
        .iter()
        .map(|o| fmt(&o.occurrence.start))
        .collect();
    assert_eq!(
        starts,
        ["2024-10-24 08:00"],
        "the regular occurrence is back"
    );
    // The cancelled override's RECURRENCE-ID is UTC; matching is by instant.
    let updated = remove_override(&updated, uid, &zoned("2024-10-29 10:00", "Europe/Berlin"))
        .expect("remove");
    assert_eq!(
        vevents(&updated)
            .iter()
            .filter(|e| e.value("UID") == Some(uid))
            .count(),
        1
    );
    assert!(matches!(
        remove_override(&updated, uid, &zoned("2024-10-29 10:00", "Europe/Berlin")),
        Err(Error::EventNotFound {
            recurrence_id: Some(_),
            ..
        })
    ));

    let single = text(include_bytes!("fixtures/zoom_invite.ics"));
    let uid = parse_calendar(single).expect("calendar").events[0]
        .uid
        .clone();
    assert_eq!(
        remove_event(single, &uid).expect("remove"),
        None,
        "nothing left: delete the resource"
    );
    let rest = remove_event(original, "floating@example.com")
        .expect("remove")
        .expect("other events remain");
    assert!(!rest.contains("Floating dinner"));
    assert!(rest.contains("Daily Berlin"));
}

#[test]
fn created_calendars_for_every_time_form() {
    let cases = [
        (
            EventTime::Date {
                date: date("2024-07-01"),
            },
            EventTime::Date {
                date: date("2024-07-03"),
            },
        ),
        (
            EventTime::Utc {
                at: utc("2024-07-01 09:00"),
            },
            EventTime::Utc {
                at: utc("2024-07-01 10:00"),
            },
        ),
        (
            EventTime::Floating {
                local: dt("2024-07-01 09:00"),
            },
            EventTime::Floating {
                local: dt("2024-07-01 10:00"),
            },
        ),
        (
            zoned("2024-07-01 09:00", "Europe/Istanbul"),
            zoned("2024-07-01 10:00", "Europe/Istanbul"),
        ),
        (
            zoned("2024-07-01 09:00", "Europe/Istanbul"),
            zoned("2024-07-01 16:00", "Asia/Tokyo"),
        ),
    ];
    for (start, end) in cases {
        let mut event = Event::new(envelope_ical::new_uid(), start.clone(), end.clone());
        event.summary = "Created".into();
        event.created = Some(now());
        event.last_modified = Some(now());
        let created = create_calendar_at(&event, now()).expect("create");
        assert!(created.contains("PRODID:-//Envelope//Envelope Mail//EN\r\n"));
        assert!(created.contains("VERSION:2.0\r\n"));
        assert!(created.lines().all(|l| l.len() <= 76));
        let parsed = parse_calendar(&created).expect("calendar");
        assert_eq!(parsed.events, [event.clone()]);
        let zones: Vec<String> = [&start, &end]
            .iter()
            .filter_map(|t| match t {
                EventTime::Zoned { tzid, .. } => Some(tzid.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(vtimezone_ids(&created), zones);
    }

    // Windows TZIDs get a generated definition under the same TZID.
    let event = Event::new(
        "w",
        zoned("2024-07-01 09:00", "Pacific Standard Time"),
        zoned("2024-07-01 10:00", "Pacific Standard Time"),
    );
    let created = create_calendar_at(&event, now()).expect("create");
    assert_eq!(vtimezone_ids(&created), ["Pacific Standard Time"]);
    assert!(created.contains("X-LIC-LOCATION:America/Los_Angeles"));
    let parsed = parse_calendar(&created).expect("calendar");
    assert_eq!(
        parsed.tz.to_utc(&parsed.events[0].start, Tz::UTC),
        utc("2024-07-01 16:00")
    );

    assert!(matches!(
        create_calendar_at(
            &Event::new(
                " ",
                EventTime::Utc { at: now() },
                EventTime::Utc { at: now() }
            ),
            now()
        ),
        Err(Error::MissingUid)
    ));
    let uid = envelope_ical::new_uid();
    assert_eq!(uid.len(), 36);
    assert_ne!(uid, envelope_ical::new_uid());
}

#[test]
fn edits_target_the_right_calendar_and_tolerate_odd_input() {
    let original = text(include_bytes!("fixtures/multi_calendar.ics"));
    let data = parse_calendar(original).expect("calendar");
    let mut second = data.events[1].clone();
    second.summary = "Edited".into();
    let updated = upsert_event_at(original, &second, now()).expect("upsert");
    let calendars = parse_components(&updated);
    assert_eq!(calendars.len(), 2);
    assert_eq!(calendars[1].components[0].value("SUMMARY"), Some("Edited"));
    assert_eq!(
        calendars[0].components[0].value("SUMMARY"),
        Some("Lowercase everything")
    );
    // The unknown subcomponent of the edited event survives.
    assert!(updated.contains("BEGIN:X-VENDOR-WIDGET"));

    assert!(matches!(
        upsert_event_at("not a calendar", &second, now()),
        Err(Error::NoCalendar)
    ));
    let created = upsert_event_at("  ", &second, now()).expect("empty text creates a calendar");
    assert_eq!(parse_calendar(&created).expect("calendar").events.len(), 1);
    // Bare VEVENTs are wrapped into a calendar.
    let bare = "BEGIN:VEVENT\r\nUID:bare\r\nDTSTART:20240101T100000Z\r\nEND:VEVENT\r\n";
    let mut event = parse_calendar(bare).expect("calendar").events.remove(0);
    event.end = EventTime::Utc {
        at: utc("2024-01-01 10:00") + Duration::hours(1),
    };
    let updated = upsert_event_at(bare, &event, now()).expect("upsert");
    assert!(updated.starts_with("BEGIN:VCALENDAR\r\n"));
}
