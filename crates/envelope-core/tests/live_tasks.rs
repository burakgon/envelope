//! CalDAV tasks end to end against the development server (Radicale): the
//! first sync of a list another client wrote (subtasks, a repeating task,
//! alarms and properties Envelope does not show), editing, completing,
//! creating and deleting tasks here, changes made on the server, and
//! creating, renaming and deleting lists.
//!
//! The test works in collections it creates under a random name and removes
//! them again, so it leaves the development data alone.
//!
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_tasks -- --nocapture
//! (with scripts/devserver/run.sh running).

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use envelope_core::db::mail;
use envelope_core::model::*;
use envelope_core::tasks::{Task, TaskInput, TaskView};
use envelope_core::{Engine, EventSink, Paths};
use envelope_dav::{ComponentSet, DavClient, DavError, NewCalendar, ObjectKind, Precondition};
use parking_lot::Mutex;

const DAV: &str = "http://127.0.0.1:5232/";
const HOME: &str = "http://127.0.0.1:5232/alex@envelope.test/";

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);
impl EventSink for Events {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().push(e);
    }
}

/// Polls `check` until it yields a value (or fails after 20 seconds).
async fn until<T, F, Fut>(what: &str, mut check: F) -> T
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
            start.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn vtodo(body: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example Corp.//Tasks 3.1//EN\r\nBEGIN:VTODO\r\n\
         DTSTAMP:20240101T000000Z\r\n{body}END:VTODO\r\nEND:VCALENDAR\r\n"
    )
}

async fn open_tasks(engine: &Engine, list: i64) -> Vec<Task> {
    engine
        .tasks_query(TaskView::List(list), false)
        .await
        .unwrap_or_default()
}

