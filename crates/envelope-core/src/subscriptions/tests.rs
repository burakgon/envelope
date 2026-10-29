//! Subscriptions in one engine with a real (temporary) database: who counts
//! as a list sender, how their conversations are counted, and which message
//! unsubscribes.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::db::mail::{self, FolderUpsert};
use crate::engine::{Engine, EventSink, Paths};
use crate::model::*;

use super::Subscription;

struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _: EngineEvent) {}
}

struct Fixture {
    engine: Engine,
    dir: PathBuf,
    account: i64,
    inbox: i64,
    archive: i64,
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
    fn new() -> Fixture {
        let dir = std::env::temp_dir().join(format!("envelope-subs-{}", uuid::Uuid::new_v4()));
        let engine = Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            NoEvents,
        )
        .unwrap();
        let (account, inbox, archive, trash) = engine
            .db
            .write(|tx| {
                let account = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    "alex@example.com",
                    "Alex Morgan",
                    "#000",
                    &AccountConfig::default(),
                )?;
                let folders = mail::sync_folders(
                    tx,
                    account,
                    &[
                        folder("INBOX", FolderRole::Inbox),
                        folder("Archive", FolderRole::Archive),
                        folder("Trash", FolderRole::Trash),
                    ],
                )?;
                Ok((
                    account,
                    folders["INBOX"],
                    folders["Archive"],
                    folders["Trash"],
                ))
            })
            .unwrap();
        Fixture {
            engine,
            dir,
            account,
            inbox,
            archive,
            trash,
        }
    }

    /// Stores a message with extra `headers` in a folder, as sync would.
    fn store(
        &self,
        folder: i64,
        uid: u32,
        from: &str,
        subject: &str,
        headers: &str,
        seen: bool,
    ) -> i64 {
        let raw = format!(
            "From: {from}\r\nTo: alex@example.com\r\nSubject: {subject}\r\n{headers}\
             Message-ID: <{uid}.{folder}@example.com>\r\nDate: Thu, 29 Oct 2026 1{}:00:00 +0000\r\n\r\n{subject}\r\n",
            uid % 10
        );
        let account = self.account;
        self.engine
            .db
            .write(|tx| {
                let (mut meta, body) = crate::mime::parse_full(raw.as_bytes()).unwrap();
                if seen {
                    meta.flags |= Flags::SEEN;
                }
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
                Ok(stored.message_id)
            })
            .unwrap()
    }
}

const ONE_CLICK: &str = "List-Unsubscribe: <https://dailybyte.test/u?id=1>\r\n\
                         List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n";

#[tokio::test]
async fn list_senders_are_counted_by_conversation() {
    let f = Fixture::new();
    let byte = "The Daily Byte <News@DailyByte.test>";
    f.store(f.inbox, 1, byte, "Issue 1", ONE_CLICK, true);
    f.store(
        f.inbox,
        2,
        byte,
        "Issue 2",
        "List-Id: <daily.dailybyte.test>\r\n",
        false,
    );
    let newest = f.store(f.inbox, 3, byte, "Issue 3", ONE_CLICK, true);
    f.store(f.trash, 4, byte, "Issue 0", ONE_CLICK, false);
    let rust = "This Week in Rust <editor@rustweekly.test>";
    f.store(
        f.archive,
        5,
        rust,
        "This Week in Rust 612",
        "List-Id: <twir.rustweekly.test>\r\n",
        true,
    );
    // Notifications named after whoever acted: the list's name is theirs.
    let list = "List-Id: lumen/envelope <envelope.lumen.gitforge.test>\r\n";
    f.store(
        f.inbox,
        8,
        "Daniel Okafor <notifications@gitforge.test>",
        "[lumen/envelope] PR #1",
        list,
        true,
    );
    f.store(
        f.inbox,
        9,
        "Sofia Martinez <notifications@gitforge.test>",
        "[lumen/envelope] PR #2",
        list,
        true,
    );
    // A person, and the user writing to a list: neither is a subscription.
    f.store(
        f.inbox,
        6,
        "Maya Chen <maya@northwind.design>",
        "Lunch?",
        "",
        false,
    );
    f.store(
        f.inbox,
        7,
        "Alex Morgan <alex@example.com>",
        "Re: [rust] nom 8",
        "List-Id: <rust.lists.test>\r\n",
        true,
    );

    let subs = f.engine.subscriptions().await.unwrap();
    assert_eq!(
        subs,
        [
            Subscription {
                email: "news@dailybyte.test".into(),
                name: Some("The Daily Byte".into()),
                conversations: 3,
                unread: 1,
                in_inbox: 3,
                last_date: subs[0].last_date,
                unsubscribe_message_id: Some(newest),
                one_click: true,
            },
            Subscription {
                email: "notifications@gitforge.test".into(),
                name: Some("lumen/envelope".into()),
                conversations: 2,
                unread: 0,
                in_inbox: 2,
                last_date: subs[1].last_date,
                unsubscribe_message_id: None,
                one_click: false,
            },
            Subscription {
                email: "editor@rustweekly.test".into(),
                name: Some("This Week in Rust".into()),
                conversations: 1,
                unread: 0,
                in_inbox: 0,
                last_date: subs[2].last_date,
                unsubscribe_message_id: None,
                one_click: false,
            },
        ]
    );
}

#[tokio::test]
async fn nothing_listed_without_list_mail() {
    let f = Fixture::new();
    f.store(
        f.inbox,
        1,
        "Maya Chen <maya@northwind.design>",
        "Lunch?",
        "",
        false,
    );
    assert!(f.engine.subscriptions().await.unwrap().is_empty());
}
