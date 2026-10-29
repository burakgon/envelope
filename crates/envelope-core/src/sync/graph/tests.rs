//! Microsoft Graph mail: mapping functions, and folder/message/operation sync
//! against a mock Graph server with a real (temporary) database.

use std::sync::Arc;

use base64::Engine as _;
use envelope_graph::{GraphClient, RetryPolicy, StaticToken};
use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::engine::{EventSink, Paths};

// ---------------------------------------------------------------------------
// Mapping

fn graph_message(value: Value) -> Message {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("message fixture: {e}"))
}

fn person(name: &str, address: &str) -> Value {
    json!({ "emailAddress": { "name": name, "address": address } })
}

#[test]
fn maps_listed_messages() {
    let m = graph_message(json!({
        "id": "AAMkAD=",
        "internetMessageId": "<plan@contoso.com>",
        "conversationId": "AAQkAD=",
        "subject": "  Quarterly plan ",
        "bodyPreview": "Here is the plan.\r\n\r\nOn Monday, Dana wrote:\r\n> old",
        "from": person("Dana Swope", "danas@contoso.com"),
        "sender": person("Assistant", "assistant@contoso.com"),
        "toRecipients": [person("Megan Bowen", "MeganB@contoso.com"), { "emailAddress": { "name": "X.500 only" } }],
        "ccRecipients": null,
        "replyTo": [person("", "replies@contoso.com")],
        "receivedDateTime": "2026-10-01T09:00:00Z",
        "sentDateTime": "2026-10-01T08:59:00Z",
        "isRead": false,
        "isDraft": false,
        "flag": { "flagStatus": "flagged" },
        "hasAttachments": true,
        "categories": ["Red category"],
        "inferenceClassification": "other"
    }));
    let meta = message_meta(&m);
    assert_eq!(meta.remote_id.as_deref(), Some("AAMkAD="));
    assert_eq!(meta.remote_thread_id.as_deref(), Some("AAQkAD="));
    assert_eq!(meta.message_id.as_deref(), Some("plan@contoso.com"));
    assert_eq!(meta.subject, "Quarterly plan");
    let from = meta.from.expect("from");
    assert_eq!(from.email, "danas@contoso.com");
    assert_eq!(from.name.as_deref(), Some("Dana Swope"));
    assert_eq!(
        meta.to.len(),
        1,
        "recipients without an address are skipped"
    );
    assert_eq!(meta.reply_to[0].name, None);
    let at = |s: &str| chrono::DateTime::parse_from_rfc3339(s).map(|d| d.timestamp_millis());
    assert_eq!(Ok(meta.date), at("2026-10-01T09:00:00Z"));
    assert_eq!(meta.flags, Flags::FLAGGED);
    assert!(meta.has_attachments);
    assert_eq!(meta.snippet.as_deref(), Some("Here is the plan."));
    assert_eq!(meta.keywords, vec![mail::OTHER_INBOX_KEYWORD.to_string()]);

    let draft = graph_message(json!({
        "id": "d", "isRead": "true", "isDraft": true,
        "sender": person("Megan Bowen", "megan@contoso.com"),
        "sentDateTime": "2026-10-01T08:59:00Z",
        "flag": { "flagStatus": "complete" },
        "inferenceClassification": "focused"
    }));
    let meta = message_meta(&draft);
    assert_eq!(meta.flags, Flags::SEEN | Flags::DRAFT);
    assert_eq!(
        meta.from.map(|a| a.email).as_deref(),
        Some("megan@contoso.com")
    );
    assert_eq!(
        Ok(meta.date),
        at("2026-10-01T08:59:00Z"),
        "falls back to the sent time"
    );
    assert!(meta.keywords.is_empty());
    assert_eq!(meta.remote_thread_id, None);
}

fn mail_folder(
    id: &str,
    name: &str,
    parent: &str,
    role: Option<envelope_graph::FolderRole>,
) -> MailFolder {
    let mut folder: MailFolder = serde_json::from_value(json!({
        "id": id, "displayName": name, "parentFolderId": parent, "childFolderCount": 0,
        "unreadItemCount": 0, "totalItemCount": 0, "isHidden": false
    }))
    .unwrap_or_else(|e| panic!("folder fixture: {e}"));
    folder.role = role;
    folder
}

#[test]
fn maps_the_folder_tree_and_categories() {
    use envelope_graph::FolderRole as G;
    let tree = vec![
        mail_folder("inbox", "Inbox", "root", Some(G::Inbox)),
        mail_folder("sent", "Sent Items", "root", Some(G::SentItems)),
        mail_folder("deleted", "Deleted Items", "root", Some(G::DeletedItems)),
        mail_folder("junk", "Junk Email", "root", Some(G::JunkEmail)),
        mail_folder("drafts", "Drafts", "root", Some(G::Drafts)),
        mail_folder("outbox", "Outbox", "root", Some(G::Outbox)),
        mail_folder("archive", "Archive", "root", Some(G::Archive)),
        mail_folder("projects", "Projects", "inbox", None),
        mail_folder("alpha", "Alpha", "projects", None),
    ];
    let categories: Vec<OutlookCategory> = serde_json::from_value(json!([
        { "id": "1", "displayName": "Red category", "color": "preset0" },
        { "id": "2", "displayName": "Plain", "color": "none" },
        { "id": "3", "displayName": "red CATEGORY", "color": "preset7" }
    ]))
    .unwrap_or_default();
    let upserts = folder_upserts(&tree, &categories);
    let by_id = |id: &str| upserts.iter().find(|u| u.remote_id == id);
    assert!(by_id("outbox").is_none(), "the outbox is not synchronized");
    assert_eq!(by_id("inbox").and_then(|u| u.role), Some(FolderRole::Inbox));
    assert_eq!(by_id("sent").and_then(|u| u.role), Some(FolderRole::Sent));
    assert_eq!(
        by_id("deleted").and_then(|u| u.role),
        Some(FolderRole::Trash)
    );
    assert_eq!(by_id("junk").and_then(|u| u.role), Some(FolderRole::Junk));
    assert_eq!(
        by_id("drafts").and_then(|u| u.role),
        Some(FolderRole::Drafts)
    );
    assert_eq!(
        by_id("archive").and_then(|u| u.role),
        Some(FolderRole::Archive)
    );
    let alpha = by_id("alpha").expect("alpha");
    assert_eq!(alpha.path, "Inbox/Projects/Alpha");
    assert_eq!(alpha.parent_remote_id.as_deref(), Some("projects"));
    assert_eq!(
        by_id("inbox").and_then(|u| u.parent_remote_id.clone()),
        None
    );
    let red = by_id("category:Red category").expect("category label");
    assert_eq!(red.kind, "label");
    assert!(!red.selectable);
    assert_eq!(red.color.as_deref(), Some("#e74856"));
    assert_eq!(by_id("category:Plain").and_then(|u| u.color.clone()), None);
    assert!(
        by_id("category:red CATEGORY").is_none(),
        "names are case-insensitive"
    );
}

