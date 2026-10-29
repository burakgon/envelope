//! Folders, mail and contacts against recorded EWS responses.

mod common;

use common::{bodies, fixture, on, requests, setup, soap_fault, soap_ok};
use envelope_ews::{
    AttachmentKind, DeleteType, DistinguishedFolder, FindMessagesOptions, FlagStatus, FolderKind,
    FolderRole, GetMessagesOptions, ItemChange, ItemId, ItemKind, MessageUpdate, MimeDisposition,
    ResolveScope, ResponseCode, SyncOptions,
};
use wiremock::ResponseTemplate;

#[tokio::test]
async fn folder_tree_assigns_roles_and_orders_parents_first() {
    let (server, client) = setup().await;
    on(&server, "<t:BaseShape>IdOnly</t:BaseShape></m:FolderShape><m:FolderIds><t:DistinguishedFolderId Id=\"inbox\"/>", soap_ok("get_folder_well_known.xml")).await;
    on(
        &server,
        "<t:DistinguishedFolderId Id=\"archive\"/>",
        soap_fault(fixture("fault_schema_validation.xml")),
    )
    .await;
    on(&server, "<m:FindFolder", soap_ok("find_folder_deep.xml")).await;

    let tree = client.folder_tree().await.expect("folder tree");
    let names: Vec<&str> = tree.iter().map(|f| f.display_name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Inbox",
            "Sent Items",
            "Deleted Items",
            "Archive",
            "Calendar",
            "Contacts",
            "Conversation Action Settings",
            "Projects & Plans"
        ]
    );
    let role = |name: &str| {
        tree.iter()
            .find(|f| f.display_name == name)
            .and_then(|f| f.role)
    };
    assert_eq!(role("Inbox"), Some(FolderRole::Inbox));
    assert_eq!(role("Sent Items"), Some(FolderRole::SentItems));
    assert_eq!(role("Deleted Items"), Some(FolderRole::DeletedItems));
    assert_eq!(role("Archive"), Some(FolderRole::Archive), "found by name");
    assert_eq!(role("Calendar"), Some(FolderRole::Calendar));
    assert_eq!(role("Projects & Plans"), None);
    let inbox = tree.iter().find(|f| f.display_name == "Inbox").unwrap();
    assert_eq!(inbox.unread_count, Some(5));
    assert_eq!(inbox.total_count, Some(42));
    assert!(inbox.is_mail_folder());
    let calendar = tree.iter().find(|f| f.display_name == "Calendar").unwrap();
    assert_eq!(calendar.kind, FolderKind::Calendar);
    assert!(
        tree.iter()
            .any(|f| f.is_hidden && f.display_name == "Conversation Action Settings")
    );

    // The archive id is not asked for again once the server rejected it.
    let before = requests(&server).await.len();
    let roles = client.well_known_folder_ids().await.unwrap();
    assert_eq!(requests(&server).await.len(), before + 1);
    assert!(
        !roles.contains_key(&FolderRole::Notes),
        "missing folders are skipped"
    );
    assert_eq!(
        roles.get(&FolderRole::Outbox).map(|id| id.id.as_str()),
        Some("outbox-id")
    );
}

#[tokio::test]
async fn sync_folder_items_reports_every_change_kind() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:SyncFolderItems>",
        soap_ok("sync_folder_items.xml"),
    )
    .await;
    let page = client
        .sync_folder_items(DistinguishedFolder::Inbox, None, &SyncOptions::default())
        .await
        .expect("sync");
    assert!(!page.includes_last_item_in_range);
    assert!(page.sync_state.starts_with("H4sI"));
    assert_eq!(page.changes.len(), 5);
    let ItemChange::Created(first) = &page.changes[0] else {
        panic!("expected a created message");
    };
    assert_eq!(first.kind, ItemKind::Message);
    assert_eq!(first.item_class.as_deref(), Some("IPM.Note"));
    assert!(
        matches!(&page.changes[1], ItemChange::Created(m) if m.kind == ItemKind::MeetingRequest && m.kind.is_mail())
    );
    assert!(matches!(&page.changes[2], ItemChange::Updated(m) if m.id.id == "AAMkADk1-msg-0"));
    assert!(matches!(
        &page.changes[3],
        ItemChange::ReadFlagChanged { is_read: true, .. }
    ));
    assert!(matches!(&page.changes[4], ItemChange::Deleted { id } if id.id == "AAMkADk1-msg-gone"));

    // The next round sends the state back.
    client
        .sync_folder_items(
            DistinguishedFolder::Inbox,
            Some(&page.sync_state),
            &SyncOptions::default(),
        )
        .await
        .unwrap();
    let bodies = bodies(&server).await;
    assert!(!bodies[0].contains("<m:SyncState>"));
    assert!(bodies[1].contains(&format!("<m:SyncState>{}</m:SyncState>", page.sync_state)));
    assert!(bodies[1].contains("<m:MaxChangesReturned>512</m:MaxChangesReturned>"));
}

