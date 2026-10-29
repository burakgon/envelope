//! VIPs in one engine with a real (temporary) database: the list, their
//! mailbox, `is:vip`, and the notifications they get through.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::db::mail::{self, FolderUpsert};
use crate::engine::{Engine, EventSink, Paths};
use crate::model::*;

#[derive(Default, Clone)]
struct Captured(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Captured {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

struct Fixture {
    engine: Engine,
    events: Captured,
    dir: PathBuf,
    account: i64,
    inbox: i64,
    trash: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn folder(remote: &str, role: FolderRole) -> FolderUpsert {
    FolderUpsert {
        remote_id: remote.into(),
        name: remote.into(),
        path: remote.into(),
        parent_remote_id: None,
        role: Some(role),
        kind: "folder".into(),
        delimiter: None,
        selectable: true,
        sort_order: 0,
        color: None,
    }
}

impl Fixture {
    fn new(notify: NotifyMode) -> Fixture {
        let dir = std::env::temp_dir().join(format!("envelope-vips-{}", uuid::Uuid::new_v4()));
        let events = Captured::default();
        let engine = Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            events.clone(),
        )
        .unwrap();
        let (account, inbox, trash) = engine
            .db
            .write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    "alex@example.com",
                    "Alex Morgan",
                    "#000",
                    &AccountConfig {
                        notify,
                        ..AccountConfig::default()
                    },
                )?;
                let folders = mail::sync_folders(
                    tx,
                    account,
                    &[
                        folder("INBOX", FolderRole::Inbox),
                        folder("Trash", FolderRole::Trash),
                    ],
                )?;
                Ok((account, folders["INBOX"], folders["Trash"]))
            })
            .unwrap();
        Fixture {
            engine,
            events,
            dir,
            account,
            inbox,
            trash,
        }
    }

    /// Stores a message from `from` in a folder, as sync would.
    fn store(&self, folder: i64, uid: u32, from: &str, subject: &str) -> i64 {
        let raw = format!(
            "From: {from}\r\nTo: alex@example.com\r\nSubject: {subject}\r\n\
             Message-ID: <{uid}.{folder}@example.com>\r\nDate: Thu, 29 Oct 2026 10:00:00 +0000\r\n\r\n{subject}\r\n"
        );
        let account = self.account;
        self.engine
            .db
            .write(|tx| {
                let (meta, body) = crate::mime::parse_full(raw.as_bytes()).unwrap();
                let stored = mail::upsert_message(tx, account, folder, Some(uid), &meta)?;
                mail::store_body(
                    tx,
                    stored.message_id,
                    body.html.as_deref(),
                    body.text.as_deref(),
                    &body.snippet,
                    Some(&body.attachments),
                )?;
                mail::refresh_threads(tx, &BTreeSet::from([stored.thread_id]))?;
                Ok(stored.thread_id)
            })
            .unwrap()
    }

    fn subjects(&self, mailbox: Mailbox, search: Option<&str>) -> Vec<String> {
        let mut subjects: Vec<String> = self
            .engine
            .threads(&ThreadQuery {
                mailbox,
                search: search.map(str::to_string),
                filter: ListFilter::default(),
                offset: 0,
                limit: 50,
            })
            .unwrap()
            .items
            .into_iter()
            .map(|t| t.subject)
            .collect();
        subjects.sort();
        subjects
    }
}

#[tokio::test]
async fn vips_are_kept_by_address_and_named() {
    let f = Fixture::new(NotifyMode::All);
    f.engine
        .set_vip(" Maya@Northwind.design ", Some("Maya Chen"), true)
        .unwrap();
    f.engine.set_vip("daniel@lumen.dev", None, true).unwrap();
    // Again, without a name: the name stays.
    f.engine
        .set_vip("maya@northwind.design", None, true)
        .unwrap();
    let names = |f: &Fixture| -> Vec<(String, Option<String>)> {
        f.engine
            .vips()
            .unwrap()
            .into_iter()
            .map(|v| (v.email, v.name))
            .collect()
    };
    assert_eq!(
        names(&f),
        [
            ("daniel@lumen.dev".to_string(), None),
            (
                "maya@northwind.design".to_string(),
                Some("Maya Chen".to_string())
            ),
        ]
    );
    f.engine
        .set_vip("MAYA@northwind.design", None, false)
        .unwrap();
    assert_eq!(names(&f), [("daniel@lumen.dev".to_string(), None)]);
    f.engine.set_vip("  ", None, true).unwrap();
    assert_eq!(f.engine.vips().unwrap().len(), 1);
}

