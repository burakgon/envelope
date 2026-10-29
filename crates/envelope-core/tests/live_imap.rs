//! End-to-end test against the development server (scripts/devserver/run.sh).
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_imap -- --nocapture

use std::sync::Arc;
use std::time::{Duration, Instant};

use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};
use parking_lot::Mutex;

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);
impl EventSink for Events {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().push(e);
    }
}

async fn wait_for<F: FnMut() -> bool>(mut f: F, secs: u64) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_read_archive_send() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        eprintln!("skipping: ENVELOPE_TEST_IMAP not set");
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let reset =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/devserver/reset.sh");
    assert!(
        std::process::Command::new(reset)
            .status()
            .unwrap()
            .success(),
        "reset devserver"
    );
    let dir = std::env::temp_dir().join(format!("envelope-test-{}", uuid::Uuid::new_v4()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .unwrap();

    let t0 = Instant::now();
    let account = engine
        .add_password_account(NewAccount {
            email: "alex@envelope.test".into(),
            name: "Alex Morgan".into(),
            provider: "generic".into(),
            password: "password".into(),
            config: AccountConfig {
                imap: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1993,
                    security: Security::Tls,
                    username: String::new(),
                }),
                smtp: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1587,
                    security: Security::None,
                    username: String::new(),
                }),
                accept_invalid_certs: true,
                ..Default::default()
            },
        })
        .await
        .expect("add account");
    println!("account added in {:?}", t0.elapsed());

    // Wait for the inbox to fill.
    let inbox_query = ThreadQuery {
        mailbox: Mailbox::Unified {
            role: FolderRole::Inbox,
        },
        search: None,
        filter: ListFilter::default(),
        sort: SortOrder::Newest,
        messages: false,
        offset: 0,
        limit: 50,
    };
    assert!(
        wait_for(
            || engine
                .threads(&inbox_query)
                .map(|p| p.total > 40)
                .unwrap_or(false),
            30
        )
        .await,
        "inbox synced"
    );
    println!("inbox ready after {:?}", t0.elapsed());
    // Let body prefetch finish.
    assert!(
        wait_for(
            || matches!(engine.sync_state(account.id), SyncState::Idle)
                && engine
                    .threads(&inbox_query)
                    .unwrap()
                    .items
                    .iter()
                    .all(|t| !t.snippet.is_empty()),
            60
        )
        .await,
        "idle with snippets"
    );
    println!("idle after {:?}", t0.elapsed());

    let folders = engine.folders().unwrap();
    for f in &folders {
        println!(
            "folder {:<10} {:?} total={} unread={}",
            f.name, f.role, f.total_count, f.unread_count
        );
    }
    let page = engine.threads(&inbox_query).unwrap();
    for t in page.items.iter().take(8) {
        println!(
            "{:>3} msgs {} unread {} | {} | {}",
            t.message_count,
            t.message_count,
            t.unread_count,
            t.subject,
            t.snippet.chars().take(60).collect::<String>()
        );
    }
    // Threading: the roadmap thread has 4 messages (inbox + sent).
    let roadmap = page
        .items
        .iter()
        .find(|t| t.subject.contains("Q4 roadmap"))
        .expect("roadmap thread");
    assert_eq!(
        roadmap.message_count, 4,
        "thread merges inbox and sent messages"
    );
    let detail = engine.thread(roadmap.id).unwrap();
    assert!(detail.messages.iter().all(|m| m.body.is_some()));
    assert!(
        detail.messages[0]
            .attachments
            .iter()
            .any(|a| a.filename == "Q4-Roadmap-Draft.pdf")
    );

    // Search.
    let search = |q: &str| {
        engine
            .threads(&ThreadQuery {
                mailbox: Mailbox::Everything,
                search: Some(q.into()),
                filter: ListFilter::default(),
                sort: SortOrder::Newest,
                messages: false,
                offset: 0,
                limit: 20,
            })
            .unwrap()
    };
    let t = Instant::now();
    let r = search("roadmap");
    println!(
        "search 'roadmap' → {} results in {:?}",
        r.total,
        t.elapsed()
    );
    assert!(r.total >= 1);
    assert!(search("kahveciye").total >= 1, "turkish body search");
    assert!(search("has:attachment from:lukas").total >= 1);
    assert!(search("東京").total >= 1 || search("tanaka").total >= 1);

    // Attachment download.
    let att = &detail.messages[0].attachments[0];
    let path = engine
        .attachment_file(att.id)
        .await
        .expect("download attachment");
    let data = std::fs::read(&path).unwrap();
    assert!(data.starts_with(b"%PDF"), "pdf decoded");

    // Inline image (cid) in Maya's message.
    let maya = page
        .items
        .iter()
        .find(|t| t.subject.contains("onboarding"))
        .expect("maya thread");
    let md = engine.thread(maya.id).unwrap();
    let inline = md.messages[0]
        .attachments
        .iter()
        .find(|a| a.inline)
        .expect("inline image");
    let (mime, p) = engine
        .inline_image(md.messages[0].id, inline.content_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(mime, "image/png");
    assert!(std::fs::read(p).unwrap().starts_with(b"\x89PNG"));

    // Mark read, then archive with undo window → lands in Archive on the server.
    engine
        .set_flags(
            &[maya.id],
            &[],
            &FlagChange {
                seen: Some(true),
                flagged: None,
            },
        )
        .unwrap();
    let inbox_id = folders
        .iter()
        .find(|f| f.role == Some(FolderRole::Inbox))
        .unwrap()
        .id;
    let archive_id = folders
        .iter()
        .find(|f| f.role == Some(FolderRole::Archive))
        .unwrap()
        .id;
    let before = engine.threads(&inbox_query).unwrap().total;
    let token = engine
        .move_threads(&[maya.id], Some(inbox_id), &MoveTarget::Archive)
        .unwrap();
    assert_eq!(
        engine.threads(&inbox_query).unwrap().total,
        before - 1,
        "instantly gone from inbox"
    );
    // Undo right away, then archive for real.
    engine.undo(&token).unwrap();
    assert_eq!(
        engine.threads(&inbox_query).unwrap().total,
        before,
        "undo restores"
    );
    engine
        .move_threads(&[maya.id], Some(inbox_id), &MoveTarget::Archive)
        .unwrap();
    let archive_q = ThreadQuery {
        mailbox: Mailbox::Folder { id: archive_id },
        search: None,
        filter: ListFilter::default(),
        sort: SortOrder::Newest,
        messages: false,
        offset: 0,
        limit: 100,
    };
    assert!(
        engine
            .threads(&archive_q)
            .unwrap()
            .items
            .iter()
            .any(|t| t.id == maya.id)
    );
    // After the undo window the server move happens and the next sync must not duplicate.
    tokio::time::sleep(Duration::from_secs(8)).await;
    engine.sync_now(Some(account.id));
    tokio::time::sleep(Duration::from_secs(3)).await;
    let archived: Vec<_> = engine
        .threads(&archive_q)
        .unwrap()
        .items
        .into_iter()
        .filter(|t| t.subject.contains("onboarding"))
        .collect();
    assert_eq!(archived.len(), 1, "no duplicate after server move");
    assert_eq!(archived[0].message_count, 1);

    // Send a reply to ourselves through SMTP; it must arrive via IDLE.
    let mut draft = engine
        .compose_reply(md.messages[0].id, "reply", &QuoteText::default())
        .unwrap();
    draft.to = vec![Address::new(Some("Alex".into()), "alex@envelope.test")];
    let marker = uuid::Uuid::new_v4().to_string();
    draft.html = format!("<p>Looks great! {marker}</p>{}", draft.html);
    let outbox = engine.send(None, &draft, 0, None).await.unwrap();
    println!("queued outbox {outbox}");
    let arrived = wait_for(|| search(&format!("\"{marker}\"")).total >= 1, 30).await;
    assert!(arrived, "sent message arrived in inbox");
    // The reply joins the onboarding thread.
    let r = search(&format!("\"{marker}\""));
    assert_eq!(r.items[0].id, maya.id, "reply threads with original");
    println!("events: {}", events.0.lock().len());
    engine.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