#[test]
fn falls_back_to_a_folder_named_archive() {
    let tree = vec![
        mail_folder(
            "inbox",
            "Inbox",
            "root",
            Some(envelope_graph::FolderRole::Inbox),
        ),
        mail_folder("nested", "Archive", "inbox", None),
        mail_folder("top", "archive", "root", None),
    ];
    let upserts = folder_upserts(&tree, &[]);
    let role = |id: &str| {
        upserts
            .iter()
            .find(|u| u.remote_id == id)
            .and_then(|u| u.role)
    };
    assert_eq!(role("top"), Some(FolderRole::Archive));
    assert_eq!(role("nested"), None);
}

#[test]
fn delta_state_survives_the_folder_state() {
    let mut state = FolderSyncState::default();
    assert_eq!(DeltaState::load(&state), DeltaState::default());
    let delta = DeltaState {
        delta: Some(DeltaCursor::Done(
            "https://graph.example/delta?$deltatoken=x".into(),
        )),
        full: false,
        prune: true,
    };
    delta.store(&mut state);
    assert_eq!(DeltaState::load(&state), delta);
    state.cursor = Some("not json".into());
    assert_eq!(DeltaState::load(&state), DeltaState::default());
}

fn api_error(status: u16, code: &str) -> GraphError {
    GraphError::Api(Box::new(envelope_graph::ApiError {
        status,
        code: code.into(),
        message: format!("{code} happened"),
        inner_code: None,
        request_id: None,
        client_request_id: None,
        retry_after: None,
    }))
}

#[test]
fn classifies_graph_failures() {
    assert!(graph_error(api_error(401, "InvalidAuthenticationToken"), None).is_auth());
    for status in [0, 429, 500, 503, 504] {
        assert!(
            graph_error(api_error(status, "x"), None).is_transient(),
            "{status}"
        );
    }
    assert!(matches!(
        graph_error(api_error(404, "ErrorItemNotFound"), None),
        Error::NotFound(_)
    ));
    assert!(matches!(
        graph_error(api_error(413, "x"), None),
        Error::Invalid(_)
    ));
    let denied = graph_error(api_error(403, "ErrorAccessDenied"), None);
    assert!(!denied.is_auth() && !denied.is_transient());
    assert_eq!(denied.to_string(), "ErrorAccessDenied happened");
    let rest = graph_error(api_error(401, "MailboxNotEnabledForRESTAPI"), None);
    assert!(!rest.is_auth(), "re-signing in cannot help: {rest}");

    let token = |failure| graph_error(GraphError::Auth("no token".into()), failure);
    assert!(token(None).is_auth());
    assert!(token(Some(TokenFailure::Auth)).is_auth());
    assert!(token(Some(TokenFailure::Network)).is_transient());
    let other = token(Some(TokenFailure::Other));
    assert!(!other.is_auth() && !other.is_transient());
    assert!(!graph_error(GraphError::Decode("bad".into()), None).is_transient());
}

#[test]
fn adds_bcc_recipients_to_the_headers() {
    let mime = b"From: Megan <megan@contoso.com>\r\nTo: Dana <danas@contoso.com>\r\nCc: alex@contoso.com\r\nSubject: hi\r\n\r\nBody\r\n";
    let recipients = vec![
        "DanaS@contoso.com".to_string(),
        "alex@contoso.com".to_string(),
        "hidden@example.com".to_string(),
        "HIDDEN@example.com".to_string(),
        "boss@example.com".to_string(),
    ];
    let out = with_bcc_header(mime, &recipients);
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.starts_with("Bcc: hidden@example.com,\r\n boss@example.com\r\nFrom: "),
        "{text}"
    );
    assert!(text.ends_with("\r\n\r\nBody\r\n"));
    let parsed = mime::meta_from_headers(&out[..header_end(&out)]);
    assert_eq!(parsed.bcc.len(), 2);
    // Nothing hidden: the message is sent untouched.
    let visible = vec!["danas@contoso.com".to_string()];
    assert!(matches!(with_bcc_header(mime, &visible), Cow::Borrowed(_)));
}

// ---------------------------------------------------------------------------
// Sync against a mock server

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

impl Events {
    fn new_mail(&self) -> Vec<Vec<ThreadSummary>> {
        self.0
            .lock()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::NewMail { threads, .. } => Some(threads.clone()),
                _ => None,
            })
            .collect()
    }
}

struct FolderView {
    parent_id: Option<i64>,
    color: Option<String>,
    hidden: bool,
    total_count: i64,
    unread_count: i64,
}

struct Fixture {
    server: MockServer,
    engine: Engine,
    events: Events,
    account: AccountRow,
    graph: GraphMail,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    let dir =
        std::env::temp_dir().join(format!("envelope-graph-{}", uuid::Uuid::new_v4().simple()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .expect("engine");
    let config = AccountConfig {
        auth: AuthMethod::OAuthMicrosoft,
        ..Default::default()
    };
    let id = engine
        .db
        .write(|tx| {
            let id = mail::insert_account(
                tx,
                AccountKind::Microsoft,
                "outlook",
                "megan@contoso.com",
                "Megan Bowen",
                "#0ea5e9",
                &config,
            )?;
            // Keep the calendar scheduler (real Graph, real tokens) away.
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [id])?;
            Ok(id)
        })
        .expect("account");
    let account = engine.account_row(id).expect("account row");
    let client = GraphClient::builder(Arc::new(StaticToken::new("token")))
        .base_url(format!("{}/v1.0", server.uri()))
        .retry_policy(RetryPolicy::none())
        .build()
        .expect("client");
    Fixture {
        server,
        engine,
        events,
        account,
        graph: GraphMail::with_client(client),
        dir,
    }
}

impl Fixture {
    fn base(&self) -> String {
        format!("{}/v1.0", self.server.uri())
    }

    fn session(&self) -> Session {
        Session::new(
            self.engine.clone(),
            self.account.clone(),
            self.graph.clone(),
            CancellationToken::new(),
        )
    }

    fn folder(&self, remote_id: &str) -> FolderRow {
        self.engine
            .db
            .read(|c| mail::folder_by_remote(c, self.account.id, remote_id))
            .expect("query")
            .unwrap_or_else(|| panic!("folder {remote_id}"))
    }

