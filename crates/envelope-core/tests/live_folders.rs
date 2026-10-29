//! Folder management and spam keywords against the development server
//! (scripts/devserver/run.sh). Uses sam@envelope.test and folders with a
//! unique name, removed at the end; the seeded mail is left alone.
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_folders -- --nocapture

use std::sync::Arc;
use std::time::{Duration, Instant};

use envelope_core::db::misc::{self, PendingOp};
use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};
use envelope_imap::{Auth, Client, Config};
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

/// A connection of our own, to see what the server has.
async fn server() -> Client {
    let mut config = Config::new("127.0.0.1", 1993, envelope_imap::Security::Tls);
    config.accept_invalid_certs = true;
    let mut client = Client::connect(&config).await.expect("connect");
    client
        .authenticate(&Auth::Password {
            user: "sam@envelope.test".into(),
            password: "password".into(),
        })
        .await
        .expect("login");
    client
}

async fn mailboxes(client: &mut Client) -> Vec<String> {
    let (entries, _) = client.list(false).await.expect("list");
    entries.into_iter().map(|e| e.name).collect()
}

fn local(engine: &Engine, account: i64, path: &str) -> Option<Folder> {
    engine
        .folders()
        .expect("folders")
        .into_iter()
        .find(|f| f.account_id == account && f.path == path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folders_and_spam_on_a_real_server() {
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
        Events::default(),
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
    assert!(
        wait_for(
            || local(&engine, id, "Junk").is_some() && local(&engine, id, "INBOX").is_some(),
            60
        )
        .await,
        "folders listed"
    );
    let mut sam = server().await;
    let base = format!("EnvTest{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let delim = "/";

    // Create a folder, and one inside it.
    let top = engine
        .create_folder(id, None, &base, false, None)
        .await
        .expect("create");
    assert_eq!(top.path, base);
    let inner_name = format!("{base}In");
    let inner = engine
        .create_folder(id, Some(top.id), &inner_name, false, None)
        .await
        .expect("create inside");
    assert_eq!(inner.parent_id, Some(top.id));
    let names = mailboxes(&mut sam).await;
    assert!(
        names.contains(&base) && names.contains(&format!("{base}{delim}{inner_name}")),
        "{names:?}"
    );
    let taken = engine.create_folder(id, None, &base, false, None).await;
    assert!(taken.is_err(), "the name is taken");

    // Mail in the inner folder, so the rename has something to carry.
    let marker = format!("<{}@envelope.test>", uuid::Uuid::new_v4());
    let message = format!(
        "From: Pat <pat@example.com>\r\nTo: sam@envelope.test\r\nSubject: {base} keywords\r\nMessage-ID: {marker}\r\nDate: Thu, 29 Oct 2026 10:00:00 +0000\r\n\r\nHello\r\n"
    );
    sam.append(
        &format!("{base}{delim}{inner_name}"),
        &[],
        None,
        message.as_bytes(),
    )
    .await
    .expect("append");

    // Rename the top folder: the inner one moves along, here and there.
    let renamed = format!("{base}R");
    engine
        .rename_folder(top.id, &renamed)
        .await
        .expect("rename");
    let names = mailboxes(&mut sam).await;
    assert!(
        names.contains(&format!("{renamed}{delim}{inner_name}")) && !names.contains(&base),
        "{names:?}"
    );
    let inner_now =
        local(&engine, id, &format!("{renamed}{delim}{inner_name}")).expect("inner moved along");
    assert_eq!(inner_now.id, inner.id);

    // Drag the inner folder to the top.
    engine
        .move_folder(inner.id, None)
        .await
        .expect("to the top");
    let names = mailboxes(&mut sam).await;
    assert!(names.contains(&inner_name), "{names:?}");
    assert_eq!(
        local(&engine, id, &inner_name).map(|f| f.parent_id),
        Some(None)
    );
    // Back inside, so deleting the top folder takes it along.
    engine
        .move_folder(inner.id, Some(top.id))
        .await
        .expect("back inside");
    assert!(
        mailboxes(&mut sam)
            .await
            .contains(&format!("{renamed}{delim}{inner_name}"))
    );

    // Emptying (as Trash and Spam are emptied): the folder's mail, then the folders inside.
    let holder = engine
        .create_folder(id, None, &format!("{base}Bin"), false, None)
        .await
        .expect("bin");
    engine
        .create_folder(id, Some(holder.id), "Old", false, None)
        .await
        .expect("bin/old");
    sam.append(&holder.path, &[], None, message.as_bytes())
        .await
        .expect("append");
    sam.append(&holder.path, &[], None, message.as_bytes())
        .await
        .expect("append");
    let status = sam.status(&holder.path).await.expect("status");
    engine
        .db
        .write(|tx| {
            misc::queue_op(
                tx,
                id,
                &PendingOp::EmptyFolder {
                    folder: holder.path.clone(),
                    max_uid: status.uid_next.map(|n| n - 1),
                    subfolders: vec![format!("{}{delim}Old", holder.path)],
                },
                0,
            )
            .map(drop)
        })
        .expect("queue");
    engine.sync_now(Some(id));
    let mut emptied = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let names = mailboxes(&mut sam).await;
        let count = sam.status(&holder.path).await.ok().and_then(|s| s.messages);
        if count == Some(0) && !names.contains(&format!("{}{delim}Old", holder.path)) {
            emptied = true;
            break;
        }
    }
    assert!(emptied, "emptied on the server");

    // Spam: moving into Junk sets $Junk, moving out sets $NotJunk.
    let subject = format!("{base} spam check");
    let spam = format!(
        "From: Pat <pat@example.com>\r\nTo: sam@envelope.test\r\nSubject: {subject}\r\nMessage-ID: <{}@envelope.test>\r\nDate: Thu, 29 Oct 2026 11:00:00 +0000\r\n\r\nWin a prize\r\n",
        uuid::Uuid::new_v4()
    );
    sam.append("Junk", &[], None, spam.as_bytes())
        .await
        .expect("append spam");
    engine.sync_now(Some(id));
    let junk = local(&engine, id, "Junk").expect("junk");
    let inbox = local(&engine, id, "INBOX").expect("inbox");
    let find = |folder: i64| {
        engine
            .threads(&ThreadQuery {
                mailbox: Mailbox::Folder { id: folder },
                search: None,
                filter: ListFilter::default(),
                offset: 0,
                limit: 200,
            })
            .ok()
            .and_then(|p| p.items.into_iter().find(|t| t.subject == subject))
    };
    assert!(
        wait_for(|| find(junk.id).is_some(), 90).await,
        "spam synced"
    );
    let thread = find(junk.id).expect("spam thread").id;
    engine
        .move_threads(&[thread], Some(junk.id), &MoveTarget::Inbox)
        .expect("not spam");
    assert!(find(inbox.id).is_some(), "in the inbox at once");
    // After the undo window the server has it in INBOX, marked not junk.
    let mut flags = Vec::new();
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        sam.select("INBOX", None).await.expect("select");
        let uids = sam
            .uid_search(&format!("SUBJECT \"{subject}\""))
            .await
            .expect("search");
        if let Some(uid) = uids.last() {
            let items = sam
                .uid_fetch(&uid.to_string(), "FLAGS")
                .await
                .expect("fetch");
            flags = items
                .first()
                .and_then(|i| i.flags.clone())
                .unwrap_or_default();
            // Clean up the test message.
            sam.uid_store(&uids, "+FLAGS.SILENT", &["\\Deleted"])
                .await
                .expect("flag");
            sam.uid_expunge(&uids).await.expect("expunge");
            break;
        }
    }
    assert!(flags.iter().any(|f| f == "$NotJunk"), "flags {flags:?}");
    assert!(!flags.iter().any(|f| f == "$Junk"), "flags {flags:?}");

    // Delete the test folders (the inner folder's mail goes to Trash first).
    engine.delete_folder(top.id).await.expect("delete");
    engine.delete_folder(holder.id).await.expect("delete bin");
    let names = mailboxes(&mut sam).await;
    assert!(!names.iter().any(|n| n.starts_with(&base)), "{names:?}");
    assert!(local(&engine, id, &renamed).is_none());
    // Our test message is in Trash now: remove it there too.
    sam.select("Trash", None).await.expect("trash");
    let leftovers = sam
        .uid_search(&format!("SUBJECT \"{base} keywords\""))
        .await
        .expect("search");
    assert!(
        !leftovers.is_empty(),
        "the deleted folder's mail went to Trash"
    );
    sam.uid_store(&leftovers, "+FLAGS.SILENT", &["\\Deleted"])
        .await
        .expect("flag");
    sam.uid_expunge(&leftovers).await.expect("expunge");
    let _ = sam.logout().await;
    engine.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
