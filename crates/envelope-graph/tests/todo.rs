//! Microsoft To Do against a mock Graph server: lists, the task delta with
//! paging and removals, task writes, steps, linked resources and how
//! errors are classified.

mod common;

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use common::{fixture, json_body, prefer_contains, requests, setup};
use envelope_graph::{
    ChecklistItemPatch, DeltaCursor, GraphClient, Importance, NewLinkedResource, NewTodoTask,
    TaskStatus, TodoTaskPatch,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, Request, ResponseTemplate};

const LIST: &str = "/v1.0/me/todo/lists/AAMkList1=";
const TASKS_DELTA: &str = "/v1.0/me/todo/lists/AAMkList1=/tasks/delta";
const TASK: &str = "/v1.0/me/todo/lists/AAMkList1=/tasks/AAkALgTask1=";

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("date")
}

fn graph_error(status: u16, code: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "error": {"code": code, "message": message, "innerError": {"request-id": "rid-1"}}
    }))
}

fn todo_task(id: &str, title: &str) -> Value {
    json!({
        "@odata.etag": format!("W/\"etag-{id}\""),
        "importance": "normal",
        "isReminderOn": false,
        "status": "notStarted",
        "title": title,
        "createdDateTime": "2026-10-28T08:00:00.1234567Z",
        "lastModifiedDateTime": "2026-10-28T08:00:00.2345678Z",
        "categories": [],
        "id": id,
        "body": {"content": "", "contentType": "text"}
    })
}

