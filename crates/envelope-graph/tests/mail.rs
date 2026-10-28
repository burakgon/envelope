//! Profile, mailbox settings, folders, message sync and message operations
//! against a mock Graph server with fixtures modelled on the Graph docs.

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{TimeZone, Utc};
use common::{fixture, json_body, prefer_contains, requests, setup};
use envelope_graph::{
    AutomaticRepliesSetting, AutomaticRepliesStatus, CategoryColor, DateTimeTimeZone, DeltaCursor,
    ExternalAudienceScope, FlagStatus, FolderRole, GraphError, Importance, InferenceClassification,
    MessageDeltaOptions, MessageListOptions,
};
use serde_json::json;
use wiremock::matchers::{body_string, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

#[tokio::test]
async fn reads_mailbox_settings_and_time_zone() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailboxSettings"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "mailbox_settings.json")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailboxSettings/timeZone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#users('16f5a7b6')/mailboxSettings/timeZone",
            "value": "Pacific Standard Time"
        })))
        .mount(&server)
        .await;

    let settings = client.mailbox_settings().await.expect("settings");
    assert_eq!(settings.time_zone.as_deref(), Some("UTC"));
    assert_eq!(settings.date_format.as_deref(), Some("MM/dd/yyyy"));
    assert_eq!(
        settings.language.and_then(|l| l.locale).as_deref(),
        Some("en-US")
    );
    let replies = settings
        .automatic_replies_setting
        .expect("automatic replies");
    assert_eq!(replies.status, AutomaticRepliesStatus::Scheduled);
    assert_eq!(replies.external_audience, ExternalAudienceScope::All);
    assert!(replies.internal_reply_message.contains("worldwide reunion"));

    assert_eq!(
        client.mailbox_time_zone().await.expect("zone"),
        "Pacific Standard Time"
    );
}

