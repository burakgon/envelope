//! Exchange task folders over EWS against a mock server: the folders, a
//! first sync listing every task, every kind of upload (only the fields
//! that changed, days in the user's time zone, each read back), what
//! changed since by the sync state, an expired sync state, and folder
//! writes.

use std::collections::HashMap;

use super::*;
use crate::model::{ExchangeAuth, ExchangeConfig};

const EWS: &str = "/EWS/Exchange.asmx";
const NS: &str = r#"xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types""#;

/// An Exchange account whose EWS endpoint is `server`, signing in with a
/// password. It is disabled to keep the mail scheduler away.
fn exchange_account(fx: &Fixture, server: &MockServer) -> AccountRow {
    let config = AccountConfig {
        exchange: Some(ExchangeConfig {
            ews_url: format!("{}{EWS}", server.uri()),
            auth: ExchangeAuth::Basic,
            ..ExchangeConfig::default()
        }),
        ..AccountConfig::default()
    };
    let id = fx
        .engine
        .db
        .write(|tx| {
            let id = mail::insert_account(
                tx,
                AccountKind::Exchange,
                "exchange",
                "jane@example.com",
                "Jane",
                "#0ea5e9",
                &config,
            )?;
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [id])?;
            Ok(id)
        })
        .expect("account");
    fx.engine
        .secrets
        .preload(&crate::secrets::password_key(id), "secret");
    fx.engine.account_row(id).expect("account row")
}

/// The task service of an Exchange account, as the scheduler gets it.
async fn exchange_service(fx: &Fixture, account: &AccountRow) -> Box<dyn TaskService> {
    let service = service(&fx.engine, account)
        .await
        .expect("service")
        .expect("Exchange accounts always have task folders");
    assert_eq!(service.kind(), TaskListKind::Ews);
    service
}

fn envelope(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/xml; charset=utf-8")
        .set_body_string(format!(
            concat!(
                r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">"#,
                r#"<s:Header><h:ServerVersionInfo MajorVersion="15" MinorVersion="2" MajorBuildNumber="1544" MinorBuildNumber="4" Version="V2017_07_11" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/></s:Header>"#,
                r#"<s:Body>{}</s:Body></s:Envelope>"#
            ),
            body
        ))
}

/// A response of operation `op` carrying the given response messages.
fn response(op: &str, messages: &[String]) -> ResponseTemplate {
    envelope(&format!(
        "<m:{op}Response {NS}><m:ResponseMessages>{}</m:ResponseMessages></m:{op}Response>",
        messages.concat()
    ))
}

fn success(op: &str, inner: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:{op}ResponseMessage>"#
    )
}

fn failure(op: &str, code: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Error"><m:MessageText>{code}</m:MessageText><m:ResponseCode>{code}</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:{op}ResponseMessage>"#
    )
}

/// Answers requests whose body contains `needle`.
async fn on(server: &MockServer, needle: &str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(EWS))
        .and(body_string_contains(needle))
        .respond_with(response)
        .mount(server)
        .await;
}

/// The Tasks folder and "Work" below it.
async fn mount_folders(server: &MockServer) {
    on(
        server,
        "<m:GetFolder>",
        response(
            "GetFolder",
            &[success(
                "GetFolder",
                r#"<m:Folders><t:TasksFolder><t:FolderId Id="tasks-id" ChangeKey="tk"/><t:FolderClass>IPF.Task</t:FolderClass><t:DisplayName>Tasks</t:DisplayName></t:TasksFolder></m:Folders>"#,
            )],
        ),
    )
    .await;
    on(
        server,
        "<m:FindFolder",
        response(
            "FindFolder",
            &[success(
                "FindFolder",
                r#"<m:RootFolder TotalItemsInView="1" IncludesLastItemInRange="true"><t:Folders><t:TasksFolder><t:FolderId Id="work-id" ChangeKey="wk"/><t:ParentFolderId Id="tasks-id"/><t:FolderClass>IPF.Task</t:FolderClass><t:DisplayName>Work</t:DisplayName></t:TasksFolder></t:Folders></m:RootFolder>"#,
            )],
        ),
    )
    .await;
}