    fn message(&self, remote_id: &str) -> Option<MessageView> {
        let found = self
            .engine
            .db
            .read(|c| mail::message_by_remote(c, self.account.id, remote_id))
            .expect("query");
        found.map(|(id, _, _)| {
            self.engine
                .db
                .read(|c| mail::get_message(c, id))
                .expect("message")
        })
    }

    /// What the sidebar shows of a folder.
    fn folder_view(&self, id: i64) -> FolderView {
        self.engine
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT parent_id, color, hidden, total_count, unread_count FROM folders WHERE id = ?",
                    [id],
                    |r| {
                        Ok(FolderView {
                            parent_id: r.get(0)?,
                            color: r.get(1)?,
                            hidden: r.get::<_, i64>(2)? != 0,
                            total_count: r.get(3)?,
                            unread_count: r.get(4)?,
                        })
                    },
                )?)
            })
            .expect("folder view")
    }

    fn thread(&self, id: i64) -> ThreadSummary {
        self.engine
            .db
            .read(|c| mail::thread_summary(c, id))
            .expect("thread")
    }

    async fn mount(&self, mock: Mock) {
        mock.mount(&self.server).await;
    }

    /// The folder tree, well-known folders, categories and empty deltas
    /// for every folder without a more specific mock.
    async fn mount_mailbox(&self) {
        let folder = |id: &str, name: &str, parent: &str, children: u32| {
            json!({ "id": id, "displayName": name, "parentFolderId": parent, "childFolderCount": children,
                    "unreadItemCount": 0, "totalItemCount": 0, "isHidden": false })
        };
        self.mount(
            Mock::given(method("GET"))
                .and(path("/v1.0/me/mailFolders"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                    folder("f-inbox", "Inbox", "f-root", 0),
                    folder("f-drafts", "Drafts", "f-root", 0),
                    folder("f-sent", "Sent Items", "f-root", 0),
                    folder("f-deleted", "Deleted Items", "f-root", 0),
                    folder("f-junk", "Junk Email", "f-root", 0),
                    folder("f-archive", "Archive", "f-root", 0),
                    folder("f-outbox", "Outbox", "f-root", 0),
                    folder("f-projects", "Projects", "f-root", 1),
                ] }))),
        )
        .await;
        self.mount(
            Mock::given(method("GET"))
                .and(path("/v1.0/me/mailFolders/f-projects/childFolders"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                    folder("f-alpha", "Alpha", "f-projects", 0),
                ] }))),
        )
        .await;
        let found =
            |role: &str, id: &str| json!({ "id": role, "status": 200, "body": { "id": id } });
        self.mount(
            Mock::given(method("POST"))
                .and(path("/v1.0/$batch"))
                .and(body_string_contains("mailFolders/inbox"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({ "responses": [
                    found("inbox", "f-inbox"),
                    found("drafts", "f-drafts"),
                    found("sentitems", "f-sent"),
                    found("deleteditems", "f-deleted"),
                    found("junkemail", "f-junk"),
                    found("archive", "f-archive"),
                    found("outbox", "f-outbox"),
                ] })),
                ),
        )
        .await;
        self.mount(
            Mock::given(method("GET"))
                .and(path("/v1.0/me/outlook/masterCategories"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                    { "id": "c-red", "displayName": "Red category", "color": "preset0" },
                    { "id": "c-blue", "displayName": "Blue category", "color": "preset7" },
                ] }))),
        )
        .await;
        let base = self.base();
        self.mount(
            Mock::given(method("GET"))
                .and(path_regex(r"^/v1\.0/me/mailFolders/[^/]+/messages/delta$"))
                .respond_with(move |request: &Request| {
                    let link = format!(
                        "{base}{}?$deltatoken=empty",
                        &request.url.path()["/v1.0".len()..]
                    );
                    ResponseTemplate::new(200)
                        .set_body_json(json!({ "value": [], "@odata.deltaLink": link }))
                }),
        )
        .await;
    }

    /// A delta page for a folder: `token` selects the request (`None`: the
    /// first request of a full round), `next`/`done` the link returned.
    async fn mount_delta(
        &self,
        folder: &str,
        token: Option<(&str, &str)>,
        page: Value,
        priority: u8,
    ) {
        let mut mock = Mock::given(method("GET")).and(path(format!(
            "/v1.0/me/mailFolders/{folder}/messages/delta"
        )));
        mock = match token {
            Some((name, value)) => mock.and(query_param(name, value)),
            None => mock.and(query_param("$orderby", "receivedDateTime desc")),
        };
        self.mount(
            mock.respond_with(ResponseTemplate::new(200).set_body_json(page))
                .with_priority(priority),
        )
        .await;
    }

    fn link(&self, folder: &str, query: &str) -> String {
        format!(
            "{}/me/mailFolders/{folder}/messages/delta?{query}",
            self.base()
        )
    }

    /// Mounts the first sync of the mailbox and runs it: the inbox lists
    /// m1 and m2 over two pages, sent items m3 (a reply in m1's
    /// conversation).
    async fn first_sync(&self) -> Session {
        self.mount_mailbox().await;
        self.mount_delta(
            "f-inbox",
            None,
            json!({ "value": [m1()], "@odata.nextLink": self.link("f-inbox", "$skiptoken=p2") }),
            2,
        )
        .await;
        self.mount_delta(
            "f-inbox",
            Some(("$skiptoken", "p2")),
            json!({ "value": [m2()], "@odata.deltaLink": self.link("f-inbox", "$deltatoken=d1") }),
            1,
        )
        .await;
        self.mount_delta(
            "f-sent",
            None,
            json!({ "value": [m3()], "@odata.deltaLink": self.link("f-sent", "$deltatoken=s1") }),
            2,
        )
        .await;
        let mut session = self.session();
        session.reload_folders().expect("folders");
        session.sync_folder_list().await.expect("folder list");
        let (more, clean) = session.sync_folders(None).await.expect("first sync");
        assert!(!more && clean);
        session.tidy().await.expect("tidy");
        session
    }
}

fn m1() -> Value {
    json!({
        "id": "m1", "internetMessageId": "<plan@contoso.com>", "conversationId": "c1",
        "subject": "Quarterly plan", "bodyPreview": "Here is the plan.",
        "from": person("Dana Swope", "danas@contoso.com"),
        "toRecipients": [person("Megan Bowen", "megan@contoso.com")],
        "receivedDateTime": "2026-10-01T09:00:00Z", "isRead": false, "isDraft": false,
        "flag": { "flagStatus": "flagged" }, "hasAttachments": true,
        "categories": ["Red category"], "inferenceClassification": "focused",
        "parentFolderId": "f-inbox"
    })
}

