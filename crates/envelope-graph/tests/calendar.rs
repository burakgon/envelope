//! Calendars, calendar-view sync, series masters, event CRUD, responses and
//! free/busy against a mock Graph server.

mod common;

use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
use common::{CountingTokens, fast_retry, fixture, json_body, prefer_contains, requests, setup};
use envelope_graph::{
    CalendarColor, DeltaCursor, EventResponse, EventType, EventWriteOptions, FreeBusyStatus,
    GraphClient, OnlineMeetingProvider, ResponseType, TimeWindow,
};
use envelope_ical::{
    AttendeeRole, CalendarUserType, Event, EventClass, EventStatus, EventTime, PartStat,
    Transparency,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, Request, ResponseTemplate};

const MASTER_UID: &str = "040000008200E00074C5B7101A82E0080000000028CEBE04293FD3010000000000000000100000009F85AB8AF8ED4D4FAC777FA89954BDB7";

fn march_2024() -> TimeWindow {
    let start = Utc
        .with_ymd_and_hms(2024, 3, 1, 0, 0, 0)
        .single()
        .expect("start");
    let end = Utc
        .with_ymd_and_hms(2024, 4, 1, 0, 0, 0)
        .single()
        .expect("end");
    TimeWindow::new(start, end).expect("window")
}

fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .and_then(|date| date.and_hms_opt(h, min, 0))
        .expect("valid local time")
}

#[tokio::test]
async fn lists_calendars_with_colors() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars"))
        .and(|r: &Request| {
            r.url.query_pairs().any(|(k, v)| {
                k == "$select" && v.contains("isDefaultCalendar") && v.contains("hexColor")
            })
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "calendars.json")))
        .expect(1)
        .mount(&server)
        .await;

    let calendars = client.list_calendars().await.expect("calendars");
    assert_eq!(calendars.len(), 2);
    let primary = &calendars[0];
    assert!(primary.is_default_calendar && primary.can_edit);
    assert_eq!(primary.color, CalendarColor::Auto);
    assert_eq!(primary.css_color(), None);
    assert_eq!(
        primary.owner.as_ref().and_then(|o| o.address.as_deref()),
        Some("samanthab@contoso.com")
    );
    assert_eq!(
        primary.default_online_meeting_provider,
        Some(OnlineMeetingProvider::TeamsForBusiness)
    );
    let shared = &calendars[1];
    assert!(!shared.can_edit && !shared.is_default_calendar);
    assert_eq!(shared.color, CalendarColor::LightRed);
    assert_eq!(shared.css_color().as_deref(), Some("#e3008c"));
}

