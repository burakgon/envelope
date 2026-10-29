//! Importing an mbox file into a folder of the development server
//! (scripts/devserver/run.sh), and importing it again. Uses sam@envelope.test
//! and a folder with a unique name, removed at the end.
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_import -- --nocapture

use std::time::{Duration, Instant};

use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};
use envelope_imap::{Auth, Client, Config};

struct NoEvents;
impl EventSink for NoEvents {
    fn emit(&self, _: EngineEvent) {}
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
async fn imports_an_mbox_file_once() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        eprintln!("skipping: ENVELOPE_TEST_IMAP not set");
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let dir = std::env::temp_dir().join(format!("envelope-test-{}", uuid::Uuid::new_v4()));
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        NoEvents,
    )
    .expect("engine");
    let account = engine
        .add_password_account(NewAccount {
            email: "sam@envelope.test".into(),
            name: "Sam Rivera".into(),
            provider: "generic".into(),
            password: "password".into(),
            config: AccountConfig {
                imap: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1993,
                    security: Security::Tls,
                    username: String::new(),
                }),
                accept_invalid_certs: true,
                body_sync_days: Some(1),
                ..Default::default()
            },
        })
        .await
        .expect("add account");
    let id = account.id;
    let listed = |engine: &Engine| {
        engine
            .folders()
            .expect("folders")
            .into_iter()
            .any(|f| f.account_id == id && f.path == "INBOX")
    };
    assert!(wait_for(|| listed(&engine), 60).await, "folders listed");
    let name = format!(
        "EnvImport{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let folder = engine
        .create_folder(id, None, &name, false, None)
        .await
        .expect("create");

    // Three messages, the second with a body line that started with "From ".
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let mbox = format!(
        "From - Tue Oct 29 10:00:00 2026\n\
         Message-ID: <a-{tag}@envelope.test>\nFrom: Ada <ada@example.com>\nSubject: One\nDate: Tue, 29 Oct 2026 10:00:00 +0000\n\nFirst.\n\n\
         From - Tue Oct 29 11:00:00 2026\n\
         Message-ID: <b-{tag}@envelope.test>\nFrom: Ada <ada@example.com>\nSubject: Two\nDate: Tue, 29 Oct 2026 11:00:00 +0000\n\n>From the archive.\n\n\
         From - Tue Oct 29 12:00:00 2026\n\
         Message-ID: <c-{tag}@envelope.test>\nFrom: Ada <ada@example.com>\nSubject: Three\nDate: Tue, 29 Oct 2026 12:00:00 +0000\n\nThird.\n"
    );
    let path = dir.join("archive.mbox");
    std::fs::write(&path, mbox).expect("mbox");
    let done = engine
        .import_messages(folder.id, std::slice::from_ref(&path))
        .await
        .expect("import");
    assert_eq!((done.added, done.skipped, done.refused), (3, 0, 0));

    // On the server, read, with the escaped line as written.
    let mut config = Config::new("127.0.0.1", 1993, envelope_imap::Security::Tls);
    config.accept_invalid_certs = true;
    let mut sam = Client::connect(&config).await.expect("connect");
    sam.authenticate(&Auth::Password {
        user: "sam@envelope.test".into(),
        password: "password".into(),
    })
    .await
    .expect("login");
    sam.select(&name, None).await.expect("select");
    assert_eq!(sam.uid_search("ALL").await.expect("search").len(), 3);
    assert_eq!(sam.uid_search("UNSEEN").await.expect("search").len(), 0);
    assert_eq!(
        sam.uid_search("BODY \"From the archive\"")
            .await
            .expect("search")
            .len(),
        1
    );

    // Once synced here, importing again adds nothing.
    let synced = |engine: &Engine| {
        engine
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM messages WHERE account_id = ? AND message_id LIKE ?",
                    rusqlite::params![id, format!("%-{tag}@envelope.test")],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap_or(0)
    };
    assert!(
        wait_for(|| synced(&engine) == 3, 60).await,
        "the imported mail synced"
    );
    let again = engine
        .import_messages(folder.id, std::slice::from_ref(&path))
        .await
        .expect("import again");
    assert_eq!((again.added, again.skipped), (0, 3));

    let _ = sam.unselect().await;
    sam.delete(&name).await.expect("clean up");
    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
