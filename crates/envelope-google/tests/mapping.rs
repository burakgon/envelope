//! Fixture-based tests of the Google ⇄ canonical event mapping.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use envelope_google::{
    AccessRole, ColorDefinition, ColorPalette, EntryPointKind, EventPage, EventType,
    EventTypeProperties, GoogleEvent, Reminder, ReminderMethod, Visibility,
};
use envelope_ical::{
    AlarmAction, AttendeeRole, CalendarUserType, EventClass, EventStatus, EventTime, PartStat,
    Transparency,
};
use serde_json::{Value, json};

const FULL_SYNC: &[u8] = include_bytes!("fixtures/events_full_sync.json");
const INCREMENTAL: &[u8] = include_bytes!("fixtures/events_incremental.json");

fn palette() -> Arc<ColorPalette> {
    let color = |bg: &str| ColorDefinition {
        background: bg.to_owned(),
        foreground: "#1d1d1d".to_owned(),
    };
    Arc::new(ColorPalette {
        calendar: BTreeMap::new(),
        event: BTreeMap::from([
            ("1".to_owned(), color("#a4bdfc")),
            ("11".to_owned(), color("#dc2127")),
        ]),
        updated: None,
    })
}

fn local(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").expect("valid local time")
}

fn zoned(s: &str, tz: &str) -> EventTime {
    EventTime::Zoned {
        local: local(s),
        tzid: tz.to_owned(),
    }
}

fn date(y: i32, m: u32, d: u32) -> EventTime {
    EventTime::Date {
        date: NaiveDate::from_ymd_opt(y, m, d).expect("valid date"),
    }
}

fn full_page() -> EventPage {
    EventPage::from_json(FULL_SYNC, Some(palette())).expect("page decodes")
}

fn event<'a>(page: &'a EventPage, id: &str) -> &'a GoogleEvent {
    page.events
        .iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("event {id} missing"))
}

#[test]
fn page_metadata_and_classification() {
    let page = full_page();
    assert_eq!(
        page.next_sync_token.as_deref(),
        Some("CPDAlvWDx70CEPDAlvWDx70CGAU=")
    );
    assert_eq!(page.next_page_token, None);
    assert_eq!(page.time_zone.as_deref(), Some("Europe/Istanbul"));
    assert_eq!(page.access_role, Some(AccessRole::Owner));
    assert_eq!(
        page.default_reminders,
        vec![Reminder {
            method: ReminderMethod::Popup,
            minutes: 30
        }]
    );
    let ids: Vec<&str> = page.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "allday0001aa",
            "timed0002bb",
            "weekly0003cc",
            "weekly0003cc_20240115T060000Z",
            "ooo0006ff",
            "focus0007gg",
            "bday0008hh"
        ]
    );
    assert_eq!(page.cancelled_occurrences.len(), 1);
    assert!(page.deleted_event_ids.is_empty());
    assert_eq!(page.skipped.len(), 1);
    assert_eq!(page.skipped[0].id.as_deref(), Some("broken0009ii"));
    assert!(page.skipped[0].reason.contains("no start"));
}

#[test]
fn all_day_event() {
    let page = full_page();
    let all_day = event(&page, "allday0001aa");
    let e = &all_day.event;
    assert_eq!(e.uid, "allday0001aa@google.com");
    assert_eq!(e.start, date(2024, 2, 1));
    assert_eq!(e.end, date(2024, 2, 3));
    assert!(e.is_all_day());
    assert_eq!(e.transparency, Transparency::Transparent);
    assert_eq!(e.class, EventClass::Public);
    assert_eq!(all_day.visibility, Visibility::Default);
    assert!(!all_day.use_default_reminders);
    assert!(e.alarms.is_empty());
    assert!(all_day.organizer_is_self);
    assert_eq!(
        e.organizer.as_ref().map(|o| o.email.as_str()),
        Some("ada@example.com")
    );
    assert_eq!(
        e.created,
        Some(
            Utc.with_ymd_and_hms(2024, 1, 2, 8, 0, 0)
                .single()
                .expect("instant")
        )
    );
    assert_eq!(
        all_day.html_link.as_deref(),
        Some("https://www.google.com/calendar/event?eid=YWxsZGF5MDAwMWFh")
    );
}