#[tokio::test]
async fn syncs_the_primary_calendar_view_with_delta() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendarView/delta"))
        .and(query_param("startDateTime", "2024-03-01T00:00:00Z"))
        .and(query_param("endDateTime", "2024-04-01T00:00:00Z"))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .and(prefer_contains("odata.maxpagesize=50"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture(&server, "calendar_view_delta_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendarView/delta"))
        .and(query_param("$skiptoken", "R0usmcCM996atia_s"))
        .and(prefer_contains("odata.maxpagesize=50"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture(&server, "calendar_view_delta_page2.json")),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let window = march_2024();
    let first = client
        .calendar_view_delta(&window, None, Some(50))
        .await
        .expect("first page");
    assert_eq!(first.items.len(), 3);

    let holiday = &first.items[0];
    assert_eq!(holiday.event_type, EventType::SingleInstance);
    let date = |d| NaiveDate::from_ymd_opt(2024, 3, d).expect("date");
    assert_eq!(holiday.event.start, EventTime::Date { date: date(8) });
    assert_eq!(holiday.event.end, EventTime::Date { date: date(9) });
    assert_eq!(holiday.event.transparency, Transparency::Transparent);
    assert_eq!(
        holiday.event.description.as_deref(),
        Some("Offices closed.")
    );
    assert!(holiday.event.alarms.is_empty());

    let occurrence = &first.items[1];
    let exception = &first.items[2];
    assert_eq!(occurrence.event_type, EventType::Occurrence);
    assert_eq!(exception.event_type, EventType::Exception);
    // Different iCalUIds per instance, one series-wide UID.
    assert_ne!(occurrence.ical_uid, exception.ical_uid);
    assert_eq!(occurrence.event.uid, exception.event.uid);
    assert!(occurrence.event.uid.contains("00000000A1B2C3D4"));
    assert_eq!(
        exception.event.recurrence_id,
        Some(EventTime::Utc {
            at: Utc
                .with_ymd_and_hms(2024, 3, 11, 16, 0, 0)
                .single()
                .expect("original")
        })
    );
    assert_eq!(
        exception.event.start,
        EventTime::Utc {
            at: Utc
                .with_ymd_and_hms(2024, 3, 12, 16, 0, 0)
                .single()
                .expect("moved")
        }
    );
    assert_eq!(
        exception.event.conference_url.as_deref(),
        Some("https://contoso.zoom.us/j/5551234567")
    );
    assert_eq!(
        occurrence.series_master_id.as_deref(),
        Some("AAMkSeriesMaster=")
    );
    assert_eq!(
        occurrence.response_status.as_ref().map(|s| s.response),
        Some(ResponseType::Accepted)
    );
    assert_eq!(occurrence.event.alarms[0].minutes_before, 10);
    assert_eq!(
        occurrence.original_start_time_zone.as_deref(),
        Some("Pacific Standard Time")
    );

    let cursor = first.next.clone();
    assert!(matches!(cursor, DeltaCursor::Next(_)));
    let second = client
        .calendar_view_delta(&window, Some(&cursor), Some(50))
        .await
        .expect("second page");
    assert!(second.items.is_empty());
    assert_eq!(second.removed_ids, vec!["AAMkCancelledLunch=".to_owned()]);
    assert!(second.next.is_done());
    assert!(second.next.url().contains("$deltatoken=R0usmci39OQxqJrxK4"));
}

#[tokio::test]
async fn pages_the_view_of_other_calendars() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars/AAMkShared=/calendarView"))
        .and(query_param("$top", "250"))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "calendar_view_page.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars/AAMkShared=/calendarView"))
        .and(query_param("$skip", "1"))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "calendar_view_page2.json")),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let events = client
        .calendar_view(Some("AAMkShared="), &march_2024())
        .await
        .expect("view");
    assert_eq!(events.len(), 2);
    let dentist = &events[0].event;
    assert_eq!(dentist.uid, "dentist-123@example.org");
    assert_eq!(dentist.class, EventClass::Private);
    assert_eq!(dentist.transparency, Transparency::Opaque);
    assert_eq!(events[0].show_as, FreeBusyStatus::Oof);
    assert_eq!(events[1].event.summary, "Car service");
}

