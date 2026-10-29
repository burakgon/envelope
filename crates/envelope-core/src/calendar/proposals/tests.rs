//! Proposals for a new time in one engine with a real (temporary) database:
//! an attendee's proposal e-mailed as a `COUNTER` (their answer kept on
//! their calendar), invitations that take none, and the organizer's side —
//! proposals accepted on a CalDAV calendar (a mock server), which moves the
//! event or the occurrence and sends the update to every attendee, one
//! declined with a `DECLINECOUNTER`, and proposals for events that changed,
//! are gone or are someone else's.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, TimeZone, Utc};
use envelope_ical::{
    Attendee, AttendeeRole, Event, EventTime, ItipMessage, ItipMethod, Organizer, PartStat,
};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::ProposalState;
use crate::db::calendar::{self as cdb, CalendarUpsert};
use crate::db::mail::{self, FolderUpsert};
use crate::db::misc;
use crate::engine::{Engine, EventSink, Paths};
use crate::error::Error;
use crate::model::*;

struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _: EngineEvent) {}
}

struct Fixture {
    engine: Engine,
    dir: PathBuf,
    account: i64,
    inbox: i64,
    next_uid: AtomicU32,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A queued message: who it goes to, its subject and its iCalendar part.
struct Sent {
    to: Vec<String>,
    subject: String,
    method: String,
    itip: ItipMessage,
}

const ALEX: &str = "alex@example.com";
const MAYA: &str = "maya@example.com";

fn utc(d: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, d, h, m, 0).single().unwrap()
}

fn at(d: u32, h: u32, m: u32) -> EventTime {
    EventTime::Utc { at: utc(d, h, m) }
}

fn ms(d: u32, h: u32, m: u32) -> i64 {
    utc(d, h, m).timestamp_millis()
}

fn person(email: &str, name: &str, partstat: PartStat) -> Attendee {
    Attendee {
        email: email.into(),
        name: Some(name.into()),
        partstat,
        role: AttendeeRole::Required,
        rsvp: partstat == PartStat::NeedsAction,
        ..Attendee::default()
    }
}

/// A meeting organized by `organizer`, with Alex, Maya and Sofia.
fn meeting(uid: &str, organizer: (&str, &str), start: EventTime, end: EventTime) -> Event {
    let mut event = Event::new(uid, start, end);
    event.summary = "Design review".into();
    event.location = Some("Figtree board".into());
    event.organizer = Some(Organizer {
        email: organizer.0.into(),
        name: Some(organizer.1.into()),
    });
    event.attendees = vec![
        person(organizer.0, organizer.1, PartStat::Accepted),
        person(ALEX, "Alex Morgan", PartStat::NeedsAction),
        person(MAYA, "Maya Chen", PartStat::NeedsAction),
        person("sofia@example.com", "Sofia Martínez", PartStat::Accepted),
    ];
    event.attendees.dedup_by(|a, b| a.email == b.email);
    event
}

/// Maya's proposal to move `event` to `start`–`end`, as her mail program
/// writes it from the invitation it got.
fn counter(event: &Event, start: EventTime, end: EventTime) -> String {
    let invitation = envelope_ical::build_request(std::slice::from_ref(event)).unwrap();
    envelope_ical::build_counter(
        &invitation,
        MAYA,
        PartStat::Tentative,
        &start,
        &end,
        Some("Could we do it an hour later?"),
    )
    .unwrap()
}

