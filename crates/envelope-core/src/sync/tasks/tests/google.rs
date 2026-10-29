//! Google Tasks against a mock of the API: a full pull with subtasks and
//! hidden tasks, every kind of upload (in list order, subtasks after their
//! tasks, placed after the task before them), pulls of what changed since,
//! expired change listings, list writes, and accounts whose sign-in does
//! not cover tasks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use envelope_google::{ClientConfig, GoogleClient, RetryPolicy, StaticToken};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, query_param, query_param_is_missing};

use super::*;
use crate::sync::tasks::google::Google;

const TASKS_SCOPE: &str = "https://mail.google.com/ https://www.googleapis.com/auth/tasks";
const LISTS: &str = "/tasks/v1/users/@me/lists";
const L1: &str = "/tasks/v1/lists/L1/tasks";
/// The `Date` of the mock's answers to listings…
const SERVER_DATE: &str = "Wed, 28 Oct 2026 10:00:00 GMT";
/// …and the bound of the pull after them: a minute earlier.
const NEXT_SINCE: &str = "2026-10-28T09:59:00.000Z";

fn google_tasks(fx: &Fixture, server: &MockServer) -> Google {
    let config = ClientConfig {
        retry: RetryPolicy {
            max_retries: 0,
            initial_backoff: std::time::Duration::from_millis(1),
            max_backoff: std::time::Duration::from_millis(1),
        },
        ..ClientConfig::with_api_root(&server.uri())
    };
    let client =
        GoogleClient::with_config(Arc::new(StaticToken::new("token")), config).expect("client");
    Google::new(&fx.engine, client)
}

/// A task of list L1 as Google sends it, with `extra` fields.
fn task(id: &str, title: &str, position: &str, extra: Value) -> Value {
    let mut task = json!({
        "kind": "tasks#task",
        "id": id,
        "etag": format!("\"{id}-1\""),
        "title": title,
        "updated": "2026-10-27T16:40:11.000Z",
        "selfLink": format!("https://www.googleapis.com/tasks/v1/lists/L1/tasks/{id}"),
        "position": position,
        "status": "needsAction",
        "links": [],
    });
    merge(&mut task, extra);
    task
}

/// Sets the fields of `changes` on `task`; `null` removes one.
fn merge(task: &mut Value, changes: Value) {
    let (Some(task), Value::Object(changes)) = (task.as_object_mut(), changes) else {
        return;
    };
    for (key, value) in changes {
        if value.is_null() {
            task.remove(&key);
        } else {
            task.insert(key, value);
        }
    }
}

/// A page of `tasks.list`.
fn page(tasks: &[Value]) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("Date", SERVER_DATE)
        .set_body_json(json!({ "kind": "tasks#tasks", "etag": "\"p\"", "items": tasks }))
}

fn google_error(status: u16, reason: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "error": {
            "code": status,
            "message": message,
            "errors": [{ "domain": "global", "reason": reason, "message": message }]
        }
    }))
}

fn json_body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("JSON body")
}