fn m2() -> Value {
    json!({
        "id": "m2", "internetMessageId": "<deal@shop.example>", "conversationId": "c2",
        "subject": "Weekend deals", "bodyPreview": "Save 20% this weekend",
        "from": person("Shop", "news@shop.example"),
        "toRecipients": [person("Megan Bowen", "megan@contoso.com")],
        "receivedDateTime": "2026-09-30T12:00:00Z", "isRead": true, "isDraft": false,
        "flag": { "flagStatus": "notFlagged" }, "hasAttachments": false, "categories": [],
        "inferenceClassification": "other", "parentFolderId": "f-inbox"
    })
}

fn m3() -> Value {
    json!({
        "id": "m3", "internetMessageId": "<re-plan@contoso.com>", "conversationId": "c1",
        "subject": "RE: Quarterly plan", "bodyPreview": "Looks good.",
        "from": person("Megan Bowen", "megan@contoso.com"),
        "toRecipients": [person("Dana Swope", "danas@contoso.com")],
        "receivedDateTime": "2026-10-02T10:00:00Z", "isRead": true, "isDraft": false,
        "flag": { "flagStatus": "notFlagged" }, "hasAttachments": false, "categories": [],
        "inferenceClassification": "focused", "parentFolderId": "f-sent"
    })
}

fn m4() -> Value {
    json!({
        "id": "m4", "internetMessageId": "<lunch@contoso.com>", "conversationId": "c4",
        "subject": "Lunch?", "bodyPreview": "Noon at the usual place?",
        "from": person("Alex Wilber", "alexw@contoso.com"),
        "toRecipients": [person("Megan Bowen", "megan@contoso.com")],
        "receivedDateTime": "2026-10-03T11:00:00Z", "isRead": false, "isDraft": false,
        "flag": { "flagStatus": "notFlagged" }, "hasAttachments": false, "categories": [],
        "inferenceClassification": "focused", "parentFolderId": "f-inbox"
    })
}

#[tokio::test]
async fn syncs_folders_and_messages_through_delta_rounds() {
    let f = fixture().await;
    let mut session = f.first_sync().await;

    // Folders: roles, hierarchy, categories as labels; no outbox.
    let inbox = f.folder("f-inbox");
    assert_eq!(inbox.role, Some(FolderRole::Inbox));
    assert_eq!(f.folder("f-sent").role, Some(FolderRole::Sent));
    assert_eq!(f.folder("f-deleted").role, Some(FolderRole::Trash));
    assert_eq!(f.folder("f-junk").role, Some(FolderRole::Junk));
    assert_eq!(f.folder("f-archive").role, Some(FolderRole::Archive));
    assert_eq!(f.folder("f-drafts").role, Some(FolderRole::Drafts));
    let alpha = f.folder("f-alpha");
    assert_eq!(alpha.path, "Projects/Alpha");
    assert_eq!(
        f.folder_view(alpha.id).parent_id,
        Some(f.folder("f-projects").id)
    );
    assert!(
        f.engine
            .db
            .read(|c| mail::folder_by_remote(c, f.account.id, "f-outbox"))
            .expect("query")
            .is_none()
    );
    let red = f.folder("category:Red category");
    let blue = f.folder("category:Blue category");
    assert_eq!(red.kind, "label");
    let red_view = f.folder_view(red.id);
    assert_eq!(red_view.color.as_deref(), Some("#e74856"));
    assert!(!red_view.hidden, "a category in use is shown");
    assert!(
        f.folder_view(blue.id).hidden,
        "unused categories stay out of the way"
    );

    // Messages, flags, threads and counts.
    let m1 = f.message("m1").expect("m1");
    let m2 = f.message("m2").expect("m2");
    let m3 = f.message("m3").expect("m3");
    assert!(!m1.flags.seen && m1.flags.flagged);
    assert!(m2.flags.seen);
    assert_eq!(m1.thread_id, m3.thread_id, "conversation ids group threads");
    assert_eq!(m1.snippet, "Here is the plan.");
    assert_eq!(m1.message_id.as_deref(), Some("plan@contoso.com"));
    assert!(m1.folder_ids.contains(&inbox.id) && m1.folder_ids.contains(&red.id));
    assert_eq!(m3.folder_ids, vec![f.folder("f-sent").id]);
    let plan = f.thread(m1.thread_id);
    assert_eq!(plan.message_count, 2);
    assert_eq!(plan.unread_count, 1);
    assert!(plan.flagged && plan.has_attachments);
    assert_eq!(plan.category.as_deref(), Some("people"));
    assert_eq!(
        f.thread(m2.thread_id).category.as_deref(),
        Some("notifications"),
        "Outlook's Other inbox"
    );
    let inbox_view = f.folder_view(inbox.id);
    assert_eq!((inbox_view.total_count, inbox_view.unread_count), (2, 1));
    assert_eq!(f.folder_view(red.id).total_count, 1);

    // Delta links are kept per folder; the listing produced no notifications.
    let inbox = f.folder("f-inbox");
    assert!(inbox.sync_state.complete);
    let delta = DeltaState::load(&inbox.sync_state);
    assert_eq!(
        delta.delta,
        Some(DeltaCursor::Done(f.link("f-inbox", "$deltatoken=d1")))
    );
    assert!(!delta.full);
    assert!(f.events.new_mail().is_empty());

    // Second round: m1 read and uncategorized, m2 removed, m4 arrives.
    let mut m1_update = super::tests::m1();
    m1_update["isRead"] = json!(true);
    m1_update["categories"] = json!([]);
    f.mount_delta(
        "f-inbox",
        Some(("$deltatoken", "d1")),
        json!({ "value": [
            m1_update,
            { "@odata.type": "#microsoft.graph.message", "id": "m2", "@removed": { "reason": "deleted" } },
            m4(),
        ], "@odata.deltaLink": f.link("f-inbox", "$deltatoken=d2") }),
        1,
    )
    .await;
    let (more, clean) = session.sync_folders(None).await.expect("second sync");
    assert!(!more && clean);
    let m1 = f.message("m1").expect("m1");
    assert!(m1.flags.seen && m1.flags.flagged);
    assert!(!m1.folder_ids.contains(&red.id));
    let detached = f.message("m2").expect("kept until every folder was read");
    assert!(detached.folder_ids.is_empty());
    session.tidy().await.expect("tidy");
    assert!(f.message("m2").is_none(), "removed messages are deleted");
    assert!(f.folder_view(red.id).hidden);
    let m4 = f.message("m4").expect("m4");
    let notified = f.events.new_mail();
    assert_eq!(notified.len(), 1);
    assert_eq!(notified[0][0].id, m4.thread_id);
    let inbox_view = f.folder_view(f.folder("f-inbox").id);
    assert_eq!((inbox_view.total_count, inbox_view.unread_count), (2, 1));
    assert_eq!(
        DeltaState::load(&f.folder("f-inbox").sync_state).delta,
        Some(DeltaCursor::Done(f.link("f-inbox", "$deltatoken=d2")))
    );
}