#[tokio::test]
async fn the_vip_mailbox_and_search_find_their_conversations() {
    let f = Fixture::new(NotifyMode::All);
    f.store(
        f.inbox,
        1,
        "Maya Chen <Maya@Northwind.design>",
        "Onboarding flow",
    );
    f.store(
        f.inbox,
        2,
        "The Daily Byte <news@dailybyte.test>",
        "Daily Byte",
    );
    f.store(f.inbox, 3, "Daniel <daniel@lumen.dev>", "Interview loop");
    f.store(f.trash, 4, "maya@northwind.design", "Old draft");
    assert!(f.subjects(Mailbox::Vip, None).is_empty());

    f.engine
        .set_vip("maya@northwind.design", Some("Maya Chen"), true)
        .unwrap();
    // Trash and spam stay out, as for Starred.
    assert_eq!(f.subjects(Mailbox::Vip, None), ["Onboarding flow"]);
    assert_eq!(
        f.subjects(
            Mailbox::Unified {
                role: FolderRole::Inbox
            },
            Some("is:vip")
        ),
        ["Onboarding flow"]
    );
    assert_eq!(
        f.subjects(
            Mailbox::Unified {
                role: FolderRole::Inbox
            },
            Some("-is:vip")
        ),
        ["Daily Byte", "Interview loop"]
    );

    f.engine.set_vip("daniel@lumen.dev", None, true).unwrap();
    assert_eq!(
        f.subjects(Mailbox::Vip, None),
        ["Interview loop", "Onboarding flow"]
    );
}

#[tokio::test]
async fn new_mail_from_vips_notifies() {
    let notified = |f: &Fixture| -> Option<Vec<String>> {
        f.events.0.lock().iter().rev().find_map(|e| match e {
            EngineEvent::NewMail { threads, .. } => {
                Some(threads.iter().map(|t| t.subject.clone()).collect())
            }
            _ => None,
        })
    };
    for (mode, expected) in [
        (
            NotifyMode::All,
            Some(vec!["Onboarding flow", "Daily Byte", "Interview loop"]),
        ),
        // A VIP's newsletter is still theirs.
        (
            NotifyMode::People,
            Some(vec!["Onboarding flow", "Interview loop"]),
        ),
        (NotifyMode::Vips, Some(vec!["Onboarding flow"])),
        (NotifyMode::None, None),
    ] {
        let f = Fixture::new(mode);
        f.engine
            .set_vip("maya@northwind.design", Some("Maya Chen"), true)
            .unwrap();
        let ids = [
            f.store(
                f.inbox,
                1,
                "Maya Chen <maya@northwind.design>",
                "Onboarding flow",
            ),
            f.store(
                f.inbox,
                2,
                "The Daily Byte <news@dailybyte.test>",
                "Daily Byte",
            ),
            f.store(f.inbox, 3, "Daniel <daniel@lumen.dev>", "Interview loop"),
        ];
        let mut threads = Vec::new();
        for id in ids {
            let mut t = f.engine.db.read(|c| mail::thread_summary(c, id)).unwrap();
            // As sync sorts them, Maya's mail being a newsletter.
            let people = t.subject == "Interview loop";
            t.category = Some(if people { "people" } else { "newsletters" }.into());
            threads.push(t);
        }
        f.engine.emit(EngineEvent::NewMail {
            account_id: f.account,
            threads,
        });
        assert_eq!(
            notified(&f),
            expected.map(|v| v.into_iter().map(str::to_string).collect()),
            "{mode:?}"
        );
    }
}