fn query(request: &Request) -> Vec<(String, String)> {
    request
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The lists "My Tasks" (L1, the default) and "Errands" (L2, empty).
async fn mount_lists(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(LISTS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskLists",
            "items": [
                { "kind": "tasks#taskList", "id": "L1", "title": "My Tasks" },
                { "kind": "tasks#taskList", "id": "L2", "title": "Errands" }
            ]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/@default")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "kind": "tasks#taskList", "id": "L1", "title": "My Tasks" }),
            ),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/L2/tasks"))
        .respond_with(page(&[]))
        .mount(server)
        .await;
}

/// The tasks of list L1 as the mock keeps them: writes change them and are
/// answered with the task as stored, as Google does.
#[derive(Clone, Default)]
struct Store {
    tasks: Arc<Mutex<HashMap<String, Value>>>,
    writes: Arc<AtomicUsize>,
}

impl Store {
    fn new(tasks: &[Value]) -> Store {
        let store = Store::default();
        for task in tasks {
            let id = task["id"].as_str().expect("id").to_owned();
            store.tasks.lock().insert(id, task.clone());
        }
        store
    }

    async fn mount(&self, server: &MockServer) {
        let store = self.clone();
        Mock::given(method("PATCH"))
            .and(path_regex(r"^/tasks/v1/lists/L1/tasks/[^/]+$"))
            .respond_with(move |request: &Request| {
                let id = segment(request, 1);
                store.update(&id, json_body(request))
            })
            .mount(server)
            .await;
        let store = self.clone();
        Mock::given(method("POST"))
            .and(path_regex(r"^/tasks/v1/lists/L1/tasks/[^/]+/move$"))
            .respond_with(move |request: &Request| {
                let id = segment(request, 2);
                let query: HashMap<String, String> = query(request).into_iter().collect();
                let position = store.after(query.get("previous"));
                store.update(
                    &id,
                    json!({ "parent": query.get("parent"), "position": position }),
                )
            })
            .mount(server)
            .await;
        let store = self.clone();
        Mock::given(method("POST"))
            .and(path(L1))
            .respond_with(move |request: &Request| {
                ResponseTemplate::new(200).set_body_json(store.insert(request))
            })
            .mount(server)
            .await;
        let store = self.clone();
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/tasks/v1/lists/L1/tasks/[^/]+$"))
            .respond_with(move |request: &Request| {
                match store.tasks.lock().remove(&segment(request, 1)) {
                    Some(_) => ResponseTemplate::new(204),
                    None => google_error(404, "notFound", "Not Found"),
                }
            })
            .mount(server)
            .await;
    }

    /// Stores the task an insert asks for; returns it as Google does.
    fn insert(&self, request: &Request) -> Value {
        let query: HashMap<String, String> = query(request).into_iter().collect();
        let id = format!("N{}", self.writes.fetch_add(1, Ordering::SeqCst));
        let position = self.after(query.get("previous"));
        let mut created = task(&id, "", &position, json_body(request));
        merge(&mut created, json!({ "parent": query.get("parent") }));
        self.tasks.lock().insert(id, created.clone());
        created
    }

    /// Answers inserts only after a while, to change tasks meanwhile.
    async fn insert_slowly(&self, server: &MockServer) {
        let store = self.clone();
        Mock::given(method("POST"))
            .and(path(L1))
            .respond_with(move |request: &Request| {
                ResponseTemplate::new(200)
                    .set_body_json(store.insert(request))
                    .set_delay(std::time::Duration::from_millis(300))
            })
            .with_priority(1)
            .mount(server)
            .await;
    }

    fn update(&self, id: &str, changes: Value) -> ResponseTemplate {
        let write = self.writes.fetch_add(1, Ordering::SeqCst);
        let mut tasks = self.tasks.lock();
        let Some(task) = tasks.get_mut(id) else {
            return google_error(404, "notFound", "Not Found");
        };
        merge(task, changes);
        task["etag"] = json!(format!("\"{id}-w{write}\""));
        ResponseTemplate::new(200).set_body_json(task.clone())
    }

    /// The position right after task `previous` (first without one).
    fn after(&self, previous: Option<&String>) -> String {
        let at = previous
            .and_then(|p| self.tasks.lock().get(p).cloned())
            .and_then(|t| t["position"].as_str().and_then(|p| p.parse::<u64>().ok()))
            .map_or(500, |p| p + 1);
        format!("{at:020}")
    }

    fn get(&self, id: &str) -> Value {
        self.tasks.lock()[id].clone()
    }
}

/// The `n`-th path segment from the end (1: the last).
fn segment(request: &Request, n: usize) -> String {
    let segments: Vec<&str> = request.url.path().split('/').collect();
    segments[segments.len() - n].to_owned()
}

fn ms(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("instant")
        .timestamp_millis()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_tasks_pull_push_and_pull_again() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    let first = [
        task(
            "T1",
            "Plan the trip",
            "00000000000000001000",
            json!({ "notes": "Somewhere warm\r\n", "due": "2030-06-01T00:00:00.000Z" }),
        ),
        task(
            "T2",
            "Book flights",
            "00000000000000001000",
            json!({ "parent": "T1", "status": "completed", "completed": "2030-05-01T10:00:00.000Z" }),
        ),
        task("T3", "Pay rent", "00000000000000002000", json!({})),
        task("T4", "Old idea", "00000000000000003000", json!({})),
        // Completed in a Google Tasks app, which clears it from the list.
        task(
            "T5",
            "Cleared",
            "00000000000000004000",
            json!({ "status": "completed", "completed": "2026-10-01T08:00:00.000Z", "hidden": true }),
        ),
    ];
    let store = Store::new(&first);
    store.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(L1))
        .and(query_param_is_missing("updatedMin"))
        .and(query_param("showCompleted", "true"))
        .and(query_param("showHidden", "true"))
        .and(query_param("showDeleted", "false"))
        .respond_with(page(&first))
        .expect(1)
        .mount(&server)
        .await;

    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("first sync");
    assert!(changes.lists);
    let l1 = list_named(&fx, &account, "My Tasks");
    let l2 = list_named(&fx, &account, "Errands");
    assert_eq!(changes.tasks, [l1.id]);
    assert!(l1.is_default && !l2.is_default);
    assert_eq!(l1.kind, TaskListKind::Google);
    assert_eq!(
        l1.sync_state.cursor,
        Some(json!({ "state": "done", "since": "2026-10-28T09:59:00Z" })),
        "the next pull lists what changed since a minute before this one, by Google's clock"
    );
    assert_eq!(fx.rows(l1.id).len(), 5);
    let trip = remote_row(&fx, l1.id, "T1");
    assert_eq!(
        (trip.title.as_str(), trip.notes.as_str()),
        ("Plan the trip", "Somewhere warm")
    );
    assert_eq!(
        (trip.due_date.as_deref(), trip.due_at),
        (Some("2030-06-01"), None)
    );
    assert!(!trip.important);
    let flights = remote_row(&fx, l1.id, "T2");
    assert_eq!(flights.parent_id, Some(trip.id));
    assert_eq!(
        flights.completed_at,
        Some(ms("2030-05-01T10:00:00Z")),
        "completed when Google says"
    );
    let rent = remote_row(&fx, l1.id, "T3");
    let old = remote_row(&fx, l1.id, "T4");
    assert!(remote_row(&fx, l1.id, "T5").completed_at.is_some());
    assert_eq!(
        order(&fx, l1.id, None),
        ["Plan the trip", "Pay rent", "Old idea", "Cleared"],
        "Google's order"
    );

    // Local changes of every kind.
    fx.engine
        .update_task(trip.id, input(l1.id, "Plan the summer trip"))
        .await
        .expect("rename");
    fx.engine
        .complete_task(rent.id, true)
        .await
        .expect("complete");
    fx.engine
        .reorder_task(rent.id, Some(trip.id))
        .await
        .expect("to the top");
    fx.engine
        .complete_task(flights.id, false)
        .await
        .expect("reopen");
    fx.engine.delete_task(old.id).await.expect("delete");
    let pack = fx
        .create(TaskInput {
            parent_id: Some(Some(trip.id)),
            ..input(l1.id, "Pack")
        })
        .await;
    let plants = fx
        .create(TaskInput {
            due_date: Some(Some("2030-07-01".into())),
            notes: Some("Both balconies".into()),
            // Google Tasks has no importance.
            important: Some(true),
            ..input(l1.id, "Water the plants")
        })
        .await;
    assert!(pack.pending && plants.pending);
    assert!(!plants.important);

    undo_time_over(&fx);
    sync_account(&fx.engine, &account, &service, false)
        .await
        .expect("push");
    assert!(
        fx.rows(l1.id).iter().all(|r| r.local_state == tdb::SYNCED),
        "everything is on the server"
    );
    assert!(fx.engine.db.read(|c| tdb::get_task(c, old.id)).is_err());
    let requests = server.received_requests().await.expect("requests");
    let patches: HashMap<String, Value> = requests
        .iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .map(|r| (segment(r, 1), json_body(r)))
        .collect();
    assert_eq!(patches.len(), 3);
    assert_eq!(
        patches["T1"],
        json!({ "title": "Plan the summer trip" }),
        "only what changed"
    );
    assert_eq!(patches["T3"]["status"], "completed");
    assert!(patches["T3"]["completed"].is_string(), "{}", patches["T3"]);
    assert_eq!(patches["T3"].as_object().map(|o| o.len()), Some(2));
    assert_eq!(
        patches["T2"],
        json!({ "status": "needsAction", "completed": null }),
        "reopening clears the completion time"
    );
    let moves = requests_to(&requests, "POST", "/move");
    assert_eq!(moves.len(), 1);
    assert_eq!(segment(moves[0], 2), "T3");
    assert!(query(moves[0]).is_empty(), "to the first place, top level");
    let deletes = requests_to(&requests, "DELETE", "/T4");
    assert_eq!(deletes.len(), 1);
    let inserts = requests_to(&requests, "POST", L1);
    assert_eq!(inserts.len(), 2, "tasks first, then subtasks");
    assert_eq!(
        json_body(inserts[0]),
        json!({
            "title": "Water the plants",
            "notes": "Both balconies",
            "due": "2030-07-01T00:00:00.000Z",
            "status": "needsAction"
        })
    );
    assert_eq!(
        query(inserts[0]),
        pairs(&[("previous", "T1")]),
        "after the task before it that Google shows (not the hidden one)"
    );
    assert_eq!(
        json_body(inserts[1]),
        json!({ "title": "Pack", "status": "needsAction" })
    );
    assert_eq!(
        query(inserts[1]),
        pairs(&[("parent", "T1"), ("previous", "T2")])
    );

    // What the server answered is stored. Created and moved tasks keep
    // their place here until the next pull brings Google's positions.
    let pack = fx.row(pack.id);
    assert_eq!(pack.parent_id, Some(trip.id));
    assert_eq!(pack.parent_uid.as_deref(), Some("T1"));
    assert_eq!(pack.uid, pack.remote_id.clone().expect("remote id"));
    let rent = fx.row(rent.id);
    assert!(rent.completed_at.is_some());
    assert_eq!(rent.etag, None, "listed as changed by the next pull");
    assert_eq!(
        order(&fx, l1.id, None),
        [
            "Pay rent",
            "Plan the summer trip",
            "Cleared",
            "Water the plants"
        ]
    );
    assert_eq!(order(&fx, l1.id, Some(trip.id)), ["Book flights", "Pack"]);
    assert!(fx.row(flights.id).completed_at.is_none());

    // What changed on the server since comes in, deletions included.
    server.verify().await;
    server.reset().await;
    mount_lists(&server).await;
    let plants_there = store.get(&fx.row(plants.id).remote_id.expect("remote id"));
    let pack_there = store.get(&pack.remote_id.expect("remote id"));
    Mock::given(method("GET"))
        .and(path(L1))
        .and(query_param("updatedMin", NEXT_SINCE))
        .and(query_param("showDeleted", "true"))
        .respond_with(page(&[
            task(
                "T1",
                "Plan the autumn trip",
                "00000000000000001000",
                json!({ "etag": "\"T1-9\"", "notes": "Somewhere warm", "due": "2030-06-01T00:00:00.000Z" }),
            ),
            task(
                "T3",
                "Pay rent",
                "00000000000000000500",
                json!({ "deleted": true, "etag": "\"T3-9\"" }),
            ),
            // Created here: now with Google's positions.
            plants_there,
            pack_there,
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("second sync");
    assert!(!changes.lists);
    assert_eq!(changes.tasks, [l1.id]);
    assert_eq!(fx.row(trip.id).title, "Plan the autumn trip");
    assert!(
        fx.engine.db.read(|c| tdb::get_task(c, rent.id)).is_err(),
        "deleted there"
    );
    assert_eq!(
        order(&fx, l1.id, None),
        ["Plan the autumn trip", "Water the plants", "Cleared"],
        "Google's order"
    );
    assert_eq!(order(&fx, l1.id, Some(trip.id)), ["Book flights", "Pack"]);
    assert_eq!(fx.row(plants.id).position, 1001);
    assert_eq!(fx.rows(l1.id).len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_moves_rebuild_the_order_made_here() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l1 = list_named(&fx, &account, "My Tasks");
    let first = [
        task("C", "C", "00000000000000001000", json!({})),
        task("B", "B", "00000000000000002000", json!({})),
        task("A", "A", "00000000000000003000", json!({})),
    ];
    let store = Store::new(&first);
    store.mount(&server).await;
    let listed = store.clone();
    Mock::given(method("GET"))
        .and(path(L1))
        .respond_with(move |_: &Request| {
            // In Google's order.
            let mut tasks: Vec<Value> = listed.tasks.lock().values().cloned().collect();
            tasks.sort_by_key(|t| t["position"].as_str().map(str::to_owned));
            page(&tasks)
        })
        .mount(&server)
        .await;
    pull_list(&fx.engine, &service, &l1).await.expect("pull");
    let [a, b, c] = ["A", "B", "C"].map(|id| remote_row(&fx, l1.id, id).id);
    assert_eq!(order(&fx, l1.id, None), ["C", "B", "A"]);

    // Turned around in two moves, the second relative to the first.
    fx.engine.reorder_task(a, Some(c)).await.expect("move");
    fx.engine.reorder_task(b, Some(c)).await.expect("move");
    assert_eq!(order(&fx, l1.id, None), ["A", "B", "C"]);
    push_list(&fx.engine, &service, &l1).await.expect("push");

    let mut there: Vec<(String, String)> = store
        .tasks
        .lock()
        .values()
        .map(|t| {
            (
                t["position"].as_str().unwrap_or_default().to_owned(),
                t["title"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    there.sort();
    assert_eq!(
        there
            .into_iter()
            .map(|(_, title)| title)
            .collect::<Vec<_>>(),
        ["A", "B", "C"],
        "the same order there"
    );
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(
        requests_to(&requests, "POST", "/move").len(),
        2,
        "only the tasks moved here"
    );
    assert_eq!(order(&fx, l1.id, None), ["A", "B", "C"]);
    // The next pull brings Google's positions for the moved tasks.
    pull_list(&fx.engine, &service, &list_named(&fx, &account, "My Tasks"))
        .await
        .expect("pull");
    assert_eq!(order(&fx, l1.id, None), ["A", "B", "C"]);
    assert_eq!([a, b, c].map(|id| fx.row(id).position), [500, 501, 1000]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_expired_change_listings_list_everything_again() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l1 = list_named(&fx, &account, "My Tasks");
    Mock::given(method("GET"))
        .and(path(L1))
        .respond_with(page(&[task(
            "T9",
            "Gone meanwhile",
            "00000000000000000001",
            json!({}),
        )]))
        .expect(1)
        .mount(&server)
        .await;
    pull_list(&fx.engine, &service, &l1).await.expect("pull");
    server.verify().await;

    // Too long since the last pull for Google to list the changes.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(L1))
        .and(query_param("updatedMin", NEXT_SINCE))
        .respond_with(google_error(
            410,
            "fullSyncRequired",
            "A full listing is required.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(L1))
        .and(query_param_is_missing("updatedMin"))
        .respond_with(page(&[
            task("T1", "Plan the trip", "00000000000000000001", json!({})),
            task(
                "T2",
                "Book flights",
                "00000000000000000001",
                json!({ "parent": "T1" }),
            ),
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let l1 = list_named(&fx, &account, "My Tasks");
    assert!(pull_list(&fx.engine, &service, &l1).await.expect("pull"));
    let rows = fx.rows(l1.id);
    assert_eq!(
        rows.iter()
            .map(|r| r.remote_id.as_deref().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["T1", "T2"],
        "what the full listing left out is gone"
    );
    assert_eq!(rows[1].parent_id, Some(rows[0].id));
    let l1 = list_named(&fx, &account, "My Tasks");
    assert_eq!(
        l1.sync_state.cursor,
        Some(json!({ "state": "done", "since": "2026-10-28T09:59:00Z" }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_subtasks_wait_for_their_task() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l1 = list_named(&fx, &account, "My Tasks");
    let trip = fx.create(input(l1.id, "Plan the trip")).await;
    let flights = fx
        .create(TaskInput {
            parent_id: Some(Some(trip.id)),
            ..input(l1.id, "Book flights")
        })
        .await;
    // Google refuses the task; its subtask would have nowhere to go.
    Mock::given(method("POST"))
        .and(path(L1))
        .respond_with(google_error(400, "invalid", "Invalid value."))
        .mount(&server)
        .await;

    push_list(&fx.engine, &service, &l1).await.expect("push");
    assert_eq!(fx.row(trip.id).push_attempts, 1);
    let waiting = fx.row(flights.id);
    assert_eq!(
        (waiting.push_attempts, waiting.push_error.as_deref()),
        (0, None),
        "the subtask waits for its task without counting attempts"
    );
    // Refused until Envelope gives up on the task: it goes to the list on
    // this computer, and its subtask with it.
    for _ in 1..MAX_ATTEMPTS {
        retry_now(&fx, l1.id);
        push_list(&fx.engine, &service, &l1).await.expect("push");
    }
    let local = fx.local_list();
    let kept = fx.engine.task(trip.id).await.expect("task");
    assert_eq!((kept.list_id, kept.pending), (local, false));
    let kids = fx.engine.task_subtasks(trip.id).await.expect("subtasks");
    assert_eq!(
        kids.iter()
            .map(|t| (t.id, t.list_id, t.pending))
            .collect::<Vec<_>>(),
        [(flights.id, local, false)]
    );
    assert!(fx.rows(l1.id).is_empty());
    let requests = server.received_requests().await.expect("requests");
    let inserts = requests_to(&requests, "POST", L1);
    assert_eq!(inserts.len(), MAX_ATTEMPTS as usize);
    assert!(
        inserts.iter().all(|r| query(r).is_empty()),
        "only the task was tried"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_tasks_deleted_while_uploaded_go_from_the_server_too() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l1 = list_named(&fx, &account, "My Tasks");
    let store = Store::default();
    store.mount(&server).await;
    store.insert_slowly(&server).await;
    let call = fx.create(input(l1.id, "Call Maya")).await;

    // Deleted while Google is creating it.
    let pushing = {
        let (engine, list) = (fx.engine.clone(), l1.clone());
        tokio::spawn(async move { push_list(&engine, &service, &list).await })
    };
    until_requested(&server, "POST", L1).await;
    fx.engine.delete_task(call.id).await.expect("delete");
    pushing.await.expect("join").expect("push");
    let row = fx.row(call.id);
    assert_eq!(
        (row.local_state, row.remote_id.as_deref()),
        (tdb::DELETED, Some("N0")),
        "kept to be deleted where it was made"
    );
    assert!(fx.titles(TaskView::All, false).await.is_empty());
    retry_now(&fx, l1.id);
    push_list(&fx.engine, &google_tasks(&fx, &server), &l1)
        .await
        .expect("push");
    assert!(store.tasks.lock().is_empty(), "deleted on the server too");
    assert!(fx.rows(l1.id).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_deletions_wait_while_they_can_be_undone() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l1 = list_named(&fx, &account, "My Tasks");
    let first = [
        task("T1", "Plan the trip", "00000000000000001000", json!({})),
        task(
            "T2",
            "Book flights",
            "00000000000000001000",
            json!({ "parent": "T1" }),
        ),
        task(
            "T3",
            "Find a hotel",
            "00000000000000002000",
            json!({ "parent": "T1" }),
        ),
    ];
    let store = Store::new(&first);
    store.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(L1))
        .respond_with(page(&first))
        .mount(&server)
        .await;
    pull_list(&fx.engine, &service, &l1).await.expect("pull");
    let [trip, flights, hotel] = ["T1", "T2", "T3"].map(|id| remote_row(&fx, l1.id, id).id);
    // A change and a new subtask, not uploaded yet.
    fx.engine
        .update_task(flights, input(l1.id, "Book the flights"))
        .await
        .expect("rename");
    let pack = fx
        .create(TaskInput {
            parent_id: Some(Some(trip)),
            ..input(l1.id, "Pack")
        })
        .await;

    // While the deletion can be undone, nothing reaches Google.
    fx.engine.delete_task(trip).await.expect("delete");
    assert!(fx.titles(TaskView::All, false).await.is_empty());
    push_list(&fx.engine, &service, &l1).await.expect("push");
    let writes = |server_requests: &[Request]| {
        server_requests
            .iter()
            .filter(|r| r.method.as_str() != "GET")
            .count()
    };
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(writes(&requests), 0);

    // Brought back as they were: the task still synced, the changes still
    // to be uploaded.
    let back = fx.engine.restore_task(trip).await.expect("restore");
    assert!(!back.pending);
    let pending = |id| fx.row(id).local_state;
    assert_eq!(
        [trip, flights, hotel, pack.id].map(pending),
        [tdb::SYNCED, tdb::MODIFIED, tdb::SYNCED, tdb::CREATED]
    );
    push_list(&fx.engine, &service, &l1).await.expect("push");
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(
        json_body(requests_to(&requests, "PATCH", "/T2")[0]),
        json!({ "title": "Book the flights" })
    );
    let inserts = requests_to(&requests, "POST", L1);
    assert_eq!(inserts.len(), 1);
    assert_eq!(query(inserts[0])[0], ("parent".to_owned(), "T1".to_owned()));
    assert!(requests_to(&requests, "DELETE", "").is_empty());

    // Deleted again: carried out once the time to undo it is over.
    fx.engine.delete_task(trip).await.expect("delete");
    undo_time_over(&fx);
    push_list(&fx.engine, &service, &l1).await.expect("push");
    assert!(store.tasks.lock().is_empty(), "{:?}", store.tasks.lock());
    assert!(fx.rows(l1.id).is_empty());
    let late = fx.engine.restore_task(trip).await;
    assert!(matches!(late, Err(Error::Invalid(_))), "{late:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_lists_deleted_there_keep_what_was_not_uploaded() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l2 = list_named(&fx, &account, "Errands");
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/L2/tasks"))
        .respond_with(page(&[
            task(
                "E1",
                "Renew the passport",
                "00000000000000001000",
                json!({}),
            ),
            task("E2", "Call the bank", "00000000000000002000", json!({})),
            task(
                "E3",
                "Find the account number",
                "00000000000000001000",
                json!({ "parent": "E2" }),
            ),
        ]))
        .with_priority(1)
        .mount(&server)
        .await;
    pull_list(&fx.engine, &service, &l2).await.expect("pull");
    let passport = remote_row(&fx, l2.id, "E1");
    let bank = remote_row(&fx, l2.id, "E2");
    fx.engine
        .update_task(bank.id, input(l2.id, "Call the bank on Monday"))
        .await
        .expect("edit");
    let stamps = fx.create(input(l2.id, "Buy stamps")).await;
    fx.create(TaskInput {
        parent_id: Some(Some(stamps.id)),
        ..input(l2.id, "Forever stamps")
    })
    .await;

    // Deleted on another device: Google has only "My Tasks" now.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(LISTS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskLists",
            "items": [{ "kind": "tasks#taskList", "id": "L1", "title": "My Tasks" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/@default")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "kind": "tasks#taskList", "id": "L1", "title": "My Tasks" }),
            ),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(L1))
        .respond_with(page(&[]))
        .mount(&server)
        .await;
    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("sync");
    assert!(changes.lists);
    assert!(fx.engine.db.read(|c| tdb::get_list(c, l2.id)).is_err());
    // What the server never heard about is on this computer now, with the
    // subtasks; the rest went with the list.
    let local = fx.local_list();
    assert_eq!(
        fx.titles(TaskView::List(local), false).await,
        ["Call the bank on Monday", "Buy stamps"]
    );
    for (task, subtask) in [
        (bank.id, "Find the account number"),
        (stamps.id, "Forever stamps"),
    ] {
        let kids = fx.engine.task_subtasks(task).await.expect("subtasks");
        assert_eq!(
            kids.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            [subtask]
        );
    }
    assert!(
        fx.rows(local)
            .iter()
            .all(|r| r.local_state == tdb::SYNCED && r.remote_id.is_none()),
        "tasks of this computer now"
    );
    assert!(
        fx.engine
            .db
            .read(|c| tdb::get_task(c, passport.id))
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_takes_titles_and_notes_up_to_its_limits() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let service = google_tasks(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let l1 = list_named(&fx, &account, "My Tasks");
    let local = fx.local_list();
    let refused = |result: Result<Task>, what: &str| match result {
        Err(Error::Invalid(message)) => assert!(message.contains(what), "{message}"),
        other => panic!("{other:?}"),
    };

    let longest = "a".repeat(1024);
    let task = fx.create(input(l1.id, &longest)).await;
    refused(
        fx.engine.create_task(input(l1.id, &"a".repeat(1025))).await,
        "title",
    );
    refused(
        fx.engine
            .update_task(
                task.id,
                TaskInput {
                    notes: Some("n".repeat(8193)),
                    ..input(l1.id, "Short")
                },
            )
            .await,
        "notes",
    );
    assert_eq!(fx.row(task.id).title, longest, "nothing changed");
    // Fine on this computer, but not moved to Google, subtasks included.
    let report = fx.create(input(local, "Write the report")).await;
    fx.create(TaskInput {
        parent_id: Some(Some(report.id)),
        notes: Some("n".repeat(9000)),
        ..input(local, "Collect the numbers")
    })
    .await;
    refused(
        fx.engine
            .update_task(report.id, input(l1.id, "Write the report"))
            .await,
        "notes",
    );
    assert_eq!(fx.row(report.id).list_id, local);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_accounts_without_tasks_access_are_passed_over() {
    let fx = fixture();
    // Signed in before Envelope asked for tasks.
    let before = oauth_account(
        &fx,
        AccountKind::Gmail,
        "https://mail.google.com/ https://www.googleapis.com/auth/calendar openid email",
    );
    assert!(
        service(&fx.engine, &before)
            .await
            .expect("service")
            .is_none()
    );
    assert!(matches!(
        fx.engine.create_task_list(Some(before.id), "Errands").await,
        Err(Error::Invalid(_))
    ));
    let granted = oauth_account(&fx, AccountKind::Gmail, TASKS_SCOPE);
    let found = service(&fx.engine, &granted)
        .await
        .expect("service")
        .expect("a task service");
    assert_eq!(found.kind(), TaskListKind::Google);

    // The Tasks API turned off for Envelope's Google client: nothing to sync,
    // and nothing for the scheduler to report.
    let server = MockServer::start().await;
    let service = google_tasks(&fx, &server);
    Mock::given(method("GET"))
        .and(path(LISTS))
        .respond_with(google_error(
            403,
            "accessNotConfigured",
            "Google Tasks API has not been used in project 1 before or it is disabled.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let synced = sync_account(&fx.engine, &granted, &service, true).await;
    assert!(matches!(synced, Err(Error::Invalid(_))), "{synced:?}");
    assert!(
        fx.engine
            .db
            .read(|c| tdb::account_lists(c, granted.id))
            .expect("lists")
            .is_empty()
    );

    // A change refused for missing access waits for it; it is not undone.
    server.reset().await;
    mount_lists(&server).await;
    store_lists(&fx, &granted, &service).await;
    let l1 = list_named(&fx, &granted, "My Tasks");
    let created = fx.create(input(l1.id, "Call Maya")).await;
    Mock::given(method("POST"))
        .and(path(L1))
        .respond_with(google_error(
            403,
            "insufficientPermissions",
            "Request had insufficient authentication scopes.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let pushed = sync_account(&fx.engine, &granted, &service, false).await;
    assert!(matches!(pushed, Err(Error::Network(_))), "{pushed:?}");
    let row = fx.row(created.id);
    assert_eq!((row.list_id, row.local_state), (l1.id, tdb::CREATED));
    assert!(row.push_error.is_some());
    assert!(!list_named(&fx, &granted, "My Tasks").read_only);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_lists_are_created_renamed_and_deleted() {
    let fx = fixture();
    let server = MockServer::start().await;
    let service = google_tasks(&fx, &server);
    Mock::given(method("POST"))
        .and(path(LISTS))
        .and(body_json(json!({ "title": "Errands" })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "kind": "tasks#taskList", "id": "L3", "title": "Errands" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let created = service.create_list("Errands").await.expect("create");
    assert_eq!(
        (
            created.remote_id.as_str(),
            created.name.as_str(),
            created.is_default
        ),
        ("L3", "Errands", false)
    );

    Mock::given(method("PATCH"))
        .and(path(format!("{LISTS}/L3")))
        .and(body_json(json!({ "title": "Chores" })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "kind": "tasks#taskList", "id": "L3", "title": "Chores" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let list = stored_list(TaskListKind::Google, "L3", false);
    service.rename_list(&list, "Chores").await.expect("rename");
    Mock::given(method("DELETE"))
        .and(path(format!("{LISTS}/L3")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    service.delete_list(&list).await.expect("delete");
    // Already gone: nothing left to do.
    Mock::given(method("DELETE"))
        .and(path(format!("{LISTS}/L4")))
        .respond_with(google_error(404, "notFound", "Not Found"))
        .mount(&server)
        .await;
    service
        .delete_list(&stored_list(TaskListKind::Google, "L4", false))
        .await
        .expect("gone");
}
