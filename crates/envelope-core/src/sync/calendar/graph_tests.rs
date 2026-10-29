//! Occurrences of Microsoft 365 series, which Microsoft 365 keeps as events
//! of their own: moved one at a time, and the whole series with them,
//! against a mock Graph server.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use envelope_ical::{Event, EventTime};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::calendar::EditScope;
use crate::db::calendar::{self as cdb, CalendarUpsert};
use crate::db::mail;
use crate::engine::{Engine, EventSink, Paths};
use crate::model::*;

struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _: EngineEvent) {}
}

struct Fixture {
    engine: Engine,
    server: MockServer,
    dir: PathBuf,
    calendar: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn at(day: u32, hour: u32) -> EventTime {
    EventTime::Utc {
        at: Utc.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap(),
    }
}

fn ms(day: u32, hour: u32, minute: u32) -> i64 {
    Utc.with_ymd_and_hms(2026, 11, day, hour, minute, 0)
        .unwrap()
        .timestamp_millis()
}

/// The weekly standup on Wednesdays at 9:00 UTC, as Graph sends its master.
fn master() -> Value {
    json!({
        "id": "master-1",
        "iCalUId": "standup-uid",
        "type": "seriesMaster",
        "subject": "Standup",
        "start": {"dateTime": "2026-11-04T09:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-04T09:30:00.0000000", "timeZone": "UTC"},
        "originalStartTimeZone": "UTC",
        "originalEndTimeZone": "UTC",
        "isOrganizer": true,
        "recurrence": {
            "pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["wednesday"], "firstDayOfWeek": "monday"},
            "range": {"type": "noEnd", "startDate": "2026-11-04", "recurrenceTimeZone": "UTC"}
        }
    })
}

impl Fixture {
    async fn new() -> Fixture {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1.0/me/events/master-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(master()))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path_regex("^/v1.0/me/events/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(master()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex("/events$"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "master-2", "type": "seriesMaster", "subject": "Standup",
                "start": {"dateTime": "2026-11-11T10:00:00.0000000", "timeZone": "UTC"},
                "end": {"dateTime": "2026-11-11T10:30:00.0000000", "timeZone": "UTC"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex("calendarView$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .mount(&server)
            .await;
        let dir = std::env::temp_dir().join(format!("envelope-graph-cal-{}", uuid::Uuid::new_v4()));
        let engine = Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            NoEvents,
        )
        .unwrap();
        *engine.graph_base.lock() = Some(format!("{}/v1.0", server.uri()));
        let calendar = engine
            .db
            .write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Microsoft,
                    "outlook",
                    "megan@contoso.com",
                    "Megan Bowen",
                    "#0ea5e9",
                    &AccountConfig::default(),
                )?;
                // No sync of its own: only what the test stores.
                tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [account])?;
                cdb::sync_calendars(
                    tx,
                    account,
                    "graph",
                    &[CalendarUpsert {
                        remote_id: "cal-1".into(),
                        name: "Calendar".into(),
                        color: "#0ea5e9".into(),
                        read_only: false,
                        is_default: true,
                        timezone: None,
                        sort_order: 0,
                        visible_default: true,
                    }],
                )?;
                Ok(cdb::account_calendars(tx, account)?[0].id)
            })
            .unwrap();
        Fixture {
            engine,
            server,
            dir,
            calendar,
        }
    }

    /// The occurrence of November 11, stored as Graph sync stores it.
    fn occurrence(&self) -> (i64, EventTime) {
        let mut event = Event::new("standup-uid", at(11, 9), at(11, 9));
        event.summary = "Standup".into();
        event.end = EventTime::Utc {
            at: Utc.with_ymd_and_hms(2026, 11, 11, 9, 30, 0).unwrap(),
        };
        event.recurrence_id = Some(at(11, 9));
        let pd = json!({ "id": "occ-1", "seriesMasterId": "master-1", "isOrganizer": true });
        let id = self
            .engine
            .store_event(
                self.calendar,
                &event,
                Some("occ-1"),
                None,
                None,
                Some(&pd),
                true,
            )
            .unwrap();
        (id, at(11, 9))
    }

    /// The bodies of the PATCH requests to an event.
    async fn patched(&self, id: &str) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| {
                r.method.as_str() == "PATCH" && r.url.path() == format!("/v1.0/me/events/{id}")
            })
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }
}

/// An instant of a Graph `dateTimeTimeZone` (in whatever zone it was written), in UTC.
fn instant(time: &Value) -> String {
    let local = chrono::NaiveDateTime::parse_from_str(
        &time["dateTime"].as_str().unwrap()[..19],
        "%Y-%m-%dT%H:%M:%S",
    )
    .unwrap();
    let zone =
        envelope_ical::resolve_tzid(time["timeZone"].as_str().unwrap()).unwrap_or(chrono_tz::UTC);
    zone.from_local_datetime(&local)
        .unwrap()
        .with_timezone(&Utc)
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}

#[tokio::test]
async fn one_occurrence_moves_by_its_own_id() {
    let f = Fixture::new().await;
    let (id, rid) = f.occurrence();
    f.engine
        .move_event(
            id,
            Some(rid),
            EditScope::This,
            ms(11, 10, 0),
            ms(11, 10, 30),
        )
        .await
        .unwrap();
    let bodies = f.patched("occ-1").await;
    assert_eq!(bodies.len(), 1, "one change to the occurrence");
    assert_eq!(instant(&bodies[0]["start"]), "2026-11-11T10:00");
    assert_eq!(instant(&bodies[0]["end"]), "2026-11-11T10:30");
    assert!(
        f.patched("master-1").await.is_empty(),
        "the series is left alone"
    );
}

#[tokio::test]
async fn the_whole_series_moves_as_far_as_its_occurrence() {
    let f = Fixture::new().await;
    let (id, rid) = f.occurrence();
    // An hour later and fifteen minutes longer.
    f.engine
        .move_event(id, Some(rid), EditScope::All, ms(11, 10, 0), ms(11, 10, 45))
        .await
        .unwrap();
    let bodies = f.patched("master-1").await;
    assert_eq!(bodies.len(), 1, "one change to the series");
    assert_eq!(instant(&bodies[0]["start"]), "2026-11-04T10:00");
    assert_eq!(instant(&bodies[0]["end"]), "2026-11-04T10:45");
    assert!(f.patched("occ-1").await.is_empty());
}

#[tokio::test]
async fn this_and_following_ends_the_series_and_starts_another() {
    let f = Fixture::new().await;
    let (id, rid) = f.occurrence();
    f.engine
        .move_event(
            id,
            Some(rid),
            EditScope::Following,
            ms(11, 10, 0),
            ms(11, 10, 30),
        )
        .await
        .unwrap();
    let ended = f.patched("master-1").await;
    assert_eq!(ended.len(), 1, "the series ends");
    assert_eq!(
        ended[0]["recurrence"]["range"]["type"], "endDate",
        "{}",
        ended[0]
    );
    assert_eq!(ended[0]["recurrence"]["range"]["endDate"], "2026-11-10");
    let created: Vec<Value> = f
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(created.len(), 1, "a new series starts");
    assert_eq!(instant(&created[0]["start"]), "2026-11-11T10:00");
    assert_eq!(
        created[0]["recurrence"]["pattern"]["type"], "weekly",
        "{}",
        created[0]
    );
}
