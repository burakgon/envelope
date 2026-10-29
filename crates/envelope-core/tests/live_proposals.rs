//! Proposing a new time, end to end against the development servers: Sam
//! invites Alex from his CalDAV calendar (Radicale), Alex proposes another
//! time from the invitation in his inbox (an iCalendar `COUNTER` through the
//! SMTP sink), Sam accepts it — his event moves on the server and Alex gets
//! the update — and then declines Alex's second proposal (a
//! `DECLINECOUNTER`).
//!
//! The test works in a calendar it creates for Sam under a random name and
//! deletes again, and deletes the mail it sent (also when a step fails), so
//! the development data stays as it was.
//!
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_proposals -- --nocapture
//! (with scripts/devserver/run.sh running).

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use envelope_core::calendar::{EventInput, ProposalState};
use envelope_core::db::misc;
use envelope_core::envelope_ical::PartStat;
use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};
use envelope_dav::{DavClient, NewCalendar};
use futures::FutureExt;
use parking_lot::Mutex;

const DAV: &str = "http://127.0.0.1:5232/";
const SAM_HOME: &str = "http://127.0.0.1:5232/sam@envelope.test/";
const ALEX: &str = "alex@envelope.test";
const SAM: &str = "sam@envelope.test";

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);
impl EventSink for Events {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().push(e);
    }
}

/// Polls `check` until it yields a value (or fails after `secs` seconds).
async fn until<T, F, Fut>(what: &str, secs: u64, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(value) = check().await {
            println!("{what}: {:?}", start.elapsed());
            return value;
        }
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// An engine with one of the development accounts (sent mail is not kept in
/// Sent, and only a day's bodies are fetched ahead); with `caldav`, its
/// calendars too.
async fn account(dir: &Path, email: &str, name: &str, caldav: bool) -> Engine {
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Events::default(),
    )
    .unwrap();
    engine
        .add_password_account(NewAccount {
            email: email.into(),
            name: name.into(),
            provider: "generic".into(),
            password: "password".into(),
            config: AccountConfig {
                imap: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1993,
                    security: Security::Tls,
                    username: String::new(),
                }),
                smtp: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1587,
                    security: Security::None,
                    username: String::new(),
                }),
                caldav_url: caldav.then(|| DAV.into()),
                accept_invalid_certs: true,
                save_sent_copy: Some(false),
                body_sync_days: Some(1),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    engine
}

/// The inbox threads whose subject has `marker`.
fn marked(engine: &Engine, marker: &str) -> Vec<ThreadSummary> {
    engine
        .threads(&ThreadQuery {
            mailbox: Mailbox::Unified {
                role: FolderRole::Inbox,
            },
            search: None,
            filter: ListFilter::default(),
            sort: SortOrder::Newest,
            messages: false,
            offset: 0,
            limit: 200,
        })
        .map(|page| page.items)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.subject.contains(marker))
        .collect()
}

/// The inbox messages marked with `marker` that carry the iTIP `method`.
fn calendar_mail(engine: &Engine, marker: &str, method: &str) -> Vec<MessageView> {
    marked(engine, marker)
        .iter()
        .filter_map(|t| engine.thread(t.id).ok())
        .flat_map(|detail| detail.messages)
        .filter(|m| m.calendar_method.as_deref() == Some(method))
        .collect()
}

/// Deletes the test's mail from an account's inbox, on the server too, as
/// far as it gets in a while: what the account sent goes out first.
async fn clean_up(engine: &Engine, marker: &str) {
    let Some(account) = engine.accounts().ok().and_then(|a| a.first().map(|a| a.id)) else {
        return;
    };
    let start = Instant::now();
    let idle = |engine: &Engine| {
        engine.outbox().is_ok_and(|o| o.is_empty())
            && engine
                .db
                .read(|c| misc::queued_ops(c, account))
                .is_ok_and(|ops| ops.is_empty())
    };
    while !idle(engine) && start.elapsed() < Duration::from_secs(30) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let threads: Vec<i64> = marked(engine, marker).iter().map(|t| t.id).collect();
    if engine.delete_forever(&threads).is_err() {
        return;
    }
    while !idle(engine) && start.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn propose_accept_and_decline_new_times() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        eprintln!("skipping: ENVELOPE_TEST_IMAP not set");
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,hyper=info,reqwest=info,rustls=info,h2=info")
        .try_init();
    let run = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();

    // A calendar of Sam's for the test.
    let dav = DavClient::new(DAV, envelope_dav::Auth::basic(SAM, "password")).unwrap();
    let calendar_name = format!("Envelope proposals {run}");
    let calendar_href = dav
        .create_calendar(SAM_HOME, &NewCalendar::new(&calendar_name))
        .await
        .unwrap();

    let dir = std::env::temp_dir().join(format!("envelope-proposals-{}", uuid::Uuid::new_v4()));
    let sam = account(&dir.join("sam"), SAM, "Sam Rivera", true).await;
    let alex = account(&dir.join("alex"), ALEX, "Alex Morgan", false).await;
    let outcome = AssertUnwindSafe(scenario(&sam, &alex, &dav, &calendar_href, &run))
        .catch_unwind()
        .await;
    dav.delete_collection(&calendar_href).await.unwrap();
    clean_up(&alex, &run).await;
    clean_up(&sam, &run).await;
    alex.shutdown();
    sam.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure);
    }
}