impl Fixture {
    /// An IMAP account for Alex; with `caldav`, its calendars are on that
    /// server.
    fn new(caldav: Option<&str>) -> Fixture {
        let dir = std::env::temp_dir().join(format!("envelope-proposals-{}", uuid::Uuid::new_v4()));
        let engine = Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            NoEvents,
        )
        .unwrap();
        let (account, inbox) = engine
            .db
            .write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    ALEX,
                    "Alex Morgan",
                    "#000",
                    &AccountConfig {
                        caldav_url: caldav.map(str::to_string),
                        ..AccountConfig::default()
                    },
                )?;
                let folders = mail::sync_folders(
                    tx,
                    account,
                    &[FolderUpsert {
                        remote_id: "INBOX".into(),
                        name: "INBOX".into(),
                        path: "INBOX".into(),
                        parent_remote_id: None,
                        role: Some(FolderRole::Inbox),
                        kind: "folder".into(),
                        delimiter: None,
                        selectable: true,
                        sort_order: 0,
                        color: None,
                    }],
                )?;
                Ok((account, folders["INBOX"]))
            })
            .unwrap();
        engine
            .secrets
            .preload(&crate::secrets::password_key(account), "secret");
        Fixture {
            engine,
            dir,
            account,
            inbox,
            next_uid: AtomicU32::new(1),
        }
    }

    /// A calendar of this computer.
    fn local_calendar(&self) -> i64 {
        self.engine
            .db
            .write(|tx| cdb::create_local_calendar(tx, "Personal", "#6d5efc"))
            .unwrap()
    }

    /// The account's calendar on the CalDAV server at `collection`.
    fn caldav_calendar(&self, collection: &str) -> i64 {
        let account = self.account;
        self.engine
            .db
            .write(|tx| {
                cdb::sync_calendars(
                    tx,
                    account,
                    "caldav",
                    &[CalendarUpsert {
                        remote_id: collection.into(),
                        name: "Work".into(),
                        color: "#6d5efc".into(),
                        read_only: false,
                        is_default: true,
                        timezone: None,
                        sort_order: 0,
                        visible_default: true,
                    }],
                )
                .map(|_| ())
            })
            .unwrap();
        self.engine
            .db
            .read(|c| cdb::account_calendars(c, account))
            .unwrap()[0]
            .id
    }

    /// Puts `event` on a calendar as its sync would; returns its row.
    fn put(&self, calendar: i64, event: &Event, href: Option<&str>) -> i64 {
        let raw = envelope_ical::create_calendar(event).unwrap();
        self.engine
            .store_event(
                calendar,
                event,
                href,
                Some("\"1\""),
                Some(&raw),
                None,
                false,
            )
            .unwrap()
    }

    /// Stores a message from `from` carrying `ics` (an iTIP `method`) in the
    /// inbox as sync would, its invitation downloaded.
    fn deliver(&self, from: &str, subject: &str, method: &str, ics: &str) -> i64 {
        let uid = self.next_uid.fetch_add(1, Ordering::Relaxed);
        let raw = format!(
            "From: {from}\r\nTo: Alex Morgan <{ALEX}>\r\nSubject: {subject}\r\n\
             Message-ID: <{uid}.proposals@example.com>\r\nDate: Fri, 30 Oct 2026 09:00:00 +0000\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"part\"\r\n\r\n\
             --part\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{subject}\r\n\
             --part\r\nContent-Type: text/calendar; charset=utf-8; method={method}\r\n\r\n{ics}\r\n\
             --part--\r\n"
        );
        let (account, inbox) = (self.account, self.inbox);
        let id = self
            .engine
            .db
            .write(|tx| {
                let (meta, body) = crate::mime::parse_full(raw.as_bytes()).unwrap();
                let stored = mail::upsert_message(tx, account, inbox, Some(uid), &meta)?;
                mail::store_body(
                    tx,
                    stored.message_id,
                    body.html.as_deref(),
                    body.text.as_deref(),
                    &body.snippet,
                    Some(&body.attachments),
                )?;
                mail::refresh_threads(tx, &BTreeSet::from([stored.thread_id]))?;
                Ok(stored.message_id)
            })
            .unwrap();
        let message = self.engine.db.read(|c| mail::get_message(c, id)).unwrap();
        assert_eq!(message.calendar_method.as_deref(), Some(method));
        let invite = message
            .attachments
            .iter()
            .find(|a| a.mime_type == "text/calendar")
            .expect("the invitation is kept");
        // Where a download would leave it.
        let file = self
            .dir
            .join("cache")
            .join(format!("account-{account}"))
            .join(format!("att-{}", invite.id));
        std::fs::create_dir_all(&file).unwrap();
        std::fs::write(file.join(&invite.filename), ics).unwrap();
        id
    }

    /// The mail queued to go out, oldest first.
    fn sent(&self) -> Vec<Sent> {
        let items = self.engine.db.read(misc::outbox_list).unwrap();
        items
            .iter()
            .map(|item| {
                let row = self
                    .engine
                    .db
                    .read(|c| misc::outbox_get(c, item.id))
                    .unwrap()
                    .unwrap();
                let (_, body) = crate::mime::parse_full(&row.mime).unwrap();
                let (method, ics) = body.calendar.expect("an iCalendar part");
                Sent {
                    to: row.meta.to.iter().map(|a| a.email.clone()).collect(),
                    subject: row.meta.subject.clone(),
                    method,
                    itip: envelope_ical::parse_itip(&ics).unwrap(),
                }
            })
            .collect()
    }

    fn rows(&self, uid: &str) -> Vec<cdb::EventRow> {
        self.engine.db.read(|c| cdb::rows_by_uid(c, uid)).unwrap()
    }
}

