//! Microsoft To Do against a mock of Microsoft Graph: lists (without
//! "Flagged email"), task deltas with their steps fetched in batches, every
//! kind of upload, steps as subtasks (checked, renamed, added, deleted,
//! turned into tasks and back), expired delta links, the daily pull from
//! scratch, list writes, and accounts whose sign-in does not cover tasks.

use std::collections::{BTreeMap, HashMap};

use envelope_graph::{GraphClient, RetryPolicy, StaticToken};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, query_param, query_param_is_missing};

use super::*;
use crate::sync::tasks::graph::Graph;

const TASKS_SCOPE: &str = "openid profile offline_access User.Read Mail.ReadWrite \
                           https://graph.microsoft.com/Tasks.ReadWrite";
const LISTS: &str = "/v1.0/me/todo/lists";

fn todo_service(fx: &Fixture, server: &MockServer) -> Graph {
    let client = GraphClient::builder(Arc::new(StaticToken::new("token")))
        .base_url(format!("{}/v1.0", server.uri()))
        .retry_policy(RetryPolicy::none())
        .build()
        .expect("client");
    Graph::new(&fx.engine, client)
}

/// A To Do task as Graph sends it, with `extra` properties.
fn todo_task(id: &str, title: &str, extra: Value) -> Value {
    let mut task = json!({
        "@odata.etag": format!("W/\"{id}-1\""),
        "id": id,
        "title": title,
        "importance": "normal",
        "isReminderOn": false,
        "status": "notStarted",
        "createdDateTime": "2026-10-20T07:12:00.0000000Z",
        "lastModifiedDateTime": "2026-10-27T09:30:00.0000000Z",
        "hasAttachments": false,
        "categories": [],
        "body": { "content": "", "contentType": "text" }
    });
    merge(&mut task, extra);
    task
}

fn step(id: &str, name: &str, checked: bool) -> Value {
    let checked_at = if checked {
        json!("2026-10-21T08:00:00.0000000Z")
    } else {
        Value::Null
    };
    json!({
        "id": id,
        "displayName": name,
        "isChecked": checked,
        "createdDateTime": "2026-10-20T07:12:00.0000000Z",
        "checkedDateTime": checked_at
    })
}

/// Sets the properties of `changes` on `item`.
fn merge(item: &mut Value, changes: Value) {
    if let (Some(item), Value::Object(changes)) = (item.as_object_mut(), changes) {
        item.extend(changes);
    }
}

fn graph_error(status: u16, code: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({ "error": { "code": code, "message": format!("{code}.") } }))
}

fn json_body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("JSON body")
}

/// The last delta page of a round of `list`, ending in delta token `token`.
fn delta(server: &MockServer, list: &str, token: &str, items: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "value": items,
        "@odata.deltaLink": format!(
            "{}{LISTS}/{list}/tasks/delta?$deltatoken={token}",
            server.uri()
        )
    }))
}

/// "Tasks" (L1, the default), "Flagged email" and "Groceries" (L2, empty).
async fn mount_lists(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(LISTS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                { "id": "L1", "displayName": "Tasks", "isOwner": true, "isShared": false,
                  "wellknownListName": "defaultList" },
                { "id": "FLAGGED", "displayName": "Flagged email", "isOwner": true,
                  "isShared": false, "wellknownListName": "flaggedEmails" },
                { "id": "L2", "displayName": "Groceries", "isOwner": true, "isShared": false,
                  "wellknownListName": "none" }
            ]
        })))
        .mount(server)
        .await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L2/tasks/delta")))
        .respond_with(move |_: &Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "value": [],
                "@odata.deltaLink": format!("{base}{LISTS}/L2/tasks/delta?$deltatoken=g")
            }))
        })
        .mount(server)
        .await;
}

#[derive(Default)]
struct TodoState {
    /// Tasks of list L1 by id.
    tasks: BTreeMap<String, Value>,
    /// Steps of each task, in order.
    steps: HashMap<String, Vec<Value>>,
    writes: usize,
}