#[tokio::test]
async fn moves_between_folders_keep_the_message() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    // Another client archived m1: it leaves the inbox and shows up in the
    // archive with the same immutable id.
    f.mount_delta(
        "f-inbox",
        Some(("$deltatoken", "d1")),
        json!({ "value": [{ "id": "m1", "@removed": { "reason": "deleted" } }],
                "@odata.deltaLink": f.link("f-inbox", "$deltatoken=d2") }),
        1,
    )
    .await;
    let mut archived = super::tests::m1();
    archived["parentFolderId"] = json!("f-archive");
    f.mount_delta(
        "f-archive",
        None,
        json!({ "value": [archived], "@odata.deltaLink": f.link("f-archive", "$deltatoken=a2") }),
        1,
    )
    .await;
    // The archive's first round already ran (empty) in the first sync; reset
    // it so this round lists the moved message.
    let archive = f.folder("f-archive");
    f.engine
        .db
        .write(|tx| mail::set_folder_sync_state(tx, archive.id, &FolderSyncState::default()))
        .expect("reset");
    session.sync_folders(None).await.expect("sync");
    session.tidy().await.expect("tidy");
    let moved = f.message("m1").expect("still there");
    assert_eq!(moved.id, m1.id, "same local message");
    assert!(moved.folder_ids.contains(&archive.id));
    assert!(!moved.folder_ids.contains(&f.folder("f-inbox").id));
    assert!(
        moved
            .folder_ids
            .contains(&f.folder("category:Red category").id),
        "categories move along"
    );
}

#[tokio::test]
async fn expired_delta_links_relist_and_prune() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/mailFolders/f-inbox/messages/delta"))
            .and(query_param("$deltatoken", "d1"))
            .respond_with(ResponseTemplate::new(410).set_body_json(json!({
                "error": { "code": "SyncStateNotFound", "message": "The sync state is not found." }
            })))
            .with_priority(1),
    )
    .await;
    // The fresh listing no longer has m2.
    f.mount_delta(
        "f-inbox",
        None,
        json!({ "value": [m1()], "@odata.deltaLink": f.link("f-inbox", "$deltatoken=fresh") }),
        1,
    )
    .await;
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/mailFolders/f-inbox/messages"))
            .and(query_param("$select", "id"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "value": [{ "id": "m1" }] })),
            )
            .expect(1),
    )
    .await;
    session.sync_folders(None).await.expect("sync");
    session.tidy().await.expect("tidy");
    assert!(f.message("m1").is_some());
    assert!(f.message("m2").is_none(), "pruned after the full listing");
    let state = DeltaState::load(&f.folder("f-inbox").sync_state);
    assert_eq!(
        state.delta,
        Some(DeltaCursor::Done(f.link("f-inbox", "$deltatoken=fresh")))
    );
    assert!(!state.prune && !state.full);
    assert!(
        f.events.new_mail().is_empty(),
        "a relisting does not notify"
    );
}

fn batch_ok(ids: &[(&str, Value)]) -> ResponseTemplate {
    let responses: Vec<Value> = ids
        .iter()
        .map(|(id, body)| json!({ "id": id, "status": 200, "body": body }))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({ "responses": responses }))
}

#[tokio::test]
async fn replays_local_changes_in_batches() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    let m2 = f.message("m2").expect("m2");
    let blue = f.folder("category:Blue category");

    f.engine
        .set_flags(
            &[],
            &[m1.id],
            &FlagChange {
                seen: Some(true),
                flagged: Some(false),
            },
        )
        .expect("flags");
    f.engine
        .move_threads(&[m2.thread_id], None, &MoveTarget::Archive)
        .expect("archive");
    f.engine
        .set_label(&[m1.thread_id], blue.id, true)
        .expect("label");
    // Skip the undo window.
    f.engine
        .db
        .write(|tx| Ok(tx.execute("UPDATE pending_ops SET not_before = 0", [])?))
        .expect("due");
    // Optimistic local state.
    let archive = f.folder("f-archive");
    assert_eq!(f.message("m2").expect("m2").folder_ids, vec![archive.id]);
    assert!(f.message("m3").expect("m3").folder_ids.contains(&blue.id));

    // Read and unflagged are queued as two operations.
    for change in ["\"isRead\":true", "\"flagStatus\":\"notFlagged\""] {
        f.mount(
            Mock::given(method("POST"))
                .and(path("/v1.0/$batch"))
                .and(body_string_contains(change))
                .and(body_string_contains("/me/messages/m1"))
                .respond_with(batch_ok(&[("0", json!({}))]))
                .expect(1),
        )
        .await;
    }
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains("/me/messages/m2/move"))
            .and(body_string_contains("f-archive"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "responses": [
                { "id": "0", "status": 201, "body": { "id": "m2" } }
            ] })),
            )
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains("$select=categories"))
            .respond_with(move |request: &Request| {
                // Answer for whichever messages were asked, in order.
                let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
                let responses: Vec<Value> = body["requests"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|r| {
                        let url = r["url"].as_str().unwrap_or_default();
                        let categories = if url.contains("/m1?") { json!(["Red category"]) } else { json!([]) };
                        json!({ "id": r["id"], "status": 200, "body": { "id": "x", "categories": categories } })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({ "responses": responses }))
            })
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains(
                "\"categories\":[\"Red category\",\"Blue category\"]",
            ))
            .and(body_string_contains("\"categories\":[\"Blue category\"]"))
            .respond_with(batch_ok(&[("0", json!({})), ("1", json!({}))]))
            .expect(1),
    )
    .await;

    session.process_ops().await.expect("ops");
    let left = f
        .engine
        .db
        .read(|c| misc::queued_ops(c, f.account.id))
        .expect("ops");
    assert!(left.is_empty(), "{left:?}");
    let m1 = f.message("m1").expect("m1");
    assert!(m1.flags.seen && !m1.flags.flagged);
    f.server.verify().await;
}