/// Sam's invitation, Alex's proposals, and Sam's answers to them.
async fn scenario(sam: &Engine, alex: &Engine, dav: &DavClient, calendar_href: &str, run: &str) {
    let summary = format!("Proposal check {run}");
    let calendar_name = format!("Envelope proposals {run}");
    let at = |h: u32, m: u32| {
        Local
            .with_ymd_and_hms(2030, 3, 4, h, m, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    };

    // Sam invites Alex.
    let calendar = until("Sam's calendar", 60, || async {
        sam.calendars()
            .ok()?
            .into_iter()
            .find(|c| c.name == calendar_name)
    })
    .await;
    sam.create_event(EventInput {
        calendar_id: calendar.id,
        summary: summary.clone(),
        description: None,
        location: Some("Room 2".into()),
        all_day: false,
        start: at(10, 0),
        end: at(10, 30),
        start_date: None,
        end_date: None,
        timezone: None,
        rrule: None,
        attendees: vec![Address::new(Some("Alex Morgan".into()), ALEX)],
        alarms: vec![],
        color: None,
        transparent: false,
        add_conference: false,
    })
    .await
    .unwrap();

    // Alex proposes an hour later, answering "maybe".
    let invitation = until("the invitation reaches Alex", 90, || async {
        calendar_mail(alex, run, "REQUEST").into_iter().next()
    })
    .await;
    let info = alex.invite_info(invitation.id).await.unwrap();
    assert!(info.can_propose);
    assert_eq!(info.organizer.map(|o| o.email), Some(SAM.to_string()));
    assert_eq!((info.start, info.end), (at(10, 0), at(10, 30)));
    let answer = alex
        .propose_invite_time(
            invitation.id,
            PartStat::NeedsAction,
            at(11, 0),
            at(11, 30),
            Some("Could we meet an hour later?".into()),
        )
        .await
        .unwrap();
    assert_eq!(answer, PartStat::Tentative);
    // The invitation is on Alex's calendar, answered "maybe".
    let mine = alex.occurrences(at(0, 0), at(23, 59)).unwrap();
    let filed = mine.iter().find(|o| o.title == summary).unwrap();
    assert_eq!(filed.start, at(10, 0));
    assert_eq!(filed.my_status, Some(PartStat::Tentative));

    // Sam sees the proposal against his event, and accepts it.
    let counter = until("the proposal reaches Sam", 180, || async {
        calendar_mail(sam, run, "COUNTER").into_iter().next()
    })
    .await;
    let proposal = sam.proposal_info(counter.id).await.unwrap();
    assert_eq!(proposal.state, ProposalState::Open);
    assert_eq!(proposal.attendee.email, ALEX);
    assert_eq!(proposal.status, Some(PartStat::Tentative));
    assert_eq!((proposal.start, proposal.end), (at(11, 0), at(11, 30)));
    assert_eq!(
        (proposal.current_start, proposal.current_end),
        (Some(at(10, 0)), Some(at(10, 30)))
    );
    assert_eq!(
        proposal.comment.as_deref(),
        Some("Could we meet an hour later?")
    );
    sam.accept_proposal(counter.id).await.unwrap();
    assert_eq!(
        sam.proposal_info(counter.id).await.unwrap().state,
        ProposalState::Current
    );
    // The server has the event at the new time, one sequence on.
    let objects = dav.list_etags(calendar_href).await.unwrap();
    assert_eq!(objects.len(), 1);
    let stored = dav.get(&objects[0].href).await.unwrap();
    let event = &envelope_core::envelope_ical::parse_calendar(&stored.data)
        .unwrap()
        .events[0];
    assert_eq!(event.sequence, 1);
    assert_eq!(
        envelope_core::envelope_ical::to_utc(
            &event.start,
            &Default::default(),
            envelope_core::calendar::local_tz()
        )
        .timestamp_millis(),
        at(11, 0)
    );

    // Alex gets the update; a second proposal from it is declined.
    let update = until("the update reaches Alex", 90, || async {
        for message in calendar_mail(alex, run, "REQUEST") {
            if alex.invite_info(message.id).await.ok()?.start == at(11, 0) {
                return Some(message);
            }
        }
        None
    })
    .await;
    alex.propose_invite_time(update.id, PartStat::Tentative, at(12, 0), at(12, 30), None)
        .await
        .unwrap();
    let second = until("the second proposal reaches Sam", 90, || async {
        for message in calendar_mail(sam, run, "COUNTER") {
            if sam.proposal_info(message.id).await.ok()?.start == at(12, 0) {
                return Some(message);
            }
        }
        None
    })
    .await;
    assert_eq!(
        sam.proposal_info(second.id).await.unwrap().state,
        ProposalState::Open
    );
    sam.decline_proposal(second.id, Some("The room is taken at noon.".into()))
        .await
        .unwrap();
    let declined = until("the answer reaches Alex", 90, || async {
        calendar_mail(alex, run, "DECLINECOUNTER")
            .into_iter()
            .next()
    })
    .await;
    assert_eq!(declined.subject, format!("New time declined: {summary}"));
    // The event stays at the time accepted.
    let objects = dav.list_etags(calendar_href).await.unwrap();
    let stored = dav.get(&objects[0].href).await.unwrap();
    assert!(stored.data.contains("SEQUENCE:1"));
}