/// List L1 as the mock keeps it: batched step listings read it, writes
/// change it and are answered with what is stored, as Graph does.
#[derive(Clone, Default)]
struct Todo(Arc<Mutex<TodoState>>);

impl Todo {
    fn new(tasks: Vec<(Value, Vec<Value>)>) -> Todo {
        let todo = Todo::default();
        for (task, steps) in tasks {
            todo.put(task, steps);
        }
        todo
    }

    fn put(&self, task: Value, steps: Vec<Value>) {
        let id = task["id"].as_str().expect("id").to_owned();
        let mut state = self.0.lock();
        state.tasks.insert(id.clone(), task);
        state.steps.insert(id, steps);
    }

    fn remove(&self, id: &str) {
        let mut state = self.0.lock();
        state.tasks.remove(id);
        state.steps.remove(id);
    }

    fn tasks(&self) -> Vec<Value> {
        self.0.lock().tasks.values().cloned().collect()
    }

    fn task(&self, id: &str) -> Value {
        self.0.lock().tasks[id].clone()
    }

    fn step_names(&self, task: &str) -> Vec<String> {
        self.0.lock().steps[task]
            .iter()
            .map(|s| s["displayName"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    async fn mount(&self, server: &MockServer) {
        let todo = self.clone();
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .respond_with(move |request: &Request| {
                let state = todo.0.lock();
                let responses: Vec<Value> = json_body(request)["requests"]
                    .as_array()
                    .expect("requests")
                    .iter()
                    .map(|r| {
                        let url = r["url"].as_str().unwrap_or_default();
                        let task = url.split('/').nth_back(1).unwrap_or_default();
                        match state.steps.get(task) {
                            Some(steps) => json!({
                                "id": r["id"], "status": 200,
                                "headers": { "Content-Type": "application/json" },
                                "body": { "value": steps }
                            }),
                            None => json!({
                                "id": r["id"], "status": 404, "headers": {},
                                "body": { "error": { "code": "ErrorItemNotFound", "message": "Not found." } }
                            }),
                        }
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({ "responses": responses }))
            })
            .mount(server)
            .await;
        let todo = self.clone();
        Mock::given(method("POST"))
            .and(path(format!("{LISTS}/L1/tasks")))
            .respond_with(move |request: &Request| {
                let mut state = todo.0.lock();
                state.writes += 1;
                let id = format!("N{}", state.writes);
                let mut body = json_body(request);
                let links = body
                    .as_object_mut()
                    .and_then(|b| b.remove("linkedResources"));
                let mut task = todo_task(&id, "", body);
                if let Some(Value::Array(links)) = links {
                    let links: Vec<Value> = links
                        .into_iter()
                        .enumerate()
                        .map(|(i, mut link)| {
                            merge(&mut link, json!({ "id": format!("link{i}") }));
                            link
                        })
                        .collect();
                    merge(&mut task, json!({ "linkedResources": links }));
                }
                state.tasks.insert(id.clone(), task.clone());
                state.steps.insert(id, Vec::new());
                ResponseTemplate::new(201).set_body_json(task)
            })
            .mount(server)
            .await;
        let todo = self.clone();
        Mock::given(method("PATCH"))
            .and(path_regex(r"^/v1\.0/me/todo/lists/L1/tasks/[^/]+$"))
            .respond_with(move |request: &Request| {
                let mut state = todo.0.lock();
                state.writes += 1;
                let write = state.writes;
                let id = segment(request, 1);
                let Some(task) = state.tasks.get_mut(&id) else {
                    return graph_error(404, "ErrorItemNotFound");
                };
                merge(task, json_body(request));
                task["@odata.etag"] = json!(format!("W/\"{id}-w{write}\""));
                ResponseTemplate::new(200).set_body_json(task.clone())
            })
            .mount(server)
            .await;
        let todo = self.clone();
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/v1\.0/me/todo/lists/L1/tasks/[^/]+$"))
            .respond_with(move |request: &Request| {
                let id = segment(request, 1);
                let mut state = todo.0.lock();
                state.steps.remove(&id);
                match state.tasks.remove(&id) {
                    Some(_) => ResponseTemplate::new(204),
                    None => graph_error(404, "ErrorItemNotFound"),
                }
            })
            .mount(server)
            .await;
        let todo = self.clone();
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/v1\.0/me/todo/lists/L1/tasks/[^/]+/checklistItems$",
            ))
            .respond_with(move |request: &Request| {
                let task = segment(request, 2);
                let mut state = todo.0.lock();
                state.writes += 1;
                let body = json_body(request);
                let item = step(
                    &format!("S{}", state.writes),
                    body["displayName"].as_str().unwrap_or_default(),
                    body["isChecked"].as_bool().unwrap_or_default(),
                );
                match state.steps.get_mut(&task) {
                    Some(steps) => {
                        steps.push(item.clone());
                        ResponseTemplate::new(201).set_body_json(item)
                    }
                    None => graph_error(404, "ErrorItemNotFound"),
                }
            })
            .mount(server)
            .await;
        let todo = self.clone();
        Mock::given(method("PATCH"))
            .and(path_regex(
                r"^/v1\.0/me/todo/lists/L1/tasks/[^/]+/checklistItems/[^/]+$",
            ))
            .respond_with(move |request: &Request| {
                let (task, id) = (segment(request, 3), segment(request, 1));
                let mut state = todo.0.lock();
                let item = state
                    .steps
                    .get_mut(&task)
                    .and_then(|steps| steps.iter_mut().find(|s| s["id"] == id.as_str()));
                match item {
                    Some(item) => {
                        merge(item, json_body(request));
                        ResponseTemplate::new(200).set_body_json(item.clone())
                    }
                    None => graph_error(404, "ErrorItemNotFound"),
                }
            })
            .mount(server)
            .await;
        let todo = self.clone();
        Mock::given(method("DELETE"))
            .and(path_regex(
                r"^/v1\.0/me/todo/lists/L1/tasks/[^/]+/checklistItems/[^/]+$",
            ))
            .respond_with(move |request: &Request| {
                let (task, id) = (segment(request, 3), segment(request, 1));
                let mut state = todo.0.lock();
                let Some(steps) = state.steps.get_mut(&task) else {
                    return graph_error(404, "ErrorItemNotFound");
                };
                let before = steps.len();
                steps.retain(|s| s["id"] != id.as_str());
                if steps.len() < before {
                    ResponseTemplate::new(204)
                } else {
                    graph_error(404, "ErrorItemNotFound")
                }
            })
            .mount(server)
            .await;
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

/// Midnight of `day` in the user's zone, in UTC, as To Do wants due days.
fn local_midnight(day: &str) -> String {
    let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").expect("day");
    chrono::DateTime::from_timestamp_millis(local_at(day, 0))
        .expect("instant")
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

/// The JSON bodies of the requests made with `verb` to paths ending in
/// `suffix`.
fn bodies(requests: &[Request], verb: &str, suffix: &str) -> Vec<Value> {
    requests_to(requests, verb, suffix)
        .into_iter()
        .map(json_body)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_pull_push_and_pull_again() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Microsoft, TASKS_SCOPE);
    let service = todo_service(&fx, &server);
    mount_lists(&server).await;
    let todo = Todo::new(vec![
        (
            todo_task(
                "T1",
                "Plan the offsite",
                json!({
                    "importance": "high",
                    "body": { "content": "Agenda first", "contentType": "text" },
                    "dueDateTime": { "dateTime": "2030-06-01T00:00:00.0000000", "timeZone": "UTC" }
                }),
            ),
            vec![
                step("S1", "Book the room", false),
                step("S2", "Invite the team", true),
                step("S3", "Print badges", false),
            ],
        ),
        (
            todo_task(
                "T2",
                "Water the plants",
                json!({
                    "status": "completed",
                    "completedDateTime": { "dateTime": "2026-10-27T00:00:00.0000000", "timeZone": "UTC" },
                    "lastModifiedDateTime": "2026-10-27T15:20:00.0000000Z",
                    "recurrence": {
                        "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["tuesday"] },
                        "range": { "type": "noEnd", "startDate": "2026-10-27" }
                    }
                }),
            ),
            Vec::new(),
        ),
        (todo_task("T3", "Renew the passport", json!({})), Vec::new()),
        (todo_task("T4", "Old idea", json!({})), Vec::new()),
    ]);
    todo.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(delta(&server, "L1", "d1", todo.tasks()))
        .expect(1)
        .mount(&server)
        .await;

    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("first sync");
    assert!(changes.lists);
    let names: Vec<String> = fx
        .engine
        .db
        .read(|c| tdb::account_lists(c, account.id))
        .expect("lists")
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["Tasks", "Groceries"], "no flagged messages");
    let l1 = list_named(&fx, &account, "Tasks");
    assert!(l1.is_default);
    assert_eq!(l1.kind, TaskListKind::Graph);
    assert_eq!(changes.tasks, [l1.id]);
    assert_eq!(fx.rows(l1.id).len(), 7);
    let offsite = remote_row(&fx, l1.id, "T1");
    assert!(offsite.important);
    assert_eq!(offsite.notes, "Agenda first");
    assert_eq!(
        (offsite.due_date.as_deref(), offsite.due_at),
        (Some("2030-06-01"), None)
    );
    assert_eq!(
        order(&fx, l1.id, Some(offsite.id)),
        ["Book the room", "Invite the team", "Print badges"],
        "steps are subtasks, in their order"
    );
    let room = remote_row(&fx, l1.id, "T1|S1");
    let invite = remote_row(&fx, l1.id, "T1|S2");
    let badges = remote_row(&fx, l1.id, "T1|S3");
    assert_eq!(
        (room.parent_uid.as_deref(), room.uid.as_str()),
        (Some("T1"), "T1|S1")
    );
    assert!(room.completed_at.is_none() && invite.completed_at.is_some());
    let plants = remote_row(&fx, l1.id, "T2");
    assert!(plants.repeats);
    assert_eq!(
        plants.completed_at,
        Some(ms("2026-10-27T15:20:00Z")),
        "To Do keeps the day; the last change tells the time"
    );
    let passport = remote_row(&fx, l1.id, "T3");
    let old = remote_row(&fx, l1.id, "T4");
    let cursor = l1.sync_state.cursor.clone().expect("cursor");
    assert_eq!(
        cursor["delta"],
        json!({
            "state": "done",
            "url": format!("{}{LISTS}/L1/tasks/delta?$deltatoken=d1", server.uri())
        })
    );
    assert!(cursor["fullAt"].is_i64());

    // Local changes of every kind, to tasks and to steps.
    fx.engine
        .complete_task(offsite.id, true)
        .await
        .expect("complete");
    fx.engine
        .complete_task(plants.id, false)
        .await
        .expect("reopen");
    fx.engine
        .update_task(
            passport.id,
            TaskInput {
                important: Some(true),
                due_date: Some(Some("2030-01-15".into())),
                notes: Some("Photos first".into()),
                ..input(l1.id, "Renew both passports")
            },
        )
        .await
        .expect("edit");
    fx.engine.delete_task(old.id).await.expect("delete");
    let (message, _) = inbox_message(&fx, &account);
    let offer = fx
        .create(TaskInput {
            from_message: Some(message),
            ..input(l1.id, "")
        })
        .await;
    fx.engine.complete_task(room.id, true).await.expect("check");
    fx.engine
        .update_task(invite.id, input(l1.id, "Invite everyone"))
        .await
        .expect("rename a step");
    fx.engine
        .delete_task(badges.id)
        .await
        .expect("delete a step");
    let lunch = fx
        .create(TaskInput {
            parent_id: Some(Some(offsite.id)),
            notes: Some("Vegetarian options".into()),
            ..input(l1.id, "Order lunch")
        })
        .await;
    let old_one = fx
        .create(TaskInput {
            parent_id: Some(Some(passport.id)),
            ..input(l1.id, "Find the old one")
        })
        .await;

    sync_account(&fx.engine, &account, &service, false)
        .await
        .expect("push");
    assert!(
        fx.rows(l1.id).iter().all(|r| r.local_state == tdb::SYNCED),
        "everything is on the server"
    );
    for gone in [old.id, badges.id] {
        assert!(fx.engine.db.read(|c| tdb::get_task(c, gone)).is_err());
    }
    let requests = server.received_requests().await.expect("requests");
    let completed = bodies(&requests, "PATCH", "/tasks/T1");
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["status"], "completed");
    assert_eq!(completed[0]["completedDateTime"]["timeZone"], "UTC");
    assert_eq!(completed[0].as_object().map(|o| o.len()), Some(2));
    assert_eq!(
        bodies(&requests, "PATCH", "/tasks/T2"),
        [json!({ "status": "notStarted", "completedDateTime": null })],
        "reopened"
    );
    assert_eq!(
        bodies(&requests, "PATCH", "/tasks/T3"),
        [json!({
            "title": "Renew both passports",
            "body": { "content": "Photos first", "contentType": "text" },
            "importance": "high",
            "dueDateTime": { "dateTime": local_midnight("2030-01-15"), "timeZone": "UTC" }
        })],
        "only what changed, the day as midnight where the user is"
    );
    assert_eq!(requests_to(&requests, "DELETE", "/tasks/T4").len(), 1);
    assert_eq!(
        bodies(&requests, "POST", "/L1/tasks"),
        [json!({
            "title": "The offer",
            "importance": "normal",
            "status": "notStarted",
            "linkedResources": [{
                "applicationName": "Envelope",
                "displayName": "The offer",
                "externalId": "offer-1@northwind.example"
            }]
        })],
        "linked to the message it was made from"
    );
    assert_eq!(
        bodies(&requests, "PATCH", "/checklistItems/S1"),
        [json!({ "isChecked": true })]
    );
    assert_eq!(
        bodies(&requests, "PATCH", "/checklistItems/S2"),
        [json!({ "displayName": "Invite everyone" })]
    );
    assert_eq!(
        requests_to(&requests, "DELETE", "/T1/checklistItems/S3").len(),
        1
    );
    assert_eq!(
        bodies(&requests, "POST", "/T1/checklistItems"),
        [json!({ "displayName": "Order lunch", "isChecked": false })],
        "a step has a title and whether it is done"
    );
    assert_eq!(
        bodies(&requests, "POST", "/T3/checklistItems"),
        [json!({ "displayName": "Find the old one", "isChecked": false })]
    );
    let offer = fx.row(offer.id);
    assert!(!offer.remote_id.clone().expect("remote id").contains('|'));
    assert!(offer.source.is_some());
    let lunch = fx.row(lunch.id);
    let lunch_id = lunch.remote_id.clone().expect("remote id");
    assert!(lunch_id.starts_with("T1|S"), "{lunch_id}");
    assert_eq!(
        (lunch.parent_id, lunch.notes.as_str()),
        (Some(offsite.id), ""),
        "nothing but the title and done"
    );
    assert_eq!(
        todo.step_names("T1"),
        ["Book the room", "Invite everyone", "Order lunch"]
    );

    // Changes on the server come in by the delta link, deletions included;
    // the steps of the tasks it reports come along.
    server.verify().await;
    server.reset().await;
    mount_lists(&server).await;
    todo.mount(&server).await;
    todo.remove("T2");
    let mut renamed = todo.task("T3");
    merge(
        &mut renamed,
        json!({ "title": "Renew the passports", "@odata.etag": "W/\"T3-9\"" }),
    );
    let mut steps = todo.0.lock().steps["T3"].clone();
    steps.push(step("S99", "Book an appointment", false));
    todo.put(renamed.clone(), steps);
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .and(query_param("$deltatoken", "d1"))
        .respond_with(delta(
            &server,
            "L1",
            "d2",
            vec![
                renamed,
                json!({ "id": "T2", "@removed": { "reason": "deleted" } }),
            ],
        ))
        .expect(1)
        .mount(&server)
        .await;
    let changes = sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("second sync");
    assert!(!changes.lists);
    assert_eq!(changes.tasks, [l1.id]);
    assert_eq!(fx.row(passport.id).title, "Renew the passports");
    assert!(
        fx.engine.db.read(|c| tdb::get_task(c, plants.id)).is_err(),
        "deleted there"
    );
    assert_eq!(
        order(&fx, l1.id, Some(passport.id)),
        ["Find the old one", "Book an appointment"]
    );
    assert_eq!(fx.row(old_one.id).local_state, tdb::SYNCED);
    let l1 = list_named(&fx, &account, "Tasks");
    assert!(
        l1.sync_state.cursor.expect("cursor")["delta"]["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("$deltatoken=d2"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_steps_and_tasks_trade_places() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Microsoft, TASKS_SCOPE);
    let service = todo_service(&fx, &server);
    mount_lists(&server).await;
    let todo = Todo::new(vec![
        (
            todo_task("T1", "Trip", json!({})),
            vec![step("S1", "Book flights", false)],
        ),
        (todo_task("T2", "Pack", json!({})), Vec::new()),
    ]);
    todo.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .respond_with(delta(&server, "L1", "d1", todo.tasks()))
        .mount(&server)
        .await;
    sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("first sync");
    let l1 = list_named(&fx, &account, "Tasks");
    let trip = remote_row(&fx, l1.id, "T1");
    let flights = remote_row(&fx, l1.id, "T1|S1");
    let pack = remote_row(&fx, l1.id, "T2");

    // To Do keeps its own order: neither tasks nor steps can be moved.
    assert!(!TaskListKind::Graph.features().reorder);
    for (task, before) in [(pack.id, Some(trip.id)), (flights.id, None)] {
        let moved = fx.engine.reorder_task(task, before).await;
        assert!(matches!(moved, Err(Error::Invalid(_))), "{moved:?}");
    }

    // A step becomes a task of its own, and a task a step of another.
    fx.engine
        .update_task(
            flights.id,
            TaskInput {
                parent_id: Some(None),
                ..input(l1.id, "Book flights")
            },
        )
        .await
        .expect("promote");
    fx.engine
        .update_task(
            pack.id,
            TaskInput {
                parent_id: Some(Some(trip.id)),
                ..input(l1.id, "Pack")
            },
        )
        .await
        .expect("demote");
    sync_account(&fx.engine, &account, &service, false)
        .await
        .expect("push");

    let flights = fx.row(flights.id);
    let flights_id = flights.remote_id.clone().expect("remote id");
    assert!(!flights_id.contains('|'), "{flights_id}");
    assert_eq!(flights.parent_id, None);
    assert_eq!(todo.task(&flights_id)["title"], "Book flights");
    let pack = fx.row(pack.id);
    assert!(
        pack.remote_id
            .as_deref()
            .is_some_and(|id| id.starts_with("T1|S")),
        "{:?}",
        pack.remote_id
    );
    assert_eq!(pack.parent_id, Some(trip.id));
    assert_eq!(
        todo.step_names("T1"),
        ["Pack"],
        "the old step went, the new one is there"
    );
    assert!(
        !todo.0.lock().tasks.contains_key("T2"),
        "the task that became a step went"
    );
    assert!(fx.rows(l1.id).iter().all(|r| r.local_state == tdb::SYNCED));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_steps_wait_for_their_task() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Microsoft, TASKS_SCOPE);
    let service = todo_service(&fx, &server);
    mount_lists(&server).await;
    let todo = Todo::new(vec![(
        todo_task("T1", "Trip", json!({})),
        vec![step("S1", "Book flights", false)],
    )]);
    todo.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .respond_with(delta(&server, "L1", "d1", todo.tasks()))
        .mount(&server)
        .await;
    sync_account(&fx.engine, &account, &service, true)
        .await
        .expect("first sync");
    let l1 = list_named(&fx, &account, "Tasks");
    let flights = remote_row(&fx, l1.id, "T1|S1");
    // A new task, and the step moved under it, which To Do refuses to make.
    let visa = fx.create(input(l1.id, "Get the visa")).await;
    let added = fx
        .create(TaskInput {
            parent_id: Some(Some(visa.id)),
            ..input(l1.id, "Fill in the form")
        })
        .await;
    fx.engine
        .update_task(
            flights.id,
            TaskInput {
                parent_id: Some(Some(visa.id)),
                ..input(l1.id, "Book flights")
            },
        )
        .await
        .expect("move the step");
    Mock::given(method("POST"))
        .and(path(format!("{LISTS}/L1/tasks")))
        .respond_with(graph_error(400, "invalidRequest"))
        .with_priority(1)
        .mount(&server)
        .await;
    for _ in 0..MAX_ATTEMPTS {
        push_list(&fx.engine, &service, &l1).await.expect("push");
        for waiting in [added.id, flights.id] {
            assert_eq!(fx.row(waiting).push_attempts, 0, "steps wait");
        }
        retry_now(&fx, l1.id);
    }
    // Given up, the task is kept on this computer with both steps; the one
    // that was on the server is deleted there, as moving it asked.
    let local = fx.local_list();
    assert_eq!(fx.row(visa.id).list_id, local);
    let kids = fx.engine.task_subtasks(visa.id).await.expect("subtasks");
    assert_eq!(
        kids.iter()
            .map(|t| (t.title.as_str(), t.list_id))
            .collect::<Vec<_>>(),
        [("Fill in the form", local), ("Book flights", local)]
    );
    push_list(&fx.engine, &service, &l1).await.expect("push");
    assert!(todo.step_names("T1").is_empty());
    assert!(fx.rows(l1.id).iter().all(|r| r.local_state == tdb::SYNCED));
    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests_to(&requests, "POST", "/checklistItems").is_empty(),
        "no step went up without its task"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_expired_delta_links_and_the_daily_pull_list_everything() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = oauth_account(&fx, AccountKind::Microsoft, TASKS_SCOPE);
    let service = todo_service(&fx, &server);
    mount_lists(&server).await;
    store_lists(&fx, &account, &service).await;
    let todo = Todo::new(vec![(
        todo_task("T1", "Gone meanwhile", json!({})),
        vec![step("S1", "And its step", false)],
    )]);
    todo.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .respond_with(delta(&server, "L1", "d1", todo.tasks()))
        .expect(1)
        .mount(&server)
        .await;
    let l1 = list_named(&fx, &account, "Tasks");
    pull_list(&fx.engine, &service, &l1).await.expect("pull");
    assert_eq!(fx.rows(l1.id).len(), 2);
    server.verify().await;

    // The delta link expired: the list is listed from scratch, and what the
    // listing leaves out goes.
    server.reset().await;
    todo.remove("T1");
    todo.put(todo_task("T2", "Still here", json!({})), Vec::new());
    todo.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .and(query_param("$deltatoken", "d1"))
        .respond_with(graph_error(410, "syncStateNotFound"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(delta(&server, "L1", "d2", todo.tasks()))
        .expect(1)
        .mount(&server)
        .await;
    let l1 = list_named(&fx, &account, "Tasks");
    assert!(pull_list(&fx.engine, &service, &l1).await.expect("pull"));
    let remote_ids: Vec<Option<String>> = fx.rows(l1.id).into_iter().map(|r| r.remote_id).collect();
    assert_eq!(remote_ids, [Some("T2".to_owned())]);
    server.verify().await;

    // Once a day the list is pulled from scratch, which also brings in steps
    // changed without their task changing.
    server.reset().await;
    todo.put(
        todo_task("T2", "Still here", json!({})),
        vec![step("S5", "Added on the phone", false)],
    );
    todo.mount(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .and(query_param("$deltatoken", "d2"))
        .respond_with(delta(&server, "L1", "d3", Vec::new()))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/L1/tasks/delta")))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(delta(&server, "L1", "d3", todo.tasks()))
        .expect(1)
        .mount(&server)
        .await;
    let mut state = list_named(&fx, &account, "Tasks").sync_state;
    let mut cursor = state.cursor.take().expect("cursor");
    cursor["fullAt"] = json!(crate::db::now_ms() - 25 * 3_600_000);
    state.cursor = Some(cursor);
    fx.engine
        .db
        .write(|tx| tdb::set_list_sync_state(tx, l1.id, &state))
        .expect("state");
    let l1 = list_named(&fx, &account, "Tasks");
    assert!(pull_list(&fx.engine, &service, &l1).await.expect("pull"));
    let t2 = remote_row(&fx, l1.id, "T2");
    assert_eq!(order(&fx, l1.id, Some(t2.id)), ["Added on the phone"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_accounts_without_tasks_access_are_passed_over() {
    let fx = fixture();
    // Signed in before Envelope asked for tasks.
    let before = oauth_account(
        &fx,
        AccountKind::Microsoft,
        "openid profile offline_access User.Read Mail.ReadWrite Mail.Send Calendars.ReadWrite",
    );
    assert!(
        service(&fx.engine, &before)
            .await
            .expect("service")
            .is_none()
    );
    let granted = oauth_account(&fx, AccountKind::Microsoft, TASKS_SCOPE);
    let found = service(&fx.engine, &granted)
        .await
        .expect("service")
        .expect("a task service");
    assert_eq!(found.kind(), TaskListKind::Graph);

    // Microsoft refuses To Do for the account: nothing to sync, and nothing
    // for the scheduler to report.
    let server = MockServer::start().await;
    let service = todo_service(&fx, &server);
    Mock::given(method("GET"))
        .and(path(LISTS))
        .respond_with(graph_error(403, "ErrorAccessDenied"))
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_lists_are_created_renamed_and_deleted() {
    let fx = fixture();
    let server = MockServer::start().await;
    let service = todo_service(&fx, &server);
    Mock::given(method("POST"))
        .and(path(LISTS))
        .and(body_json(json!({ "displayName": "Holiday" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "L3", "displayName": "Holiday", "isOwner": true, "isShared": false,
            "wellknownListName": "none"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let created = service.create_list("Holiday").await.expect("create");
    assert_eq!(
        (
            created.remote_id.as_str(),
            created.name.as_str(),
            created.is_default
        ),
        ("L3", "Holiday", false)
    );

    // The built-in Tasks list keeps its name.
    let renamed = service
        .rename_list(&stored_list(TaskListKind::Graph, "L1", true), "Chores")
        .await;
    assert!(matches!(renamed, Err(Error::Invalid(_))), "{renamed:?}");
    Mock::given(method("PATCH"))
        .and(path(format!("{LISTS}/L3")))
        .and(body_json(json!({ "displayName": "Holiday 2027" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "L3", "displayName": "Holiday 2027", "wellknownListName": "none"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let list = stored_list(TaskListKind::Graph, "L3", false);
    service
        .rename_list(&list, "Holiday 2027")
        .await
        .expect("rename");
    Mock::given(method("DELETE"))
        .and(path(format!("{LISTS}/L3")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    service.delete_list(&list).await.expect("delete");
    Mock::given(method("DELETE"))
        .and(path(format!("{LISTS}/L4")))
        .respond_with(graph_error(404, "ErrorItemNotFound"))
        .mount(&server)
        .await;
    service
        .delete_list(&stored_list(TaskListKind::Graph, "L4", false))
        .await
        .expect("gone");
}
