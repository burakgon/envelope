//! Google Tasks against a local mock of the API: task list paging and
//! writes, full and incremental pulls, task writes and moves, and how
//! errors are classified.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use envelope_google::{
    ClientConfig, Error, GoogleClient, NewTask, RetryPolicy, StaticToken, TaskCursor, TaskPatch,
    TaskPosition, TaskQuery, TaskStatus, TokenSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{
    bearer_token, body_json, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LISTS: &str = "/tasks/v1/users/@me/lists";
const L1_TASKS: &str = "/tasks/v1/lists/L1/tasks";

fn client_with(server: &MockServer, tokens: Arc<dyn TokenSource>) -> GoogleClient {
    let config = ClientConfig {
        retry: RetryPolicy {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(4),
        },
        ..ClientConfig::with_api_root(&server.uri())
    };
    GoogleClient::with_config(tokens, config).expect("client builds")
}

fn client(server: &MockServer) -> GoogleClient {
    client_with(server, Arc::new(StaticToken::new("test-token")))
}

fn google_error(status: u16, reason: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "error": {
            "errors": [{ "domain": "global", "reason": reason, "message": message }],
            "code": status,
            "message": message
        }
    }))
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .expect("instant")
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("date")
}

fn task_json(id: &str, title: &str) -> Value {
    json!({
        "kind": "tasks#task",
        "id": id,
        "etag": format!("\"etag-{id}\""),
        "title": title,
        "updated": "2026-10-27T16:40:11.000Z",
        "selfLink": format!("https://www.googleapis.com/tasks/v1/lists/L1/tasks/{id}"),
        "position": "00000000000000000000",
        "status": "needsAction",
        "links": [],
        "webViewLink": format!("https://tasks.google.com/task/{id}")
    })
}

async fn requests_to(server: &MockServer, http_method: &str, request_path: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("request recording is enabled")
        .into_iter()
        .filter(|r| r.method.as_str() == http_method && r.url.path() == request_path)
        .collect()
}

fn query(request: &Request, key: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// A token source that hands out `stale` until invalidated, then `fresh`.
struct RefreshingToken {
    current: Mutex<String>,
    invalidations: AtomicUsize,
}

#[async_trait]
impl TokenSource for RefreshingToken {
    async fn access_token(&self) -> Result<String, String> {
        Ok(self.current.lock().map_err(|e| e.to_string())?.clone())
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut current) = self.current.lock() {
            *current = "fresh".to_owned();
        }
    }
}