fn partstat_of(event: &Event, email: &str) -> Option<PartStat> {
    event
        .attendees
        .iter()
        .find(|a| a.email == email)
        .map(|a| a.partstat)
}

fn start_ms(itip: &ItipMessage) -> (i64, i64) {
    let event = &itip.events[0];
    let ms = |t: &EventTime| {
        envelope_ical::to_utc(t, &itip.tz, crate::calendar::local_tz()).timestamp_millis()
    };
    (ms(&event.start), ms(&event.end))
}

#[tokio::test]
async fn a_proposal_goes_to_the_organizer_with_the_answer() {
    let f = Fixture::new(None);
    let calendar = f.local_calendar();
    let mut review = meeting(
        "review@example.com",
        ("priya@example.com", "Priya Raman"),
        at(5, 10, 0),
        at(5, 10, 45),
    );
    review.summary = "Roadmap review".into();
    review.sequence = 2;
    let invitation = envelope_ical::build_request(std::slice::from_ref(&review)).unwrap();
    let message = f.deliver(
        "Priya Raman <priya@example.com>",
        "Invitation: Roadmap review",
        "REQUEST",
        &invitation,
    );
    let info = f.engine.invite_info(message).await.unwrap();
    assert!(info.can_propose);
    assert_eq!(info.my_status, Some(PartStat::NeedsAction));

    // Any answer but a decline becomes "maybe".
    let answer = f
        .engine
        .propose_invite_time(
            message,
            PartStat::NeedsAction,
            ms(5, 11, 0),
            ms(5, 11, 45),
            Some("  Could we meet an hour later?  ".into()),
        )
        .await
        .unwrap();
    assert_eq!(answer, PartStat::Tentative);
    let sent = f.sent();
    assert_eq!(sent.len(), 1, "one message, no separate reply");
    let proposal = &sent[0];
    assert_eq!(proposal.to, ["priya@example.com"]);
    assert_eq!(proposal.subject, "New time proposed: Roadmap review");
    assert_eq!(proposal.method, "COUNTER");
    assert_eq!(proposal.itip.method, Some(ItipMethod::Counter));
    let event = &proposal.itip.events[0];
    assert_eq!(event.uid, "review@example.com");
    assert_eq!(event.sequence, 2);
    assert_eq!(start_ms(&proposal.itip), (ms(5, 11, 0), ms(5, 11, 45)));
    assert_eq!(event.attendees.len(), 1);
    assert_eq!(event.attendees[0].email, ALEX);
    assert_eq!(event.attendees[0].partstat, PartStat::Tentative);
    assert_eq!(
        proposal.itip.comment().as_deref(),
        Some("Could we meet an hour later?")
    );
    // The invitation went on the calendar, answered "maybe", at its time.
    let rows = f.rows("review@example.com");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].calendar_id, calendar);
    assert_eq!(rows[0].start_ts, ms(5, 10, 0));
    assert_eq!(partstat_of(&rows[0].event, ALEX), Some(PartStat::Tentative));

    // A decline stays one, on the copy there, and still no reply goes out.
    let answer = f
        .engine
        .propose_invite_time(message, PartStat::Declined, ms(6, 9, 0), ms(6, 9, 45), None)
        .await
        .unwrap();
    assert_eq!(answer, PartStat::Declined);
    let sent = f.sent();
    assert_eq!(
        sent.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(),
        ["COUNTER", "COUNTER"]
    );
    let declined = &sent[1].itip;
    assert_eq!(declined.events[0].attendees[0].partstat, PartStat::Declined);
    assert_eq!(start_ms(declined), (ms(6, 9, 0), ms(6, 9, 45)));
    assert_eq!(declined.comment(), None);
    let rows = f.rows("review@example.com");
    assert_eq!(rows.len(), 1);
    assert_eq!(partstat_of(&rows[0].event, ALEX), Some(PartStat::Declined));

    // A time that ends before it starts is refused.
    assert!(matches!(
        f.engine
            .propose_invite_time(message, PartStat::Tentative, ms(6, 9, 0), ms(6, 8, 0), None)
            .await,
        Err(Error::Invalid(_))
    ));
}

