//! Tasks: the engine API on lists of this computer (views, ordering,
//! subtasks, moves, links to mail) and two-way CalDAV sync with a mock
//! server (pulls, conditional uploads, merges, repeating tasks, refused
//! changes), all with a real (temporary) database. Google Tasks, Microsoft
//! To Do and Exchange have modules of their own.

mod ews;
mod google;
mod graph;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use envelope_dav::{Auth, DavClient};
use parking_lot::Mutex;
use wiremock::matchers::{body_string_contains, header, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::caldav::Caldav;
use super::*;
use crate::calendar::local_tz;
use crate::engine::{EventSink, Paths};
use crate::model::{AccountConfig, Address, AuthMethod, FolderRole};
use crate::oauth::{Provider, TokenSet};
use crate::tasks::{LOCAL_LIST_NAME, Task, TaskInput, TaskView};

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

impl Events {
    fn tasks_changed(&self) -> Vec<Vec<i64>> {
        self.0
            .lock()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TasksChanged { list_ids } => Some(list_ids.clone()),
                _ => None,
            })
            .collect()
    }
}

struct Fixture {
    engine: Engine,
    events: Events,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture() -> Fixture {
    let dir =
        std::env::temp_dir().join(format!("envelope-tasks-{}", uuid::Uuid::new_v4().simple()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .expect("engine");
    Fixture {
        engine,
        events,
        dir,
    }
}

impl Fixture {
    /// An account the background schedulers leave alone (IMAP without a
    /// CalDAV server in its settings); the tests drive its task sync with a
    /// service of their own.
    fn account(&self, email: &str) -> AccountRow {
        let id = self
            .engine
            .db
            .write(|tx| {
                mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    email,
                    "Test",
                    "#0ea5e9",
                    &AccountConfig::default(),
                )
            })
            .expect("account");
        self.engine.account_row(id).expect("account row")
    }

    fn local_list(&self) -> i64 {
        self.engine
            .task_lists()
            .expect("lists")
            .into_iter()
            .find(|l| l.account_id.is_none() && l.is_default)
            .expect("local list")
            .id
    }

    async fn create(&self, input: TaskInput) -> Task {
        self.engine.create_task(input).await.expect("create")
    }

    async fn titles(&self, view: TaskView, completed: bool) -> Vec<String> {
        self.engine
            .tasks_query(view, completed)
            .await
            .expect("query")
            .into_iter()
            .map(|t| t.title)
            .collect()
    }

    fn row(&self, id: i64) -> TaskRow {
        self.engine.db.read(|c| tdb::get_task(c, id)).expect("row")
    }

    fn rows(&self, list: i64) -> Vec<TaskRow> {
        self.engine
            .db
            .read(|c| {
                let mut st = c.prepare("SELECT id FROM tasks WHERE list_id = ? ORDER BY id")?;
                let ids: Vec<i64> = st
                    .query_map([list], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                ids.into_iter().map(|id| tdb::get_task(c, id)).collect()
            })
            .expect("rows")
    }
}

fn input(list: i64, title: &str) -> TaskInput {
    TaskInput {
        list_id: list,
        title: title.into(),
        ..TaskInput::default()
    }
}

fn today() -> NaiveDate {
    Utc::now().with_timezone(&local_tz()).date_naive()
}

fn day(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// `hour` o'clock on `date`, in the user's zone.
fn local_at(date: NaiveDate, hour: u32) -> i64 {
    local_tz()
        .from_local_datetime(&date.and_hms_opt(hour, 0, 0).expect("time"))
        .earliest()
        .expect("local time")
        .timestamp_millis()
}

// ---------------------------------------------------------------------------
// Lists on this computer

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smart_views_order_tasks_by_due_date() {
    let fx = fixture();
    let lists = fx.engine.task_lists().expect("lists");
    assert_eq!(lists.len(), 1, "the list on this computer always exists");
    let local = &lists[0];
    assert_eq!(
        (
            local.kind,
            local.name.as_str(),
            local.account_id,
            local.is_default
        ),
        (TaskListKind::Local, LOCAL_LIST_NAME, None, true)
    );
    assert!(local.due_times && local.importance && local.subtasks);
    assert!(!local.read_only);
    let list = local.id;
    let today = today();

    let milk = fx.create(input(list, "  Buy   oat milk ")).await;
    assert_eq!(milk.title, "Buy oat milk");
    assert!(!milk.pending, "local tasks need no upload");
    let call = fx
        .create(TaskInput {
            due_at: Some(Some(local_at(today, 15))),
            important: Some(true),
            ..input(list, "Call Maya")
        })
        .await;
    assert_eq!(call.due_date, Some(day(today)));
    assert_eq!(call.due_at, Some(local_at(today, 15)));
    assert!(call.important);
    fx.create(TaskInput {
        due_date: Some(Some(day(today))),
        ..input(list, "Water the plants")
    })
    .await;
    fx.create(TaskInput {
        due_date: Some(Some(day(today - Duration::days(1)))),
        ..input(list, "Pay rent")
    })
    .await;
    let trip = fx
        .create(TaskInput {
            due_date: Some(Some(day(today + Duration::days(3)))),
            notes: Some("Somewhere warm\r\n".into()),
            ..input(list, "Plan the trip")
        })
        .await;
    assert_eq!(trip.notes, "Somewhere warm");
    fx.create(TaskInput {
        important: Some(true),
        ..input(list, "Write the report")
    })
    .await;
    let flights = fx
        .create(TaskInput {
            parent_id: Some(Some(trip.id)),
            due_date: Some(Some(day(today - Duration::days(2)))),
            ..input(list, "Book flights")
        })
        .await;
    let hotel = fx
        .create(TaskInput {
            parent_id: Some(Some(trip.id)),
            ..input(list, "Find a hotel")
        })
        .await;

    assert_eq!(
        fx.titles(TaskView::Today, false).await,
        ["Book flights", "Pay rent", "Call Maya", "Water the plants"],
        "overdue first, timed before untimed within a day, subtasks included"
    );
    assert_eq!(
        fx.titles(TaskView::Upcoming, false).await,
        ["Plan the trip"]
    );
    assert_eq!(
        fx.titles(TaskView::Important, false).await,
        ["Call Maya", "Write the report"],
        "undated last"
    );
    let all = [
        "Buy oat milk",
        "Call Maya",
        "Water the plants",
        "Pay rent",
        "Plan the trip",
        "Write the report",
    ];
    assert_eq!(fx.titles(TaskView::All, false).await, all);
    assert_eq!(
        fx.titles(TaskView::List(list), true).await,
        all,
        "subtasks are not listed"
    );

    // Subtasks.
    let subtasks = fx.engine.task_subtasks(trip.id).await.expect("subtasks");
    assert_eq!(
        subtasks
            .iter()
            .map(|t| t.title.as_str())
            .collect::<Vec<_>>(),
        ["Book flights", "Find a hotel"]
    );
    assert_eq!(subtasks[0].parent_id, Some(trip.id));
    fx.engine
        .complete_task(hotel.id, true)
        .await
        .expect("complete");
    let trip_now = fx.engine.task(trip.id).await.expect("task");
    assert_eq!((trip_now.subtask_count, trip_now.subtasks_done), (2, 1));
    let flights_now = fx.engine.task(flights.id).await.expect("task");
    assert_eq!(
        (flights_now.subtask_count, flights_now.subtasks_done),
        (0, 0)
    );
    assert_eq!(
        fx.engine
            .task_subtasks(trip.id)
            .await
            .expect("subtasks")
            .len(),
        2,
        "completed subtasks are included"
    );

    // Completed tasks follow the open ones, the latest first.
    fx.engine
        .complete_task(milk.id, true)
        .await
        .expect("complete");
    let report = fx
        .engine
        .tasks_query(TaskView::Important, false)
        .await
        .expect("query")
        .pop()
        .expect("report");
    let done = fx
        .engine
        .complete_task(report.id, true)
        .await
        .expect("complete");
    assert!(done.completed_at.is_some());
    assert_eq!(
        fx.titles(TaskView::List(list), true).await,
        [
            "Call Maya",
            "Water the plants",
            "Pay rent",
            "Plan the trip",
            "Write the report",
            "Buy oat milk"
        ]
    );
    assert_eq!(fx.titles(TaskView::List(list), false).await.len(), 4);
    assert!(
        !fx.titles(TaskView::Important, false)
            .await
            .contains(&"Write the report".to_owned())
    );
    // Reopened, it is back in its place.
    fx.engine
        .complete_task(milk.id, false)
        .await
        .expect("reopen");
    assert_eq!(
        fx.titles(TaskView::List(list), false).await[0],
        "Buy oat milk"
    );
    assert!(
        fx.events
            .tasks_changed()
            .iter()
            .any(|ids| ids.as_slice() == [list])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_keep_what_they_leave_out() {
    let fx = fixture();
    let list = fx.local_list();
    let today = today();
    let call = fx
        .create(TaskInput {
            due_at: Some(Some(local_at(today, 9))),
            important: Some(true),
            notes: Some("About the offer".into()),
            ..input(list, "Call Maya")
        })
        .await;

    // Only a day: the time goes, everything else stays.
    let moved = fx
        .engine
        .update_task(
            call.id,
            TaskInput {
                due_date: Some(Some(day(today + Duration::days(1)))),
                ..input(list, "Call Maya back")
            },
        )
        .await
        .expect("update");
    assert_eq!(moved.title, "Call Maya back");
    assert_eq!(moved.due_date, Some(day(today + Duration::days(1))));
    assert_eq!(moved.due_at, None);
    assert_eq!(moved.notes, "About the offer");
    assert!(moved.important);
    // A time sets the day too; null clears.
    let timed = fx
        .engine
        .update_task(
            call.id,
            TaskInput {
                due_at: Some(Some(local_at(today, 18))),
                important: Some(false),
                notes: Some(String::new()),
                ..input(list, "Call Maya back")
            },
        )
        .await
        .expect("update");
    assert_eq!(
        (timed.due_date, timed.due_at),
        (Some(day(today)), Some(local_at(today, 18)))
    );
    assert!(!timed.important);
    assert_eq!(timed.notes, "");
    // Clearing only the time keeps the day.
    let untimed = fx
        .engine
        .update_task(
            call.id,
            TaskInput {
                due_at: Some(None),
                ..input(list, "Call Maya back")
            },
        )
        .await
        .expect("update");
    assert_eq!((untimed.due_date, untimed.due_at), (Some(day(today)), None));
    let cleared = fx
        .engine
        .update_task(
            call.id,
            TaskInput {
                due_date: Some(None),
                ..input(list, "Call Maya back")
            },
        )
        .await
        .expect("update");
    assert_eq!((cleared.due_date, cleared.due_at), (None, None));
    // Saving what is there changes nothing.
    let version = fx.row(call.id).version;
    fx.engine
        .update_task(call.id, input(list, "Call Maya back"))
        .await
        .expect("update");
    assert_eq!(fx.row(call.id).version, version);

    // What the engine refuses.
    let blank = fx.engine.create_task(input(list, "  ")).await;
    assert!(matches!(blank, Err(Error::Invalid(_))));
    let bad_day = fx
        .engine
        .create_task(TaskInput {
            due_date: Some(Some("tomorrow".into())),
            ..input(list, "x")
        })
        .await;
    assert!(matches!(bad_day, Err(Error::Invalid(_))));
    let sub = fx
        .create(TaskInput {
            parent_id: Some(Some(call.id)),
            ..input(list, "Find the number")
        })
        .await;
    let nested = fx
        .engine
        .create_task(TaskInput {
            parent_id: Some(Some(sub.id)),
            ..input(list, "Too deep")
        })
        .await;
    assert!(matches!(nested, Err(Error::Invalid(_))), "one level deep");
    let parent_with_kids = fx
        .engine
        .update_task(
            call.id,
            TaskInput {
                parent_id: Some(Some(sub.id)),
                ..input(list, "Call Maya back")
            },
        )
        .await;
    assert!(matches!(parent_with_kids, Err(Error::Invalid(_))));
    // A subtask can become a task again.
    let promoted = fx
        .engine
        .update_task(
            sub.id,
            TaskInput {
                parent_id: Some(None),
                ..input(list, "Find the number")
            },
        )
        .await
        .expect("promote");
    assert_eq!(promoted.parent_id, None);
    assert_eq!(fx.titles(TaskView::List(list), false).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reordering_deleting_and_clearing() {
    let fx = fixture();
    let list = fx.local_list();
    let mut ids = Vec::new();
    for title in ["A", "B", "C", "D"] {
        ids.push(fx.create(input(list, title)).await.id);
    }
    let [a, b, c, d] = ids[..] else {
        unreachable!()
    };
    fx.engine.reorder_task(d, Some(a)).await.expect("reorder");
    assert_eq!(
        fx.titles(TaskView::List(list), false).await,
        ["D", "A", "B", "C"]
    );
    fx.engine.reorder_task(d, None).await.expect("reorder");
    assert_eq!(
        fx.titles(TaskView::List(list), false).await,
        ["A", "B", "C", "D"]
    );
    fx.engine.reorder_task(c, Some(b)).await.expect("reorder");
    assert_eq!(
        fx.titles(TaskView::List(list), false).await,
        ["A", "C", "B", "D"]
    );
    // Without room between two neighbours, the list is spaced out again.
    fx.engine
        .db
        .write(|tx| {
            tx.execute("UPDATE tasks SET position = 1 WHERE id = ?", [a])?;
            tx.execute("UPDATE tasks SET position = 2 WHERE id = ?", [c])?;
            Ok(())
        })
        .expect("crowd");
    fx.engine.reorder_task(d, Some(c)).await.expect("reorder");
    assert_eq!(
        fx.titles(TaskView::List(list), false).await,
        ["A", "D", "C", "B"]
    );
    let positions: Vec<i64> = [a, d, c, b].iter().map(|id| fx.row(*id).position).collect();
    assert!(
        positions.windows(2).all(|w| w[1] - w[0] >= 2),
        "{positions:?}"
    );

    let sub = fx
        .create(TaskInput {
            parent_id: Some(Some(b)),
            ..input(list, "B1")
        })
        .await;
    let across = fx.engine.reorder_task(a, Some(sub.id)).await;
    assert!(matches!(across, Err(Error::Invalid(_))));

    // Deleting a task takes its subtasks along.
    fx.engine.delete_task(b).await.expect("delete");
    assert!(fx.engine.task(sub.id).await.is_err());
    assert!(fx.engine.task_subtasks(b).await.is_err());
    // Clearing completed tasks: the completed top-level ones go with their
    // subtasks, completed subtasks of open tasks stay.
    let d1 = fx
        .create(TaskInput {
            parent_id: Some(Some(d)),
            ..input(list, "D1")
        })
        .await;
    fx.engine
        .complete_task(d1.id, true)
        .await
        .expect("complete");
    fx.create(TaskInput {
        parent_id: Some(Some(c)),
        ..input(list, "C1")
    })
    .await;
    fx.engine.complete_task(a, true).await.expect("complete");
    fx.engine.complete_task(c, true).await.expect("complete");
    let counts = |fx: &Fixture| {
        fx.engine
            .task_lists()
            .expect("lists")
            .into_iter()
            .find(|l| l.id == list)
            .map(|l| (l.open_count, l.completed_count))
    };
    assert_eq!(
        counts(&fx),
        Some((2, 2)),
        "D and C1 are open; A and C are what clearing deletes"
    );
    fx.engine.clear_completed_tasks(list).await.expect("clear");
    assert_eq!(fx.titles(TaskView::List(list), true).await, ["D"]);
    let kids = fx.engine.task_subtasks(d).await.expect("subtasks");
    assert_eq!(
        kids.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["D1"]
    );
    assert_eq!(fx.rows(list).len(), 2, "gone at once on this computer");
    assert_eq!(counts(&fx), Some((1, 0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_on_this_computer_and_moves_between_them() {
    let fx = fixture();
    let tasks = fx.local_list();
    let groceries = fx
        .engine
        .create_task_list(None, " Groceries ")
        .await
        .expect("list");
    assert_eq!(
        (
            groceries.kind,
            groceries.name.as_str(),
            groceries.is_default
        ),
        (TaskListKind::Local, "Groceries", false)
    );
    fx.engine
        .rename_task_list(groceries.id, "Shopping")
        .await
        .expect("rename");
    let names: Vec<String> = fx
        .engine
        .task_lists()
        .expect("lists")
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, [LOCAL_LIST_NAME, "Shopping"]);
    assert!(
        fx.events.tasks_changed().iter().any(|ids| ids.is_empty()),
        "list changes reload everything"
    );

    let party = fx.create(input(tasks, "Party")).await;
    let cake = fx
        .create(TaskInput {
            parent_id: Some(Some(party.id)),
            ..input(tasks, "Cake")
        })
        .await;
    let moved = fx
        .engine
        .update_task(party.id, input(groceries.id, "Party"))
        .await
        .expect("move");
    assert_eq!(
        moved.id, party.id,
        "moving between local lists keeps the task"
    );
    assert_eq!(moved.list_id, groceries.id);
    let kids = fx.engine.task_subtasks(party.id).await.expect("subtasks");
    assert_eq!(kids.len(), 1);
    assert_eq!((kids[0].id, kids[0].list_id), (cake.id, groceries.id));
    let lists = fx.engine.task_lists().expect("lists");
    assert_eq!(
        lists.iter().map(|l| l.open_count).collect::<Vec<_>>(),
        [0, 2],
        "open counts include subtasks"
    );

    assert!(matches!(
        fx.engine.delete_task_list(tasks).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        fx.engine.create_task_list(None, "  ").await,
        Err(Error::Invalid(_))
    ));
    fx.engine
        .delete_task_list(groceries.id)
        .await
        .expect("delete list");
    assert!(fx.engine.task(party.id).await.is_err());
    assert_eq!(fx.engine.task_lists().expect("lists").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_from_mail_link_back_to_the_conversation() {
    let fx = fixture();
    let account = fx.account("me@example.com");
    let (message, thread) = inbox_message(&fx, &account);
    let list = fx.local_list();
    let task = fx
        .create(TaskInput {
            from_message: Some(message),
            ..input(list, "")
        })
        .await;
    assert_eq!(task.title, "The offer", "titled with the subject");
    let source = task.source.expect("source");
    assert_eq!(source.message_id, "offer-1@northwind.example");
    assert_eq!(source.thread_id, Some(thread));
    assert_eq!(source.account_id, Some(account.id));
    assert_eq!(source.subject, "The offer");
    assert_eq!(source.from.as_deref(), Some("Maya Chen"));

    // Without the account the link goes, the task stays.
    fx.engine.remove_account(account.id).await.expect("remove");
    let source = fx
        .engine
        .task(task.id)
        .await
        .expect("task")
        .source
        .expect("source");
    assert_eq!((source.thread_id, source.account_id), (None, None));
    assert_eq!(source.message_id, "offer-1@northwind.example");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn due_times_are_announced_once() {
    let fx = fixture();
    let list = fx.local_list();
    // Due times are kept to the second.
    let now = crate::db::now_ms() / 1000 * 1000;
    let due = fx
        .create(TaskInput {
            due_at: Some(Some(now - 60_000)),
            ..input(list, "Stand up")
        })
        .await;
    fx.create(TaskInput {
        due_at: Some(Some(now - 3 * 3_600_000)),
        ..input(list, "Long ago")
    })
    .await;
    let later = now + 3_600_000;
    fx.create(TaskInput {
        due_at: Some(Some(later)),
        ..input(list, "Later")
    })
    .await;
    assert_eq!(
        fx.engine.notify_due_tasks(now).expect("notify"),
        Some(later)
    );
    assert_eq!(
        fx.engine.notify_due_tasks(now).expect("notify"),
        Some(later)
    );
    let announced: Vec<(i64, String)> = fx
        .events
        .0
        .lock()
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TaskDue { task_id, title, .. } => Some((*task_id, title.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        announced,
        [(due.id, "Stand up".to_owned())],
        "once, and not hours late"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn days_of_timed_tasks_follow_the_time_zone() {
    let fx = fixture();
    let list = fx.local_list();
    let today = today();
    let tomorrow = day(today + Duration::days(1));
    let call = fx
        .create(TaskInput {
            due_at: Some(Some(local_at(today, 15))),
            ..input(list, "Call Maya")
        })
        .await;
    let plants = fx
        .create(TaskInput {
            due_date: Some(Some(tomorrow.clone())),
            ..input(list, "Water the plants")
        })
        .await;
    // Worked out in a zone far ahead, where 15:00 here is tomorrow already.
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE tasks SET due_date = ? WHERE id = ?",
                rusqlite::params![tomorrow, call.id],
            )?;
            crate::db::misc::kv_set(tx, crate::tasks::TIME_ZONE_KEY, &"Pacific/Kiritimati")
        })
        .expect("another zone");

    fx.engine.follow_time_zone().expect("follow");
    assert_eq!(fx.row(call.id).due_date, Some(day(today)));
    assert_eq!(
        fx.row(plants.id).due_date,
        Some(tomorrow.clone()),
        "days without a time stay"
    );
    assert_eq!(fx.titles(TaskView::Today, false).await, ["Call Maya"]);
    assert_eq!(
        fx.titles(TaskView::Upcoming, false).await,
        ["Water the plants"]
    );
    assert!(
        fx.events
            .tasks_changed()
            .iter()
            .any(|ids| ids.as_slice() == [list])
    );
    let zone: Option<String> = fx
        .engine
        .db
        .read(|c| crate::db::misc::kv_get(c, crate::tasks::TIME_ZONE_KEY))
        .expect("zone");
    assert_eq!(zone.as_deref(), Some(local_tz().name()));
}

// ---------------------------------------------------------------------------
// CalDAV

const HOME: &str = "/dav/home/";

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn multistatus(responses: &str) -> ResponseTemplate {
    ResponseTemplate::new(207).set_body_raw(
        format!(r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:x1="http://apple.com/ns/ical/">{responses}</d:multistatus>"#),
        "application/xml; charset=utf-8",
    )
}

/// A calendar collection in a PROPFIND answer.
fn collection(href: &str, name: &str, ctag: &str, components: &[&str], writable: bool) -> String {
    let comps: String = components
        .iter()
        .map(|c| format!(r#"<cal:comp name="{c}"/>"#))
        .collect();
    let privilege = if writable { "<d:all/>" } else { "<d:read/>" };
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>\
         <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>\
         <d:displayname>{name}</d:displayname><x1:calendar-color>#FF2968FF</x1:calendar-color>\
         <cs:getctag>{ctag}</cs:getctag><d:sync-token>{ctag}-token</d:sync-token>\
         <cal:supported-calendar-component-set>{comps}</cal:supported-calendar-component-set>\
         <d:current-user-privilege-set><d:privilege>{privilege}</d:privilege></d:current-user-privilege-set>\
         </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

fn object(href: &str, etag: &str, data: Option<&str>) -> String {
    let data = data
        .map(|d| format!("<cal:calendar-data>{}</cal:calendar-data>", xml_escape(d)))
        .unwrap_or_default();
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>\"{etag}\"</d:getetag>{data}\
         </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

fn gone(href: &str) -> String {
    format!(
        "<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
    )
}

fn vtodo(body: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Tasks//EN\r\nBEGIN:VTODO\r\nDTSTAMP:20240101T000000Z\r\n\
         {body}END:VTODO\r\nEND:VCALENDAR\r\n"
    )
}

fn parent_task() -> String {
    vtodo(
        "UID:t1\r\nCREATED:20240101T000000Z\r\nSUMMARY:Plan the trip\r\nDUE:20300601T090000Z\r\nPRIORITY:1\r\n\
         X-APPLE-SORT-ORDER:100\r\nX-CUSTOM;X-A=1:keep me\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\n\
         DESCRIPTION:Reminder\r\nEND:VALARM\r\n",
    )
}

fn child_task() -> String {
    vtodo(
        "UID:t2\r\nSUMMARY:Book flights\r\nRELATED-TO:t1\r\nDUE;VALUE=DATE:20300515\r\nSTATUS:NEEDS-ACTION\r\n\
         X-APPLE-SORT-ORDER:5\r\n",
    )
}

/// An account whose CalDAV calendar home is on `server`, and the service
/// for it.
fn caldav(fx: &Fixture, server: &MockServer) -> (AccountRow, Caldav) {
    let account = fx.account("alex@example.com");
    fx.engine
        .db
        .write(|tx| {
            crate::db::contacts::set_account_state_value(
                tx,
                account.id,
                "caldavHome",
                &format!("{}{HOME}", server.uri()).into(),
            )?;
            crate::db::contacts::set_account_state_value(
                tx,
                account.id,
                "caldavUrl",
                &server.uri().into(),
            )
        })
        .expect("state");
    let account = fx.engine.account_row(account.id).expect("account");
    let client = DavClient::builder(&server.uri())
        .auth(Auth::None)
        .build()
        .expect("client");
    let service = Caldav::new(&fx.engine, &account, client, &server.uri());
    (account, service)
}

async fn mount_home(server: &MockServer, tasks_ctag: &str) {
    Mock::given(method("PROPFIND"))
        .and(path(HOME))
        .respond_with(multistatus(&format!(
            "{}{}{}",
            collection("/dav/home/tasks/", "Tasks", tasks_ctag, &["VTODO"], true),
            collection(
                "/dav/home/personal/",
                "Personal",
                "p1",
                &["VEVENT", "VTODO"],
                true
            ),
            collection("/dav/home/work/", "Work", "w1", &["VEVENT"], true),
        )))
        .mount(server)
        .await;
}

fn list_named(fx: &Fixture, account: &AccountRow, name: &str) -> ListRow {
    fx.engine
        .db
        .read(|c| tdb::account_lists(c, account.id))
        .expect("lists")
        .into_iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("no list {name}"))
}

fn requests_to<'a>(requests: &'a [Request], verb: &str, suffix: &str) -> Vec<&'a Request> {
    requests
        .iter()
        .filter(|r| r.method.as_str() == verb && r.url.path().ends_with(suffix))
        .collect()
}

fn body(request: &Request) -> String {
    String::from_utf8_lossy(&request.body).replace("\r\n ", "")
}

/// An account of `kind` (Gmail or Microsoft) signed in with OAuth, granted
/// `scope`. It is disabled, which keeps the mail and calendar schedulers
/// (they would go to the real servers) away; the tests drive its task sync
/// and read its tasks from the database, as a disabled account's tasks are
/// out of sight.
fn oauth_account(fx: &Fixture, kind: AccountKind, scope: &str) -> AccountRow {
    let (provider, auth, provider_id) = match kind {
        AccountKind::Gmail => (Provider::Google, AuthMethod::OAuthGoogle, "gmail"),
        _ => (Provider::Microsoft, AuthMethod::OAuthMicrosoft, "outlook"),
    };
    let config = AccountConfig {
        auth,
        ..AccountConfig::default()
    };
    let id = fx
        .engine
        .db
        .write(|tx| {
            let id = mail::insert_account(
                tx,
                kind,
                provider_id,
                "alex@example.com",
                "Alex",
                "#0ea5e9",
                &config,
            )?;
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [id])?;
            Ok(id)
        })
        .expect("account");
    let tokens = TokenSet {
        provider,
        access_token: "token".into(),
        refresh_token: Some("refresh".into()),
        expires_at: i64::from(i32::MAX),
        scope: Some(scope.into()),
        client: Default::default(),
    };
    fx.engine.secrets.preload(
        &crate::secrets::oauth_key(id),
        &serde_json::to_string(&tokens).expect("tokens"),
    );
    fx.engine.account_row(id).expect("account row")
}

/// Stores the lists `service` reports as the account's lists.
async fn store_lists(fx: &Fixture, account: &AccountRow, service: &dyn TaskService) {
    let lists = service.lists().await.expect("lists");
    fx.engine
        .db
        .write(|tx| tdb::sync_lists(tx, account.id, service.kind(), &lists))
        .expect("sync lists");
}

/// The stored task of `list` with `remote_id`.
fn remote_row(fx: &Fixture, list: i64, remote_id: &str) -> TaskRow {
    fx.rows(list)
        .into_iter()
        .find(|r| r.remote_id.as_deref() == Some(remote_id))
        .unwrap_or_else(|| panic!("no task {remote_id}"))
}

/// The titles of the tasks of `list` under `parent` (top-level ones with
/// `None`), in order, completed ones included.
fn order(fx: &Fixture, list: i64, parent: Option<i64>) -> Vec<String> {
    fx.engine
        .db
        .read(|c| {
            tdb::siblings(c, list, parent)?
                .into_iter()
                .map(|(id, _)| tdb::get_task(c, id).map(|t| t.title))
                .collect()
        })
        .expect("order")
}

/// A list of an account as stored, for calls that only need its server
/// id.
fn stored_list(kind: TaskListKind, remote_id: &str, is_default: bool) -> ListRow {
    ListRow {
        id: 0,
        account_id: Some(1),
        kind,
        remote_id: remote_id.to_owned(),
        name: "List".into(),
        color: String::new(),
        read_only: false,
        is_default,
        sort_order: 0,
        sync_state: ListSyncState::default(),
    }
}

/// A message in the inbox of `account`, to make tasks from: its id and its
/// conversation's.
fn inbox_message(fx: &Fixture, account: &AccountRow) -> (i64, i64) {
    fx.engine
        .db
        .write(|tx| {
            let folders = mail::sync_folders(
                tx,
                account.id,
                &[mail::FolderUpsert {
                    remote_id: "INBOX".into(),
                    name: "Inbox".into(),
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
            let stored = mail::upsert_message(
                tx,
                account.id,
                folders["INBOX"],
                Some(1),
                &mail::MessageMeta {
                    message_id: Some("offer-1@northwind.example".into()),
                    subject: "The offer".into(),
                    from: Some(Address::new(
                        Some("Maya Chen".into()),
                        "maya@northwind.example",
                    )),
                    date: 1_000,
                    ..Default::default()
                },
            )?;
            mail::refresh_threads(tx, &[stored.thread_id].into())?;
            Ok((stored.message_id, stored.thread_id))
        })
        .expect("message")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caldav_lists_pull_and_push() {
    let fx = fixture();
    let server = MockServer::start().await;
    let (account, service) = caldav(&fx, &server);
    mount_home(&server, "c1").await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/tasks/"))
        .and(body_string_contains("sync-collection"))
        .respond_with(multistatus(&format!(
            "{}{}<d:sync-token>s1</d:sync-token>",
            object("/dav/home/tasks/t1.ics", "t1-1", None),
            object("/dav/home/tasks/t2.ics", "t2-1", None)
        )))
        .mount(&server)
        .await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/tasks/"))
        .and(body_string_contains("calendar-multiget"))
        .respond_with(multistatus(&format!(
            "{}{}",
            object("/dav/home/tasks/t2.ics", "t2-1", Some(&child_task())),
            object("/dav/home/tasks/t1.ics", "t1-1", Some(&parent_task()))
        )))
        .mount(&server)
        .await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/personal/"))
        .and(body_string_contains("calendar-query"))
        .respond_with(multistatus(&object(
            "/dav/home/personal/p1.ics",
            "p1-1",
            None,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/personal/"))
        .and(body_string_contains("calendar-multiget"))
        .respond_with(multistatus(&object(
            "/dav/home/personal/p1.ics",
            "p1-1",
            Some(&vtodo("UID:p1\r\nSUMMARY:Renew the passport\r\n")),
        )))
        .mount(&server)
        .await;

    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("first sync");
    assert!(changes.lists);
    let tasks_list = list_named(&fx, &account, "Tasks");
    let personal = list_named(&fx, &account, "Personal");
    assert_eq!(changes.tasks.len(), 2);
    assert!(
        tasks_list.is_default,
        "tasks-only collections are the default"
    );
    assert!(!personal.is_default);
    assert_eq!(tasks_list.color, "#ff2968");
    assert_eq!(
        fx.engine.task_lists().expect("lists").len(),
        3,
        "event calendars are not lists"
    );
    assert_eq!(tasks_list.sync_state.sync_token.as_deref(), Some("s1"));

    let open = fx
        .engine
        .tasks_query(TaskView::List(tasks_list.id), false)
        .await
        .expect("query");
    assert_eq!(open.len(), 1, "the child is a subtask");
    let trip = &open[0];
    assert_eq!(trip.title, "Plan the trip");
    assert!(trip.important);
    assert_eq!(trip.due_at, Some(1_906_534_800_000));
    assert_eq!(trip.subtask_count, 1);
    let flights = fx
        .engine
        .task_subtasks(trip.id)
        .await
        .expect("subtasks")
        .remove(0);
    assert_eq!(flights.due_date.as_deref(), Some("2030-05-15"));
    assert_eq!(flights.due_at, None);
    let passport = fx
        .engine
        .tasks_query(TaskView::List(personal.id), false)
        .await
        .expect("query")
        .remove(0);

    // Local changes: renamed, completed, a new subtask, a deletion.
    fx.engine
        .update_task(trip.id, input(tasks_list.id, "Plan the summer trip"))
        .await
        .expect("rename");
    fx.engine
        .complete_task(flights.id, true)
        .await
        .expect("complete");
    let pack = fx
        .create(TaskInput {
            parent_id: Some(Some(trip.id)),
            ..input(tasks_list.id, "Pack")
        })
        .await;
    assert!(pack.pending);
    fx.engine.delete_task(passport.id).await.expect("delete");
    assert!(
        fx.engine
            .tasks_query(TaskView::List(personal.id), false)
            .await
            .expect("query")
            .is_empty(),
        "hidden right away"
    );
    assert_eq!(fx.row(passport.id).local_state, tdb::DELETED);

    Mock::given(method("PUT"))
        .and(path("/dav/home/tasks/t1.ics"))
        .and(header("If-Match", "\"t1-1\""))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"t1-2\""))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/dav/home/tasks/t2.ics"))
        .and(header("If-Match", "\"t2-1\""))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"t2-2\""))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(r"^/dav/home/tasks/[0-9a-f-]{36}\.ics$"))
        .and(header("If-None-Match", "*"))
        .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"n-1\""))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/dav/home/personal/p1.ics"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    sync_account(&fx.engine, &account, &service, false)
        .await
        .expect("push");
    for list in [tasks_list.id, personal.id] {
        assert!(
            fx.rows(list).iter().all(|r| r.local_state == tdb::SYNCED),
            "everything is on the server"
        );
    }
    assert!(
        fx.engine
            .db
            .read(|c| tdb::get_task(c, passport.id))
            .is_err()
    );
    assert_eq!(fx.row(trip.id).etag.as_deref(), Some("\"t1-2\""));
    assert!(!fx.engine.task(pack.id).await.expect("task").pending);

    let requests = server.received_requests().await.expect("requests");
    let renamed = body(requests_to(&requests, "PUT", "/t1.ics")[0]);
    assert!(
        renamed.contains("SUMMARY:Plan the summer trip\r\n"),
        "{renamed}"
    );
    assert!(renamed.contains("X-CUSTOM;X-A=1:keep me\r\n"), "{renamed}");
    assert!(renamed.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\n"));
    assert!(
        renamed.contains("DUE:20300601T090000Z\r\n"),
        "the due time keeps its form"
    );
    assert!(renamed.contains("PRIORITY:1\r\n"));
    let completed = body(requests_to(&requests, "PUT", "/t2.ics")[0]);
    assert!(completed.contains("STATUS:COMPLETED\r\n"), "{completed}");
    assert!(completed.contains("PERCENT-COMPLETE:100\r\n"));
    assert!(completed.contains("COMPLETED:"));
    assert!(completed.contains("RELATED-TO:t1\r\n"));
    let new = requests
        .iter()
        .find(|r| r.method.as_str() == "PUT" && r.headers.contains_key("if-none-match"))
        .map(body)
        .expect("new task upload");
    assert!(new.contains("SUMMARY:Pack\r\n"), "{new}");
    assert!(new.contains("RELATED-TO;RELTYPE=PARENT:t1\r\n"), "{new}");
    assert!(new.contains("STATUS:NEEDS-ACTION\r\n"));
    assert!(new.contains("X-APPLE-SORT-ORDER:"));

    // Changes on the server come in by the sync token.
    server.reset().await;
    mount_home(&server, "c2").await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/tasks/"))
        .and(body_string_contains("<D:sync-token>s1</D:sync-token>"))
        .respond_with(multistatus(&format!(
            "{}{}<d:sync-token>s2</d:sync-token>",
            object("/dav/home/tasks/t1.ics", "t1-3", None),
            gone("/dav/home/tasks/t2.ics")
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/tasks/"))
        .and(body_string_contains("calendar-multiget"))
        .respond_with(multistatus(&object(
            "/dav/home/tasks/t1.ics",
            "t1-3",
            Some(&parent_task().replace("Plan the trip", "Plan the autumn trip")),
        )))
        .mount(&server)
        .await;
    Mock::given(method("REPORT"))
        .and(path("/dav/home/personal/"))
        .respond_with(multistatus(""))
        .expect(0)
        .mount(&server)
        .await;
    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("second sync");
    assert!(!changes.lists);
    assert_eq!(changes.tasks, [tasks_list.id]);
    assert_eq!(fx.row(trip.id).title, "Plan the autumn trip");
    assert!(fx.engine.task(flights.id).await.is_err(), "deleted there");
    let kids = fx.engine.task_subtasks(trip.id).await.expect("subtasks");
    assert_eq!(
        kids.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["Pack"]
    );

    // A disabled account's lists and tasks are out of sight.
    fx.engine
        .db
        .write(|tx| {
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [account.id])?;
            Ok(())
        })
        .expect("disable");
    assert_eq!(fx.engine.task_lists().expect("lists").len(), 1);
    assert!(fx.titles(TaskView::All, false).await.is_empty());
    assert!(fx.titles(TaskView::Important, false).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_calendar_gets_the_tasks_due_in_its_range() {
    let fx = fixture();
    let list = fx.local_list();
    let today = today();
    let tomorrow = today + Duration::days(1);
    let timed = fx
        .create(TaskInput {
            due_at: Some(Some(local_at(tomorrow, 10))),
            ..input(list, "Dentist")
        })
        .await;
    fx.create(TaskInput {
        due_date: Some(Some(day(tomorrow))),
        ..input(list, "Send the invoice")
    })
    .await;
    let done = fx
        .create(TaskInput {
            due_date: Some(Some(day(tomorrow))),
            ..input(list, "Done already")
        })
        .await;
    fx.engine
        .complete_task(done.id, true)
        .await
        .expect("complete");
    fx.create(TaskInput {
        due_date: Some(Some(day(today + Duration::days(9)))),
        ..input(list, "Next week")
    })
    .await;
    fx.create(input(list, "Some day")).await;

    let titles = |tasks: Vec<Task>| tasks.into_iter().map(|t| t.title).collect::<Vec<_>>();
    let day_from = local_at(tomorrow, 0);
    let day_to = local_at(tomorrow + Duration::days(1), 0);
    assert_eq!(
        titles(fx.engine.tasks_due(day_from, day_to).await.expect("due")),
        ["Dentist", "Send the invoice"],
        "open tasks of the day, timed ones first"
    );
    // Part of a day: the tasks at a time in it, and the tasks of its day.
    assert_eq!(
        titles(
            fx.engine
                .tasks_due(local_at(tomorrow, 11), local_at(tomorrow, 12))
                .await
                .expect("due")
        ),
        ["Send the invoice"]
    );
    assert_eq!(
        titles(
            fx.engine
                .tasks_due(timed.due_at.expect("due"), local_at(tomorrow, 11))
                .await
                .expect("due")
        ),
        ["Dentist", "Send the invoice"]
    );
    let week = fx
        .engine
        .tasks_due(local_at(today, 0), local_at(today + Duration::days(14), 0))
        .await
        .expect("due");
    assert_eq!(week.len(), 3);
    assert!(
        fx.engine
            .tasks_due(day_to, day_from)
            .await
            .expect("due")
            .is_empty()
    );
}

/// Stores one task of a CalDAV list as a pull would.
async fn stored(fx: &Fixture, list: &ListRow, href: &str, etag: &str, data: &str) -> i64 {
    let task = super::caldav::remote_task(href, Some(format!("\"{etag}\"")), data).expect("task");
    store_pulled(
        &fx.engine,
        list.id,
        Pulled {
            changed: vec![task],
            ..Pulled::default()
        },
    )
    .await
    .expect("store");
    fx.rows(list.id)
        .into_iter()
        .find(|r| r.remote_id.as_deref() == Some(href))
        .expect("row")
        .id
}

async fn task_list(
    fx: &Fixture,
    server: &MockServer,
    service: &Caldav,
    account: &AccountRow,
) -> ListRow {
    mount_home(server, "c1").await;
    let lists = service.lists().await.expect("lists");
    fx.engine
        .db
        .write(|tx| tdb::sync_lists(tx, account.id, TaskListKind::Caldav, &lists))
        .expect("sync lists");
    server.reset().await;
    list_named(fx, account, "Tasks")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caldav_conflicts_merge_with_the_server_copy() {
    let fx = fixture();
    let server = MockServer::start().await;
    let (account, service) = caldav(&fx, &server);
    let list = task_list(&fx, &server, &service, &account).await;
    let href = format!("{}/dav/home/tasks/t1.ics", server.uri());
    let id = stored(&fx, &list, &href, "e1", &parent_task()).await;
    fx.engine.complete_task(id, true).await.expect("complete");

    // Renamed on another device meanwhile.
    let theirs = parent_task().replace("Plan the trip", "Plan the trip to Rome");
    Mock::given(method("PUT"))
        .and(path("/dav/home/tasks/t1.ics"))
        .and(header("If-Match", "\"e1\""))
        .respond_with(ResponseTemplate::new(412))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dav/home/tasks/t1.ics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(theirs.clone(), "text/calendar")
                .insert_header("ETag", "\"e2\""),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/dav/home/tasks/t1.ics"))
        .and(header("If-Match", "\"e2\""))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"e3\""))
        .expect(1)
        .mount(&server)
        .await;
    assert!(push_list(&fx.engine, &service, &list).await.expect("push"));
    let row = fx.row(id);
    assert_eq!(row.local_state, tdb::SYNCED);
    assert_eq!(row.title, "Plan the trip to Rome", "their rename is kept");
    assert!(row.completed_at.is_some(), "and the completion");
    assert_eq!(row.etag.as_deref(), Some("\"e3\""));
    let requests = server.received_requests().await.expect("requests");
    let merged = body(requests.last().expect("request"));
    assert!(
        merged.contains("SUMMARY:Plan the trip to Rome\r\n"),
        "{merged}"
    );
    assert!(merged.contains("STATUS:COMPLETED\r\n"), "{merged}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caldav_repeating_tasks_move_on_when_completed() {
    let fx = fixture();
    let server = MockServer::start().await;
    let (account, service) = caldav(&fx, &server);
    let list = task_list(&fx, &server, &service, &account).await;
    let href = format!("{}/dav/home/tasks/r1.ics", server.uri());
    let weekly = vtodo(
        "UID:r1\r\nSUMMARY:Weekly report\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\n\
         DTSTART;TZID=Europe/Berlin:20300301T090000\r\nDUE;TZID=Europe/Berlin:20300301T170000\r\n",
    );
    let id = stored(&fx, &list, &href, "r-1", &weekly).await;
    let first_due = fx.row(id).due_at.expect("due");
    let task = fx.engine.complete_task(id, true).await.expect("complete");
    assert!(task.repeats);
    assert_eq!(task.completed_at, None, "it stays open");
    assert_eq!(task.due_at, Some(first_due + 7 * 86_400_000));
    assert!(task.pending);

    Mock::given(method("PUT"))
        .and(path("/dav/home/tasks/r1.ics"))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"r-2\""))
        .mount(&server)
        .await;
    push_list(&fx.engine, &service, &list).await.expect("push");
    let requests = server.received_requests().await.expect("requests");
    let moved = body(requests.last().expect("request"));
    assert!(
        moved.contains("DTSTART;TZID=Europe/Berlin:20300308T090000\r\n"),
        "{moved}"
    );
    assert!(
        moved.contains("DUE;TZID=Europe/Berlin:20300308T170000\r\n"),
        "{moved}"
    );
    assert!(moved.contains("RRULE:FREQ=WEEKLY;COUNT=2\r\n"), "{moved}");
    assert!(!moved.contains("STATUS:COMPLETED"));

    // Twice more: the last occurrence is completed for good.
    let task = fx.engine.complete_task(id, true).await.expect("complete");
    assert_eq!(task.due_at, Some(first_due + 14 * 86_400_000));
    push_list(&fx.engine, &service, &list).await.expect("push");
    let task = fx.engine.complete_task(id, true).await.expect("complete");
    assert!(task.completed_at.is_some());
    push_list(&fx.engine, &service, &list).await.expect("push");
    let requests = server.received_requests().await.expect("requests");
    let last = body(requests.last().expect("request"));
    assert!(last.contains("RRULE:FREQ=WEEKLY;COUNT=1\r\n"), "{last}");
    assert!(last.contains("STATUS:COMPLETED\r\n"), "{last}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_changes_are_retried_then_given_up() {
    let fx = fixture();
    let server = MockServer::start().await;
    let (account, service) = caldav(&fx, &server);
    let list = task_list(&fx, &server, &service, &account).await;
    let created = fx.create(input(list.id, "Rejected")).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(415))
        .mount(&server)
        .await;
    push_list(&fx.engine, &service, &list).await.expect("push");
    let task = fx.engine.task(created.id).await.expect("task");
    assert!(task.pending);
    assert!(task.error.is_some(), "the task says why");
    assert_eq!(fx.row(created.id).push_attempts, 1);

    // The last attempt: the task the server would not take stays here.
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE tasks SET push_attempts = 4, push_after = 0 WHERE id = ?",
                [created.id],
            )?;
            Ok(())
        })
        .expect("attempts");
    push_list(&fx.engine, &service, &list).await.expect("push");
    let kept = fx.engine.task(created.id).await.expect("task");
    assert_eq!(kept.list_id, fx.local_list());
    assert!(!kept.pending);
    assert_eq!(kept.error, None);

    // A change the server forbids is undone, and the list is read-only.
    server.reset().await;
    let href = format!("{}/dav/home/tasks/t1.ics", server.uri());
    let id = stored(&fx, &list, &href, "e1", &parent_task()).await;
    fx.engine
        .update_task(id, input(list.id, "Mine"))
        .await
        .expect("edit");
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    push_list(&fx.engine, &service, &list).await.expect("push");
    let list = fx
        .engine
        .db
        .read(|c| tdb::get_list(c, list.id))
        .expect("list");
    assert!(list.read_only);
    assert_eq!(
        list.sync_state,
        ListSyncState::default(),
        "synced again from scratch"
    );
    assert!(fx.engine.db.read(|c| tdb::get_task(c, id)).is_err());
    assert!(matches!(
        fx.engine.create_task(input(list.id, "x")).await,
        Err(Error::Invalid(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caldav_lists_are_created_renamed_and_deleted() {
    let fx = fixture();
    let server = MockServer::start().await;
    let (_, service) = caldav(&fx, &server);
    Mock::given(method("MKCALENDAR"))
        .and(path_regex(r"^/dav/home/[0-9a-f-]{36}/$"))
        .and(body_string_contains(r#"<C:comp name="VTODO"/>"#))
        .and(body_string_contains(
            "<D:displayname>Errands</D:displayname>",
        ))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path_regex(r"^/dav/home/[0-9a-f-]{36}/$"))
        .respond_with(|request: &Request| {
            multistatus(&collection(
                request.url.path(),
                "Errands",
                "e0",
                &["VTODO"],
                true,
            ))
        })
        .mount(&server)
        .await;
    let created = service.create_list("Errands").await.expect("create");
    assert!(
        created.remote_id.starts_with(&server.uri()),
        "{}",
        created.remote_id
    );
    assert_eq!(created.name, "Errands");
    assert_eq!(created.color.as_deref(), Some("#ff2968"));
    let row = |remote_id: &str| ListRow {
        id: 0,
        account_id: Some(1),
        kind: TaskListKind::Caldav,
        remote_id: remote_id.to_owned(),
        name: "Errands".into(),
        color: String::new(),
        read_only: false,
        is_default: false,
        sort_order: 0,
        sync_state: ListSyncState::default(),
    };

    Mock::given(method("PROPPATCH"))
        .and(body_string_contains("<D:displayname>Chores</D:displayname>"))
        .respond_with(ResponseTemplate::new(207).set_body_raw(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/x/</d:href><d:propstat><d:prop><d:displayname/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
            "application/xml",
        ))
        .expect(1)
        .mount(&server)
        .await;
    service
        .rename_list(&row(&created.remote_id), "Chores")
        .await
        .expect("rename");
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    service
        .delete_list(&row(&created.remote_id))
        .await
        .expect("delete");

    // A collection holding events too is a calendar: it is not deleted.
    Mock::given(method("PROPFIND"))
        .and(path("/dav/home/personal/"))
        .respond_with(multistatus(&collection(
            "/dav/home/personal/",
            "Personal",
            "p1",
            &["VEVENT", "VTODO"],
            true,
        )))
        .mount(&server)
        .await;
    let mixed = service
        .delete_list(&row(&format!("{}/dav/home/personal/", server.uri())))
        .await;
    assert!(matches!(mixed, Err(Error::Invalid(_))), "{mixed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_a_task_to_caldav_creates_it_there() {
    let fx = fixture();
    let server = MockServer::start().await;
    let (account, service) = caldav(&fx, &server);
    let list = task_list(&fx, &server, &service, &account).await;
    let local = fx.local_list();
    let party = fx
        .create(TaskInput {
            important: Some(true),
            ..input(local, "Party")
        })
        .await;
    fx.create(TaskInput {
        parent_id: Some(Some(party.id)),
        ..input(local, "Cake")
    })
    .await;
    let moved = fx
        .engine
        .update_task(party.id, input(list.id, "Party"))
        .await
        .expect("move");
    assert_ne!(moved.id, party.id);
    assert!(moved.pending && moved.important);
    assert!(
        fx.rows(local).is_empty(),
        "gone from the list on this computer"
    );
    let rows = fx.rows(list.id);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.local_state == tdb::CREATED));
    assert!(
        rows.iter()
            .all(|r| r.remote_id.as_deref().is_some_and(|h| h.ends_with(".ics")))
    );
    let cake = rows.iter().find(|r| r.title == "Cake").expect("cake");
    assert_eq!(cake.parent_id, Some(moved.id));
    assert_eq!(cake.parent_uid.as_deref(), Some(rows[0].uid.as_str()));
}