/// A complete `SyncFolderItems` answer.
fn sync_page(state: &str, changes: &str) -> ResponseTemplate {
    response(
        "SyncFolderItems",
        &[success(
            "SyncFolderItems",
            &format!(
                "<m:SyncState>{state}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{changes}</m:Changes>"
            ),
        )],
    )
}

fn created(id: &str, key: &str) -> String {
    format!(
        r#"<t:Create><t:Task><t:ItemId Id="{id}" ChangeKey="{key}"/><t:ItemClass>IPM.Task</t:ItemClass></t:Task></t:Create>"#
    )
}

fn updated(id: &str, key: &str) -> String {
    format!(
        r#"<t:Update><t:Task><t:ItemId Id="{id}" ChangeKey="{key}"/><t:ItemClass>IPM.Task</t:ItemClass></t:Task></t:Update>"#
    )
}

fn deleted(id: &str) -> String {
    format!(r#"<t:Delete><t:ItemId Id="{id}"/></t:Delete>"#)
}

/// Answers the sync of folder `folder` with no sync state (`None`) or the
/// given one.
async fn on_sync(server: &MockServer, folder: &str, state: Option<&str>, page: ResponseTemplate) {
    let folder = format!(r#"Id="{folder}""#);
    let state = state.map(|s| format!("<m:SyncState>{s}</m:SyncState>"));
    Mock::given(method("POST"))
        .and(path(EWS))
        .and(body_string_contains("<m:SyncFolderItems"))
        .and(move |request: &Request| {
            let body = String::from_utf8_lossy(&request.body);
            body.contains(&folder)
                && match &state {
                    Some(state) => body.contains(state.as_str()),
                    None => !body.contains("<m:SyncState>"),
                }
        })
        .respond_with(page)
        .mount(server)
        .await;
}

/// A task as `GetItem` returns it.
fn task_xml(id: &str, key: &str, fields: &str) -> String {
    format!(
        r#"<t:Task><t:ItemId Id="{id}" ChangeKey="{key}"/><t:ParentFolderId Id="tasks-id"/><t:ItemClass>IPM.Task</t:ItemClass>{fields}<t:DateTimeCreated>2026-10-20T07:12:00Z</t:DateTimeCreated></t:Task>"#
    )
}

fn open(subject: &str, extra: &str) -> String {
    format!(
        "<t:Subject>{subject}</t:Subject>{extra}<t:IsComplete>false</t:IsComplete><t:PercentComplete>0</t:PercentComplete><t:Status>NotStarted</t:Status><t:LastModifiedTime>2026-10-27T09:30:00Z</t:LastModifiedTime>"
    )
}

fn done(subject: &str) -> String {
    format!(
        "<t:Subject>{subject}</t:Subject><t:CompleteDate>2026-10-28T00:00:00Z</t:CompleteDate><t:IsComplete>true</t:IsComplete><t:PercentComplete>100</t:PercentComplete><t:Status>Completed</t:Status><t:LastModifiedTime>2026-10-28T14:05:00Z</t:LastModifiedTime>"
    )
}

/// The ids a request names (`<t:ItemId Id="…"`), in order.
fn item_ids(request: &Request) -> Vec<String> {
    let body = String::from_utf8_lossy(&request.body);
    body.split(r#"<t:ItemId Id=""#)
        .skip(1)
        .filter_map(|rest| rest.split('"').next().map(str::to_owned))
        .collect()
}

/// The tasks the mock has, by id: `GetItem` answers with them, and with
/// `ErrorItemNotFound` for others.
#[derive(Clone, Default)]
struct Items(Arc<Mutex<HashMap<String, String>>>);

impl Items {
    fn put(&self, id: &str, xml: String) {
        self.0.lock().insert(id.to_owned(), xml);
    }

    async fn mount(&self, server: &MockServer) {
        let items = self.clone();
        Mock::given(method("POST"))
            .and(path(EWS))
            .and(body_string_contains("<m:GetItem"))
            .respond_with(move |request: &Request| {
                let items = items.0.lock();
                let messages: Vec<String> = item_ids(request)
                    .iter()
                    .map(|id| match items.get(id) {
                        Some(xml) => success("GetItem", &format!("<m:Items>{xml}</m:Items>")),
                        None => failure("GetItem", "ErrorItemNotFound"),
                    })
                    .collect();
                response("GetItem", &messages)
            })
            .mount(server)
            .await;
    }
}

/// The bodies of the requests for operation `op` naming item `id`.
fn writes(requests: &[Request], op: &str, id: &str) -> Vec<String> {
    let id = format!(r#"Id="{id}""#);
    requests
        .iter()
        .map(body)
        .filter(|b| b.contains(&format!("<m:{op}")) && b.contains(&id))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_tasks_pull_push_and_pull_again() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = exchange_account(&fx, &server);
    let service = exchange_service(&fx, &account).await;
    mount_folders(&server).await;
    let items = Items::default();
    items.put(
        "T1",
        task_xml(
            "T1",
            "T1-1",
            &open(
                "Renew the lease",
                r#"<t:Body BodyType="Text">Call the landlord first</t:Body><t:Importance>Normal</t:Importance><t:DueDate>2030-06-01T00:00:00Z</t:DueDate>"#,
            ),
        ),
    );
    items.put("T2", task_xml("T2", "T2-1", &open("File the receipts", "")));
    items.put("T3", task_xml("T3", "T3-1", &done("Book the venue")));
    items.put(
        "T4",
        task_xml(
            "T4",
            "T4-1",
            &open(
                "Water the plants",
                "<t:Importance>High</t:Importance><t:IsRecurring>true</t:IsRecurring>",
            ),
        ),
    );
    items.mount(&server).await;
    on_sync(
        &server,
        "tasks-id",
        None,
        sync_page(
            "s1",
            &[
                created("T1", "T1-1"),
                created("T2", "T2-1"),
                created("T3", "T3-1"),
                created("T4", "T4-1"),
            ]
            .concat(),
        ),
    )
    .await;
    on_sync(&server, "work-id", None, sync_page("w1", "")).await;

    let changes = sync_account(&fx.engine, &account, service.as_ref(), true)
        .await
        .expect("first sync");
    assert!(changes.lists);
    let tasks = list_named(&fx, &account, "Tasks");
    let work = list_named(&fx, &account, "Work");
    assert!(tasks.is_default && !work.is_default);
    assert_eq!(changes.tasks, [tasks.id]);
    assert_eq!(
        tasks.sync_state.cursor,
        Some(serde_json::json!({ "syncState": "s1" }))
    );
    let lease = remote_row(&fx, tasks.id, "T1");
    assert_eq!(lease.notes, "Call the landlord first");
    assert_eq!(
        (lease.due_date.as_deref(), lease.due_at, lease.important),
        (Some("2030-06-01"), None, false)
    );
    assert_eq!(lease.etag.as_deref(), Some("T1-1"));
    let receipts = remote_row(&fx, tasks.id, "T2");
    let venue = remote_row(&fx, tasks.id, "T3");
    assert_eq!(
        venue.completed_at,
        Some(
            Utc.with_ymd_and_hms(2026, 10, 28, 14, 5, 0)
                .single()
                .expect("instant")
                .timestamp_millis()
        ),
        "Exchange keeps the day; the last change tells the time"
    );
    let plants = remote_row(&fx, tasks.id, "T4");
    assert!(plants.important && plants.repeats);
    // Exchange keeps no order Envelope could change.
    assert!(!TaskListKind::Ews.features().reorder);
    let moved = fx.engine.reorder_task(plants.id, Some(lease.id)).await;
    assert!(matches!(moved, Err(Error::Invalid(_))), "{moved:?}");

    // Local changes of every kind.
    fx.engine
        .update_task(
            lease.id,
            TaskInput {
                important: Some(true),
                due_date: Some(Some("2030-05-15".into())),
                ..input(tasks.id, "Renew the lease early")
            },
        )
        .await
        .expect("edit");
    fx.engine
        .complete_task(receipts.id, true)
        .await
        .expect("complete");
    fx.engine
        .complete_task(venue.id, false)
        .await
        .expect("reopen");
    fx.engine.delete_task(plants.id).await.expect("delete");
    let call = fx
        .create(TaskInput {
            important: Some(true),
            due_date: Some(Some("2030-07-01".into())),
            notes: Some("About the offer".into()),
            ..input(tasks.id, "Call Maya")
        })
        .await;
    // What the server has after the writes, which the service reads back.
    items.put(
        "T1",
        task_xml(
            "T1",
            "T1-2",
            &open(
                "Renew the lease early",
                r#"<t:Body BodyType="Text">Call the landlord first</t:Body><t:Importance>High</t:Importance><t:DueDate>2030-05-15T00:00:00Z</t:DueDate>"#,
            ),
        ),
    );
    items.put("T2", task_xml("T2", "T2-2", &done("File the receipts")));
    items.put("T3", task_xml("T3", "T3-2", &open("Book the venue", "")));
    items.put(
        "N1",
        task_xml(
            "N1",
            "N1-1",
            &open(
                "Call Maya",
                r#"<t:Body BodyType="Text">About the offer</t:Body><t:Importance>High</t:Importance><t:DueDate>2030-07-01T00:00:00Z</t:DueDate>"#,
            ),
        ),
    );
    Mock::given(method("POST"))
        .and(path(EWS))
        .and(body_string_contains("<m:UpdateItem"))
        .respond_with(|request: &Request| {
            let id = item_ids(request).remove(0);
            response(
                "UpdateItem",
                &[success(
                    "UpdateItem",
                    &format!(
                        r#"<m:Items><t:Task><t:ItemId Id="{id}" ChangeKey="{id}-2"/></t:Task></m:Items><m:ConflictResults><t:Count>0</t:Count></m:ConflictResults>"#
                    ),
                )],
            )
        })
        .expect(3)
        .mount(&server)
        .await;
    on(
        &server,
        "<m:CreateItem",
        response(
            "CreateItem",
            &[success(
                "CreateItem",
                r#"<m:Items><t:Task><t:ItemId Id="N1" ChangeKey="N1-1"/></t:Task></m:Items>"#,
            )],
        ),
    )
    .await;
    on(
        &server,
        "<m:DeleteItem",
        response("DeleteItem", &[success("DeleteItem", "")]),
    )
    .await;

    sync_account(&fx.engine, &account, service.as_ref(), false)
        .await
        .expect("push");
    assert!(
        fx.rows(tasks.id)
            .iter()
            .all(|r| r.local_state == tdb::SYNCED),
        "everything is on the server"
    );
    assert!(fx.engine.db.read(|c| tdb::get_task(c, plants.id)).is_err());
    let requests = server.received_requests().await.expect("requests");
    let edit = writes(&requests, "UpdateItem", "T1");
    assert_eq!(edit.len(), 1);
    let edit = &edit[0];
    assert!(edit.contains("<t:TimeZoneContext>"), "{edit}");
    assert!(
        edit.contains("<t:Subject>Renew the lease early</t:Subject>"),
        "{edit}"
    );
    assert!(edit.contains("<t:Importance>High</t:Importance>"), "{edit}");
    assert!(
        edit.contains("<t:DueDate>2030-05-15T00:00:00</t:DueDate>"),
        "a day, at midnight in the user's zone: {edit}"
    );
    for untouched in ["item:Body", "task:Status", "task:CompleteDate"] {
        assert!(!edit.contains(untouched), "only what changed: {edit}");
    }
    let complete = writes(&requests, "UpdateItem", "T2").concat();
    assert!(
        complete.contains(r#"FieldURI="task:CompleteDate""#),
        "{complete}"
    );
    assert!(!complete.contains("item:Subject"), "{complete}");
    let reopen = writes(&requests, "UpdateItem", "T3").concat();
    assert!(
        reopen.contains("<t:Status>NotStarted</t:Status>"),
        "{reopen}"
    );
    let delete = writes(&requests, "DeleteItem", "T4").concat();
    assert!(
        delete.contains(r#"DeleteType="MoveToDeletedItems""#),
        "{delete}"
    );
    let create = requests
        .iter()
        .map(body)
        .find(|b| b.contains("<m:CreateItem"))
        .expect("created");
    assert!(
        create
            .contains(r#"<m:SavedItemFolderId><t:FolderId Id="tasks-id"/></m:SavedItemFolderId>"#),
        "{create}"
    );
    assert!(
        create.contains(
            r#"<t:Subject>Call Maya</t:Subject><t:Body BodyType="Text">About the offer</t:Body><t:Importance>High</t:Importance><t:DueDate>2030-07-01T00:00:00</t:DueDate>"#
        ),
        "{create}"
    );
    let lease = fx.row(lease.id);
    assert_eq!(
        (lease.etag.as_deref(), lease.title.as_str()),
        (Some("T1-2"), "Renew the lease early"),
        "read back after the write"
    );
    assert!(fx.row(receipts.id).completed_at.is_some());
    assert!(fx.row(venue.id).completed_at.is_none());
    let call = fx.row(call.id);
    assert_eq!(
        (call.remote_id.as_deref(), call.etag.as_deref()),
        (Some("N1"), Some("N1-1"))
    );

    // Changes on the server come in by the sync state.
    server.verify().await;
    server.reset().await;
    mount_folders(&server).await;
    items.put(
        "T1",
        task_xml(
            "T1",
            "T1-3",
            &open(
                "Renew the lease today",
                r#"<t:Importance>High</t:Importance><t:DueDate>2030-05-15T00:00:00Z</t:DueDate>"#,
            ),
        ),
    );
    items.mount(&server).await;
    on_sync(
        &server,
        "tasks-id",
        Some("s1"),
        sync_page("s2", &[updated("T1", "T1-3"), deleted("T3")].concat()),
    )
    .await;
    on_sync(&server, "work-id", Some("w1"), sync_page("w2", "")).await;
    let changes = sync_account(&fx.engine, &account, service.as_ref(), true)
        .await
        .expect("second sync");
    assert!(!changes.lists);
    assert_eq!(changes.tasks, [tasks.id]);
    let lease = fx.row(lease.id);
    assert_eq!(lease.title, "Renew the lease today");
    assert_eq!(lease.notes, "", "notes cleared there");
    assert!(
        fx.engine.db.read(|c| tdb::get_task(c, venue.id)).is_err(),
        "deleted there"
    );
    let requests = server.received_requests().await.expect("requests");
    let fetched: Vec<String> = requests
        .iter()
        .filter(|r| body(r).contains("<m:GetItem"))
        .flat_map(item_ids)
        .collect();
    assert_eq!(fetched, ["T1"], "only what changed is fetched");
    assert_eq!(
        list_named(&fx, &account, "Tasks").sync_state.cursor,
        Some(serde_json::json!({ "syncState": "s2" }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_tasks_cleared_while_uploaded_go_from_the_server() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = exchange_account(&fx, &server);
    let service = exchange_service(&fx, &account).await;
    mount_folders(&server).await;
    store_lists(&fx, &account, service.as_ref()).await;
    let tasks = list_named(&fx, &account, "Tasks");
    let venue = fx.create(input(tasks.id, "Book the venue")).await;
    fx.engine
        .complete_task(venue.id, true)
        .await
        .expect("complete");
    on(
        &server,
        "<m:CreateItem",
        response(
            "CreateItem",
            &[success(
                "CreateItem",
                r#"<m:Items><t:Task><t:ItemId Id="N1" ChangeKey="N1-1"/></t:Task></m:Items>"#,
            )],
        )
        .set_delay(std::time::Duration::from_millis(300)),
    )
    .await;
    on(
        &server,
        "<m:DeleteItem",
        response("DeleteItem", &[success("DeleteItem", "")]),
    )
    .await;
    Items::default().mount(&server).await;

    // Cleared away while Exchange is creating it.
    let pushing = {
        let (engine, list) = (fx.engine.clone(), tasks.clone());
        tokio::spawn(async move { push_list(&engine, service.as_ref(), &list).await })
    };
    until_received(&server, |r| body(r).contains("<m:CreateItem")).await;
    fx.engine
        .clear_completed_tasks(tasks.id)
        .await
        .expect("clear");
    pushing.await.expect("join").expect("push");
    let row = fx.row(venue.id);
    assert_eq!(
        (row.local_state, row.remote_id.as_deref()),
        (tdb::DELETED, Some("N1"))
    );
    retry_now(&fx, tasks.id);
    let service = exchange_service(&fx, &account).await;
    push_list(&fx.engine, service.as_ref(), &tasks)
        .await
        .expect("push");
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(writes(&requests, "DeleteItem", "N1").len(), 1);
    assert!(fx.rows(tasks.id).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_expired_sync_states_list_the_folder_again() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = exchange_account(&fx, &server);
    let service = exchange_service(&fx, &account).await;
    mount_folders(&server).await;
    store_lists(&fx, &account, service.as_ref()).await;
    let items = Items::default();
    items.put("T1", task_xml("T1", "T1-1", &open("Gone meanwhile", "")));
    items.mount(&server).await;
    on_sync(
        &server,
        "tasks-id",
        None,
        sync_page("s1", &created("T1", "T1-1")),
    )
    .await;
    let tasks = list_named(&fx, &account, "Tasks");
    pull_list(&fx.engine, service.as_ref(), &tasks)
        .await
        .expect("pull");
    assert_eq!(fx.rows(tasks.id).len(), 1);

    server.reset().await;
    items.0.lock().clear();
    items.put("T2", task_xml("T2", "T2-1", &open("Still here", "")));
    items.mount(&server).await;
    on_sync(
        &server,
        "tasks-id",
        Some("s1"),
        response(
            "SyncFolderItems",
            &[failure("SyncFolderItems", "ErrorInvalidSyncStateData")],
        ),
    )
    .await;
    on_sync(
        &server,
        "tasks-id",
        None,
        sync_page("s2", &created("T2", "T2-1")),
    )
    .await;
    let tasks = list_named(&fx, &account, "Tasks");
    assert!(
        pull_list(&fx.engine, service.as_ref(), &tasks)
            .await
            .expect("pull")
    );
    let remote_ids: Vec<Option<String>> =
        fx.rows(tasks.id).into_iter().map(|r| r.remote_id).collect();
    assert_eq!(
        remote_ids,
        [Some("T2".to_owned())],
        "what the new listing left out is gone"
    );
    assert_eq!(
        list_named(&fx, &account, "Tasks").sync_state.cursor,
        Some(serde_json::json!({ "syncState": "s2" }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_task_folders_are_created_renamed_and_deleted() {
    let fx = fixture();
    let server = MockServer::start().await;
    let account = exchange_account(&fx, &server);
    let service = exchange_service(&fx, &account).await;
    on(
        &server,
        "<m:CreateFolder",
        response(
            "CreateFolder",
            &[success(
                "CreateFolder",
                r#"<m:Folders><t:TasksFolder><t:FolderId Id="new-id" ChangeKey="n1"/></t:TasksFolder></m:Folders>"#,
            )],
        ),
    )
    .await;
    let created = service.create_list("Errands").await.expect("create");
    assert_eq!(
        (
            created.remote_id.as_str(),
            created.name.as_str(),
            created.is_default
        ),
        ("new-id", "Errands", false)
    );
    let requests = server.received_requests().await.expect("requests");
    let create = body(&requests[0]);
    assert!(
        create.contains(r#"<t:DistinguishedFolderId Id="tasks"/>"#),
        "below the Tasks folder: {create}"
    );
    assert!(create.contains("IPF.Task"), "{create}");
    assert!(create.contains("Errands"), "{create}");

    // The Tasks folder keeps its name.
    let renamed = service
        .rename_list(&stored_list(TaskListKind::Ews, "tasks-id", true), "Chores")
        .await;
    assert!(matches!(renamed, Err(Error::Invalid(_))), "{renamed:?}");
    on(
        &server,
        "<m:UpdateFolder",
        response(
            "UpdateFolder",
            &[success(
                "UpdateFolder",
                r#"<m:Folders><t:TasksFolder><t:FolderId Id="new-id" ChangeKey="n2"/></t:TasksFolder></m:Folders>"#,
            )],
        ),
    )
    .await;
    let list = stored_list(TaskListKind::Ews, "new-id", false);
    service.rename_list(&list, "Chores").await.expect("rename");
    on(
        &server,
        r#"<m:DeleteFolder DeleteType="MoveToDeletedItems""#,
        response("DeleteFolder", &[success("DeleteFolder", "")]),
    )
    .await;
    service.delete_list(&list).await.expect("delete");
    server.reset().await;
    on(
        &server,
        "<m:DeleteFolder",
        response(
            "DeleteFolder",
            &[failure("DeleteFolder", "ErrorFolderNotFound")],
        ),
    )
    .await;
    service.delete_list(&list).await.expect("already gone");
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
}