#[tokio::test]
async fn proposals_are_for_single_events_and_occurrences_of_others() {
    let f = Fixture::new(None);
    f.local_calendar();
    let invite = |event: &Event, extra: &str| {
        let mut ics = envelope_ical::build_request(std::slice::from_ref(event)).unwrap();
        if !extra.is_empty() {
            ics = ics.replace("END:VEVENT", &format!("{extra}\r\nEND:VEVENT"));
        }
        f.deliver(
            "Priya Raman <priya@example.com>",
            "Invitation",
            "REQUEST",
            &ics,
        )
    };
    let priya = ("priya@example.com", "Priya Raman");
    let single = meeting("single@example.com", priya, at(5, 10, 0), at(5, 11, 0));
    let mut series = meeting("series@example.com", priya, at(2, 9, 0), at(2, 9, 15));
    series.rrule = Some("FREQ=WEEKLY;COUNT=4".into());
    let mut occurrence = meeting("series@example.com", priya, at(9, 10, 0), at(9, 10, 15));
    occurrence.recurrence_id = Some(at(9, 9, 0));
    let mut all_day = meeting(
        "day@example.com",
        priya,
        EventTime::Date {
            date: utc(5, 0, 0).date_naive(),
        },
        EventTime::Date {
            date: utc(6, 0, 0).date_naive(),
        },
    );
    all_day.summary = "Offsite".into();
    let mine = meeting(
        "mine@example.com",
        (ALEX, "Alex Morgan"),
        at(5, 10, 0),
        at(5, 11, 0),
    );

    let cases = [
        ("a single event", invite(&single, ""), true),
        ("one occurrence", invite(&occurrence, ""), true),
        ("a whole series", invite(&series, ""), false),
        ("an all-day event", invite(&all_day, ""), false),
        ("the user's own meeting", invite(&mine, ""), false),
        (
            "an organizer who asked for none",
            invite(&single, "X-MICROSOFT-DISALLOW-COUNTER:TRUE"),
            false,
        ),
    ];
    for (what, message, takes) in cases {
        let info = f.engine.invite_info(message).await.unwrap();
        assert_eq!(info.can_propose, takes, "{what}");
        if !takes {
            assert!(
                f.engine
                    .propose_invite_time(
                        message,
                        PartStat::Tentative,
                        ms(7, 9, 0),
                        ms(7, 10, 0),
                        None
                    )
                    .await
                    .is_err(),
                "{what}"
            );
        }
    }
    assert!(f.sent().is_empty());

    // The occurrence's proposal says which occurrence it is for.
    let message = invite(&occurrence, "");
    f.engine
        .propose_invite_time(
            message,
            PartStat::Tentative,
            ms(9, 11, 0),
            ms(9, 11, 15),
            None,
        )
        .await
        .unwrap();
    let sent = f.sent();
    assert_eq!(sent.len(), 1);
    let counter = &sent[0].itip.events[0];
    assert_eq!(counter.recurrence_id, Some(at(9, 9, 0)));
    assert_eq!(counter.rrule, None);
}