#[test]
fn timed_event_with_zone_attendees_and_html() {
    let page = full_page();
    let timed = event(&page, "timed0002bb");
    let e = &timed.event;
    // 17:00 at +03:00 is 09:00 in New York (EST).
    assert_eq!(e.start, zoned("2024-01-15T09:00:00", "America/New_York"));
    assert_eq!(e.end, zoned("2024-01-15T10:30:00", "America/New_York"));
    assert_eq!(
        e.description.as_deref(),
        Some(
            "Agenda:\n• Review Q4\n• Plan Q1\nNotes: planning doc <https://docs.example.com/plan>"
        )
    );
    assert!(
        e.description_html
            .as_deref()
            .is_some_and(|h| h.starts_with("Agenda:<br>"))
    );
    assert_eq!(
        e.location.as_deref(),
        Some("Room 5, 800 Howard St., San Francisco, CA 94103")
    );
    assert_eq!(timed.color_id.as_deref(), Some("11"));
    assert_eq!(e.color.as_deref(), Some("#dc2127"));
    assert_eq!(e.class, EventClass::Private);
    assert_eq!(e.sequence, 2);
    assert_eq!(
        e.url.as_deref(),
        Some("https://mail.google.com/mail/u/0/#inbox/18c1f2")
    );
    assert_eq!(timed.source_title.as_deref(), Some("Planning thread"));
    assert!(timed.guests_can_modify);
    assert!(!timed.guests_can_see_other_guests);
    assert!(
        timed.guests_can_invite_others,
        "defaults to true when absent"
    );
    assert!(!timed.organizer_is_self);

    // Attendees: organizer → chair, optional, resource, self, extra guests.
    assert_eq!(e.attendees.len(), 5);
    let grace = &e.attendees[0];
    assert_eq!(
        grace.email, "grace@example.com",
        "addresses are lower-cased"
    );
    assert_eq!(grace.role, AttendeeRole::Chair);
    assert_eq!(grace.partstat, PartStat::Accepted);
    assert!(!grace.rsvp);
    let alan = &e.attendees[2];
    assert_eq!(alan.role, AttendeeRole::Optional);
    assert_eq!(alan.partstat, PartStat::Declined);
    let room = &e.attendees[3];
    assert_eq!(room.cutype, CalendarUserType::Resource);
    assert_eq!(room.name.as_deref(), Some("Room 5"));
    assert_eq!(e.attendees[4].partstat, PartStat::NeedsAction);
    assert!(e.attendees[4].rsvp);

    assert_eq!(timed.self_response(), Some(PartStat::Tentative));
    let me = timed.self_attendee().expect("self attendee");
    assert_eq!(me.email, "ada@example.com");
    let my_details = timed.attendee_details("ADA@example.com").expect("details");
    assert_eq!(my_details.comment.as_deref(), Some("Might be late"));
    assert_eq!(
        timed
            .attendee_details("linus@example.com")
            .map(|d| d.additional_guests),
        Some(1)
    );

    // `useDefault` resolves to the calendar's default reminders.
    assert!(timed.use_default_reminders);
    assert_eq!(e.alarms.len(), 1);
    assert_eq!(e.alarms[0].action, AlarmAction::Display);
    assert_eq!(e.alarms[0].minutes_before, 30);

    assert_eq!(timed.attachments.len(), 1);
    assert_eq!(timed.attachments[0].title.as_deref(), Some("Plan.pdf"));
    assert_eq!(
        timed
            .extended_properties
            .private
            .get("envelopeRef")
            .map(String::as_str),
        Some("42")
    );
}

#[test]
fn recurring_master_with_meet_and_overrides() {
    let page = full_page();
    let master = event(&page, "weekly0003cc");
    let e = &master.event;
    assert!(e.is_recurring());
    assert_eq!(e.uid, "weekly0003cc@google.com");
    assert_eq!(e.start, zoned("2024-01-01T09:00:00", "Europe/Istanbul"));
    assert_eq!(
        e.rrule.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO;UNTIL=20240325T055959Z")
    );
    assert_eq!(
        e.exdates,
        vec![zoned("2024-01-08T09:00:00", "Europe/Istanbul")]
    );
    assert!(e.rdates.is_empty());
    assert!(master.extra_recurrence.is_empty());

    assert_eq!(
        e.conference_url.as_deref(),
        Some("https://meet.google.com/abc-defg-hij")
    );
    let conference = master.conference.as_ref().expect("conference");
    assert_eq!(conference.solution_name.as_deref(), Some("Google Meet"));
    assert_eq!(conference.solution_type.as_deref(), Some("hangoutsMeet"));
    assert_eq!(conference.conference_id.as_deref(), Some("abc-defg-hij"));
    let kinds: Vec<EntryPointKind> = conference.entry_points.iter().map(|p| p.kind).collect();
    assert_eq!(
        kinds,
        [
            EntryPointKind::Video,
            EntryPointKind::More,
            EntryPointKind::Phone
        ]
    );
    assert_eq!(conference.entry_points[2].pin.as_deref(), Some("123456789"));
    assert_eq!(
        conference.entry_points[2].region_code.as_deref(),
        Some("US")
    );

    assert!(!master.use_default_reminders);
    let alarms: Vec<(AlarmAction, i64)> = e
        .alarms
        .iter()
        .map(|a| (a.action, a.minutes_before))
        .collect();
    assert_eq!(
        alarms,
        [(AlarmAction::Email, 1440), (AlarmAction::Display, 10)]
    );
}