#[tokio::test]
async fn invalid_sync_state_requires_a_resync() {
    let (server, client) = setup().await;
    let body = r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:SyncFolderItemsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Error"><m:MessageText>Synchronization state data is corrupt or otherwise invalid.</m:MessageText><m:ResponseCode>ErrorInvalidSyncStateData</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey><m:SyncState/><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse></s:Body></s:Envelope>"#;
    on(
        &server,
        "<m:SyncFolderItems>",
        ResponseTemplate::new(200).set_body_string(body),
    )
    .await;
    let err = client
        .sync_folder_items(
            DistinguishedFolder::Inbox,
            Some("stale"),
            &SyncOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(err.requires_resync(), "{err:?}");
    assert!(!err.is_transient());
}

#[tokio::test]
async fn find_messages_pages_newest_first() {
    let (server, client) = setup().await;
    on(&server, "<m:FindItem", soap_ok("find_item_page.xml")).await;
    let since = chrono::DateTime::parse_from_rfc3339("2024-04-01T00:00:00Z")
        .unwrap()
        .to_utc();
    let page = client
        .find_messages(
            DistinguishedFolder::Inbox,
            &FindMessagesOptions {
                page_size: 2,
                received_since: Some(since),
                ..FindMessagesOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next_offset, Some(2));
    assert_eq!(page.total_items_in_view, Some(5));
    assert_eq!(page.items[0].subject.as_deref(), Some("Newest"));
    assert_eq!(page.items[0].is_read, Some(false));
    assert_eq!(
        page.items[1].from.as_ref().and_then(|f| f.email.as_deref()),
        Some("sender2@example.com")
    );
    let body = &bodies(&server).await[0];
    assert!(body.contains(
        r#"<m:IndexedPageItemView MaxEntriesReturned="2" Offset="0" BasePoint="Beginning"/>"#
    ));
    assert!(body.contains(r#"<t:FieldOrder Order="Descending"><t:FieldURI FieldURI="item:DateTimeReceived"/></t:FieldOrder>"#));
    assert!(body.contains(r#"<t:Constant Value="2024-04-01T00:00:00Z"/>"#));
    assert!(body.contains(r#"<t:FieldURI FieldURI="message:IsRead"/>"#));
}

#[tokio::test]
async fn search_uses_query_strings() {
    let (server, client) = setup().await;
    on(&server, "<m:QueryString>", soap_ok("find_item_page.xml")).await;
    let page = client
        .search_messages(
            &[
                DistinguishedFolder::Inbox.into(),
                DistinguishedFolder::SentItems.into(),
            ],
            "from:alice \"quarterly report\"",
            0,
            25,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    let body = &bodies(&server).await[0];
    assert!(
        body.contains("<m:QueryString>from:alice &quot;quarterly report&quot;</m:QueryString>")
    );
    assert!(body.contains(
        r#"<t:DistinguishedFolderId Id="inbox"/><t:DistinguishedFolderId Id="sentitems"/>"#
    ));
    assert!(client.search_messages(&[], "x", 0, 10).await.is_err());
}

#[tokio::test]
async fn get_messages_with_mime_and_per_item_errors() {
    let (server, client) = setup().await;
    on(&server, "<m:GetItem>", soap_ok("get_item_messages.xml")).await;
    let ids = [
        ItemId::new("AAMkADk1-msg-1"),
        ItemId::new("AAMkADk1-mr-1"),
        ItemId::new("AAMkADk1-deleted"),
    ];
    let results = client
        .get_messages(
            &ids,
            &GetMessagesOptions {
                include_mime_content: true,
                ..GetMessagesOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    let message = results[0].as_ref().expect("first message");
    assert_eq!(message.subject.as_deref(), Some("Quarterly report"));
    assert_eq!(
        message.from.as_ref().and_then(|m| m.smtp_address()),
        Some("alice@example.com")
    );
    assert_eq!(message.to[0].name.as_deref(), Some("Jane Doe"));
    assert_eq!(message.cc[0].mailbox_type.as_deref(), Some("PublicDL"));
    assert_eq!(
        message.reply_to[0].email.as_deref(),
        Some("finance@example.com")
    );
    assert_eq!(
        message.internet_message_id.as_deref(),
        Some("<report-1@example.com>")
    );
    assert_eq!(
        message.references.as_deref(),
        Some("<thread-0@example.com>")
    );
    assert_eq!(message.conversation_id.as_deref(), Some("AAQkADk1-conv-1"));
    assert_eq!(message.categories, ["Finance", "Red category"]);
    assert_eq!(message.importance, Some(envelope_ews::Importance::High));
    assert_eq!(message.is_read, Some(false));
    assert_eq!(message.is_draft, Some(false));
    assert!(message.is_flagged());
    assert!(message.flag.as_ref().unwrap().due_date.is_some());
    assert_eq!(
        message.preview.as_deref(),
        Some("Hello Jane, the report is attached.")
    );
    assert_eq!(message.size, Some(2048));
    assert_eq!(message.attachments.len(), 1);
    assert_eq!(message.attachments[0].kind, AttachmentKind::File);
    assert_eq!(message.attachments[0].name.as_deref(), Some("report.pdf"));
    let mime = String::from_utf8(message.mime_content.clone().unwrap()).unwrap();
    assert!(mime.starts_with("From: Alice Example <alice@example.com>\r\n"));
    assert!(mime.contains("Subject: Quarterly report\r\n"));

    let request = results[1].as_ref().unwrap();
    assert_eq!(request.kind, ItemKind::MeetingRequest);
    assert_eq!(
        request
            .associated_calendar_item_id
            .as_ref()
            .map(|i| i.id.as_str()),
        Some("AAMkADk1-cal-1")
    );
    let missing = results[2].as_ref().unwrap_err();
    assert_eq!(missing.code, ResponseCode::ErrorItemNotFound);
    assert!(missing.code.is_not_found());

    let body = &bodies(&server).await[0];
    assert!(body.contains("<t:IncludeMimeContent>true</t:IncludeMimeContent>"));
    assert!(body.contains(r#"<t:FieldURI FieldURI="message:ToRecipients"/>"#));
    assert!(body.contains(r#"<t:FieldURI FieldURI="item:Preview"/>"#));
}

#[tokio::test]
async fn updates_flags_read_state_and_categories_in_one_batch() {
    let (server, client) = setup().await;
    on(&server, "<m:UpdateItem", soap_ok("update_item.xml")).await;
    let results = client
        .update_messages(&[
            (
                ItemId::with_change_key("AAMkADk1-msg-1", "old"),
                MessageUpdate {
                    is_read: Some(true),
                    flag: Some(FlagStatus::Flagged),
                    categories: Some(vec!["Finance".into()]),
                },
            ),
            (
                ItemId::new("AAMkADk1-gone"),
                MessageUpdate {
                    flag: Some(FlagStatus::NotFlagged),
                    ..MessageUpdate::default()
                },
            ),
        ])
        .await
        .unwrap();
    assert_eq!(
        results[0]
            .as_ref()
            .unwrap()
            .as_ref()
            .and_then(|id| id.change_key.as_deref()),
        Some("CQAAABYAAAD-new")
    );
    assert!(results[1].as_ref().unwrap_err().code.is_not_found());
    let body = &bodies(&server).await[0];
    assert!(body.starts_with(r#"<?xml version="1.0" encoding="utf-8"?>"#));
    assert!(body.contains(r#"<m:UpdateItem MessageDisposition="SaveOnly" ConflictResolution="AlwaysOverwrite" SuppressReadReceipts="true">"#));
    assert!(body.contains(r#"<t:ItemId Id="AAMkADk1-msg-1" ChangeKey="old"/>"#));
    assert!(body.contains("<t:Flag><t:FlagStatus>Flagged</t:FlagStatus></t:Flag>"));
    assert!(body.contains("<t:Flag><t:FlagStatus>NotFlagged</t:FlagStatus></t:Flag>"));
    assert!(body.contains("<t:Categories><t:String>Finance</t:String></t:Categories>"));
    assert!(
        client
            .update_messages(&[(ItemId::new("x"), MessageUpdate::default())])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn moves_copies_and_deletes() {
    let (server, client) = setup().await;
    on(&server, "<m:MoveItem>", soap_ok("move_item.xml")).await;
    on(&server, "<m:DeleteItem", soap_ok("delete_item.xml")).await;
    let moved = client
        .move_items(
            &[ItemId::new("a"), ItemId::new("b")],
            DistinguishedFolder::Archive,
        )
        .await
        .unwrap();
    assert_eq!(
        moved[0].as_ref().unwrap().as_ref().map(|id| id.id.as_str()),
        Some("AAMkADk1-moved-1")
    );
    assert_eq!(moved[1].as_ref().unwrap(), &None);
    client
        .delete_items(&[ItemId::new("c")], DeleteType::HardDelete)
        .await
        .unwrap()
        .remove(0)
        .unwrap();
    let bodies = bodies(&server).await;
    assert!(bodies[0].contains("<m:ReturnNewItemIds>true</m:ReturnNewItemIds>"));
    assert!(bodies[1].contains(r#"<m:DeleteItem DeleteType="HardDelete" SendMeetingCancellations="SendToNone" AffectedTaskOccurrences="AllOccurrences" SuppressReadReceipts="true">"#));
}

#[tokio::test]
async fn sends_and_saves_mime_messages() {
    let (server, client) = setup().await;
    on(
        &server,
        "MessageDisposition=\"SendAndSaveCopy\"",
        soap_ok("create_item_sent.xml"),
    )
    .await;
    on(
        &server,
        "MessageDisposition=\"SaveOnly\"",
        soap_ok("create_item_draft.xml"),
    )
    .await;
    let mime = "From: jane@example.com\r\nTo: alice@example.com\r\nBcc: boss@example.com\r\nSubject: Ünïcode ✓\r\n\r\nHi\r\n";
    client.send_mime(mime.as_bytes()).await.expect("sent");
    let draft = client
        .save_draft_mime(mime.as_bytes())
        .await
        .expect("draft");
    assert_eq!(draft.id, "AAMkADk1-draft-1");
    let imported = client
        .create_mime_message(
            mime.as_bytes(),
            &MimeDisposition::Import {
                folder: DistinguishedFolder::Inbox.into(),
                is_read: true,
            },
        )
        .await
        .unwrap();
    assert!(imported.is_some());
    let bodies = bodies(&server).await;
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(mime);
    assert!(bodies[0].contains(&format!(
        r#"<t:MimeContent CharacterSet="UTF-8">{encoded}</t:MimeContent>"#
    )));
    assert!(bodies[0].contains(r#"<t:DistinguishedFolderId Id="sentitems"/>"#));
    assert!(bodies[1].contains(r#"<t:DistinguishedFolderId Id="drafts"/>"#));
    assert!(bodies[1].contains("<t:Value>9</t:Value>"));
    // Imported as read, and not as a draft.
    assert!(
        bodies[2].contains(r#"PropertyTag="0x0e07" PropertyType="Integer"/><t:Value>1</t:Value>"#)
    );
    assert!(client.send_mime(b"").await.is_err());
}

#[tokio::test]
async fn downloads_attachments() {
    let (server, client) = setup().await;
    on(&server, "<m:GetAttachment>", soap_ok("get_attachment.xml")).await;
    let attachment = client.get_attachment("AAMkADk1-att-1").await.unwrap();
    assert_eq!(attachment.info.name.as_deref(), Some("report.pdf"));
    assert_eq!(
        attachment.content.as_deref(),
        Some(&b"%PDF-1.4 fake report"[..])
    );
    assert!(!attachment.info.is_inline);
}

#[tokio::test]
async fn resolves_names_for_autocomplete() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:UnresolvedEntry>ali</m:UnresolvedEntry>",
        soap_ok("resolve_names.xml"),
    )
    .await;
    on(
        &server,
        "<m:UnresolvedEntry>zz</m:UnresolvedEntry>",
        soap_ok("resolve_names_none.xml"),
    )
    .await;
    let found = client
        .resolve_names("ali", ResolveScope::ActiveDirectoryContacts, true)
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].mailbox.smtp_address(), Some("alice@example.com"));
    let contact = found[0].contact.as_ref().unwrap();
    assert_eq!(contact.job_title.as_deref(), Some("Controller"));
    assert_eq!(contact.primary_email(), Some("alice@example.com"));
    assert_eq!(found[1].mailbox.mailbox_type.as_deref(), Some("Contact"));
    assert!(
        client
            .resolve_names("zz", ResolveScope::Contacts, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        client
            .resolve_names("  ", ResolveScope::Contacts, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(requests(&server).await.len(), 2);
}
