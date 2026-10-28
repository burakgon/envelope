//! Event extraction from realistic producer output.

mod common;

use chrono_tz::Tz;
use common::{date, dt, text, utc};
use envelope_ical::{
    AlarmAction, AttendeeRole, CalendarUserType, EventClass, EventStatus, EventTime, PartStat,
    Transparency, parse_calendar,
};

fn zoned(local: &str, tzid: &str) -> EventTime {
    EventTime::Zoned {
        local: dt(local),
        tzid: tzid.to_owned(),
    }
}

#[test]
fn google_calendar_export() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/google_export.ics"))).expect("calendar");
    assert_eq!(data.method.as_deref(), Some("PUBLISH"));
    assert_eq!(data.events.len(), 3);

    let master = &data.events[0];
    assert_eq!(master.uid, "7kukuqrfedlm2f9t0vr7gpjm1h@google.com");
    assert!(master.is_recurring());
    assert_eq!(master.summary, "Team sync");
    assert_eq!(master.start, zoned("2024-03-05 10:00", "America/New_York"));
    assert_eq!(master.end, zoned("2024-03-05 10:30", "America/New_York"));
    assert_eq!(
        master.rrule.as_deref(),
        Some("FREQ=WEEKLY;WKST=SU;BYDAY=TU")
    );
    assert_eq!(
        master.exdates,
        [zoned("2024-03-26 10:00", "America/New_York")]
    );
    assert_eq!(master.location, None, "empty LOCATION is no location");
    assert_eq!(
        master.conference_url.as_deref(),
        Some("https://meet.google.com/abc-mnop-xyz")
    );
    assert_eq!(master.sequence, 2);
    assert_eq!(master.created, Some(utc("2024-02-28 15:00")));
    assert_eq!(master.status, EventStatus::Confirmed);
    assert_eq!(master.transparency, Transparency::Opaque);
    let organizer = master.organizer.as_ref().expect("organizer");
    assert_eq!(organizer.email, "jane.doe@example.com");
    assert_eq!(organizer.name.as_deref(), Some("Jane Doe"));
    assert_eq!(master.attendees.len(), 2);
    assert_eq!(master.attendees[1].email, "burak@example.com");
    assert_eq!(master.attendees[1].partstat, PartStat::NeedsAction);
    assert!(master.attendees[1].rsvp);
    assert_eq!(master.alarms.len(), 1);
    assert_eq!(master.alarms[0].minutes_before, 10);
    assert_eq!(
        master.alarms[0].description, None,
        "Google's placeholder text is dropped"
    );

    let moved = &data.events[1];
    assert_eq!(moved.uid, master.uid);
    assert_eq!(
        moved.recurrence_id,
        Some(zoned("2024-03-12 10:00", "America/New_York"))
    );
    assert_eq!(moved.start, zoned("2024-03-14 15:00", "America/New_York"));
    assert!(!moved.is_recurring());

    let trip = &data.events[2];
    assert!(trip.is_all_day());
    assert_eq!(
        trip.start,
        EventTime::Date {
            date: date("2024-03-20")
        }
    );
    assert_eq!(
        trip.end,
        EventTime::Date {
            date: date("2024-03-23")
        }
    );
    assert_eq!(trip.transparency, Transparency::Transparent);
    assert_eq!(trip.description, None);
    assert_eq!(
        trip.location.as_deref(),
        Some("Moscone Center, 747 Howard St, San Francisco, CA 94103, USA")
    );
}

#[test]
fn exchange_invite_with_windows_zone() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/outlook_invite.ics"))).expect("calendar");
    assert_eq!(data.method.as_deref(), Some("REQUEST"));
    let event = &data.events[0];
    assert_eq!(event.summary, "Quartalsplanung Q2");
    assert_eq!(event.start, zoned("2024-04-10 14:00", "Europe/Berlin"));
    assert_eq!(event.end, zoned("2024-04-10 15:30", "Europe/Berlin"));
    assert_eq!(
        data.tz.to_utc(&event.start, Tz::UTC),
        utc("2024-04-10 12:00")
    );
    assert_eq!(
        event.conference_url.as_deref(),
        Some(
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_NjM4ZmE3%40thread.v2/0?context=%7b%22Tid%22%3a%2272f988bf%22%7d"
        )
    );
    assert!(
        event
            .description
            .as_deref()
            .is_some_and(|d| d.starts_with("Hallo zusammen,\n\nbitte nehmt teil."))
    );
    assert!(
        event
            .description_html
            .as_deref()
            .is_some_and(|h| h.contains("<p>Hallo zusammen,</p>"))
    );
    assert_eq!(
        event.location.as_deref(),
        Some("Microsoft Teams-Besprechung")
    );
    // Explicit TRANSP wins over X-MICROSOFT-CDO-BUSYSTATUS.
    assert_eq!(event.transparency, Transparency::Opaque);
    assert_eq!(event.class, EventClass::Public);
    let organizer = event.organizer.as_ref().expect("organizer");
    assert_eq!(organizer.name.as_deref(), Some("Hans Müller"));
    let me = &event.attendees[0];
    assert_eq!(me.email, "burak@example.com", "addresses are lower-cased");
    assert_eq!(me.name.as_deref(), Some("Burak Gön"));
    assert_eq!(event.attendees[1].role, AttendeeRole::Optional);
    assert_eq!(event.attendees[2].cutype, CalendarUserType::Resource);
    assert_eq!(event.attendees[2].role, AttendeeRole::NonParticipant);
    assert_eq!(event.alarms.len(), 1);
    assert_eq!(event.alarms[0].minutes_before, 15);
}