async fn mount_series_master(server: &wiremock::MockServer, probe_status: u16) {
    if probe_status == 400 {
        Mock::given(method("GET"))
            .and(path("/v1.0/me/events/AAMkADQwMD"))
            .and(query_param(
                "$select",
                "start,originalStartTimeZone,recurrence,cancelledOccurrences",
            ))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"code": "RequestBroker--ParseUri", "message": "Could not find a property named 'cancelledOccurrences'."}
            })))
            .expect(1)
            .mount(server)
            .await;
    }
    let master = fixture(server, "event_series_master.json");
    // Probes come without a time zone preference, so in UTC (12:00 PDT).
    let mut probe = json!({
        "id": "AAMkADQwMD",
        "start": {"dateTime": "2017-09-04T19:00:00.0000000", "timeZone": "UTC"},
        "originalStartTimeZone": "Pacific Standard Time",
        "recurrence": master["recurrence"]
    });
    if probe_status != 400 {
        probe["cancelledOccurrences"] =
            json!(["OID.AAMkADQwMD.2017-09-11", "OID.AAMkADQwMD.2017-10-09"]);
    }
    let select = if probe_status == 400 {
        "start,originalStartTimeZone,recurrence"
    } else {
        "start,originalStartTimeZone,recurrence,cancelledOccurrences"
    };
    Mock::given(method("GET"))
        .and(path("/v1.0/me/events/AAMkADQwMD"))
        .and(query_param("$select", select))
        .respond_with(ResponseTemplate::new(200).set_body_json(probe))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/events/AAMkADQwMD"))
        .and(prefer_contains(
            "outlook.timezone=\"Pacific Standard Time\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(master))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn fetches_series_masters_in_their_own_zone() {
    let (server, client, _) = setup().await;
    mount_series_master(&server, 200).await;

    let master = client
        .get_series_master("AAMkADQwMD")
        .await
        .expect("master");
    let event = &master.event;
    assert_eq!(master.event_type, EventType::SeriesMaster);
    assert_eq!(event.uid, MASTER_UID);
    assert_eq!(
        event.start,
        EventTime::Zoned {
            local: local(2017, 9, 4, 12, 0),
            tzid: "Pacific Standard Time".into()
        }
    );
    // The Windows zone name is not resolved by default; the UTC start of the
    // probe gives the offset (UTC-7 in September), so UNTIL is the end of
    // the last local day in UTC (exact up to the DST change in November).
    assert_eq!(
        event.rrule.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20180101T065959Z")
    );
    assert!(event.is_recurring());
    assert_eq!(
        event.exdates,
        vec![
            EventTime::Zoned {
                local: local(2017, 9, 11, 12, 0),
                tzid: "Pacific Standard Time".into()
            },
            EventTime::Zoned {
                local: local(2017, 10, 9, 12, 0),
                tzid: "Pacific Standard Time".into()
            }
        ]
    );
    assert_eq!(event.summary, "Let's go for lunch");
    assert_eq!(
        event.description.as_deref(),
        Some("Does late morning work for you?")
    );
    assert_eq!(event.location.as_deref(), Some("Harry's Bar"));
    assert_eq!(event.alarms[0].minutes_before, 15);
    assert_eq!(
        event.organizer.as_ref().map(|o| o.email.as_str()),
        Some("alexw@contoso.com")
    );
    assert_eq!(event.attendees[0].email, "adelev@contoso.com");
    assert_eq!(event.attendees[0].partstat, PartStat::NeedsAction);
    assert!(event.attendees[0].rsvp);
    assert!(master.is_organizer);
    assert_eq!(
        master.response_status.as_ref().map(|s| s.response),
        Some(ResponseType::Organizer)
    );
    assert_eq!(
        master
            .recurrence
            .as_ref()
            .and_then(|r| r.range.recurrence_time_zone.as_deref()),
        Some("Pacific Standard Time")
    );
    assert!(
        event
            .url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://outlook.office365.com/owa/"))
    );
}

#[tokio::test]
async fn resolves_windows_zones_with_a_custom_resolver() {
    let server = wiremock::MockServer::start().await;
    let client = GraphClient::builder(Arc::new(CountingTokens::default()))
        .base_url(common::base(&server))
        .retry_policy(fast_retry())
        .time_zone_resolver(|tz| match tz {
            "Pacific Standard Time" => Some(chrono_tz::America::Los_Angeles),
            other => envelope_graph::resolve_iana_time_zone(other),
        })
        .build()
        .expect("client");
    mount_series_master(&server, 400).await;

    let master = client
        .get_series_master("AAMkADQwMD")
        .await
        .expect("master");
    assert_eq!(
        master.event.rrule.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20180101T075959Z")
    );
    // The fallback probe cannot report cancelled occurrences.
    assert!(master.event.exdates.is_empty());
}

#[tokio::test]
async fn get_event_delivers_masters_in_their_own_zone() {
    let (server, client, _) = setup().await;
    let master = fixture(&server, "event_series_master.json");
    let mut in_utc = master.clone();
    in_utc["start"] = json!({"dateTime": "2017-09-04T19:00:00.0000000", "timeZone": "UTC"});
    in_utc["end"] = json!({"dateTime": "2017-09-04T21:00:00.0000000", "timeZone": "UTC"});
    Mock::given(method("GET"))
        .and(path("/v1.0/me/events/AAMkADQwMD"))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(in_utc))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/events/AAMkADQwMD"))
        .and(prefer_contains(
            "outlook.timezone=\"Pacific Standard Time\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(master))
        .expect(1)
        .mount(&server)
        .await;

    let event = client.get_event("AAMkADQwMD", None).await.expect("event");
    assert_eq!(
        event.event.start,
        EventTime::Zoned {
            local: local(2017, 9, 4, 12, 0),
            tzid: "Pacific Standard Time".into()
        }
    );
    assert_eq!(
        event.event.rrule.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20180101T065959Z")
    );
}

#[tokio::test]
async fn lists_series_masters_through_a_batch() {
    let (server, client, _) = setup().await;
    let master = fixture(&server, "event_series_master.json");
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars/AAMkAGI2TGuLAAA=/events"))
        .and(query_param("$filter", "type eq 'seriesMaster'"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
            {"id": "AAMkADQwMD", "originalStartTimeZone": "Pacific Standard Time", "recurrence": master["recurrence"],
             "start": {"dateTime": "2017-09-04T19:00:00.0000000", "timeZone": "UTC"}},
            {"id": "AAMkGone=", "originalStartTimeZone": "UTC", "recurrence": null}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"responses": [
            {"id": "1", "status": 404, "body": {"error": {"code": "ErrorItemNotFound", "message": "gone"}}},
            {"id": "0", "status": 200, "headers": {"Content-Type": "application/json"}, "body": master}
        ]})))
        .expect(1)
        .mount(&server)
        .await;

    let masters = client
        .list_series_masters(Some("AAMkAGI2TGuLAAA="))
        .await
        .expect("masters");
    assert_eq!(masters.len(), 1);
    assert_eq!(masters[0].id, "AAMkADQwMD");
    assert_eq!(
        masters[0].event.rrule.as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20180101T065959Z")
    );
    let body: Value = requests(&server)
        .await
        .into_iter()
        .find(|r| r.url.path().ends_with("$batch"))
        .and_then(|r| r.body_json().ok())
        .expect("batch body");
    assert_eq!(body["requests"][0]["url"], "/me/events/AAMkADQwMD");
    assert_eq!(
        body["requests"][0]["headers"]["Prefer"],
        "IdType=\"ImmutableId\", outlook.timezone=\"Pacific Standard Time\""
    );
    assert_eq!(
        body["requests"][1]["headers"]["Prefer"],
        "IdType=\"ImmutableId\", outlook.timezone=\"UTC\""
    );
}

