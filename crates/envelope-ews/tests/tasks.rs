//! Task folders, task sync, reading tasks and task writes against recorded
//! EWS responses.

mod common;

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use common::{EWS_PATH, bodies, fixture, on, requests, setup, soap_fault, soap_ok};
use envelope_ews::{
    DeleteType, DistinguishedFolder, EwsError, FolderId, FolderRole, Importance, ItemId, NewTask,
    ResponseCode, TaskStatus, TaskSyncPage, TaskUpdate,
};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

const ISTANBUL: Tz = Tz::Europe__Istanbul;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// A response with one message of the given operation failing with `code`.
fn error_response(operation: &str, code: &str, text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:{operation}Response xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:{operation}ResponseMessage ResponseClass="Error"><m:MessageText>{text}</m:MessageText><m:ResponseCode>{code}</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:{operation}ResponseMessage></m:ResponseMessages></m:{operation}Response></s:Body></s:Envelope>"#
    ))
}

#[tokio::test]
async fn task_folders_are_the_tasks_folder_and_the_task_folders_below_it() {
    let (server, client) = setup().await;
    on(&server, "<m:GetFolder>", soap_ok("get_folder_tasks.xml")).await;
    on(&server, "<m:FindFolder", soap_ok("find_folder_tasks.xml")).await;

    let folders = client.task_folders().await.expect("task folders");
    let names: Vec<&str> = folders.iter().map(|f| f.display_name.as_str()).collect();
    assert_eq!(
        names,
        ["Tasks", "Work", "Done 2025"],
        "parents first; notes and hidden folders are left out"
    );
    assert_eq!(folders[0].role, Some(FolderRole::Tasks));
    assert!(folders.iter().all(|f| f.is_tasks()));
    assert_eq!(
        folders[1].parent_id.as_ref().map(|p| p.id.as_str()),
        Some("tasks-id")
    );

    let bodies = bodies(&server).await;
    assert!(
        bodies[0].contains(r#"<m:FolderIds><t:DistinguishedFolderId Id="tasks"/></m:FolderIds>"#)
    );
    assert!(bodies[1].contains(r#"<m:FindFolder Traversal="Deep">"#));
    assert!(bodies[1].contains(
        r#"<m:ParentFolderIds><t:FolderId Id="tasks-id" ChangeKey="BAAAABYAAAAtasks"/></m:ParentFolderIds>"#
    ));
}

#[tokio::test]
async fn task_sync_pages_until_the_last_change() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .and(body_string_contains(
            "<m:SyncState>H4sIAAAAAAAEAO29B2AcSZYlJi9tasks1</m:SyncState>",
        ))
        .respond_with(soap_ok("sync_folder_items_tasks_page2.xml"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    on(
        &server,
        "<m:SyncFolderItems>",
        soap_ok("sync_folder_items_tasks_page1.xml"),
    )
    .await;

    let work = FolderId::with_change_key("work-id", "BAAAABYAAAAwork");
    let mut state: Option<String> = None;
    let mut pages: Vec<TaskSyncPage> = Vec::new();
    loop {
        let page = client
            .sync_tasks(work.clone(), state.as_deref(), 100)
            .await
            .expect("sync");
        state = Some(page.sync_state.clone());
        let last = page.includes_last_item_in_range;
        pages.push(page);
        if last {
            break;
        }
    }
    assert_eq!(pages.len(), 2);
    let first = &pages[0];
    let created: Vec<&str> = first.created.iter().map(|id| id.id.as_str()).collect();
    assert_eq!(
        created,
        ["AAMkTask1", "AAMkTask2"],
        "the message is no task"
    );
    assert!(first.updated.is_empty() && first.deleted.is_empty());
    let second = &pages[1];
    assert_eq!(
        second.updated,
        [ItemId::with_change_key("AAMkTask1", "EwAAABYAAAD4")]
    );
    assert_eq!(second.deleted[0].id, "AAMkTask9");
    let changed: Vec<&str> = second.changed().map(|id| id.id.as_str()).collect();
    assert_eq!(changed, ["AAMkTask1"]);
    // The page is stored as JSON with the sync state.
    let json = serde_json::to_string(second).unwrap();
    assert_eq!(
        serde_json::from_str::<TaskSyncPage>(&json).unwrap(),
        *second
    );

    let bodies = bodies(&server).await;
    assert!(!bodies[0].contains("<m:SyncState>"));
    assert!(bodies[0].contains(
        r#"<m:SyncFolderId><t:FolderId Id="work-id" ChangeKey="BAAAABYAAAAwork"/></m:SyncFolderId>"#
    ));
    assert!(bodies[0].contains(r#"<t:FieldURI FieldURI="item:ItemClass"/>"#));
    assert!(bodies[0].contains("<m:MaxChangesReturned>100</m:MaxChangesReturned>"));
    assert!(
        !bodies[0].contains("TimeZoneContext"),
        "ids need no time zone"
    );
}

#[tokio::test]
async fn tasks_are_read_with_their_days_in_the_users_zone() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .and(body_string_contains(r#"<t:ItemId Id="AAMkTask2"/>"#))
        .respond_with(soap_ok("get_item_tasks.xml"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let single = fixture("get_item_tasks.xml");
    let (start, end) = (
        single.find("<m:GetItemResponseMessage").unwrap(),
        single.find("</m:ResponseMessages>").unwrap(),
    );
    let third = single.rfind("<m:GetItemResponseMessage").unwrap();
    let single = format!(
        "{}{}{}",
        &single[..start],
        &single[third..end],
        &single[end..]
    );
    on(
        &server,
        "<m:GetItem>",
        ResponseTemplate::new(200).set_body_string(single),
    )
    .await;

    let ids = [
        ItemId::with_change_key("AAMkTask1", "EwAAABYAAAD4"),
        ItemId::new("AAMkTask2"),
        ItemId::new("AAMkTask3"),
    ];
    let results = client.get_tasks(&ids, ISTANBUL).await.expect("get tasks");
    assert_eq!(results.len(), 3);
    let lease = results[0].as_ref().expect("first task");
    assert_eq!(lease.subject.as_deref(), Some("Renew the lease"));
    assert_eq!(
        lease.body.as_deref(),
        Some("Call the landlord first\nThen sign")
    );
    assert_eq!(lease.importance, Some(Importance::High));
    assert_eq!(lease.due_date, Some(date(2026, 11, 6)));
    assert_eq!(lease.start_date, Some(date(2026, 11, 2)));
    assert_eq!(lease.status, Some(TaskStatus::InProgress));
    assert_eq!(lease.percent_complete, Some(50));
    assert_eq!(lease.is_recurring, Some(true));
    assert_eq!(lease.reminder_is_set, Some(true));
    assert_eq!(lease.categories, ["Home"]);
    assert!(!lease.is_completed());
    let gone = results[1].as_ref().expect_err("second task is gone");
    assert_eq!(gone.code, ResponseCode::ErrorItemNotFound);
    let receipts = results[2].as_ref().expect("third task");
    assert_eq!(receipts.body, None);
    assert_eq!(receipts.completed_on, Some(date(2026, 10, 28)));
    assert_eq!(
        receipts.completed_at(),
        Utc.with_ymd_and_hms(2026, 10, 28, 14, 5, 0).single(),
        "the last change, on the day it was completed"
    );

    let task = client
        .get_task(&ItemId::new("AAMkTask3"), ISTANBUL)
        .await
        .expect("one task");
    assert_eq!(task, *receipts);

    let bodies = bodies(&server).await;
    assert!(bodies[0].contains(
        r#"<t:TimeZoneContext><t:TimeZoneDefinition Id="Turkey Standard Time"/></t:TimeZoneContext>"#
    ));
    assert!(bodies[0].contains("<t:BaseShape>IdOnly</t:BaseShape><t:BodyType>Text</t:BodyType>"));
    for field in [
        "task:DueDate",
        "task:StartDate",
        "task:CompleteDate",
        "task:IsRecurring",
    ] {
        assert!(
            bodies[0].contains(&format!(r#"<t:FieldURI FieldURI="{field}"/>"#)),
            "{field}"
        );
    }
}

#[tokio::test]
async fn tasks_are_created_updated_and_deleted() {
    let (server, client) = setup().await;
    on(&server, "<m:CreateItem>", soap_ok("create_item_task.xml")).await;
    on(&server, "<m:UpdateItem", soap_ok("update_item_task.xml")).await;
    on(&server, "<m:DeleteItem", soap_ok("delete_item.xml")).await;

    let id = client
        .create_task(
            DistinguishedFolder::Tasks,
            &NewTask {
                subject: "Book the venue".into(),
                body: Some("Ask about parking".into()),
                importance: Some(Importance::High),
                due_date: Some(date(2026, 11, 6)),
                ..NewTask::default()
            },
            ISTANBUL,
        )
        .await
        .expect("create");
    assert_eq!(
        id,
        ItemId::with_change_key("AAMkTaskNew", "EwAAABYAAAD-new")
    );

    let reopened = client
        .update_task(
            &id,
            &TaskUpdate {
                subject: Some("Book the bigger venue".into()),
                due_date: Some(None),
                completed: Some(None),
                ..TaskUpdate::default()
            },
            ISTANBUL,
        )
        .await
        .expect("clear the due date and reopen");
    assert_eq!(reopened.change_key.as_deref(), Some("EwAAABYAAAD-v2"));
    let done_at = Utc
        .with_ymd_and_hms(2026, 10, 29, 8, 0, 0)
        .single()
        .unwrap();
    client
        .update_task(
            &reopened,
            &TaskUpdate {
                due_date: Some(Some(date(2026, 11, 7))),
                completed: Some(Some(done_at)),
                ..TaskUpdate::default()
            },
            ISTANBUL,
        )
        .await
        .expect("move and complete");
    let unchanged = client
        .update_task(&reopened, &TaskUpdate::default(), ISTANBUL)
        .await
        .expect("nothing to change");
    assert_eq!(unchanged, reopened);
    client
        .delete_task(&reopened, DeleteType::MoveToDeletedItems)
        .await
        .expect("delete");

    let bodies = bodies(&server).await;
    assert_eq!(bodies.len(), 4, "an empty update sends nothing");
    let create = &bodies[0];
    assert!(create.contains(
        r#"<t:TimeZoneContext><t:TimeZoneDefinition Id="Turkey Standard Time"/></t:TimeZoneContext>"#
    ));
    assert!(create.contains(
        "<m:SavedItemFolderId><t:DistinguishedFolderId Id=\"tasks\"/></m:SavedItemFolderId>"
    ));
    assert!(create.contains(
        "<t:Task><t:Subject>Book the venue</t:Subject><t:Body BodyType=\"Text\">Ask about parking</t:Body>\
         <t:Importance>High</t:Importance><t:DueDate>2026-11-06T00:00:00</t:DueDate></t:Task>"
    ));
    let reopen = &bodies[1];
    assert!(reopen.contains("Turkey Standard Time"));
    assert!(reopen.contains(r#"<t:ItemId Id="AAMkTaskNew" ChangeKey="EwAAABYAAAD-new"/>"#));
    assert!(reopen.contains(r#"<t:DeleteItemField><t:FieldURI FieldURI="task:DueDate"/></t:DeleteItemField><t:DeleteItemField><t:FieldURI FieldURI="task:StartDate"/></t:DeleteItemField>"#));
    assert!(reopen.contains(r#"<t:SetItemField><t:FieldURI FieldURI="task:Status"/><t:Task><t:Status>NotStarted</t:Status></t:Task></t:SetItemField>"#));
    let complete = &bodies[2];
    assert!(complete.contains("<t:Task><t:DueDate>2026-11-07T00:00:00</t:DueDate></t:Task>"));
    let due = complete.find("task:DueDate").unwrap();
    let completion = complete.find("task:CompleteDate").unwrap();
    assert!(due < completion, "the completion goes last");
    assert!(complete.contains("<t:CompleteDate>2026-10-29T08:00:00Z</t:CompleteDate>"));
    let delete = &bodies[3];
    assert!(delete.contains(r#"DeleteType="MoveToDeletedItems""#));
    assert!(delete.contains(r#"AffectedTaskOccurrences="AllOccurrences""#));
    assert!(delete.contains(r#"<t:ItemId Id="AAMkTaskNew" ChangeKey="EwAAABYAAAD-v2"/>"#));
}

#[tokio::test]
async fn task_errors_are_classified() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:SyncState>stale</m:SyncState>",
        error_response(
            "SyncFolderItems",
            "ErrorInvalidSyncStateData",
            "Synchronization state data is corrupt or otherwise invalid.",
        ),
    )
    .await;
    on(
        &server,
        r#"<t:FolderId Id="gone-id"/>"#,
        error_response(
            "SyncFolderItems",
            "ErrorFolderNotFound",
            "The specified folder could not be found in the store.",
        ),
    )
    .await;
    on(
        &server,
        r#"<t:FolderId Id="shared-id"/>"#,
        error_response("CreateItem", "ErrorAccessDenied", "Access is denied."),
    )
    .await;
    on(
        &server,
        "<m:DeleteItem",
        error_response(
            "DeleteItem",
            "ErrorItemNotFound",
            "The specified object was not found in the store.",
        ),
    )
    .await;
    // Throttled once, then answered.
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .and(body_string_contains("<m:UpdateItem"))
        .respond_with(soap_fault(
            fixture("fault_server_busy.xml").replace("{{backoff}}", "10"),
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    on(&server, "<m:UpdateItem", soap_ok("update_item_task.xml")).await;

    let stale = client
        .sync_tasks(DistinguishedFolder::Tasks, Some("stale"), 512)
        .await
        .expect_err("stale state");
    assert!(stale.requires_resync(), "{stale:?}");
    let gone = client
        .sync_tasks(FolderId::new("gone-id"), None, 512)
        .await
        .expect_err("deleted folder");
    assert!(gone.is_not_found());
    let denied = client
        .create_task(FolderId::new("shared-id"), &NewTask::default(), ISTANBUL)
        .await
        .expect_err("read-only folder");
    assert!(
        denied
            .response_code()
            .is_some_and(ResponseCode::is_access_denied)
    );
    let missing = client
        .delete_task(&ItemId::new("AAMkGone"), DeleteType::MoveToDeletedItems)
        .await
        .expect_err("already deleted");
    assert!(missing.is_not_found());
    let updated = client
        .update_task(
            &ItemId::new("AAMkTaskNew"),
            &TaskUpdate {
                importance: Some(Importance::Low),
                ..TaskUpdate::default()
            },
            ISTANBUL,
        )
        .await
        .expect("retried after ErrorServerBusy");
    assert_eq!(updated.change_key.as_deref(), Some("EwAAABYAAAD-v2"));
    let updates = requests(&server)
        .await
        .into_iter()
        .filter(|r| String::from_utf8_lossy(&r.body).contains("<m:UpdateItem"))
        .count();
    assert_eq!(updates, 2);

    // Rejected credentials and a blocked endpoint.
    let (server, client) = setup().await;
    on(&server, "<m:GetFolder>", ResponseTemplate::new(401)).await;
    on(&server, "<m:SyncFolderItems>", ResponseTemplate::new(403)).await;
    let err = client.task_folders().await.expect_err("401");
    assert!(err.is_unauthorized() && err.is_auth_error());
    let err = client
        .sync_tasks(DistinguishedFolder::Tasks, None, 512)
        .await
        .expect_err("403");
    assert!(matches!(err, EwsError::Forbidden { .. }), "{err:?}");
    assert_eq!(err.status(), Some(403));
}