#[test]
fn outlook_display_name_tzids_and_midnight_all_day() {
    let data = parse_calendar(text(include_bytes!("fixtures/outlook_display_tzid.ics")))
        .expect("calendar");
    let review = &data.events[0];
    assert_eq!(review.start, zoned("2024-01-08 10:00", "Europe/Berlin"));
    assert_eq!(
        review.rrule.as_deref(),
        Some("FREQ=WEEKLY;UNTIL=20240401T090000Z;INTERVAL=2;BYDAY=MO;WKST=MO")
    );
    let birthday = &data.events[1];
    assert!(
        birthday.is_all_day(),
        "X-MICROSOFT-CDO-ALLDAYEVENT midnight times become dates"
    );
    assert_eq!(
        birthday.start,
        EventTime::Date {
            date: date("2024-02-19")
        }
    );
    assert_eq!(
        birthday.end,
        EventTime::Date {
            date: date("2024-02-20")
        }
    );
    assert_eq!(birthday.transparency, Transparency::Transparent);
    assert_eq!(birthday.class, EventClass::Private);
    assert_eq!(birthday.summary, "Doğum günü");
}

#[test]
fn icloud_event_with_apple_extensions() {
    let data = parse_calendar(text(include_bytes!("fixtures/icloud_event.ics"))).expect("calendar");
    let event = &data.events[0];
    assert_eq!(event.summary, "Akşam yemeği");
    assert_eq!(event.start, zoned("2024-03-01 19:00", "Europe/Istanbul"));
    assert_eq!(
        data.tz.to_utc(&event.start, Tz::UTC),
        utc("2024-03-01 16:00")
    );
    assert_eq!(
        event.location.as_deref(),
        Some(
            "Karaköy Lokantası\nKemankeş Karamustafa Paşa, Kemankeş Cd. No:37/A, 34425 Beyoğlu/İstanbul, Türkiye"
        )
    );
    assert_eq!(
        event.url.as_deref(),
        Some("https://example.com/reservation/42")
    );
    let organizer = event.organizer.as_ref().expect("organizer");
    assert_eq!(
        organizer.email, "burak@icloud.com",
        "EMAIL parameter used for urn:uuid addresses"
    );
    assert_eq!(event.attendees[0].role, AttendeeRole::Chair);
    assert_eq!(event.attendees[0].partstat, PartStat::Accepted);
    assert_eq!(event.attendees[1].email, "elif@example.com");
    // ACTION:NONE (Apple's "no default alarm" marker) is not an alarm.
    assert_eq!(event.alarms.len(), 1);
    assert_eq!(event.alarms[0].minutes_before, 30);
    assert_eq!(event.alarms[0].description.as_deref(), Some("Hatırlatıcı"));
    assert_eq!(event.conference_url, None);
}

#[test]
fn thunderbird_mozilla_prefixed_zones() {
    let data = parse_calendar(text(include_bytes!("fixtures/thunderbird.ics"))).expect("calendar");
    let dentist = &data.events[0];
    assert_eq!(dentist.start, zoned("2024-01-15 08:30", "Europe/Berlin"));
    assert_eq!(dentist.class, EventClass::Private);
    assert_eq!(dentist.alarms[0].minutes_before, 60);
    assert_eq!(dentist.alarms[0].description, None);
    let review = &data.events[1];
    assert_eq!(review.color.as_deref(), Some("#ff8c00"));
    assert_eq!(review.rrule.as_deref(), Some("FREQ=MONTHLY;BYDAY=-1WE"));
}