/// A mock CalDAV server taking every upload, with the bodies it got.
async fn caldav_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"2\""))
        .mount(&server)
        .await;
    server
}

async fn uploads(server: &MockServer) -> Vec<(String, String)> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r: &&Request| r.method.as_str() == "PUT")
        .map(|r| {
            (
                r.url.path().to_string(),
                String::from_utf8_lossy(&r.body).into_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_proposal_files_the_invitation_on_caldav_without_inviting_anyone() {
    let server = caldav_server().await;
    let f = Fixture::new(Some(&server.uri()));
    let collection = format!("{}/alex/work/", server.uri());
    let calendar = f.caldav_calendar(&collection);
    let review = meeting(
        "review@example.com",
        ("priya@example.com", "Priya Raman"),
        at(5, 10, 0),
        at(5, 10, 45),
    );
    let message = f.deliver(
        "Priya Raman <priya@example.com>",
        "Invitation: Design review",
        "REQUEST",
        &envelope_ical::build_request(std::slice::from_ref(&review)).unwrap(),
    );
    f.engine
        .propose_invite_time(
            message,
            PartStat::Tentative,
            ms(5, 11, 0),
            ms(5, 11, 45),
            None,
        )
        .await
        .unwrap();
    // On the server, at its time, answered "maybe"…
    let put = uploads(&server).await;
    assert_eq!(put.len(), 1);
    let filed = envelope_ical::parse_calendar(&put[0].1).unwrap();
    assert_eq!(crate::calendar::to_ms(&filed.events[0].start), ms(5, 10, 0));
    assert_eq!(
        partstat_of(&filed.events[0], ALEX),
        Some(PartStat::Tentative)
    );
    assert_eq!(f.rows("review@example.com")[0].calendar_id, calendar);
    // …and Priya's meeting isn't sent to the others as Alex's invitation.
    let sent = f.sent();
    assert_eq!(
        sent.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(),
        ["COUNTER"]
    );
}

#[tokio::test]
async fn accepting_a_proposal_moves_the_event_and_updates_every_attendee() {
    let server = caldav_server().await;
    let f = Fixture::new(Some(&server.uri()));
    let collection = format!("{}/alex/work/", server.uri());
    let calendar = f.caldav_calendar(&collection);
    let href = format!("{collection}design.ics");
    let review = meeting(
        "design@example.com",
        (ALEX, "Alex Morgan"),
        at(4, 12, 0),
        at(4, 12, 30),
    );
    let row = f.put(calendar, &review, Some(&href));
    let message = f.deliver(
        "Maya Chen <maya@example.com>",
        "New time proposed: Design review",
        "COUNTER",
        &counter(&review, at(4, 13, 0), at(4, 13, 30)),
    );

    let info = f.engine.proposal_info(message).await.unwrap();
    assert_eq!(info.state, ProposalState::Open);
    assert_eq!(info.summary, "Design review");
    assert_eq!(info.attendee.email, MAYA);
    assert_eq!(info.attendee.name.as_deref(), Some("Maya Chen"));
    assert_eq!(info.status, Some(PartStat::Tentative));
    assert_eq!((info.start, info.end), (ms(4, 13, 0), ms(4, 13, 30)));
    assert_eq!(
        (info.current_start, info.current_end),
        (Some(ms(4, 12, 0)), Some(ms(4, 12, 30)))
    );
    assert_eq!(
        info.comment.as_deref(),
        Some("Could we do it an hour later?")
    );
    assert_eq!(info.event_id, Some(row));

    f.engine.accept_proposal(message).await.unwrap();
    // The server has the event at the new time, one sequence on.
    let put = uploads(&server).await;
    assert_eq!(put.len(), 1);
    assert_eq!(put[0].0, "/alex/work/design.ics");
    let uploaded = envelope_ical::parse_calendar(&put[0].1).unwrap();
    assert_eq!(uploaded.events[0].sequence, 1);
    assert_eq!(
        crate::calendar::to_ms(&uploaded.events[0].start),
        ms(4, 13, 0)
    );
    // Every attendee but the organizer gets the update.
    let sent = f.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "REQUEST");
    let mut to = sent[0].to.clone();
    to.sort();
    assert_eq!(to, [MAYA, "sofia@example.com"]);
    let update = &sent[0].itip.events[0];
    assert_eq!(update.uid, "design@example.com");
    assert_eq!(update.sequence, 1);
    assert_eq!(start_ms(&sent[0].itip), (ms(4, 13, 0), ms(4, 13, 30)));
    let moved = f.engine.db.read(|c| cdb::get_event(c, row)).unwrap();
    assert_eq!(
        (moved.start_ts, moved.end_ts),
        (ms(4, 13, 0), ms(4, 13, 30))
    );
    // The proposal is the event's time now; it can't be accepted again.
    let info = f.engine.proposal_info(message).await.unwrap();
    assert_eq!(info.state, ProposalState::Current);
    assert!(f.engine.accept_proposal(message).await.is_err());
    assert!(f.engine.decline_proposal(message, None).await.is_err());
    assert_eq!(f.sent().len(), 1);
}

#[tokio::test]
async fn accepting_a_proposal_for_one_occurrence_moves_only_it() {
    let server = caldav_server().await;
    let f = Fixture::new(Some(&server.uri()));
    let collection = format!("{}/alex/work/", server.uri());
    let calendar = f.caldav_calendar(&collection);
    // Mondays at 09:00 UTC, four times; Maya proposes 10:00 for the second.
    let mut standup = meeting(
        "standup@example.com",
        (ALEX, "Alex Morgan"),
        at(2, 9, 0),
        at(2, 9, 15),
    );
    standup.summary = "Standup".into();
    standup.rrule = Some("FREQ=WEEKLY;COUNT=4".into());
    standup.sequence = 3;
    let series = f.put(
        calendar,
        &standup,
        Some(&format!("{collection}standup.ics")),
    );
    let mut second = standup.clone();
    second.rrule = None;
    second.recurrence_id = Some(at(9, 9, 0));
    second.start = at(9, 9, 0);
    second.end = at(9, 9, 15);
    let message = f.deliver(
        "Maya Chen <maya@example.com>",
        "New time proposed: Standup",
        "COUNTER",
        &counter(&second, at(9, 10, 0), at(9, 10, 15)),
    );
    let info = f.engine.proposal_info(message).await.unwrap();
    assert_eq!(info.state, ProposalState::Open);
    assert_eq!(info.event_id, Some(series));
    assert_eq!(
        (info.current_start, info.current_end),
        (Some(ms(9, 9, 0)), Some(ms(9, 9, 15)))
    );

    f.engine.accept_proposal(message).await.unwrap();
    let sent = f.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "REQUEST");
    let update = &sent[0].itip.events;
    assert_eq!(update.len(), 1, "only the occurrence");
    assert_eq!(
        update[0].recurrence_id.as_ref().map(crate::calendar::to_ms),
        Some(ms(9, 9, 0))
    );
    assert_eq!(update[0].sequence, 4);
    assert_eq!(start_ms(&sent[0].itip), (ms(9, 10, 0), ms(9, 10, 15)));
    // The series keeps its other occurrences; the second has a row of its own.
    let rows = f.rows("standup@example.com");
    assert_eq!(rows.len(), 2);
    let moved = rows.iter().find(|r| r.recurrence_id.is_some()).unwrap();
    assert_eq!(moved.start_ts, ms(9, 10, 0));
    let master = rows.iter().find(|r| r.recurrence_id.is_none()).unwrap();
    assert_eq!(master.event.rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=4"));
    assert_eq!(master.start_ts, ms(2, 9, 0));
    assert_eq!(
        f.engine.proposal_info(message).await.unwrap().state,
        ProposalState::Current
    );
}

#[tokio::test]
async fn declining_a_proposal_tells_the_attendee() {
    let f = Fixture::new(None);
    let calendar = f.local_calendar();
    let review = meeting(
        "design@example.com",
        (ALEX, "Alex Morgan"),
        at(4, 12, 0),
        at(4, 12, 30),
    );
    let row = f.put(calendar, &review, None);
    let message = f.deliver(
        "Maya Chen <maya@example.com>",
        "New time proposed: Design review",
        "COUNTER",
        &counter(&review, at(4, 13, 0), at(4, 13, 30)),
    );
    f.engine
        .decline_proposal(message, Some(" The others can't move. ".into()))
        .await
        .unwrap();
    let sent = f.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, [MAYA]);
    assert_eq!(sent[0].subject, "New time declined: Design review");
    assert_eq!(sent[0].method, "DECLINECOUNTER");
    let answer = &sent[0].itip;
    assert_eq!(answer.method, Some(ItipMethod::DeclineCounter));
    assert_eq!(answer.events[0].uid, "design@example.com");
    assert_eq!(answer.events[0].sequence, 0);
    assert_eq!(answer.events[0].attendees.len(), 1);
    assert_eq!(answer.events[0].attendees[0].email, MAYA);
    // It stays at its time.
    assert_eq!(start_ms(answer), (ms(4, 12, 0), ms(4, 12, 30)));
    assert_eq!(answer.comment().as_deref(), Some("The others can't move."));
    let kept = f.engine.db.read(|c| cdb::get_event(c, row)).unwrap();
    assert_eq!((kept.start_ts, kept.event.sequence), (ms(4, 12, 0), 0));
}

#[tokio::test]
async fn proposals_for_changed_vanished_or_foreign_events_say_so() {
    let f = Fixture::new(None);
    let calendar = f.local_calendar();
    let propose = |event: &Event| {
        f.deliver(
            "Maya Chen <maya@example.com>",
            "New time proposed",
            "COUNTER",
            &counter(event, at(4, 13, 0), at(4, 13, 30)),
        )
    };
    // Moved by the organizer after Maya proposed: a newer sequence.
    let mut changed = meeting(
        "changed@example.com",
        (ALEX, "Alex Morgan"),
        at(4, 12, 0),
        at(4, 12, 30),
    );
    changed.sequence = 1;
    let outdated = propose(&changed);
    changed.sequence = 2;
    changed.start = at(4, 15, 0);
    changed.end = at(4, 15, 30);
    f.put(calendar, &changed, None);
    // Not on the calendar (any more).
    let gone = propose(&meeting(
        "gone@example.com",
        (ALEX, "Alex Morgan"),
        at(4, 12, 0),
        at(4, 12, 30),
    ));
    // Priya's meeting, on Alex's calendar.
    let theirs = meeting(
        "theirs@example.com",
        ("priya@example.com", "Priya Raman"),
        at(4, 12, 0),
        at(4, 12, 30),
    );
    f.put(calendar, &theirs, None);
    let foreign = propose(&theirs);

    for (message, state) in [
        (outdated, ProposalState::Outdated),
        (gone, ProposalState::Gone),
        (foreign, ProposalState::ReadOnly),
    ] {
        let info = f.engine.proposal_info(message).await.unwrap();
        assert_eq!(info.state, state);
        assert!(f.engine.accept_proposal(message).await.is_err());
        assert!(f.engine.decline_proposal(message, None).await.is_err());
    }
    let gone_info = f.engine.proposal_info(gone).await.unwrap();
    assert_eq!((gone_info.current_start, gone_info.event_id), (None, None));
    assert!(f.sent().is_empty());
    // Nothing moved.
    let row = &f.rows("changed@example.com")[0];
    assert_eq!(row.start_ts, ms(4, 15, 0));
    // An invitation is no proposal.
    let invitation = f.deliver(
        "Priya Raman <priya@example.com>",
        "Invitation",
        "REQUEST",
        &envelope_ical::build_request(std::slice::from_ref(&theirs)).unwrap(),
    );
    assert!(matches!(
        f.engine.proposal_info(invitation).await,
        Err(Error::Invalid(_))
    ));
}