#[tokio::test]
async fn lists_are_listed_created_renamed_and_deleted() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/todo/lists"))
        .and(query_param_is_missing("$skip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, "todo_lists.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/todo/lists"))
        .and(query_param("$skip", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"displayName": "Groceries", "isOwner": false, "isShared": true,
                       "wellknownListName": "none", "id": "AAMkList2="}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/todo/lists"))
        .and(json_body(json!({"displayName": "Holiday"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "displayName": "Holiday", "isOwner": true, "isShared": false,
            "wellknownListName": "none", "id": "AAMkList3="
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/todo/lists/AAMkList3="))
        .and(json_body(json!({"displayName": "Holiday 2027"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "displayName": "Holiday 2027", "isOwner": true, "isShared": false,
            "wellknownListName": "none", "id": "AAMkList3="
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/todo/lists/AAMkList3="))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let lists = client.todo_lists().await.expect("lists");
    let names: Vec<&str> = lists.iter().map(|l| l.display_name.as_str()).collect();
    assert_eq!(names, ["Tasks", "Flagged email", "Groceries"]);
    assert!(lists[0].is_default());
    assert!(lists[1].is_flagged_emails());
    assert!(lists[2].is_shared && !lists[2].is_owner);

    let created = client.create_todo_list("Holiday").await.expect("create");
    assert_eq!(created.id, "AAMkList3=");
    let renamed = client
        .rename_todo_list("AAMkList3=", "Holiday 2027")
        .await
        .expect("rename");
    assert_eq!(renamed.display_name, "Holiday 2027");
    client.delete_todo_list("AAMkList3=").await.expect("delete");
}

#[tokio::test]
async fn the_task_delta_pages_reports_removals_and_resumes() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path(TASKS_DELTA))
        .and(query_param_is_missing("$skiptoken"))
        .and(query_param_is_missing("$deltatoken"))
        .and(prefer_contains("outlook.body-content-type=\"text\""))
        .and(prefer_contains("odata.maxpagesize=50"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture(&server, "todo_tasks_delta_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TASKS_DELTA))
        .and(query_param(
            "$skiptoken",
            "w0vf2jHg2mBXU-I2AK0FSWl0dopNtG8u5YoM",
        ))
        .and(prefer_contains("outlook.body-content-type=\"text\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture(&server, "todo_tasks_delta_page2.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut changed = todo_task(
        "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0AzqXX6-FeAAA=",
        "Plan the offsite",
    );
    changed["dueDateTime"] = json!(null);
    Mock::given(method("GET"))
        .and(path(TASKS_DELTA))
        .and(query_param(
            "$deltatoken",
            "MoVMZ_DzHG4AhT3WE8VioVS1IXZJ-ArqK5fknOjnKFY",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.deltaLink": format!("{}/me/todo/lists/AAMkList1=/tasks/delta?$deltatoken=round2", common::base(&server)),
            "value": [changed]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TASKS_DELTA))
        .and(query_param("$deltatoken", "expired"))
        .respond_with(graph_error(
            410,
            "syncStateNotFound",
            "The sync state generation is not found.",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let istanbul = Tz::Europe__Istanbul;
    let first = client
        .todo_tasks_delta("AAMkList1=", None, Some(50))
        .await
        .expect("page 1");
    assert!(matches!(first.next, DeltaCursor::Next(_)));
    assert!(first.removed_ids.is_empty());
    let offsite = &first.items[0];
    assert_eq!(offsite.importance, Importance::High);
    assert_eq!(offsite.notes(), "Agenda first\nThen the venue");
    assert_eq!(
        offsite.due_date(istanbul),
        Some(date(2026, 11, 6)),
        "set in the To Do app at Istanbul midnight"
    );
    assert_eq!(
        offsite.etag.as_deref(),
        Some("W/\"4rfRVIPi9EqXgDbc8U7HGwADLLQ9xQ==\"")
    );
    assert_eq!(offsite.checklist_items, None, "delta pages carry no steps");
    let plants = &first.items[1];
    assert_eq!(plants.status, TaskStatus::Completed);
    assert!(plants.repeats());
    assert_eq!(plants.due_date(istanbul), Some(date(2026, 10, 27)));
    assert_eq!(
        plants.completed_at(),
        Some(
            Utc.with_ymd_and_hms(2026, 10, 27, 15, 20, 0)
                .single()
                .expect("instant")
        )
    );

    // The cursor is stored as JSON between pages.
    let stored = serde_json::to_string(&first.next).expect("serializes");
    let cursor: DeltaCursor = serde_json::from_str(&stored).expect("parses");
    let second = client
        .todo_tasks_delta("AAMkList1=", Some(&cursor), Some(50))
        .await
        .expect("page 2");
    assert!(second.next.is_done());
    assert_eq!(
        second.removed_ids,
        ["AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0AzqXX6-FhAAA="]
    );
    let passport = &second.items[0];
    assert_eq!(passport.status, TaskStatus::InProgress);
    assert_eq!(passport.notes(), "Photos first");
    assert!(passport.is_reminder_on);
    assert_eq!(passport.due_date(istanbul), None);

    let round = client
        .todo_tasks_delta("AAMkList1=", Some(&second.next), None)
        .await
        .expect("next round");
    assert_eq!(round.items.len(), 1);
    assert_eq!(round.items[0].due_date(istanbul), None, "due date removed");
    assert!(round.next.url().ends_with("$deltatoken=round2"));

    let expired = DeltaCursor::Done(format!(
        "{}/me/todo/lists/AAMkList1=/tasks/delta?$deltatoken=expired",
        common::base(&server)
    ));
    let err = client
        .todo_tasks_delta("AAMkList1=", Some(&expired), None)
        .await
        .expect_err("gone");
    assert!(err.is_resync_required(), "got {err:?}");
}

#[tokio::test]
async fn tasks_are_created_updated_and_deleted() {
    let (server, client, _) = setup().await;
    let mut created = todo_task("AAkALgTask1=", "Reply to Maya");
    created["importance"] = json!("high");
    created["body"] = json!({"content": "About the venue", "contentType": "text"});
    created["dueDateTime"] = json!({"dateTime": "2026-10-30T04:00:00.0000000", "timeZone": "UTC"});
    created["linkedResources"] = json!([{
        "id": "f9cddce2-dce2-f9cd-e2dc-cdf9e2dccdf9",
        "applicationName": "Envelope",
        "displayName": "Venue for Saturday",
        "externalId": "CAF=abc@mail.example.com"
    }]);
    Mock::given(method("POST"))
        .and(path(format!("{LIST}/tasks")))
        .and(prefer_contains("outlook.body-content-type=\"text\""))
        .and(json_body(json!({
            "title": "Reply to Maya",
            "body": {"content": "About the venue", "contentType": "text"},
            "importance": "high",
            "dueDateTime": {"dateTime": "2026-10-30T04:00:00", "timeZone": "UTC"},
            "status": "notStarted",
            "linkedResources": [{
                "applicationName": "Envelope",
                "displayName": "Venue for Saturday",
                "externalId": "CAF=abc@mail.example.com"
            }]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(created.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut reopened = created.clone();
    reopened["dueDateTime"] = Value::Null;
    Mock::given(method("PATCH"))
        .and(path(TASK))
        .and(json_body(json!({
            "dueDateTime": null,
            "status": "notStarted",
            "completedDateTime": null
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(reopened.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut completed = reopened.clone();
    completed["status"] = json!("completed");
    completed["importance"] = json!("normal");
    completed["completedDateTime"] =
        json!({"dateTime": "2026-10-29T00:00:00.0000000", "timeZone": "UTC"});
    completed["lastModifiedDateTime"] = json!("2026-10-29T08:30:01.0000000Z");
    Mock::given(method("PATCH"))
        .and(path(TASK))
        .and(json_body(json!({
            "importance": "normal",
            "status": "completed",
            "completedDateTime": {"dateTime": "2026-10-29T08:30:00", "timeZone": "UTC"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(completed.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TASK))
        .and(prefer_contains("outlook.body-content-type=\"text\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(completed.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(TASK))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let new = NewTodoTask {
        title: "Reply to Maya".into(),
        notes: Some("About the venue".into()),
        importance: Importance::High,
        due_date: Some(date(2026, 10, 30)),
        completed: None,
        linked_resources: vec![NewLinkedResource::message(
            "Venue for Saturday",
            "<CAF=abc@mail.example.com>",
            None,
        )],
    };
    let task = client
        .create_todo_task("AAMkList1=", &new, Tz::America__New_York)
        .await
        .expect("create");
    assert_eq!(task.id, "AAkALgTask1=");
    assert_eq!(
        task.due_date(Tz::America__New_York),
        Some(date(2026, 10, 30))
    );
    let links = task
        .linked_resources
        .as_deref()
        .expect("created with the task");
    assert_eq!(
        links[0].external_id.as_deref(),
        Some("CAF=abc@mail.example.com")
    );

    let task = client
        .update_todo_task(
            "AAMkList1=",
            "AAkALgTask1=",
            &TodoTaskPatch::new()
                .due_date(None, Tz::America__New_York)
                .completed(None),
        )
        .await
        .expect("clear the due date and reopen");
    assert_eq!(task.due_date_time, None);

    let at = Utc
        .with_ymd_and_hms(2026, 10, 29, 8, 30, 0)
        .single()
        .expect("instant");
    let task = client
        .update_todo_task(
            "AAMkList1=",
            "AAkALgTask1=",
            &TodoTaskPatch::new()
                .importance(Importance::Normal)
                .completed(Some(at)),
        )
        .await
        .expect("complete");
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(
        task.completed_at(),
        Some(at + chrono::Duration::seconds(1)),
        "the time of day comes back from the last change"
    );
    let same = client
        .update_todo_task("AAMkList1=", "AAkALgTask1=", &TodoTaskPatch::new())
        .await
        .expect("an empty patch reads the task");
    assert_eq!(same, task);
    client
        .delete_todo_task("AAMkList1=", "AAkALgTask1=")
        .await
        .expect("delete");
}

#[tokio::test]
async fn steps_and_linked_resources() {
    let (server, client, _) = setup().await;
    let base = common::base(&server);
    Mock::given(method("GET"))
        .and(path(format!("{TASK}/checklistItems")))
        .and(query_param_is_missing("$skiptoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.nextLink": format!("{base}/me/todo/lists/AAMkList1=/tasks/AAkALgTask1=/checklistItems?$skiptoken=s2"),
            "value": [
                {"displayName": "Buy candles", "createdDateTime": "2026-10-28T09:00:00Z", "isChecked": false, "id": "c1"},
                {"displayName": "Order the cake", "createdDateTime": "2026-10-28T09:01:00Z",
                 "checkedDateTime": "2026-10-28T12:00:00Z", "isChecked": true, "id": "c2"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{TASK}/checklistItems")))
        .and(query_param("$skiptoken", "s2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"displayName": "Invite the team", "isChecked": false, "id": "c3"}]
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(move |request: &Request| {
            let body: Value = request.body_json().expect("batch body");
            let urls: Vec<&str> = body["requests"]
                .as_array()
                .expect("requests")
                .iter()
                .filter_map(|r| r["url"].as_str())
                .collect();
            assert_eq!(
                urls,
                [
                    "/me/todo/lists/AAMkList1=/tasks/AAkALgTask1=/checklistItems",
                    "/me/todo/lists/AAMkList1=/tasks/AAkALgGone=/checklistItems",
                    "/me/todo/lists/AAMkList1=/tasks/AAkALgBare=/checklistItems",
                ]
            );
            ResponseTemplate::new(200).set_body_json(json!({
                "responses": [
                    {"id": "1", "status": 404, "headers": {},
                     "body": {"error": {"code": "ErrorItemNotFound", "message": "The specified object was not found in the store."}}},
                    {"id": "0", "status": 200, "headers": {"Content-Type": "application/json"}, "body": {
                        "@odata.nextLink": format!("{base}/me/todo/lists/AAMkList1=/tasks/AAkALgTask1=/checklistItems?$skiptoken=s2"),
                        "value": [{"displayName": "Buy candles", "isChecked": false, "id": "c1"}]
                    }},
                    {"id": "2", "status": 200, "headers": {}, "body": {"value": []}}
                ]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{TASK}/checklistItems")))
        .and(json_body(
            json!({"displayName": "Book the room", "isChecked": false}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "displayName": "Book the room", "createdDateTime": "2026-10-28T13:00:00Z",
            "isChecked": false, "id": "c4"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{TASK}/checklistItems/c4")))
        .and(json_body(json!({"isChecked": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "displayName": "Book the room", "isChecked": true,
            "checkedDateTime": "2026-10-28T14:00:00Z", "id": "c4"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{TASK}/checklistItems/c4")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{TASK}/linkedResources")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "l1", "webUrl": "https://outlook.office365.com/owa/?ItemID=AAMk",
                       "applicationName": "Outlook", "displayName": "Venue for Saturday"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{TASK}/linkedResources")))
        .and(json_body(json!({
            "webUrl": "https://mail.example.com/m/1",
            "applicationName": "Envelope",
            "displayName": "Venue for Saturday",
            "externalId": "CAF=abc@mail.example.com"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "l2", "webUrl": "https://mail.example.com/m/1", "applicationName": "Envelope",
            "displayName": "Venue for Saturday", "externalId": "CAF=abc@mail.example.com"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let steps = client
        .todo_checklist_items("AAMkList1=", "AAkALgTask1=")
        .await
        .expect("steps");
    let names: Vec<&str> = steps.iter().map(|s| s.display_name.as_str()).collect();
    assert_eq!(names, ["Buy candles", "Order the cake", "Invite the team"]);
    assert!(steps[1].is_checked);
    assert!(steps[1].checked_date_time.is_some());

    let all = client
        .todo_checklists(&[
            ("AAMkList1=", "AAkALgTask1="),
            ("AAMkList1=", "AAkALgGone="),
            ("AAMkList1=", "AAkALgBare="),
        ])
        .await
        .expect("batch");
    assert_eq!(all.len(), 3);
    let first = all[0].as_ref().expect("steps of the first task");
    assert_eq!(first.len(), 2, "the second page is fetched directly");
    let gone = all[1].as_ref().expect_err("the second task is gone");
    assert!(gone.is_not_found());
    assert!(all[2].as_ref().expect("no steps").is_empty());
    assert!(
        client
            .todo_checklists(&[])
            .await
            .expect("nothing")
            .is_empty()
    );

    let step = client
        .create_checklist_item("AAMkList1=", "AAkALgTask1=", "Book the room", false)
        .await
        .expect("create");
    assert_eq!(step.id, "c4");
    let step = client
        .update_checklist_item(
            "AAMkList1=",
            "AAkALgTask1=",
            "c4",
            &ChecklistItemPatch {
                is_checked: Some(true),
                ..ChecklistItemPatch::default()
            },
        )
        .await
        .expect("check off");
    assert!(step.is_checked);
    client
        .delete_checklist_item("AAMkList1=", "AAkALgTask1=", "c4")
        .await
        .expect("delete");

    let links = client
        .todo_linked_resources("AAMkList1=", "AAkALgTask1=")
        .await
        .expect("links");
    assert_eq!(links[0].application_name.as_deref(), Some("Outlook"));
    let link = client
        .create_linked_resource(
            "AAMkList1=",
            "AAkALgTask1=",
            &NewLinkedResource::message(
                "Venue for Saturday",
                "CAF=abc@mail.example.com",
                Some("https://mail.example.com/m/1"),
            ),
        )
        .await
        .expect("link");
    assert_eq!(link.id, "l2");
}

#[tokio::test]
async fn todo_errors_are_classified() {
    let (server, client, tokens) = setup().await;
    // A rejected token is refreshed once.
    Mock::given(method("GET"))
        .and(path(TASK))
        .and(header("authorization", "Bearer token-1"))
        .respond_with(graph_error(
            401,
            "InvalidAuthenticationToken",
            "Access token has expired or is not yet valid.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TASK))
        .and(header("authorization", "Bearer token-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(todo_task("AAkALgTask1=", "Plan")))
        .expect(1)
        .mount(&server)
        .await;
    // A grant without Tasks.ReadWrite.
    Mock::given(method("GET"))
        .and(path("/v1.0/me/todo/lists"))
        .respond_with(graph_error(
            403,
            "ErrorAccessDenied",
            "Access is denied. Check credentials and try again.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/todo/lists/AAMkList1=/tasks/AAkALgGone="))
        .respond_with(graph_error(
            404,
            "ErrorItemNotFound",
            "The specified object was not found in the store.",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/todo/lists/AAMkBusy=/tasks/delta"))
        .respond_with(
            graph_error(429, "TooManyRequests", "Please retry again later.")
                .insert_header("Retry-After", "0"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/todo/lists/AAMkBusy=/tasks/delta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.deltaLink": format!("{}/me/todo/lists/AAMkBusy=/tasks/delta?$deltatoken=d1", common::base(&server)),
            "value": []
        })))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(TASK))
        .respond_with(graph_error(
            412,
            "ErrorIrresolvableConflict",
            "The send or update operation could not be performed because the change key passed in the request does not match the current change key for the item.",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let task = client
        .get_todo_task("AAMkList1=", "AAkALgTask1=")
        .await
        .expect("after a refresh");
    assert_eq!(task.title, "Plan");
    assert_eq!(tokens.invalidations(), 1);

    let denied = client.todo_lists().await.expect_err("403");
    assert!(denied.is_forbidden(), "got {denied:?}");
    assert_eq!(denied.code(), Some("ErrorAccessDenied"));
    assert!(!denied.is_transient() && !denied.is_unauthorized());

    let missing = client
        .get_todo_task("AAMkList1=", "AAkALgGone=")
        .await
        .expect_err("404");
    assert!(missing.is_not_found());

    let page = client
        .todo_tasks_delta("AAMkBusy=", None, None)
        .await
        .expect("retried after 429");
    assert!(page.next.is_done() && page.items.is_empty());

    let conflict = client
        .update_todo_task(
            "AAMkList1=",
            "AAkALgTask1=",
            &TodoTaskPatch::new().title("x"),
        )
        .await
        .expect_err("412");
    assert!(conflict.is_precondition_failed());

    let patches = requests(&server)
        .await
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .count();
    assert_eq!(patches, 1, "a 412 is not retried");
}

fn assert_send<T: Send>(_: &T) {}

#[test]
fn todo_futures_are_send() {
    let client = GraphClient::new(std::sync::Arc::new(envelope_graph::StaticToken::new("t")))
        .expect("client");
    assert_send(&client.todo_lists());
    assert_send(&client.create_todo_list("x"));
    assert_send(&client.rename_todo_list("l", "x"));
    assert_send(&client.delete_todo_list("l"));
    assert_send(&client.todo_tasks_delta("l", None, None));
    assert_send(&client.get_todo_task("l", "t"));
    assert_send(&client.create_todo_task("l", &NewTodoTask::default(), Tz::UTC));
    assert_send(&client.update_todo_task("l", "t", &TodoTaskPatch::new()));
    assert_send(&client.delete_todo_task("l", "t"));
    assert_send(&client.todo_checklist_items("l", "t"));
    assert_send(&client.todo_checklists(&[]));
    assert_send(&client.create_checklist_item("l", "t", "x", false));
    assert_send(&client.update_checklist_item("l", "t", "c", &ChecklistItemPatch::default()));
    assert_send(&client.delete_checklist_item("l", "t", "c"));
    assert_send(&client.todo_linked_resources("l", "t"));
    assert_send(&client.create_linked_resource("l", "t", &NewLinkedResource::default()));
}