#[tokio::test]
async fn task_lists_are_paged_created_renamed_and_deleted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(LISTS))
        .and(query_param("maxResults", "1000"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskLists",
            "etag": "\"lists-1\"",
            "nextPageToken": "lp2",
            "items": [{
                "kind": "tasks#taskList",
                "id": "L1",
                "etag": "\"l1\"",
                "title": "My Tasks",
                "updated": "2026-10-27T16:40:11.512Z",
                "selfLink": "https://www.googleapis.com/tasks/v1/users/@me/lists/L1"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(LISTS))
        .and(query_param("pageToken", "lp2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskLists",
            "items": [
                {"kind": "tasks#taskList", "id": "L2", "title": "Groceries"},
                {"kind": "tasks#taskList", "title": "no id"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{LISTS}/@default")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskList", "id": "L1", "title": "My Tasks"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(LISTS))
        .and(body_json(json!({"title": "Work"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskList", "id": "L3", "etag": "\"l3\"", "title": "Work",
            "updated": "2026-10-28T08:00:00.000Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{LISTS}/L3")))
        .and(body_json(json!({"title": "Work & errands"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#taskList", "id": "L3", "etag": "\"l3b\"", "title": "Work & errands"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{LISTS}/L3")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{LISTS}/gone")))
        .respond_with(google_error(410, "deleted", "Resource has been deleted"))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let lists = client.list_task_lists().await.expect("lists");
    let titles: Vec<&str> = lists.iter().map(|l| l.title.as_str()).collect();
    assert_eq!(
        titles,
        ["My Tasks", "Groceries"],
        "lists without id are skipped"
    );
    assert_eq!(lists[0].etag.as_deref(), Some("\"l1\""));
    assert_eq!(
        lists[0].updated.map(|u| u.timestamp_millis()),
        Some(utc(2026, 10, 27, 16, 40, 11).timestamp_millis() + 512)
    );
    assert_eq!(client.default_task_list().await.expect("default").id, "L1");

    let work = client.insert_task_list("Work").await.expect("insert");
    assert_eq!(work.id, "L3");
    assert_eq!(work.updated, Some(utc(2026, 10, 28, 8, 0, 0)));
    let renamed = client
        .rename_task_list("L3", "Work & errands")
        .await
        .expect("rename");
    assert_eq!(renamed.title, "Work & errands");
    client.delete_task_list("L3").await.expect("delete");
    client
        .delete_task_list("gone")
        .await
        .expect("an already deleted list is fine");
}

#[tokio::test]
async fn a_full_listing_is_followed_by_incremental_pulls() {
    let server = MockServer::start().await;
    let mut subtask = task_json("t2", "Buy candles");
    subtask["parent"] = json!("t1");
    let mut due = task_json("t1", "Plan the party");
    due["due"] = json!("2026-11-07T00:00:00.000Z");
    due["notes"] = json!("Ask Maya about the venue");
    Mock::given(method("GET"))
        .and(path(L1_TASKS))
        .and(query_param("maxResults", "100"))
        .and(query_param("showCompleted", "true"))
        .and(query_param("showHidden", "true"))
        .and(query_param("showDeleted", "false"))
        .and(query_param_is_missing("updatedMin"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Wed, 28 Oct 2026 10:00:00 GMT")
                .set_body_json(json!({
                    "kind": "tasks#tasks",
                    "etag": "\"page-1\"",
                    "nextPageToken": "tp2",
                    "items": [due, subtask]
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut done = task_json("t3", "Book the band");
    done["status"] = json!("completed");
    done["completed"] = json!("2026-10-26T19:05:00.000Z");
    done["hidden"] = json!(true);
    Mock::given(method("GET"))
        .and(path(L1_TASKS))
        .and(query_param("pageToken", "tp2"))
        .and(query_param("showHidden", "true"))
        .and(query_param_is_missing("updatedMin"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Wed, 28 Oct 2026 10:00:01 GMT")
                .set_body_json(json!({"kind": "tasks#tasks", "items": [done]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut changed = task_json("t1", "Plan the party (Saturday)");
    changed["updated"] = json!("2026-10-28T11:30:00.000Z");
    let mut deleted = task_json("t2", "Buy candles");
    deleted["deleted"] = json!(true);
    Mock::given(method("GET"))
        .and(path(L1_TASKS))
        .and(query_param("updatedMin", "2026-10-28T09:59:00.000Z"))
        .and(query_param("showDeleted", "true"))
        .and(query_param("showHidden", "true"))
        .and(query_param("showCompleted", "true"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Wed, 28 Oct 2026 12:00:00 GMT")
                .set_body_json(json!({"kind": "tasks#tasks", "items": [changed, deleted]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let first = client.task_changes("L1", None).await.expect("page 1");
    assert!(first.full);
    assert_eq!(first.tasks.len(), 2);
    assert_eq!(first.tasks[0].due, Some(day(2026, 11, 7)));
    assert_eq!(
        first.tasks[0].notes.as_deref(),
        Some("Ask Maya about the venue")
    );
    assert_eq!(first.tasks[1].parent.as_deref(), Some("t1"));
    assert_eq!(
        first.next,
        TaskCursor::Next {
            updated_min: None,
            page_token: "tp2".into(),
            started: utc(2026, 10, 28, 10, 0, 0),
        }
    );
    // The cursor survives being stored as JSON.
    let stored = serde_json::to_string(&first.next).expect("serializes");
    let cursor: TaskCursor = serde_json::from_str(&stored).expect("parses");

    let second = client
        .task_changes("L1", Some(&cursor))
        .await
        .expect("page 2");
    assert!(second.full);
    let book = &second.tasks[0];
    assert_eq!(book.status, TaskStatus::Completed);
    assert!(book.hidden, "tasks completed in the Tasks apps come hidden");
    assert_eq!(book.completed, Some(utc(2026, 10, 26, 19, 5, 0)));
    assert_eq!(
        second.next,
        TaskCursor::Done {
            since: utc(2026, 10, 28, 9, 59, 0)
        },
        "the next run reaches back to before this one started"
    );

    let third = client
        .task_changes("L1", Some(&second.next))
        .await
        .expect("incremental");
    assert!(!third.full);
    assert_eq!(third.tasks[0].title, "Plan the party (Saturday)");
    assert!(third.tasks[1].deleted);
    assert_eq!(
        third.next,
        TaskCursor::Done {
            since: utc(2026, 10, 28, 11, 59, 0)
        }
    );
}

#[tokio::test]
async fn lower_level_pages_take_every_parameter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(L1_TASKS))
        .and(query_param("maxResults", "100"))
        .and(query_param("showCompleted", "false"))
        .and(query_param("showHidden", "false"))
        .and(query_param("showDeleted", "true"))
        .and(query_param("showAssigned", "true"))
        .and(query_param("updatedMin", "2026-10-01T00:00:00.000Z"))
        .and(query_param("pageToken", "p7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#tasks",
            "nextPageToken": "p8",
            "items": [{"id": "a1", "title": "Review the draft", "assignmentInfo": {"surfaceType": "DOCUMENT"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(L1_TASKS))
        .and(query_param("pageToken", "same"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "tasks#tasks", "nextPageToken": "same", "items": []
        })))
        .mount(&server)
        .await;
    let client = client(&server);

    let query = TaskQuery {
        show_completed: false,
        show_hidden: false,
        show_assigned: true,
        page_size: 500,
        ..TaskQuery::changed_since(utc(2026, 10, 1, 0, 0, 0))
    };
    let page = client
        .tasks_page("L1", &query, Some("p7"))
        .await
        .expect("page");
    assert_eq!(page.next_page_token.as_deref(), Some("p8"));
    assert!(page.tasks[0].assigned);
    let stuck = client
        .tasks_page("L1", &TaskQuery::full(), Some("same"))
        .await
        .expect_err("paging must advance");
    assert!(matches!(stuck, Error::InvalidResponse(_)), "got {stuck:?}");
}

#[tokio::test]
async fn tasks_are_inserted_patched_moved_and_deleted() {
    let server = MockServer::start().await;
    let mut created = task_json("t4", "Buy a cake");
    created["parent"] = json!("t1");
    created["due"] = json!("2026-11-06T00:00:00.000Z");
    created["notes"] = json!("Chocolate, no nuts");
    Mock::given(method("POST"))
        .and(path(L1_TASKS))
        .and(query_param("parent", "t1"))
        .and(query_param("previous", "t2"))
        .and(body_json(json!({
            "title": "Buy a cake",
            "notes": "Chocolate, no nuts",
            "due": "2026-11-06T00:00:00.000Z",
            "status": "needsAction"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(created.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut cleared = created.clone();
    cleared.as_object_mut().map(|t| t.remove("due"));
    Mock::given(method("PATCH"))
        .and(path(format!("{L1_TASKS}/t4")))
        .and(body_json(
            json!({"due": null, "status": "needsAction", "completed": null}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(cleared.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut completed = cleared.clone();
    completed["status"] = json!("completed");
    completed["completed"] = json!("2026-10-29T08:00:00.000Z");
    Mock::given(method("PATCH"))
        .and(path(format!("{L1_TASKS}/t4")))
        .and(body_json(json!({
            "title": "Buy the cake",
            "status": "completed",
            "completed": "2026-10-29T08:00:00.000Z"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completed.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{L1_TASKS}/t4")))
        .respond_with(ResponseTemplate::new(200).set_body_json(completed.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut moved = completed.clone();
    moved.as_object_mut().map(|t| t.remove("parent"));
    Mock::given(method("POST"))
        .and(path(format!("{L1_TASKS}/t4/move")))
        .and(query_param("destinationTasklist", "L2"))
        .and(query_param("previous", "t9"))
        .and(query_param_is_missing("parent"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(moved.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{L1_TASKS}/t5/move")))
        .and(query_param_is_missing("parent"))
        .and(query_param_is_missing("previous"))
        .and(query_param_is_missing("destinationTasklist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json("t5", "First")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/tasks/v1/lists/L2/tasks/t4"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{L1_TASKS}/gone")))
        .respond_with(google_error(410, "deleted", "Resource has been deleted"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{L1_TASKS}/missing")))
        .respond_with(google_error(404, "notFound", "Not Found"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/tasks/v1/lists/L1/clear"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);

    let new = NewTask {
        title: "Buy a cake".into(),
        notes: Some("Chocolate, no nuts".into()),
        due: Some(day(2026, 11, 6)),
        completed: None,
    };
    let task = client
        .insert_task("L1", &new, &TaskPosition::new(Some("t1"), Some("t2")))
        .await
        .expect("insert");
    assert_eq!(task.id, "t4");
    assert_eq!(task.due, Some(day(2026, 11, 6)));

    let task = client
        .patch_task("L1", "t4", &TaskPatch::new().due(None).completed(None))
        .await
        .expect("clear due and reopen");
    assert_eq!(task.due, None);
    assert_eq!(task.status, TaskStatus::NeedsAction);

    let task = client
        .patch_task(
            "L1",
            "t4",
            &TaskPatch::new()
                .title("Buy the cake")
                .completed(Some(utc(2026, 10, 29, 8, 0, 0))),
        )
        .await
        .expect("complete");
    assert_eq!(task.completed, Some(utc(2026, 10, 29, 8, 0, 0)));
    let same = client
        .patch_task("L1", "t4", &TaskPatch::new())
        .await
        .expect("an empty patch reads the task");
    assert_eq!(same, task);

    let moved = client
        .move_task("L1", "t4", &TaskPosition::new(None, Some("t9")), Some("L2"))
        .await
        .expect("move to another list");
    assert_eq!(moved.parent, None);
    client
        .move_task("L1", "t5", &TaskPosition::default(), None)
        .await
        .expect("move to the top");

    client.delete_task("L2", "t4").await.expect("delete");
    client
        .delete_task("L1", "gone")
        .await
        .expect("an already deleted task is fine");
    let missing = client
        .delete_task("L1", "missing")
        .await
        .expect_err("404 is reported");
    assert!(missing.is_not_found());
    client.clear_completed_tasks("L1").await.expect("clear");
}

#[tokio::test]
async fn task_errors_are_classified() {
    let server = MockServer::start().await;
    // A rejected token is refreshed once.
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/L1/tasks/t1"))
        .and(bearer_token("stale"))
        .respond_with(google_error(401, "authError", "Invalid Credentials"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/L1/tasks/t1"))
        .and(bearer_token("fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(task_json("t1", "Plan")))
        .expect(1)
        .mount(&server)
        .await;
    // Grants from before Envelope asked for the Tasks scope.
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/old/tasks"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {
                "code": 403,
                "message": "Request had insufficient authentication scopes.",
                "errors": [{"message": "Insufficient Permission", "domain": "global", "reason": "insufficientPermissions"}],
                "status": "PERMISSION_DENIED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT",
                    "domain": "googleapis.com",
                    "metadata": {"service": "tasks.googleapis.com", "method": "google.tasks.v1.TasksService.ListTasks"}
                }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(LISTS))
        .respond_with(google_error(
            403,
            "accessNotConfigured",
            "Google Tasks API has not been used in project 1234 before or it is disabled.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    // Rate limiting is retried.
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/busy/tasks"))
        .respond_with(google_error(
            429,
            "rateLimitExceeded",
            "Rate Limit Exceeded",
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/busy/tasks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"kind": "tasks#tasks"})))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    // Changes that far back can no longer be listed.
    Mock::given(method("GET"))
        .and(path(L1_TASKS))
        .and(query_param("updatedMin", "2026-01-01T00:00:00.000Z"))
        .respond_with(google_error(410, "fullSyncRequired", "Sync required"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tasks/v1/lists/L1/tasks/nope"))
        .respond_with(google_error(404, "notFound", "Not Found"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/tasks/v1/lists/L1/tasks/t1"))
        .respond_with(google_error(412, "conditionNotMet", "Precondition Failed"))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(RefreshingToken {
        current: Mutex::new("stale".to_owned()),
        invalidations: AtomicUsize::new(0),
    });
    let client = client_with(&server, tokens.clone());

    let task = client.get_task("L1", "t1").await.expect("after a refresh");
    assert_eq!(task.title, "Plan");
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);

    let scope = client
        .task_changes("old", None)
        .await
        .expect_err("missing scope");
    assert!(scope.is_insufficient_scope(), "got {scope:?}");
    assert!(!scope.is_unauthorized());

    let disabled = client.list_task_lists().await.expect_err("API disabled");
    assert!(disabled.is_api_disabled() && !disabled.is_insufficient_scope());

    let page = client
        .task_changes("busy", None)
        .await
        .expect("retried after 429");
    assert!(page.tasks.is_empty() && page.next.is_done());

    let expired = client
        .task_changes(
            "L1",
            Some(&TaskCursor::Done {
                since: utc(2026, 1, 1, 0, 0, 0),
            }),
        )
        .await
        .expect_err("410 on an incremental pull");
    assert!(
        matches!(expired, Error::SyncTokenExpired),
        "got {expired:?}"
    );

    let missing = client.get_task("L1", "nope").await.expect_err("404");
    assert!(missing.is_not_found());

    let stale = client
        .patch_task("L1", "t1", &TaskPatch::new().title("x"))
        .await
        .expect_err("412");
    assert!(stale.is_precondition_failed() && stale.is_stale_etag());

    let sent = requests_to(&server, "PATCH", "/tasks/v1/lists/L1/tasks/t1").await;
    assert_eq!(sent.len(), 1, "a 412 is not retried");
    let incremental = requests_to(&server, "GET", L1_TASKS).await;
    assert_eq!(
        query(&incremental[0], "showDeleted").as_deref(),
        Some("true")
    );
}

#[test]
fn task_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let client = GoogleClient::new(Arc::new(StaticToken::new("t"))).expect("client");
    let task = NewTask::default();
    let position = TaskPosition::default();
    let patch = TaskPatch::new();
    let cursor = TaskCursor::Done { since: Utc::now() };
    assert_send(&client.list_task_lists());
    assert_send(&client.default_task_list());
    assert_send(&client.insert_task_list("x"));
    assert_send(&client.rename_task_list("l", "x"));
    assert_send(&client.delete_task_list("l"));
    assert_send(&client.tasks_page("l", &TaskQuery::full(), None));
    assert_send(&client.task_changes("l", Some(&cursor)));
    assert_send(&client.get_task("l", "t"));
    assert_send(&client.insert_task("l", &task, &position));
    assert_send(&client.patch_task("l", "t", &patch));
    assert_send(&client.delete_task("l", "t"));
    assert_send(&client.move_task("l", "t", &position, Some("l2")));
    assert_send(&client.clear_completed_tasks("l"));
}