/// Every task of a list (subtasks too) is on the server.
async fn all_saved(engine: &Engine, list: i64) -> bool {
    let tasks = engine
        .tasks_query(TaskView::List(list), true)
        .await
        .unwrap_or_default();
    for task in &tasks {
        if task.pending {
            return false;
        }
        let subtasks = engine.task_subtasks(task.id).await.unwrap_or_default();
        if subtasks.iter().any(|t| t.pending) {
            return false;
        }
    }
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caldav_tasks_round_trip() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,envelope_core=debug,hyper=info,reqwest=info,rustls=info,h2=info")
        .try_init();
    let run = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();

    // A list written by another client.
    let dav = DavClient::new(
        DAV,
        envelope_dav::Auth::basic("alex@envelope.test", "password"),
    )
    .unwrap();
    let list_name = format!("Envelope test {run}");
    let list_href = dav
        .create_calendar(
            HOME,
            &NewCalendar::new(&list_name).with_components(ComponentSet::TASKS),
        )
        .await
        .unwrap();
    let put = |uid: String, data: String| {
        let href = envelope_dav::object_href(&list_href, &uid, ObjectKind::Calendar).unwrap();
        let dav = dav.clone();
        async move {
            dav.put(&href, &data, Precondition::IfNoneMatch)
                .await
                .unwrap();
            href
        }
    };
    let parent_uid = format!("trip-{run}");
    let parent = put(
        parent_uid.clone(),
        vtodo(&format!(
            "UID:{parent_uid}\r\nSUMMARY:Plan the trip\r\nDUE;VALUE=DATE:20300601\r\nPRIORITY:1\r\n\
             CATEGORIES:Travel\r\nX-EXAMPLE-COLOR:teal\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\n\
             TRIGGER;RELATED=END:-PT30M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\n"
        )),
    )
    .await;
    let child = put(
        format!("flights-{run}"),
        vtodo(&format!(
            "UID:flights-{run}\r\nSUMMARY:Book flights\r\nRELATED-TO;RELTYPE=PARENT:{parent_uid}\r\n\
             STATUS:NEEDS-ACTION\r\n"
        )),
    )
    .await;
    let weekly = put(
        format!("report-{run}"),
        vtodo(&format!(
            "UID:report-{run}\r\nSUMMARY:Weekly report\r\nRRULE:FREQ=WEEKLY;COUNT=2\r\n\
             DTSTART;TZID=Europe/Istanbul:20300301T090000\r\nDUE;TZID=Europe/Istanbul:20300301T170000\r\n"
        )),
    )
    .await;

    // An account with the development server's CalDAV, added without the
    // mail side.
    let dir = std::env::temp_dir().join(format!("envelope-tasks-{}", uuid::Uuid::new_v4()));
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Events::default(),
    )
    .unwrap();
    let account_id = engine
        .db
        .write(|tx| {
            mail::insert_account(
                tx,
                AccountKind::Imap,
                "generic",
                "alex@envelope.test",
                "Alex Morgan",
                "#0ea5e9",
                &AccountConfig {
                    caldav_url: Some(DAV.into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    engine
        .secrets
        .set(
            &envelope_core::secrets::password_key(account_id),
            "password",
        )
        .await
        .unwrap();
    engine.sync_now(None);

    let list = until("first sync", || async {
        let list = engine
            .task_lists()
            .ok()?
            .into_iter()
            .find(|l| l.name == list_name)?;
        (open_tasks(&engine, list.id).await.len() == 2).then_some(list)
    })
    .await;
    assert_eq!(list.account_id, Some(account_id));
    assert!(list.due_times && list.importance && list.subtasks);
    let tasks = open_tasks(&engine, list.id).await;
    let trip = tasks.iter().find(|t| t.title == "Plan the trip").unwrap();
    assert!(trip.important);
    assert_eq!(trip.due_date.as_deref(), Some("2030-06-01"));
    assert_eq!(trip.subtask_count, 1);
    let report = tasks.iter().find(|t| t.title == "Weekly report").unwrap();
    assert!(report.repeats);
    let flights = engine.task_subtasks(trip.id).await.unwrap().remove(0);
    assert_eq!(flights.title, "Book flights");

    // Changes here: renamed, completed, a repeating task moved on, new ones.
    engine
        .update_task(
            trip.id,
            TaskInput {
                list_id: list.id,
                title: "Plan the summer trip".into(),
                notes: Some("Somewhere warm".into()),
                ..TaskInput::default()
            },
        )
        .await
        .unwrap();
    engine.complete_task(flights.id, true).await.unwrap();
    let moved = engine.complete_task(report.id, true).await.unwrap();
    assert_eq!(moved.completed_at, None);
    assert_eq!(moved.due_at, report.due_at.map(|at| at + 7 * 86_400_000));
    let created = engine
        .create_task(TaskInput {
            list_id: list.id,
            title: "Pack".into(),
            parent_id: Some(Some(trip.id)),
            ..TaskInput::default()
        })
        .await
        .unwrap();
    assert!(created.pending);
    until("uploads", || async {
        all_saved(&engine, list.id).await.then_some(())
    })
    .await;

    let parent_text = dav.get(&parent).await.unwrap().data;
    assert!(
        parent_text.contains("SUMMARY:Plan the summer trip"),
        "{parent_text}"
    );
    assert!(parent_text.contains("DESCRIPTION:Somewhere warm"));
    assert!(parent_text.contains("CATEGORIES:Travel"));
    assert!(parent_text.contains("X-EXAMPLE-COLOR:teal"));
    assert!(parent_text.contains("TRIGGER;RELATED=END:-PT30M"));
    let child_text = dav.get(&child).await.unwrap().data;
    assert!(child_text.contains("STATUS:COMPLETED"), "{child_text}");
    let weekly_text = dav.get(&weekly).await.unwrap().data;
    assert!(
        weekly_text.contains("DUE;TZID=Europe/Istanbul:20300308T170000"),
        "{weekly_text}"
    );
    assert!(weekly_text.contains("COUNT=1"));
    let pack = dav
        .get(
            &envelope_dav::object_href(
                &list_href,
                &engine
                    .db
                    .read(|c| envelope_core::db::tasks::get_task(c, created.id))
                    .unwrap()
                    .uid,
                ObjectKind::Calendar,
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .data;
    assert!(pack.contains("SUMMARY:Pack"), "{pack}");
    assert!(pack.contains(&format!("RELATED-TO;RELTYPE=PARENT:{parent_uid}")));

    // Changes made by another client come in.
    let server = dav.get(&parent).await.unwrap();
    dav.put(
        &parent,
        &server
            .data
            .replace("Plan the summer trip", "Plan the trip to Rome"),
        Precondition::IfMatch(server.etag.unwrap()),
    )
    .await
    .unwrap();
    dav.delete(&child, None).await.unwrap();
    engine.sync_now(None);
    until("server changes", || async {
        let tasks = open_tasks(&engine, list.id).await;
        let trip = tasks.iter().find(|t| t.id == trip.id)?;
        let kids = engine.task_subtasks(trip.id).await.ok()?;
        (trip.title == "Plan the trip to Rome" && kids.len() == 1).then_some(())
    })
    .await;

    // Deleting here deletes there.
    engine.delete_task(report.id).await.unwrap();
    until("deletion", || async {
        matches!(dav.get(&weekly).await, Err(DavError::NotFound { .. })).then_some(())
    })
    .await;

    // Lists.
    let made = engine
        .create_task_list(Some(account_id), &format!("Errands {run}"))
        .await
        .unwrap();
    assert_eq!(made.account_id, Some(account_id));
    let made_href = engine
        .db
        .read(|c| envelope_core::db::tasks::get_list(c, made.id))
        .unwrap()
        .remote_id;
    assert!(dav.get_calendar(&made_href).await.unwrap().components.vtodo);
    engine
        .rename_task_list(made.id, &format!("Chores {run}"))
        .await
        .unwrap();
    assert_eq!(
        dav.get_calendar(&made_href)
            .await
            .unwrap()
            .display_name
            .as_deref(),
        Some(format!("Chores {run}").as_str())
    );
    engine.delete_task_list(made.id).await.unwrap();
    assert!(dav.get_calendar(&made_href).await.is_err());
    engine.sync_now(None);
    until("lists after a sync", || async {
        let lists = engine.task_lists().ok()?;
        (lists.iter().any(|l| l.id == list.id) && !lists.iter().any(|l| l.id == made.id))
            .then_some(())
    })
    .await;

    engine.shutdown();
    dav.delete_collection(&list_href).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
