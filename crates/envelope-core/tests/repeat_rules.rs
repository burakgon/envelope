//! Repeat rules and time zones as the event editor makes them, through the
//! engine on a calendar on this computer: stored, expanded across DST
//! changes and read back.

use envelope_core::calendar::{EditScope, EventInput, Occurrence};
use envelope_core::model::EngineEvent;
use envelope_core::{Engine, EventSink, Paths};
use serde_json::json;

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

fn ms(at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .unwrap_or_else(|e| panic!("{at}: {e}"))
        .timestamp_millis()
}

fn utc(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// The editor's input: times as instants, with the zone they were typed in.
fn input(calendar_id: i64, summary: &str, value: serde_json::Value) -> EventInput {
    let mut base = json!({
        "calendarId": calendar_id,
        "summary": summary,
        "allDay": false,
        "attendees": [],
        "alarms": [],
    });
    if let (Some(base), Some(extra)) = (base.as_object_mut(), value.as_object()) {
        base.extend(extra.clone());
    }
    serde_json::from_value(base).unwrap_or_else(|e| panic!("{e}"))
}

fn starts(engine: &Engine, title: &str, from: &str, to: &str) -> Vec<String> {
    engine
        .occurrences(ms(from), ms(to))
        .unwrap()
        .into_iter()
        .filter(|o: &Occurrence| o.title == title)
        .map(|o| utc(o.start))
        .collect()
}

#[tokio::test]
async fn editor_rules_and_zones_round_trip() {
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let dir = std::env::temp_dir().join(format!("envelope-repeat-rules-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Quiet,
    )
    .unwrap();
    let calendar = engine
        .calendars()
        .unwrap()
        .into_iter()
        .find(|c| !c.read_only)
        .expect("a calendar on this computer");

    // 10:00 in New York on the second Tuesday of every month, six times:
    // 15:00 UTC in winter, 14:00 UTC once DST starts on 14 March 2027.
    let id = engine
        .create_event(input(
            calendar.id,
            "Board meeting",
            json!({
                "start": ms("2026-11-10T15:00:00Z"),
                "end": ms("2026-11-10T16:00:00Z"),
                "timezone": "America/New_York",
                "rrule": "FREQ=MONTHLY;BYDAY=2TU;COUNT=6",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        starts(
            &engine,
            "Board meeting",
            "2026-11-01T00:00:00Z",
            "2027-12-31T00:00:00Z"
        ),
        [
            "2026-11-10 15:00",
            "2026-12-08 15:00",
            "2027-01-12 15:00",
            "2027-02-09 15:00",
            "2027-03-09 15:00",
            "2027-04-13 14:00",
        ]
    );
    let detail = engine.event_detail(id, None).unwrap();
    assert_eq!(detail.timezone.as_deref(), Some("America/New_York"));
    assert_eq!(
        detail.event.rrule.as_deref(),
        Some("FREQ=MONTHLY;BYDAY=2TU;COUNT=6")
    );

    // The last Friday, every month, until the end of 25 December in New
    // York (05:00 UTC the next day); DST ends on 1 November 2026.
    engine
        .create_event(input(
            calendar.id,
            "Review",
            json!({
                "start": ms("2026-10-30T14:00:00Z"),
                "end": ms("2026-10-30T15:00:00Z"),
                "timezone": "America/New_York",
                "rrule": "FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20261226T045959Z",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        starts(
            &engine,
            "Review",
            "2026-10-01T00:00:00Z",
            "2027-03-01T00:00:00Z"
        ),
        ["2026-10-30 14:00", "2026-11-27 15:00", "2026-12-25 15:00"]
    );

    // Every other week on Monday and Wednesday, and day 31 of every month,
    // which months without one skip.
    engine
        .create_event(input(
            calendar.id,
            "Gym",
            json!({
                "start": ms("2026-11-02T06:00:00Z"),
                "end": ms("2026-11-02T07:00:00Z"),
                "timezone": "Europe/Berlin",
                "rrule": "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=5",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        starts(
            &engine,
            "Gym",
            "2026-11-01T00:00:00Z",
            "2027-01-01T00:00:00Z"
        ),
        [
            "2026-11-02 06:00",
            "2026-11-04 06:00",
            "2026-11-16 06:00",
            "2026-11-18 06:00",
            "2026-11-30 06:00",
        ]
    );
    engine
        .create_event(input(
            calendar.id,
            "Invoices",
            json!({
                "start": ms("2026-10-31T08:00:00Z"),
                "end": ms("2026-10-31T08:30:00Z"),
                "timezone": "Europe/Istanbul",
                "rrule": "FREQ=MONTHLY;BYMONTHDAY=31;COUNT=3",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        starts(
            &engine,
            "Invoices",
            "2026-10-01T00:00:00Z",
            "2027-06-01T00:00:00Z"
        ),
        ["2026-10-31 08:00", "2026-12-31 08:00", "2027-01-31 08:00"]
    );

    // All day, every two years on its date, until a date.
    engine
        .create_event(input(
            calendar.id,
            "Reunion",
            json!({
                "allDay": true,
                "startDate": "2026-11-10",
                "endDate": "2026-11-11",
                "rrule": "FREQ=YEARLY;INTERVAL=2;UNTIL=20301110",
            }),
        ))
        .await
        .unwrap();
    let reunions: Vec<String> = engine
        .occurrences(ms("2026-01-01T00:00:00Z"), ms("2034-01-01T00:00:00Z"))
        .unwrap()
        .into_iter()
        .filter(|o| o.title == "Reunion")
        .filter_map(|o| o.start_date)
        .collect();
    assert_eq!(reunions, ["2026-11-10", "2028-11-10", "2030-11-10"]);

    // Moved to another zone in the editor: the same wall-clock time there.
    let mut moved = input(
        calendar.id,
        "Board meeting",
        json!({
            "start": ms("2026-11-10T01:00:00Z"),
            "end": ms("2026-11-10T02:00:00Z"),
            "timezone": "Asia/Tokyo",
            "rrule": "FREQ=MONTHLY;BYDAY=2TU;COUNT=6",
        }),
    );
    engine
        .update_event(id, None, EditScope::All, moved.clone())
        .await
        .unwrap();
    let detail = engine.event_detail(id, None).unwrap();
    assert_eq!(detail.timezone.as_deref(), Some("Asia/Tokyo"));
    assert_eq!(
        starts(
            &engine,
            "Board meeting",
            "2026-11-01T00:00:00Z",
            "2027-01-01T00:00:00Z"
        ),
        // 10:00 in Tokyo on the second Tuesdays.
        ["2026-11-10 01:00", "2026-12-08 01:00"]
    );

    // A zone written the Windows way is understood, and kept by its IANA name.
    moved.timezone = Some("Tokyo Standard Time".into());
    engine
        .update_event(id, None, EditScope::All, moved)
        .await
        .unwrap();
    let detail = engine.event_detail(id, None).unwrap();
    assert_eq!(detail.timezone.as_deref(), Some("Asia/Tokyo"));
    assert_eq!(
        starts(
            &engine,
            "Board meeting",
            "2026-11-01T00:00:00Z",
            "2026-12-01T00:00:00Z"
        ),
        ["2026-11-10 01:00"]
    );

    // Dragging an occurrence keeps the series' zone.
    engine
        .move_event(
            id,
            None,
            EditScope::All,
            ms("2026-11-10T02:00:00Z"),
            ms("2026-11-10T03:00:00Z"),
        )
        .await
        .unwrap();
    let detail = engine.event_detail(id, None).unwrap();
    assert_eq!(detail.timezone.as_deref(), Some("Asia/Tokyo"));
    assert_eq!(
        starts(
            &engine,
            "Board meeting",
            "2026-11-01T00:00:00Z",
            "2026-12-31T00:00:00Z"
        ),
        ["2026-11-10 02:00", "2026-12-08 02:00"]
    );

    drop(engine);
    let _ = std::fs::remove_dir_all(&dir);
}