#[tokio::test]
async fn deletes_forever_from_the_trash() {
    let f = fixture().await;
    let session = f.first_sync().await;
    let m2 = f.message("m2").expect("m2");
    let trash = f.folder("f-deleted");
    // Pretend m2 sits in Deleted Items.
    f.engine
        .db
        .write(|tx| mail::set_only_folder(tx, m2.id, trash.id).map(|_| ()))
        .expect("move");
    f.engine.delete_forever(&[m2.thread_id]).expect("delete");
    assert!(f.message("m2").is_none());
    let mut session = session;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains("\"method\":\"DELETE\""))
            .and(body_string_contains("/me/messages/m2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "responses": [
                { "id": "0", "status": 204 }
            ] })),
            )
            .expect(1),
    )
    .await;
    session.process_ops().await.expect("ops");
    f.server.verify().await;
}

const SOURCE: &str = "From: Dana Swope <danas@contoso.com>\r\n\
To: Megan Bowen <megan@contoso.com>\r\n\
Subject: Quarterly plan\r\n\
Message-ID: <plan@contoso.com>\r\n\
In-Reply-To: <kickoff@contoso.com>\r\n\
References: <kickoff@contoso.com>\r\n\
List-Unsubscribe: <https://contoso.example/u>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=outer\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/related; boundary=inner\r\n\
\r\n\
--inner\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Here is the plan.</p><img src=\"cid:chart@contoso\">\r\n\
--inner\r\n\
Content-Type: image/png\r\n\
Content-ID: <chart@contoso>\r\n\
Content-Disposition: inline\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=plan.pdf\r\n\
Content-Disposition: attachment; filename=plan.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--outer--\r\n";

#[tokio::test]
async fn downloads_bodies_and_attachments_from_mime() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages/m1/$value"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(SOURCE.as_bytes(), "message/rfc822"),
            )
            .expect(1),
    )
    .await;
    let threads = download_bodies(&f.engine, &f.graph, f.account.id, &[m1.id], true)
        .await
        .expect("bodies");
    assert!(threads.contains(&m1.thread_id));
    let m1 = f.message("m1").expect("m1");
    let body = m1.body.expect("body");
    assert!(body.html.unwrap_or_default().contains("cid:chart@contoso"));
    assert_eq!(m1.in_reply_to.as_deref(), Some("kickoff@contoso.com"));
    assert!(m1.unsubscribe.is_some());
    assert!(m1.size > 0);
    let pdf = m1
        .attachments
        .iter()
        .find(|a| a.filename == "plan.pdf")
        .expect("pdf");
    assert!(!pdf.inline);
    let image = f
        .engine
        .db
        .read(|c| mail::attachment_by_cid(c, m1.id, "chart@contoso"))
        .expect("query")
        .expect("inline image");
    // Both come from the cached source: no second download.
    let pdf_row = f
        .engine
        .db
        .read(|c| mail::get_attachment(c, pdf.id))
        .expect("row");
    let data = attachment_with(&f.engine, &f.graph, f.account.id, m1.id, &pdf_row.part_id)
        .await
        .expect("pdf data");
    assert_eq!(data, b"%PDF-");
    let png = attachment_with(&f.engine, &f.graph, f.account.id, m1.id, &image.part_id)
        .await
        .expect("image data");
    assert!(png.starts_with(b"\x89PNG"));
    f.server.verify().await;
}

#[tokio::test]
async fn missing_messages_do_not_block_body_downloads() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    let m2 = f.message("m2").expect("m2");
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages/m2/$value"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": { "code": "ErrorItemNotFound", "message": "Not found." }
            }))),
    )
    .await;
    download_bodies(&f.engine, &f.graph, f.account.id, &[m2.id], false)
        .await
        .expect("a missing message is not an error");
    let state: i64 = f
        .engine
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT body_state FROM messages WHERE id = ?",
                [m2.id],
                |r| r.get(0),
            )?)
        })
        .expect("state");
    assert_eq!(state, 2, "marked as failed so prefetch moves on");

    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages/m3/$value"))
            .respond_with(ResponseTemplate::new(503)),
    )
    .await;
    let m3 = f.message("m3").expect("m3");
    let err = download_bodies(&f.engine, &f.graph, f.account.id, &[m3.id], false)
        .await
        .expect_err("server trouble is reported");
    assert!(err.is_transient());
}

#[tokio::test]
async fn sends_mime_with_bcc_recipients() {
    let f = fixture().await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/sendMail"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1),
    )
    .await;
    let mime = b"From: megan@contoso.com\r\nTo: danas@contoso.com\r\nSubject: hi\r\n\r\nhello\r\n";
    send_with(
        &f.graph,
        mime,
        &["danas@contoso.com".into(), "secret@example.com".into()],
    )
    .await
    .expect("sent");
    let requests = f.server.received_requests().await.unwrap_or_default();
    let sent = requests
        .iter()
        .find(|r| r.url.path().ends_with("/sendMail"))
        .expect("sendMail");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&sent.body)
        .expect("base64 body");
    let text = String::from_utf8_lossy(&decoded);
    assert!(
        text.starts_with("Bcc: secret@example.com\r\nFrom: megan@contoso.com"),
        "{text}"
    );

    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/sendMail"))
            .respond_with(ResponseTemplate::new(413))
            .with_priority(1),
    )
    .await;
    let err = send_with(&f.graph, mime, &[]).await.expect_err("too large");
    assert!(matches!(err, Error::Invalid(_)), "{err}");
}

#[tokio::test]
async fn uploads_drafts_and_replaces_the_old_copy() {
    let f = fixture().await;
    let session = f.first_sync().await;
    let draft = DraftInput {
        account_id: f.account.id,
        to: vec![Address::new(None, "danas@contoso.com")],
        bcc: vec![Address::new(None, "hidden@example.com")],
        subject: "Draft".into(),
        html: "<p>Work in progress</p>".into(),
        ..Default::default()
    };
    let draft_id = f
        .engine
        .db
        .write(|tx| {
            let id = misc::draft_save(tx, None, &draft)?;
            misc::draft_set_remote(tx, id, None, Some("old-draft"))?;
            Ok(id)
        })
        .expect("draft");
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/messages"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "id": "new-draft", "isDraft": true })),
            )
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("DELETE"))
            .and(path("/v1.0/me/messages/old-draft"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1),
    )
    .await;
    session.save_draft(draft_id).await.expect("saved");
    let row = f
        .engine
        .db
        .read(|c| misc::draft_get(c, draft_id))
        .expect("draft row");
    assert_eq!(row.remote_id.as_deref(), Some("new-draft"));
    let requests = f.server.received_requests().await.unwrap_or_default();
    let upload = requests
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v1.0/me/messages")
        .expect("upload");
    let mime = base64::engine::general_purpose::STANDARD
        .decode(&upload.body)
        .expect("base64 MIME");
    let mime = String::from_utf8_lossy(&mime);
    assert!(mime.starts_with("Bcc: hidden@example.com\r\n"), "{mime}");
    assert!(mime.contains("Subject: Draft"));
    f.server.verify().await;
}