#[tokio::test]
async fn gets_and_sets_automatic_replies() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailboxSettings/automaticRepliesSetting"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "automatic_replies.json")),
        )
        .mount(&server)
        .await;
    let expected = json!({
        "automaticRepliesSetting": {
            "status": "scheduled",
            "externalAudience": "all",
            "scheduledStartDateTime": {"dateTime": "2030-03-20T18:00:00", "timeZone": "UTC"},
            "scheduledEndDateTime": {"dateTime": "2030-03-28T18:00:00", "timeZone": "UTC"},
            "internalReplyMessage": "<p>Away until the 28th.</p>",
            "externalReplyMessage": "<p>I am out of office.</p>"
        }
    });
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/mailboxSettings"))
        .and(json_body(expected.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(expected))
        .expect(1)
        .mount(&server)
        .await;

    let current = client.automatic_replies().await.expect("current");
    assert_eq!(current.status, AutomaticRepliesStatus::AlwaysEnabled);
    assert_eq!(
        current.external_audience,
        ExternalAudienceScope::ContactsOnly
    );

    let start = Utc
        .with_ymd_and_hms(2030, 3, 20, 18, 0, 0)
        .single()
        .expect("start");
    let end = Utc
        .with_ymd_and_hms(2030, 3, 28, 18, 0, 0)
        .single()
        .expect("end");
    let setting = AutomaticRepliesSetting {
        status: AutomaticRepliesStatus::Scheduled,
        external_audience: ExternalAudienceScope::All,
        scheduled_start_date_time: Some(DateTimeTimeZone::utc(start)),
        scheduled_end_date_time: Some(DateTimeTimeZone::utc(end)),
        internal_reply_message: "<p>Away until the 28th.</p>".into(),
        external_reply_message: "<p>I am out of office.</p>".into(),
    };
    client.set_automatic_replies(&setting).await.expect("saved");

    let incomplete = AutomaticRepliesSetting {
        scheduled_end_date_time: None,
        ..setting
    };
    assert!(matches!(
        client.set_automatic_replies(&incomplete).await,
        Err(GraphError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn builds_the_folder_tree_with_roles() {
    let (server, client, _) = setup().await;
    let select =
        "id,displayName,parentFolderId,childFolderCount,unreadItemCount,totalItemCount,isHidden";
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders"))
        .and(query_param("$select", select))
        .and(query_param("$top", "100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "mail_folders_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders"))
        .and(query_param("$skip", "4"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "mail_folders_page2.json")),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    for (id, name) in [
        ("AQMkADYAAAIBDAAAAA==", "mail_folders_inbox_children.json"),
        ("AAMkADYProjects==", "mail_folders_projects_children.json"),
        ("AQMkADYAAAIBFQAAAA==", "mail_folders_empty.json"),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/v1.0/me/mailFolders/{id}/childFolders")))
            .and(query_param("$select", select))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(&server, name)))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "well_known_batch.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let folders = client.mail_folder_tree().await.expect("tree");
    let names: Vec<&str> = folders.iter().map(|f| f.display_name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Archive",
            "Conversation History",
            "Deleted Items",
            "Drafts",
            "Inbox",
            "Junk Email",
            "Outbox",
            "Sent Items",
            "Projects",
            "Envelope"
        ]
    );
    let by_name = |name: &str| folders.iter().find(|f| f.display_name == name).expect(name);
    assert_eq!(by_name("Inbox").role, Some(FolderRole::Inbox));
    assert_eq!(by_name("Inbox").unread_count, 2);
    assert_eq!(by_name("Sent Items").role, Some(FolderRole::SentItems));
    assert_eq!(
        by_name("Deleted Items").role,
        Some(FolderRole::DeletedItems)
    );
    assert_eq!(by_name("Junk Email").role, Some(FolderRole::JunkEmail));
    assert_eq!(by_name("Outbox").role, Some(FolderRole::Outbox));
    assert_eq!(by_name("Drafts").role, Some(FolderRole::Drafts));
    // The archive lookup returned 404 in the batch.
    assert_eq!(by_name("Archive").role, None);
    assert_eq!(
        by_name("Projects").parent_id.as_deref(),
        Some("AQMkADYAAAIBDAAAAA==")
    );
    assert_eq!(
        by_name("Envelope").parent_id.as_deref(),
        Some("AAMkADYProjects==")
    );

    // The batch asked for every well-known folder by name.
    let batch = requests(&server)
        .await
        .into_iter()
        .find(|r| r.url.path().ends_with("$batch"))
        .expect("batch request");
    let body: serde_json::Value = batch.body_json().expect("json");
    let urls: Vec<&str> = body["requests"]
        .as_array()
        .expect("requests")
        .iter()
        .filter_map(|r| r["url"].as_str())
        .collect();
    assert!(urls.contains(&"/me/mailFolders/inbox?$select=id"));
    assert!(urls.contains(&"/me/mailFolders/archive?$select=id"));
    assert_eq!(urls.len(), 7);
}

#[tokio::test]
async fn manages_folders() {
    let (server, client, _) = setup().await;
    let created = json!({"id": "AAMkNew=", "displayName": "Receipts", "parentFolderId": "AQMkADYAAAIBDAAAAA==",
                         "childFolderCount": 0, "unreadItemCount": 0, "totalItemCount": 0, "isHidden": false});
    Mock::given(method("POST"))
        .and(path("/v1.0/me/mailFolders/inbox/childFolders"))
        .and(json_body(
            json!({"displayName": "Receipts", "isHidden": false}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(created.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/mailFolders"))
        .and(json_body(json!({"displayName": "Top", "isHidden": false})))
        .respond_with(ResponseTemplate::new(201).set_body_json(created.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/mailFolders/AAMkNew="))
        .and(json_body(json!({"displayName": "Invoices"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(created))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/mailFolders/AAMkNew="))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let folder = client
        .create_mail_folder(Some("inbox"), "Receipts")
        .await
        .expect("created");
    assert_eq!(folder.id, "AAMkNew=");
    client
        .create_mail_folder(None, "Top")
        .await
        .expect("top-level");
    client
        .rename_mail_folder("AAMkNew=", "Invoices")
        .await
        .expect("renamed");
    client
        .delete_mail_folder("AAMkNew=")
        .await
        .expect("deleted");
}

#[tokio::test]
async fn syncs_a_folder_page_by_page() {
    let (server, client, _) = setup().await;
    let select = envelope_graph::MESSAGE_LIST_FIELDS.join(",");
    Mock::given(method("GET"))
        .and(path(
            "/v1.0/me/mailFolders/AQMkADNkNAAAgEMAAAA/messages/delta",
        ))
        .and(query_param("$select", select.as_str()))
        .and(query_param(
            "$filter",
            "receivedDateTime ge 2017-07-01T00:00:00Z",
        ))
        .and(query_param("$orderby", "receivedDateTime desc"))
        .and(prefer_contains("odata.maxpagesize=2"))
        .and(prefer_contains("IdType=\"ImmutableId\""))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "message_delta_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/v1.0/me/mailfolders('AQMkADNkNAAAgEMAAAA')/messages/delta",
        ))
        .and(query_param("$skiptoken", "GwcBoTmPuoTQWfcsAbkYM"))
        .and(prefer_contains("odata.maxpagesize=2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "message_delta_page2.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let options = MessageDeltaOptions {
        page_size: Some(2),
        received_since: Utc.with_ymd_and_hms(2017, 7, 1, 0, 0, 0).single(),
        ..MessageDeltaOptions::default()
    };
    let first = client
        .message_delta("AQMkADNkNAAAgEMAAAA", None, &options)
        .await
        .expect("first page");
    assert_eq!(first.items.len(), 2);
    assert!(first.removed_ids.is_empty());
    assert!(matches!(first.next, DeltaCursor::Next(_)));
    let message = &first.items[0];
    assert_eq!(message.subject.as_deref(), Some("Holiday hours update"));
    assert_eq!(
        message.from.as_ref().and_then(|r| r.address()),
        Some("danas@contoso.com")
    );
    assert_eq!(message.cc_recipients.len(), 1);
    assert!(message.is_flagged());
    assert!(message.has_attachments);
    assert_eq!(message.importance, Importance::High);
    assert_eq!(message.categories, vec!["Red category".to_owned()]);
    assert_eq!(
        message.inference_classification,
        Some(InferenceClassification::Focused)
    );
    assert_eq!(
        message.received_date_time,
        Utc.with_ymd_and_hms(2017, 7, 22, 1, 53, 57).single()
    );

    // Persist the cursor, then resume from it.
    let stored = serde_json::to_string(&first.next).expect("cursor serializes");
    let cursor: DeltaCursor = serde_json::from_str(&stored).expect("cursor parses");
    let second = client
        .message_delta("ignored-when-resuming", Some(&cursor), &options)
        .await
        .expect("second page");
    assert_eq!(
        second.removed_ids,
        vec!["AAMkADk0MGFkODE3LWE4MmYtNDRhOS0Dh_6qB-pB2Sa2pUum19a6YAAKnLuxoAAA=".to_owned()]
    );
    assert_eq!(second.items.len(), 1);
    assert!(
        second.items[0].is_read,
        "string \"true\" from the docs sample"
    );
    match &second.next {
        DeltaCursor::Done(link) => assert!(link.ends_with("$deltatoken=GwcBoTmPuoGNlgXgF1nyUNMXY")),
        other => panic!("expected a delta link, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_expired_delta_links() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/inbox/messages/delta"))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({
            "error": {"code": "SyncStateNotFound", "message": "The sync state is no longer valid."}
        })))
        .mount(&server)
        .await;
    let stale = DeltaCursor::Done(format!(
        "{}/me/mailFolders/inbox/messages/delta?$deltatoken=old",
        common::base(&server)
    ));
    let err = client
        .message_delta("inbox", Some(&stale), &MessageDeltaOptions::default())
        .await
        .expect_err("expired");
    assert!(err.is_resync_required());
}

#[tokio::test]
async fn lists_and_searches_messages() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/inbox/messages"))
        .and(query_param("$top", "2"))
        .and(query_param("$orderby", "receivedDateTime desc"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "messages_list.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/inbox/messages"))
        .and(query_param("$skip", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages"))
        .and(query_param(
            "$search",
            "\"from:danas subject:\\\"holiday hours\\\"\"",
        ))
        .and(query_param("$top", "25"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "messages_list.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let options = MessageListOptions {
        top: 2,
        ..MessageListOptions::default()
    };
    let page = client.list_messages("inbox", &options).await.expect("page");
    assert_eq!(page.items.len(), 2);
    assert!(page.items[1].to_recipients.is_empty(), "null recipients");
    let next = page.next_link.expect("next link");
    let rest = client.list_messages_next(&next).await.expect("next page");
    assert!(rest.items.is_empty() && rest.next_link.is_none());

    let found = client
        .search_messages("from:danas subject:\"holiday hours\"", None, 25)
        .await
        .expect("search");
    assert_eq!(found.items.len(), 2);
    let raw_query = requests(&server)
        .await
        .into_iter()
        .find(|r| r.url.path() == "/v1.0/me/messages")
        .and_then(|r| r.url.query().map(str::to_owned))
        .unwrap_or_default();
    assert!(raw_query.contains("$search=%22from:danas%20subject:%5C%22holiday%20hours%5C%22%22"));
}

#[tokio::test]
async fn fetches_mime_and_headers() {
    let (server, client, _) = setup().await;
    let mime = "From: Dana Swope <danas@contoso.com>\r\nTo: MeganB@contoso.com\r\nSubject: Holiday hours update\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe office will be closed on Friday.\r\n";
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages/AAMkADhAAAW-VPeAAA=/$value"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(mime, "text/plain"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages/AAMkADhAAAW-VPeAAA="))
        .and(query_param("$select", "internetMessageHeaders"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "message_headers.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let bytes = client
        .get_message_mime("AAMkADhAAAW-VPeAAA=")
        .await
        .expect("mime");
    assert_eq!(&bytes[..], mime.as_bytes());
    let headers = client
        .get_message_headers("AAMkADhAAAW-VPeAAA=")
        .await
        .expect("headers");
    assert_eq!(headers.len(), 3);
    assert_eq!(headers[1].name, "List-Unsubscribe");
}

#[tokio::test]
async fn updates_flags_read_state_and_categories() {
    let (server, client, _) = setup().await;
    for body in [
        json!({"isRead": true}),
        json!({"flag": {"flagStatus": "complete"}}),
        json!({"categories": ["Red category", "Blue category"]}),
    ] {
        Mock::given(method("PATCH"))
            .and(path("/v1.0/me/messages/AAMk1="))
            .and(header("content-type", "application/json"))
            .and(json_body(body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "AAMk1="})))
            .expect(1)
            .mount(&server)
            .await;
    }
    client.set_message_read("AAMk1=", true).await.expect("read");
    client
        .set_message_flag("AAMk1=", FlagStatus::Complete)
        .await
        .expect("flag");
    client
        .set_message_categories("AAMk1=", &["Red category".into(), "Blue category".into()])
        .await
        .expect("categories");
    // Empty updates do not hit the network.
    client
        .update_message("AAMk1=", &envelope_graph::MessageUpdate::new())
        .await
        .expect("noop");
}

#[tokio::test]
async fn moves_copies_and_deletes_messages() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/messages/AAMkOld=/move"))
        .and(json_body(json!({"destinationId": "deleteditems"})))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(fixture(&server, "message_move.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/messages/AAMkOld=/move"))
        .and(json_body(json!({"destinationId": "AQMkADYAAAIBXQAAAA=="})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "AAMkMoved="})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/messages/AAMkOld=/copy"))
        .and(json_body(json!({"destinationId": "inbox"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "AAMkCopy="})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/messages/AAMkOld="))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/messages/AAMkOld=/permanentDelete"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        client.trash_message("AAMkOld=").await.expect("trash"),
        "AAMkADhAAAW-VPeAAA="
    );
    assert_eq!(
        client
            .move_message("AAMkOld=", "AQMkADYAAAIBXQAAAA==")
            .await
            .expect("move"),
        "AAMkMoved="
    );
    assert_eq!(
        client
            .copy_message("AAMkOld=", "inbox")
            .await
            .expect("copy"),
        "AAMkCopy="
    );
    client.delete_message("AAMkOld=").await.expect("delete");
    client
        .permanently_delete_message("AAMkOld=")
        .await
        .expect("purge");
}

#[tokio::test]
async fn moves_messages_in_a_batch() {
    let (server, client, _) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"responses": [
                {"id": "1", "status": 201, "headers": {}, "body": {"id": "new-b"}},
                {"id": "0", "status": 201, "headers": {}, "body": {"id": "new-a"}}
            ]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let moved = client
        .move_messages(&["a".into(), "b".into()], "archive")
        .await
        .expect("batch");
    let ids: Vec<String> = moved.into_iter().map(|r| r.expect("moved")).collect();
    assert_eq!(ids, vec!["new-a".to_owned(), "new-b".to_owned()]);
    let body: serde_json::Value = requests(&server).await[0].body_json().expect("json");
    assert_eq!(body["requests"][0]["url"], "/me/messages/a/move");
    assert_eq!(
        body["requests"][0]["body"],
        json!({"destinationId": "archive"})
    );
}

#[tokio::test]
async fn sends_mime_and_manages_drafts() {
    let (server, client, _) = setup().await;
    let mime = b"From: Megan Bowen <MeganB@contoso.com>\r\nTo: Alex Wilber <AlexW@contoso.com>\r\nSubject: Test Message\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\ntest text body\r\n";
    let encoded = BASE64.encode(mime);
    Mock::given(method("POST"))
        .and(path("/v1.0/me/sendMail"))
        .and(header("content-type", "text/plain"))
        .and(body_string(encoded.clone()))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/messages"))
        .and(header("content-type", "text/plain"))
        .and(body_string(encoded))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "AAMkADA1MTAAAAqldOAAA=", "subject": "Test Message", "isDraft": true, "isRead": true,
            "toRecipients": [{"emailAddress": {"name": "Alex Wilber", "address": "AlexW@contoso.com"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/messages/AAMkADA1MTAAAAqldOAAA=/send"))
        .and(header("content-length", "0"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/messages/AAMkADA1MTAAAAqldOAAA="))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/sendMail"))
        .and(body_string("bm90IG1pbWU="))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": "ErrorMimeContentInvalidBase64String", "message": "Invalid base64 string for MIME content."}
        })))
        .mount(&server)
        .await;

    client.send_mime(mime).await.expect("sent");
    let draft = client.create_draft_mime(mime, None).await.expect("draft");
    assert!(draft.is_draft);
    assert_eq!(draft.subject.as_deref(), Some("Test Message"));
    client.send_draft(&draft.id).await.expect("draft sent");
    client.delete_draft(&draft.id).await.expect("draft deleted");

    let err = client.send_mime(b"not mime").await.expect_err("rejected");
    assert_eq!(err.code(), Some("ErrorMimeContentInvalidBase64String"));
    assert!(matches!(
        client.send_mime(b"").await,
        Err(GraphError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn reads_and_edits_master_categories() {
    let (server, client, _) = setup().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/outlook/masterCategories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture(&server, "master_categories.json")),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/outlook/masterCategories"))
        .and(json_body(json!({"displayName": "Project expenses", "color": "preset9"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "ee9d6f23-c7cc-4db1-8f0f-5e5d0d5d2f9e", "displayName": "Project expenses", "color": "preset9"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(
            "/v1.0/me/outlook/masterCategories/ee9d6f23-c7cc-4db1-8f0f-5e5d0d5d2f9e",
        ))
        .and(json_body(json!({"color": "preset11"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let categories = client.master_categories().await.expect("categories");
    assert_eq!(categories.len(), 6);
    assert_eq!(categories[0].display_name, "Red category");
    assert_eq!(categories[0].color, CategoryColor::Preset0);
    assert_eq!(categories[0].color.hex(), Some("#e74856"));
    assert_eq!(categories[4].color.name(), "Blue");

    let created = client
        .create_category("Project expenses", CategoryColor::Preset9)
        .await
        .expect("created");
    assert_eq!(created.color.name(), "Cranberry");
    client
        .set_category_color(&created.id, CategoryColor::Preset11)
        .await
        .expect("recolored");
}
