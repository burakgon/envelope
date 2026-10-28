//! Calendars subscribed to by URL: fetched once, stored read-only, fetched
//! again only when the feed changed, and removed with their events.

use envelope_core::model::EngineEvent;
use envelope_core::{Engine, EventSink, Paths};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

const V1: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Holidays\r\n\
BEGIN:VEVENT\r\nUID:ny@h\r\nDTSTART;VALUE=DATE:20270101\r\nDTEND;VALUE=DATE:20270102\r\nSUMMARY:New Year\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:x@h\r\nDTSTART;VALUE=DATE:20261225\r\nDTEND;VALUE=DATE:20261226\r\nSUMMARY:Christmas\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

const V2: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Holidays\r\n\
BEGIN:VEVENT\r\nUID:ny@h\r\nDTSTART;VALUE=DATE:20270101\r\nDTEND;VALUE=DATE:20270102\r\nSUMMARY:New Year's Day\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

fn titles(engine: &Engine) -> Vec<String> {
    let mut t: Vec<String> = engine
        .occurrences(1_796_000_000_000, 1_800_000_000_000)
        .unwrap()
        .into_iter()
        .map(|o| o.title)
        .collect();
    t.sort();
    t
}

#[tokio::test]
async fn subscribed_calendars_follow_their_feed() {
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/holidays.ics"))
        .and(header("If-None-Match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/holidays.ics"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v1\"")
                .set_body_string(V1),
        )
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/holidays.ics"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v2\"")
                .set_body_string(V2),
        )
        .with_priority(3)
        .mount(&server)
        .await;

    let dir = std::env::temp_dir().join(format!("envelope-subscriptions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Quiet,
    )
    .unwrap();

    let url = format!("{}/holidays.ics", server.uri());
    let cal = engine.subscribe_calendar(&url).await.unwrap();
    assert_eq!(cal.name, "Holidays");
    assert!(cal.read_only);
    // A read-only subscription alone leaves room for a calendar of one's own.
    assert!(
        engine
            .calendars()
            .unwrap()
            .iter()
            .any(|c| c.kind == "local" && !c.read_only)
    );
    assert_eq!(titles(&engine), ["Christmas", "New Year"]);
    assert!(
        engine.subscribe_calendar(&url).await.is_err(),
        "subscribed twice"
    );

    // Unchanged (304): nothing is stored again.
    engine.refresh_subscription(cal.id).await.unwrap();
    assert_eq!(titles(&engine), ["Christmas", "New Year"]);

    // Changed: the calendar follows the feed.
    engine.refresh_subscription(cal.id).await.unwrap();
    assert_eq!(titles(&engine), ["New Year's Day"]);

    engine.unsubscribe_calendar(cal.id).unwrap();
    assert!(
        engine
            .calendars()
            .unwrap()
            .iter()
            .all(|c| c.kind != "subscription")
    );
    assert!(titles(&engine).is_empty());

    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