#[tokio::test]
async fn gives_up_on_rejected_changes_and_restores_the_server_state() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let m2 = f.message("m2").expect("m2");
    let inbox = f.folder("f-inbox");
    f.engine
        .move_threads(&[m2.thread_id], Some(inbox.id), &MoveTarget::Archive)
        .expect("archive");
    // The server keeps refusing: this is the last attempt.
    f.engine
        .db
        .write(|tx| Ok(tx.execute("UPDATE pending_ops SET not_before = 0, attempts = 4", [])?))
        .expect("due");
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains("/me/messages/m2/move"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "responses": [
                { "id": "0", "status": 400, "body": { "error": {
                    "code": "ErrorMoveCopyFailed", "message": "The item cannot be moved." } } }
            ] })),
            ),
    )
    .await;
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/messages/m2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(super::tests::m2()))
            .expect(1),
    )
    .await;
    assert_eq!(
        f.message("m2").expect("m2").folder_ids,
        vec![f.folder("f-archive").id],
        "optimistic move"
    );
    session.process_ops().await.expect("ops");
    assert!(
        f.engine
            .db
            .read(|c| misc::queued_ops(c, f.account.id))
            .expect("ops")
            .is_empty()
    );
    assert_eq!(
        f.message("m2").expect("m2").folder_ids,
        vec![inbox.id],
        "back where the server has it"
    );
    f.server.verify().await;
}

#[tokio::test]
async fn the_worker_syncs_replays_changes_and_stops() {
    let f = fixture().await;
    f.mount_mailbox().await;
    f.mount_delta(
        "f-inbox",
        None,
        json!({ "value": [m1(), m2()], "@odata.deltaLink": f.link("f-inbox", "$deltatoken=d1") }),
        1,
    )
    .await;
    let wake = Arc::new(Notify::new());
    let stop = CancellationToken::new();
    let worker = tokio::spawn(run_with(
        f.engine.clone(),
        f.account.id,
        f.graph.clone(),
        wake.clone(),
        stop.clone(),
    ));
    let synced = async {
        while f.message("m2").is_none() || f.engine.sync_state(f.account.id) != SyncState::Idle {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), synced)
        .await
        .expect("first sync");
    // Bodies of messages without attachments are prefetched (m2 is missing
    // on the server here: it is marked and skipped, not retried forever).

    // A local change wakes the worker, which replays it right away.
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains("\"isRead\":true"))
            .respond_with(batch_ok(&[("0", json!({}))]))
            .expect(1),
    )
    .await;
    let m1 = f.message("m1").expect("m1");
    f.engine
        .set_flags(
            &[],
            &[m1.id],
            &FlagChange {
                seen: Some(true),
                flagged: None,
            },
        )
        .expect("flags");
    wake.notify_one();
    let replayed = async {
        while !f
            .engine
            .db
            .read(|c| misc::queued_ops(c, f.account.id))
            .expect("ops")
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), replayed)
        .await
        .expect("replayed");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("stops")
        .expect("no panic");
    f.server.verify().await;
}

#[tokio::test]
async fn sends_large_messages_as_a_draft_with_uploaded_files() {
    let f = fixture().await;
    let upload = format!("{}/upload/session-1", f.server.uri());
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/messages"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "draft-1" })))
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("POST"))
            .and(path(
                "/v1.0/me/messages/draft-1/attachments/createUploadSession",
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                json!({ "uploadUrl": upload, "expirationDateTime": "2030-01-01T00:00:00Z" }),
            ))
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("PUT"))
            .and(path("/upload/session-1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2),
    )
    .await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/messages/draft-1/attachments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "att-small" })))
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/messages/draft-1/send"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1),
    )
    .await;

    let big: Vec<u8> = (0..4_000_000u32).map(|i| (i % 251) as u8).collect();
    let out = crate::compose::Outgoing {
        from: Address::new(Some("Megan".into()), "megan@contoso.com"),
        reply_to: None,
        to: vec![Address::new(None, "danas@contoso.com")],
        cc: vec![],
        bcc: vec![Address::new(None, "secret@example.com")],
        subject: "Big files".into(),
        html: "<p>Two files attached</p>".into(),
        text: None,
        in_reply_to: Some("original@contoso.com".into()),
        references: vec!["original@contoso.com".into()],
        attachments: vec![
            crate::compose::FilePart {
                filename: "video.bin".into(),
                mime_type: "application/octet-stream".into(),
                data: big,
                content_id: None,
            },
            crate::compose::FilePart {
                filename: "notes.txt".into(),
                mime_type: "text/plain".into(),
                data: b"small".to_vec(),
                content_id: None,
            },
        ],
        read_receipt: false,
        message_id: None,
        extra_headers: vec![],
        calendar: None,
        allow_no_recipients: false,
    };
    let rendered = crate::compose::render(&out).expect("rendered");
    assert!(rendered.mime.len() > MIME_LIMIT);
    send_with(&f.graph, &rendered.mime, &rendered.recipients)
        .await
        .expect("sent");

    let requests = f.server.received_requests().await.unwrap_or_default();
    let draft = requests
        .iter()
        .find(|r| r.url.path() == "/v1.0/me/messages")
        .expect("draft upload");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&draft.body)
        .expect("base64 draft");
    let text = String::from_utf8_lossy(&decoded);
    assert!(
        decoded.len() < 100_000,
        "the draft carries no files ({} bytes)",
        decoded.len()
    );
    assert!(text.contains("Bcc: secret@example.com"), "{text}");
    assert!(
        text.contains("In-Reply-To: <original@contoso.com>"),
        "{text}"
    );
    let puts: Vec<_> = requests
        .iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect();
    assert!(
        puts.iter()
            .all(|r| !r.headers.contains_key("authorization"))
    );
    let ranges: Vec<String> = puts
        .iter()
        .map(|r| {
            r.headers
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        ranges,
        vec![
            "bytes 0-3276799/4000000".to_string(),
            "bytes 3276800-3999999/4000000".to_string()
        ]
    );
}