#[test]
fn teams_and_zoom_links() {
    let teams =
        parse_calendar(text(include_bytes!("fixtures/teams_invite.ics"))).expect("calendar");
    let event = &teams.events[0];
    assert_eq!(
        event.conference_url.as_deref(),
        Some(
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_ZTVhNDk3%40thread.v2/0?context=%7b%22Tid%22%3a%22aaaa%22%2c%22Oid%22%3a%22bbbb%22%7d"
        ),
        "the help link and the options link are skipped"
    );
    assert_eq!(event.start, zoned("2024-06-03 11:00", "Europe/Istanbul"));
    assert_eq!(
        teams.tz.to_utc(&event.start, Tz::UTC),
        utc("2024-06-03 08:00")
    );

    let zoom = parse_calendar(text(include_bytes!("fixtures/zoom_invite.ics"))).expect("calendar");
    let event = &zoom.events[0];
    assert_eq!(
        event.conference_url.as_deref(),
        Some("https://us02web.zoom.us/j/81234567890?pwd=bG9uZ3Bhc3N3b3JkMTIz")
    );
    assert_eq!(
        zoom.tz.to_utc(&event.start, Tz::UTC),
        utc("2024-05-15 16:00")
    );
    assert!(
        event
            .description
            .as_deref()
            .is_some_and(|d| d.contains("+16699006833,,81234567890#,,,,*123456#"))
    );
}

#[test]
fn istanbul_uses_current_rules_not_outdated_definitions() {
    let data = parse_calendar(text(include_bytes!("fixtures/istanbul.ics"))).expect("calendar");
    let review = &data.events[0];
    assert_eq!(review.start, zoned("2015-01-05 10:00", "Europe/Istanbul"));
    // 2015: Istanbul still observed winter time (UTC+2).
    assert_eq!(
        data.tz.to_utc(&review.start, Tz::UTC),
        utc("2015-01-05 08:00")
    );
    // 2024: UTC+3 all year, although the embedded VTIMEZONE claims +2 in winter.
    let winter = zoned("2024-01-01 10:00", "Turkey Standard Time");
    assert_eq!(data.tz.to_utc(&winter, Tz::UTC), utc("2024-01-01 07:00"));

    // A custom TZID with a fixed +03:00 definition is mapped to an IANA zone
    // with identical offsets.
    let custom = &data.events[1];
    match &custom.start {
        EventTime::Zoned { tzid, .. } => {
            let tz: Tz = tzid.parse().expect("normalized to an IANA zone");
            let zone = envelope_ical::Zone::Iana(tz);
            assert_eq!(zone.offset_at(utc("2024-07-15 06:30")), 3 * 3600);
            assert_eq!(zone.offset_at(utc("2024-01-15 06:30")), 3 * 3600);
        }
        other => panic!("unexpected start {other:?}"),
    }
    assert_eq!(
        data.tz.to_utc(&custom.start, Tz::UTC),
        utc("2024-07-15 06:30")
    );
}

#[test]
fn custom_vtimezone_without_iana_equivalent() {
    let data = parse_calendar(text(include_bytes!("fixtures/custom_tz.ics"))).expect("calendar");
    let event = &data.events[0];
    assert_eq!(
        event.start,
        zoned("2024-04-22 09:00", "Mars Colony Time"),
        "unmatched TZIDs are kept"
    );
    // Before the first Sunday of May the zone is +05:45.
    assert_eq!(
        data.tz.to_utc(&event.start, Tz::UTC),
        utc("2024-04-22 03:15")
    );
}

#[test]
fn all_day_and_multi_day() {
    let data =
        parse_calendar(text(include_bytes!("fixtures/allday_multiday.ics"))).expect("calendar");
    let vacation = &data.events[0];
    assert_eq!(
        vacation.start,
        EventTime::Date {
            date: date("2024-07-22")
        }
    );
    assert_eq!(
        vacation.end,
        EventTime::Date {
            date: date("2024-08-03")
        }
    );
    let birthday = &data.events[1];
    assert_eq!(
        birthday.end,
        EventTime::Date {
            date: date("1992-03-01")
        },
        "default end is one day"
    );
    let overnight = &data.events[6];
    assert_eq!(
        data.tz.to_utc(&overnight.start, Tz::UTC),
        utc("2024-03-30 21:00")
    );
    assert_eq!(
        data.tz.to_utc(&overnight.end, Tz::UTC),
        utc("2024-03-31 08:00")
    );
}

#[test]
fn alarm_actions() {
    let data = parse_calendar(text(include_bytes!("fixtures/zoom_invite.ics"))).expect("calendar");
    assert_eq!(data.events[0].alarms[0].action, AlarmAction::Display);
    assert_eq!(data.events[0].alarms[0].minutes_before, 10);
}