#[test]
fn modified_and_cancelled_occurrences() {
    let page = full_page();
    let moved = event(&page, "weekly0003cc_20240115T060000Z");
    assert_eq!(moved.recurring_event_id.as_deref(), Some("weekly0003cc"));
    assert_eq!(
        moved.event.uid, "weekly0003cc@google.com",
        "shares the series UID"
    );
    assert_eq!(
        moved.event.recurrence_id,
        Some(zoned("2024-01-15T09:00:00", "Europe/Istanbul"))
    );
    assert_eq!(
        moved.event.start,
        zoned("2024-01-15T11:00:00", "Europe/Istanbul")
    );
    assert_eq!(moved.event.summary, "Team sync (moved)");
    assert!(!moved.event.is_recurring());

    let cancelled = &page.cancelled_occurrences[0];
    assert_eq!(cancelled.id, "weekly0003cc_20240122T060000Z");
    assert_eq!(cancelled.recurring_event_id, "weekly0003cc");
    assert_eq!(
        cancelled.original_start,
        zoned("2024-01-22T09:00:00", "Europe/Istanbul")
    );

    // The cancellation becomes an EXDATE of the master.
    let mut master = event(&page, "weekly0003cc").clone();
    assert_eq!(
        master.apply_cancelled_occurrences(&page.cancelled_occurrences),
        1
    );
    assert_eq!(
        master.apply_cancelled_occurrences(&page.cancelled_occurrences),
        0
    );
    assert_eq!(
        master.event.exdates,
        vec![
            zoned("2024-01-08T09:00:00", "Europe/Istanbul"),
            zoned("2024-01-22T09:00:00", "Europe/Istanbul"),
        ]
    );
}

#[test]
fn special_event_types() {
    let page = full_page();
    let ooo = event(&page, "ooo0006ff");
    assert_eq!(ooo.event_type, EventType::OutOfOffice);
    assert_eq!(ooo.visibility, Visibility::Public);
    let Some(EventTypeProperties::OutOfOffice(settings)) = &ooo.type_properties else {
        panic!("out-of-office properties expected");
    };
    assert_eq!(
        settings.auto_decline_mode.as_deref(),
        Some("declineAllConflictingInvitations")
    );
    assert_eq!(
        settings.decline_message.as_deref(),
        Some("Away until the 12th")
    );

    let focus = event(&page, "focus0007gg");
    assert_eq!(focus.event_type, EventType::FocusTime);
    let Some(EventTypeProperties::FocusTime(settings)) = &focus.type_properties else {
        panic!("focus-time properties expected");
    };
    assert_eq!(settings.chat_status.as_deref(), Some("doNotDisturb"));

    let birthday = event(&page, "bday0008hh");
    assert_eq!(birthday.event_type, EventType::Birthday);
    assert_eq!(birthday.event.rrule.as_deref(), Some("FREQ=YEARLY"));
    assert_eq!(birthday.event.class, EventClass::Private);
    let Some(EventTypeProperties::Birthday(props)) = &birthday.type_properties else {
        panic!("birthday properties expected");
    };
    assert_eq!(props.kind, "birthday");
    assert_eq!(
        props.contact.as_deref(),
        Some("people/c7700000000000000000")
    );
}

#[test]
fn incremental_page_reports_deletions_and_cancellations() {
    let page = EventPage::from_json(INCREMENTAL, Some(palette())).expect("page decodes");
    assert_eq!(page.deleted_event_ids, vec!["allday0001aa".to_owned()]);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].event.status, EventStatus::Tentative);
    assert_eq!(
        page.next_sync_token.as_deref(),
        Some("CPDAlvWDx70CEPDAlvWDx70CGAY=")
    );

    // A cancelled occurrence without a zone is reported as an instant and
    // conformed to the series' zone when applied to the master.
    let cancelled = &page.cancelled_occurrences[0];
    assert_eq!(
        cancelled.original_start,
        EventTime::Utc {
            at: local("2024-01-29T06:00:00").and_utc()
        }
    );
    let mut master = event(&full_page(), "weekly0003cc").clone();
    master.apply_cancelled_occurrences(&page.cancelled_occurrences);
    assert!(
        master
            .event
            .exdates
            .contains(&zoned("2024-01-29T09:00:00", "Europe/Istanbul"))
    );
}