// ---------------------------------------------------------------------------
// Folder and category management

impl Fixture {
    fn folder_rows(&self) -> Vec<FolderRow> {
        self.engine
            .db
            .read(|c| mail::list_folder_rows(c, self.account.id))
            .expect("folders")
    }
}

#[tokio::test]
async fn folders_are_created_renamed_nested_and_deleted_on_the_server() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    let projects = f.folder("f-projects");
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/mailFolders/f-projects/childFolders"))
            .and(body_string_contains(r#""displayName":"Clients""#))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "f-clients", "displayName": "Clients", "parentFolderId": "f-projects"
            })))
            .expect(1),
    )
    .await;
    let id = f
        .engine
        .graph_create_folder(
            &f.graph,
            f.account.id,
            &f.folder_rows(),
            Some(&projects),
            "Clients",
        )
        .await
        .expect("create");
    let clients = f.folder("f-clients");
    assert_eq!(clients.id, id);
    assert_eq!(clients.path, "Projects/Clients");
    assert_eq!(clients.parent_id, Some(projects.id));
    let again = f
        .engine
        .graph_create_folder(
            &f.graph,
            f.account.id,
            &f.folder_rows(),
            Some(&projects),
            " clients ",
        )
        .await;
    assert!(
        again.is_err(),
        "the name is taken there; the server is not asked"
    );

    f.mount(
        Mock::given(method("PATCH"))
            .and(path("/v1.0/me/mailFolders/f-clients"))
            .and(body_string_contains(r#""displayName":"Customers""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "f-clients", "displayName": "Customers", "parentFolderId": "f-projects"
            })))
            .expect(1),
    )
    .await;
    f.engine
        .graph_rename_folder(&f.graph, &f.folder_rows(), &clients, "Customers")
        .await
        .expect("rename");
    let renamed = f.folder("f-clients");
    assert_eq!(
        (renamed.name.as_str(), renamed.path.as_str()),
        ("Customers", "Projects/Customers")
    );

    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/mailFolders/f-clients/move"))
            .and(body_string_contains("msgfolderroot"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "f-clients", "displayName": "Customers", "parentFolderId": "f-root"
            })))
            .expect(1),
    )
    .await;
    f.engine
        .graph_move_folder(&f.graph, &f.folder_rows(), &renamed, None)
        .await
        .expect("to the top");
    let top = f.folder("f-clients");
    assert_eq!((top.parent_id, top.path.as_str()), (None, "Customers"));

    // Deleting moves the folder into Deleted Items, as Outlook does.
    let trash = f.folder("f-deleted");
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/mailFolders/f-projects/move"))
            .and(body_string_contains("f-deleted"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "f-projects", "displayName": "Projects", "parentFolderId": "f-deleted"
            })))
            .expect(1),
    )
    .await;
    f.engine
        .graph_delete_folder(&f.graph, &f.folder("f-projects"), Some(&trash))
        .await
        .expect("delete");
    let deleted = f.folder("f-projects");
    assert_eq!(deleted.parent_id, Some(trash.id));
    assert_eq!(deleted.path, "Deleted Items/Projects");
    assert_eq!(
        f.folder("f-alpha").path,
        "Deleted Items/Projects/Alpha",
        "folders inside go along"
    );
}

#[tokio::test]
async fn categories_are_created_recolored_and_deleted() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/me/outlook/masterCategories"))
            .and(body_string_contains(r#""displayName":"Receipts""#))
            .and(body_string_contains("preset4"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "c-new", "displayName": "Receipts", "color": "preset4"
            })))
            .expect(1),
    )
    .await;
    let id = f
        .engine
        .graph_create_category(
            &f.graph,
            f.account.id,
            &f.folder_rows(),
            "Receipts",
            Some("#47b347".into()),
        )
        .await
        .expect("create");
    let label = f.folder("category:Receipts");
    assert_eq!(label.id, id);
    assert_eq!(label.kind, "label");
    assert_eq!(label.color.as_deref(), Some("#47b347"));

    f.mount(
        Mock::given(method("PATCH"))
            .and(path("/v1.0/me/outlook/masterCategories/c-red"))
            .and(body_string_contains("preset7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1),
    )
    .await;
    crate::folders::graph_recolor_category(
        &f.graph,
        "red category",
        crate::folders::nearest_preset(Some("#3a96dd")),
    )
    .await
    .expect("recolor");

    f.mount(
        Mock::given(method("DELETE"))
            .and(path("/v1.0/me/outlook/masterCategories/c-red"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1),
    )
    .await;
    crate::folders::graph_delete_category(&f.graph, "Red category")
        .await
        .expect("delete");
    // The conversations that had it lose it here and on the server.
    let red = f.folder("category:Red category");
    let m1 = f.message("m1").expect("m1");
    assert!(m1.folder_ids.contains(&red.id));
    f.engine
        .drop_category(f.account.id, &red, "Red category")
        .expect("drop");
    assert!(!f.message("m1").expect("m1").folder_ids.contains(&red.id));
    let ops = f
        .engine
        .db
        .read(|c| misc::queued_ops(c, f.account.id))
        .expect("ops");
    assert!(
        ops.iter().any(
            |op| matches!(op, PendingOp::Labels { message_ids, remove, .. }
            if message_ids == &vec![m1.id] && remove == &vec!["Red category".to_string()])
        ),
        "{ops:?}"
    );
}

#[tokio::test]
async fn emptying_a_folder_deletes_every_message_then_the_folders_inside() {
    let f = fixture().await;
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/mailFolders/f-deleted/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [
                { "id": "x1" }, { "id": "x2" }
            ] })))
            .up_to_n_times(1)
            .with_priority(1),
    )
    .await;
    f.mount(
        Mock::given(method("GET"))
            .and(path("/v1.0/me/mailFolders/f-deleted/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": [] })))
            .with_priority(2),
    )
    .await;
    f.mount(
        Mock::given(method("POST"))
            .and(path("/v1.0/$batch"))
            .and(body_string_contains("/me/messages/x1"))
            .and(body_string_contains("/me/messages/x2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "responses": [
                { "id": "0", "status": 204 },
                { "id": "1", "status": 404, "body": { "error": { "code": "ErrorItemNotFound", "message": "gone" } } }
            ] })))
            .expect(1),
    )
    .await;
    f.mount(
        Mock::given(method("DELETE"))
            .and(path("/v1.0/me/mailFolders/f-old"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1),
    )
    .await;
    empty_folder(&f.graph, "f-deleted", &["f-old".to_string()])
        .await
        .expect("empty");
}