#[tokio::test]
async fn lists_instances_of_a_series() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/events/AAMkAGUzYRgWAAA=/instances"))
        .and(query_param("startDateTime", "2019-04-08T00:00:00Z"))
        .and(query_param("endDateTime", "2019-05-01T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "instances.json")))
        .expect(1)
        .mount(&server)
        .await;
    let window = TimeWindow::new(
        Utc.with_ymd_and_hms(2019, 4, 8, 0, 0, 0)
            .single()
            .expect("start"),
        Utc.with_ymd_and_hms(2019, 5, 1, 0, 0, 0)
            .single()
            .expect("end"),
    )
    .expect("window");
    let instances = client
        .event_instances("AAMkAGUzYRgWAAA=", &window)
        .await
        .expect("instances");
    assert_eq!(instances.len(), 4);
    assert_eq!(instances[0].event_type, EventType::Exception);
    assert_eq!(
        instances[0].event.recurrence_id, None,
        "no originalStart selected"
    );
    let occurrence = &instances[1];
    assert_eq!(occurrence.event_type, EventType::Occurrence);
    assert_eq!(
        occurrence.event.recurrence_id,
        Some(occurrence.event.start.clone())
    );
    assert!(instances.iter().all(|i| i.event.rrule.is_none()));
}

#[tokio::test]
async fn creates_a_teams_meeting() {
    let (server, client, _) = setup().await;
    let check_body = |r: &Request| {
        let Ok(body) = r.body_json::<Value>() else {
            return false;
        };
        body["subject"] == "Let's go brunch"
            && body["start"]
                == json!({"dateTime": "2017-04-15T11:00:00", "timeZone": "Pacific Standard Time"})
            && body["end"]
                == json!({"dateTime": "2017-04-15T12:00:00", "timeZone": "Pacific Standard Time"})
            && body["isAllDay"] == false
            && body["body"]
                == json!({"contentType": "html", "content": "<p>Does noon work for you?</p>"})
            && body["location"] == json!({"displayName": "Harry's Bar"})
            && body["attendees"]
                == json!([
                    {"emailAddress": {"address": "samanthab@contoso.com", "name": "Samantha Booth"}, "type": "required"},
                    {"emailAddress": {"address": "room1@contoso.com"}, "type": "resource"}
                ])
            && body["isOnlineMeeting"] == true
            && body["onlineMeetingProvider"] == "teamsForBusiness"
            && body["isReminderOn"] == true
            && body["reminderMinutesBeforeStart"] == 15
            && body["showAs"] == "busy"
            && body["sensitivity"] == "normal"
            && body["allowNewTimeProposals"] == false
            && body["transactionId"]
                .as_str()
                .is_some_and(|t| t.len() == 36)
            && body.get("recurrence").is_none()
    };
    Mock::given(method("POST"))
        .and(path("/v1.0/me/calendars/AAMkAGI2TGuLAAA=/events"))
        .and(check_body)
        .respond_with(
            ResponseTemplate::new(201).set_body_json(fixture(&server, "event_online_meeting.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut event = Event::new(
        "local-uid",
        EventTime::Zoned {
            local: local(2017, 4, 15, 11, 0),
            tzid: "Pacific Standard Time".into(),
        },
        EventTime::Zoned {
            local: local(2017, 4, 15, 12, 0),
            tzid: "Pacific Standard Time".into(),
        },
    );
    event.summary = "Let's go brunch".into();
    event.description_html = Some("<p>Does noon work for you?</p>".into());
    event.description = Some("Does noon work for you?".into());
    event.location = Some("Harry's Bar".into());
    event.attendees = vec![
        envelope_ical::Attendee {
            email: "samanthab@contoso.com".into(),
            name: Some("Samantha Booth".into()),
            ..Default::default()
        },
        envelope_ical::Attendee {
            email: "room1@contoso.com".into(),
            role: AttendeeRole::NonParticipant,
            cutype: CalendarUserType::Room,
            ..Default::default()
        },
    ];
    event.alarms = vec![envelope_ical::Alarm {
        action: envelope_ical::AlarmAction::Display,
        minutes_before: 15,
        description: None,
    }];
    let options = EventWriteOptions {
        online_meeting: Some(OnlineMeetingProvider::TeamsForBusiness),
        allow_new_time_proposals: Some(false),
        ..EventWriteOptions::default()
    };
    let created = client
        .create_event(Some("AAMkAGI2TGuLAAA="), &event, &options)
        .await
        .expect("created");
    assert_eq!(created.id, "AAMkAGI1AAAt8AHjAAA=");
    assert!(created.is_online_meeting);
    assert_eq!(
        created
            .online_meeting
            .as_ref()
            .and_then(|m| m.conference_id.as_deref()),
        Some("177513992")
    );
    assert!(
        created
            .event
            .conference_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://teams.microsoft.com/l/meetup-join/"))
    );
    assert!(
        created
            .event
            .uid
            .starts_with("040000008200E00074C5B7101A82E008")
    );
    assert_eq!(created.event.status, EventStatus::Confirmed);
}

#[tokio::test]
async fn creates_recurring_all_day_events() {
    let (server, client, _) = setup().await;
    let expected_recurrence = json!({
        "pattern": {"type": "absoluteYearly", "interval": 1, "month": 4, "dayOfMonth": 15},
        "range": {"type": "numbered", "startDate": "2024-04-15", "numberOfOccurrences": 5}
    });
    let check = move |r: &Request| {
        r.body_json::<Value>().is_ok_and(|b| {
            b["isAllDay"] == true
                && b["start"] == json!({"dateTime": "2024-04-15T00:00:00", "timeZone": "Turkey Standard Time"})
                && b["end"] == json!({"dateTime": "2024-04-16T00:00:00", "timeZone": "Turkey Standard Time"})
                && b["recurrence"] == expected_recurrence
                && b["transactionId"] == "retry-safe-1"
        })
    };
    let mut response = fixture(&server, "event_series_master.json");
    response["isAllDay"] = json!(true);
    response["start"] = json!({"dateTime": "2024-04-15T00:00:00.0000000", "timeZone": "UTC"});
    response["end"] = json!({"dateTime": "2024-04-16T00:00:00.0000000", "timeZone": "UTC"});
    response["recurrence"] = json!({
        "pattern": {"type": "absoluteYearly", "interval": 1, "month": 4, "dayOfMonth": 15, "daysOfWeek": [], "firstDayOfWeek": "sunday", "index": "first"},
        "range": {"type": "numbered", "startDate": "2024-04-15", "endDate": "0001-01-01", "recurrenceTimeZone": "Turkey Standard Time", "numberOfOccurrences": 5}
    });
    Mock::given(method("POST"))
        .and(path("/v1.0/me/events"))
        .and(check)
        .respond_with(ResponseTemplate::new(201).set_body_json(response))
        .expect(1)
        .mount(&server)
        .await;

    let day = |d| EventTime::Date {
        date: NaiveDate::from_ymd_opt(2024, 4, d).expect("date"),
    };
    let mut event = Event::new("birthday", day(15), day(16));
    event.summary = "Birthday".into();
    event.rrule = Some("FREQ=YEARLY;BYMONTH=4;BYMONTHDAY=15;COUNT=5".into());
    let options = EventWriteOptions {
        time_zone: Some("Turkey Standard Time".into()),
        transaction_id: Some("retry-safe-1".into()),
        ..EventWriteOptions::default()
    };
    let created = client
        .create_event(None, &event, &options)
        .await
        .expect("created");
    assert!(created.event.is_all_day());
    assert_eq!(
        created.event.rrule.as_deref(),
        Some("FREQ=YEARLY;BYMONTH=4;BYMONTHDAY=15;COUNT=5")
    );
}

#[tokio::test]
async fn updates_only_what_changed() {
    let (server, client, _) = setup().await;
    let master = fixture(&server, "event_series_master.json");
    let mut updated = master.clone();
    updated["subject"] = json!("Let's go for dinner");
    updated["showAs"] = json!("free");
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/events/AAMkADQwMD"))
        .and(json_body(
            json!({"subject": "Let's go for dinner", "showAs": "free"}),
        ))
        .and(prefer_contains("outlook.timezone=\"UTC\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(updated))
        .expect(1)
        .mount(&server)
        .await;

    let before =
        envelope_graph::GraphEvent::from_json(master, &envelope_graph::resolve_iana_time_zone)
            .expect("mapped")
            .event;
    let mut after = before.clone();
    after.summary = "Let's go for dinner".into();
    after.transparency = Transparency::Transparent;
    let result = client
        .update_event_diff("AAMkADQwMD", &before, &after, &EventWriteOptions::default())
        .await
        .expect("updated");
    assert_eq!(result.event.summary, "Let's go for dinner");
    assert_eq!(result.event.transparency, Transparency::Transparent);
}

#[tokio::test]
async fn deletes_cancels_and_responds() {
    let (server, client, _) = setup().await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/events/AAMkEvent="))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/events/AAMkEvent=/cancel"))
        .and(json_body(
            json!({"comment": "Cancelling for this day and all future days."}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    for (action, body) in [
        (
            "accept",
            json!({"comment": "comment-value", "sendResponse": true}),
        ),
        (
            "tentativelyAccept",
            json!({"comment": "", "sendResponse": true}),
        ),
        (
            "decline",
            json!({"comment": "Can't make it", "sendResponse": false}),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(format!("/v1.0/me/events/AAMkEvent=/{action}")))
            .and(json_body(body))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
    }

    client.delete_event("AAMkEvent=").await.expect("deleted");
    client
        .cancel_event(
            "AAMkEvent=",
            Some("Cancelling for this day and all future days."),
        )
        .await
        .expect("cancelled");
    client
        .respond_to_event(
            "AAMkEvent=",
            EventResponse::Accept,
            Some("comment-value"),
            true,
        )
        .await
        .expect("accepted");
    client
        .respond_to_event("AAMkEvent=", EventResponse::TentativelyAccept, None, true)
        .await
        .expect("tentative");
    client
        .respond_to_event(
            "AAMkEvent=",
            EventResponse::Decline,
            Some("Can't make it"),
            false,
        )
        .await
        .expect("declined");
}

#[tokio::test]
async fn reads_free_busy() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/calendar/getSchedule"))
        .and(json_body(json!({
            "schedules": ["adelev@contoso.com", "meganb@contoso.com"],
            "startTime": {"dateTime": "2019-03-15T09:00:00", "timeZone": "UTC"},
            "endTime": {"dateTime": "2019-03-15T18:00:00", "timeZone": "UTC"},
            "availabilityViewInterval": 60
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "get_schedule.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let window = TimeWindow::new(
        Utc.with_ymd_and_hms(2019, 3, 15, 9, 0, 0)
            .single()
            .expect("start"),
        Utc.with_ymd_and_hms(2019, 3, 15, 18, 0, 0)
            .single()
            .expect("end"),
    )
    .expect("window");
    let schedules = client
        .get_schedule(
            &["adelev@contoso.com".into(), "meganb@contoso.com".into()],
            &window,
            60,
        )
        .await
        .expect("schedule");
    assert_eq!(schedules.len(), 2);
    assert_eq!(schedules[0].schedule_id, "adelev@contoso.com");
    assert_eq!(schedules[0].availability_view, "000220130");
    assert_eq!(schedules[0].schedule_items[0].status, FreeBusyStatus::Busy);
    assert_eq!(
        schedules[0].schedule_items[0].location.as_deref(),
        Some("Harry's Bar")
    );
    assert_eq!(
        schedules[1].schedule_items[1].status,
        FreeBusyStatus::Tentative
    );
    assert!(
        client
            .get_schedule(&[], &window, 60)
            .await
            .expect("empty")
            .is_empty()
    );
}