#[test]
fn reverse_mapping_of_a_recurring_master() {
    let page = full_page();
    let master = event(&page, "weekly0003cc");
    let body = master.to_json(None).expect("serializes");
    assert_eq!(body["id"], json!("weekly0003cc"));
    assert_eq!(body["iCalUID"], json!("weekly0003cc@google.com"));
    assert_eq!(
        body["start"],
        json!({"date": null, "dateTime": "2024-01-01T09:00:00+03:00", "timeZone": "Europe/Istanbul"})
    );
    assert_eq!(
        body["recurrence"],
        json!([
            "RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20240325T055959Z",
            "EXDATE;TZID=Europe/Istanbul:20240108T090000"
        ])
    );
    assert_eq!(
        body["reminders"],
        json!({"useDefault": false, "overrides": [
            {"method": "email", "minutes": 1440},
            {"method": "popup", "minutes": 10}
        ]})
    );
    assert_eq!(body["visibility"], json!("default"));
    assert_eq!(body["status"], json!("confirmed"));
    assert_eq!(body["transparency"], json!("opaque"));
    // Read-only and conference fields are never written back.
    for read_only in [
        "organizer",
        "creator",
        "htmlLink",
        "conferenceData",
        "hangoutLink",
        "etag",
    ] {
        assert!(
            body.get(read_only).is_none(),
            "{read_only} must not be written"
        );
    }
}

#[test]
fn reverse_mapping_keeps_google_specific_details() {
    let page = full_page();
    let timed = event(&page, "timed0002bb");
    let body = timed.to_json(None).expect("serializes");
    assert_eq!(body["description"], json!(timed.event.description_html));
    assert_eq!(body["visibility"], json!("private"));
    assert_eq!(body["colorId"], json!("11"));
    assert_eq!(body["reminders"], json!({"useDefault": true}));
    assert_eq!(body["guestsCanModify"], json!(true));
    assert_eq!(body["guestsCanSeeOtherGuests"], json!(false));
    assert_eq!(
        body["source"],
        json!({"url": "https://mail.google.com/mail/u/0/#inbox/18c1f2", "title": "Planning thread"})
    );
    assert_eq!(
        body["extendedProperties"]["private"]["envelopeRef"],
        json!("42")
    );
    assert_eq!(
        body["attachments"][0]["fileUrl"],
        json!("https://drive.google.com/open?id=1aBcD")
    );
    let attendees = body["attendees"].as_array().expect("attendees");
    assert_eq!(attendees.len(), 5);
    assert_eq!(
        attendees[1],
        json!({"email": "ada@example.com", "displayName": "Ada Lovelace", "responseStatus": "tentative", "comment": "Might be late"})
    );
    assert_eq!(attendees[2]["optional"], json!(true));
    assert_eq!(attendees[3]["resource"], json!(true));
    assert_eq!(attendees[4]["additionalGuests"], json!(1));
    assert!(
        attendees
            .iter()
            .all(|a| a.get("organizer").is_none() && a.get("self").is_none())
    );

    // Switching to custom reminders.
    let mut edited = timed.clone();
    edited.set_alarms(vec![
        Reminder {
            method: ReminderMethod::Email,
            minutes: 60,
        }
        .to_alarm(),
    ]);
    let body = edited.to_json(None).expect("serializes");
    assert_eq!(
        body["reminders"],
        json!({"useDefault": false, "overrides": [{"method": "email", "minutes": 60}]})
    );
}

#[test]
fn all_day_round_trip_and_special_types() {
    let page = full_page();
    let all_day = event(&page, "allday0001aa")
        .to_json(None)
        .expect("serializes");
    assert_eq!(
        all_day["start"],
        json!({"date": "2024-02-01", "dateTime": null, "timeZone": null})
    );
    assert_eq!(all_day["transparency"], json!("transparent"));
    assert_eq!(
        all_day["reminders"],
        json!({"useDefault": false, "overrides": []})
    );
    assert!(all_day.get("eventType").is_none());

    let ooo = event(&page, "ooo0006ff").to_json(None).expect("serializes");
    assert_eq!(ooo["eventType"], json!("outOfOffice"));
    assert_eq!(
        ooo["outOfOfficeProperties"],
        json!({"autoDeclineMode": "declineAllConflictingInvitations", "declineMessage": "Away until the 12th"})
    );
    let birthday = event(&page, "bday0008hh")
        .to_json(None)
        .expect("serializes");
    assert_eq!(birthday["birthdayProperties"], json!({"type": "birthday"}));
    assert_eq!(birthday["recurrence"], json!(["RRULE:FREQ=YEARLY"]));
}

#[test]
fn google_events_survive_serde_persistence() {
    let page = full_page();
    for original in &page.events {
        let stored = serde_json::to_string(original).expect("serialize");
        let restored: GoogleEvent = serde_json::from_str(&stored).expect("deserialize");
        assert_eq!(&restored, original);
    }
    let value: Value = serde_json::to_value(&page.cancelled_occurrences[0]).expect("serialize");
    assert_eq!(value["recurringEventId"], json!("weekly0003cc"));
}
