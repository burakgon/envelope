//! CalDAV end-to-end test against the development server (Radicale).
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_calendar -- --nocapture

use std::sync::Arc;
use std::time::{Duration, Instant};

use envelope_core::calendar::{EditScope, EventInput};
use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};
use parking_lot::Mutex;

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);
impl EventSink for Events {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().push(e);
    }
}

fn week() -> (i64, i64) {
    use chrono::{Datelike, Local, TimeZone};
    let today = Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let start = Local.from_local_datetime(&monday.and_hms_opt(0, 0, 0).unwrap()).unwrap().timestamp_millis();
    (start, start + 7 * 86_400_000)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caldav_round_trip() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let _ = tracing_subscriber::fmt().with_env_filter("debug,hyper=info,reqwest=info,rustls=info,h2=info").try_init();
    let reset = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/devserver/reset.sh");
    assert!(std::process::Command::new(reset).status().unwrap().success());
    let dir = std::env::temp_dir().join(format!("envelope-cal-{}", uuid::Uuid::new_v4()));
    let engine = Engine::start(Paths { data: dir.join("d"), cache: dir.join("c") }, Events::default()).unwrap();
    engine
        .add_password_account(NewAccount {
            email: "alex@envelope.test".into(),
            name: "Alex Morgan".into(),
            provider: "generic".into(),
            password: "password".into(),
            config: AccountConfig {
                imap: Some(ServerConfig { host: "127.0.0.1".into(), port: 1993, security: Security::Tls, username: String::new() }),
                smtp: Some(ServerConfig { host: "127.0.0.1".into(), port: 1587, security: Security::None, username: String::new() }),
                caldav_url: Some("http://127.0.0.1:5232/".into()),
                accept_invalid_certs: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();

    let t0 = Instant::now();
    let mut cals = Vec::new();
    while t0.elapsed() < Duration::from_secs(20) {
        cals = engine.calendars().unwrap();
        if cals.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("calendars after {:?}: {:?}", t0.elapsed(), cals.iter().map(|c| (&c.name, &c.kind)).collect::<Vec<_>>());
    assert!(cals.iter().any(|c| c.name == "Work" && c.kind == "caldav"));
    assert!(!cals.iter().any(|c| c.kind == "local"), "no local calendar when CalDAV exists");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (from, to) = week();
    let occ = engine.occurrences(from, to).unwrap();
    for o in &occ {
        println!("{} {} {}", chrono::DateTime::from_timestamp_millis(o.start).unwrap().with_timezone(&chrono::Local).format("%a %H:%M"), o.title, if o.is_recurring { "(series)" } else { "" });
    }
    let standups: Vec<_> = occ.iter().filter(|o| o.title == "Daily standup").collect();
    assert_eq!(standups.len(), 5, "weekday standups expanded");

    // Create, move, and delete an event.
    let work = cals.iter().find(|c| c.name == "Work").unwrap().id;
    let start = from + 2 * 86_400_000 + 17 * 3_600_000;
    let id = engine
        .create_event(EventInput {
            calendar_id: work,
            summary: "Envelope demo".into(),
            description: Some("Created by the test".into()),
            location: Some("Online".into()),
            all_day: false,
            start,
            end: start + 3_600_000,
            start_date: None,
            end_date: None,
            timezone: None,
            rrule: None,
            attendees: vec![],
            alarms: vec![10],
            color: None,
            transparent: false,
            add_conference: false,
        })
        .await
        .unwrap();
    assert!(engine.occurrences(from, to).unwrap().iter().any(|o| o.title == "Envelope demo"));
    engine.move_event(id, None, EditScope::All, start + 3_600_000, start + 2 * 3_600_000).await.unwrap();
    let moved = engine.occurrences(from, to).unwrap().into_iter().find(|o| o.title == "Envelope demo").unwrap();
    assert_eq!(moved.start, start + 3_600_000);

    // Delete one occurrence of the standup series.
    let one = standups[1];
    engine.delete_event(one.event_id, one.recurrence_id.clone(), EditScope::This).await.unwrap();
    let after = engine.occurrences(from, to).unwrap();
    assert_eq!(after.iter().filter(|o| o.title == "Daily standup").count(), 4, "one occurrence removed");

    // A fresh engine (new database) sees the same server state.
    engine.delete_event(moved.event_id, None, EditScope::All).await.unwrap();
    engine.shutdown();
    let engine2 = Engine::start(Paths { data: dir.join("d2"), cache: dir.join("c2") }, Events::default()).unwrap();
    engine2
        .add_password_account(NewAccount {
            email: "alex@envelope.test".into(),
            name: "Alex".into(),
            provider: "generic".into(),
            password: "password".into(),
            config: AccountConfig {
                imap: Some(ServerConfig { host: "127.0.0.1".into(), port: 1993, security: Security::Tls, username: String::new() }),
                caldav_url: Some("http://127.0.0.1:5232/".into()),
                accept_invalid_certs: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let t1 = Instant::now();
    let mut n = 0;
    while t1.elapsed() < Duration::from_secs(20) {
        n = engine2.occurrences(from, to).unwrap().iter().filter(|o| o.title == "Daily standup").count();
        if n > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(n, 4, "EXDATE persisted on the server");
    assert!(!engine2.occurrences(from, to).unwrap().iter().any(|o| o.title == "Envelope demo"), "deleted on server");
    engine2.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
