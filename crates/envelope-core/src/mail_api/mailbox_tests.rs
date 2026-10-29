//! Engine actions on mail against a real temporary database. The fixture is
//! shared with the folder tests.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use super::*;
use crate::db::mail::{FolderUpsert, MessageMeta};
use crate::engine::{EventSink, Paths};
use crate::rules::MailRule;

#[derive(Clone, Default)]
pub(crate) struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

#[tokio::test]
async fn sync_failures_are_counted_until_a_sync_works() {
    let dir = std::env::temp_dir().join(format!("envelope-sync-{}", uuid::Uuid::new_v4().simple()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .expect("engine");
    // The count needs no account row.
    let id = 7;
    let failures = |s: SyncState| match s {
        SyncState::Offline { failures, .. } | SyncState::Error { failures, .. } => failures,
        _ => 0,
    };
    engine.set_sync_state(id, SyncState::Connecting);
    engine.set_sync_state(id, SyncState::failed("connection refused".into(), true));
    engine.set_sync_state(id, SyncState::Connecting);
    engine.set_sync_state(id, SyncState::failed("connection refused".into(), true));
    assert_eq!(failures(engine.sync_state(id)), 2);
    assert!(
        matches!(engine.sync_state(id), SyncState::Offline { ref message, .. } if message == "connection refused")
    );
    engine.set_sync_state(id, SyncState::Idle);
    engine.set_sync_state(id, SyncState::failed("bad certificate".into(), false));
    assert_eq!(
        failures(engine.sync_state(id)),
        1,
        "a sync that worked starts over"
    );
    // The interface hears about every failed attempt.
    let reported = events
        .0
        .lock()
        .iter()
        .filter(|e| {
            matches!(
                e,
                EngineEvent::SyncStatus {
                    sync: SyncState::Offline { .. } | SyncState::Error { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(reported, 3);
    let json = serde_json::to_value(engine.sync_state(id)).expect("json");
    assert_eq!(
        json,
        serde_json::json!({ "state": "error", "message": "bad certificate", "failures": 1 })
    );
    engine.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}

pub(crate) struct Fixture {
    pub(crate) engine: Engine,
    dir: PathBuf,
    pub(crate) account: i64,
    /// Folder ids by server path.
    pub(crate) folders: HashMap<String, i64>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) fn folder(path: &str, role: Option<FolderRole>, parent: Option<&str>) -> FolderUpsert {
    FolderUpsert {
        remote_id: path.into(),
        name: path.rsplit('/').next().unwrap_or(path).into(),
        path: path.into(),
        parent_remote_id: parent.map(Into::into),
        role,
        kind: "folder".into(),
        delimiter: Some("/".into()),
        selectable: true,
        sort_order: 0,
        color: None,
    }
}

/// An IMAP account (its worker kept away) with an inbox, Trash, Spam and folders.
pub(crate) fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "envelope-folders-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        Events::default(),
    )
    .expect("engine");
    let (account, folders) = engine
        .db
        .write(|tx| {
            let account = mail::insert_account(
                tx,
                AccountKind::Imap,
                "generic",
                "me@example.com",
                "Me",
                "#000",
                &AccountConfig::default(),
            )?;
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [account])?;
            let folders = mail::sync_folders(
                tx,
                account,
                &[
                    folder("INBOX", Some(FolderRole::Inbox), None),
                    folder("Sent", Some(FolderRole::Sent), None),
                    folder("Trash", Some(FolderRole::Trash), None),
                    folder("Trash/Old", None, Some("Trash")),
                    folder("Junk", Some(FolderRole::Junk), None),
                    folder("Archive", Some(FolderRole::Archive), None),
                    folder("Work", None, None),
                    folder("Work/Clients", None, Some("Work")),
                ],
            )?;
            Ok((account, folders))
        })
        .expect("account");
    Fixture {
        engine,
        dir,
        account,
        folders,
    }
}

/// A folder's id, by server path.
impl Fixture {
    pub(crate) fn id(&self, path: &str) -> i64 {
        self.folders[path]
    }
}

/// Stores a message in a folder; returns (message id, thread id).
impl Fixture {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn message(
        &self,
        path: &str,
        uid: u32,
        message_id: &str,
        reply_to: Option<&str>,
        from: &str,
        subject: &str,
        date: i64,
        flags: i64,
    ) -> (i64, i64) {
        let meta = MessageMeta {
            message_id: Some(message_id.into()),
            in_reply_to: reply_to.map(Into::into),
            references: reply_to.map(|r| vec![r.to_string()]).unwrap_or_default(),
            subject: subject.into(),
            from: Some(Address::new(None, from)),
            to: vec![Address::new(None, "me@example.com")],
            date,
            flags,
            ..Default::default()
        };
        let folder = self.id(path);
        let account = self.account;
        self.engine
            .db
            .write(|tx| {
                let stored = mail::upsert_message(tx, account, folder, Some(uid), &meta)?;
                let touched = mail::refresh_threads(tx, &[stored.thread_id].into())?;
                mail::refresh_folder_counts(tx, &touched)?;
                Ok((stored.message_id, stored.thread_id))
            })
            .expect("message")
    }
}

/// How many messages of a conversation are unread.
impl Fixture {
    pub(crate) fn unread(&self, thread: i64) -> i64 {
        self.engine
            .thread(thread)
            .expect("thread")
            .thread
            .unread_count
    }
}

/// The conversations a mailbox lists, newest first.
impl Fixture {
    pub(crate) fn list(&self, mailbox: Mailbox) -> Vec<i64> {
        self.engine
            .threads(&ThreadQuery {
                mailbox,
                search: None,
                filter: ListFilter::default(),
                offset: 0,
                limit: 100,
            })
            .expect("list")
            .items
            .into_iter()
            .map(|t| t.id)
            .collect()
    }
}

/// The operations waiting for the server.
impl Fixture {
    pub(crate) fn ops(&self) -> Vec<PendingOp> {
        self.engine
            .db
            .read(|c| misc::queued_ops(c, self.account))
            .expect("ops")
    }
}

#[tokio::test]
async fn mark_all_read_takes_the_list_as_shown_and_can_be_undone() {
    let fx = fixture();
    let inbox = fx.id("INBOX");
    let (m1, people) = fx.message(
        "INBOX",
        1,
        "p@x",
        None,
        "ann@example.com",
        "Dinner?",
        1_000,
        0,
    );
    let (m2, _) = fx.message(
        "INBOX",
        2,
        "p2@x",
        Some("p@x"),
        "ann@example.com",
        "Re: Dinner?",
        1_100,
        0,
    );
    let (_, news) = fx.message(
        "INBOX",
        3,
        "n@x",
        None,
        "news@example.com",
        "Weekly digest",
        2_000,
        0,
    );
    let (_, read) = fx.message("INBOX", 4, "r@x", None, "bob@example.com", "Done", 3_000, 1);
    fx.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE threads SET category = 'newsletters' WHERE id = ?",
                [news],
            )?;
            tx.execute(
                "UPDATE threads SET category = 'people' WHERE id IN (?, ?)",
                [people, read],
            )?;
            Ok(())
        })
        .expect("categories");
    // The People tab: newsletters stay unread.
    let filter = ListFilter {
        category: Some("people".into()),
        ..Default::default()
    };
    let marked = fx
        .engine
        .mark_read(&Mailbox::Folder { id: inbox }, &filter, None)
        .expect("mark");
    assert_eq!(marked.threads, 1);
    assert_eq!(
        marked.message_ids.iter().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([m1, m2])
    );
    assert_eq!(fx.unread(people), 0);
    assert_eq!(fx.unread(news), 1);
    // A search narrows it further.
    let none = fx
        .engine
        .mark_read(
            &Mailbox::Everything,
            &ListFilter::default(),
            Some("nothing-like-this"),
        )
        .expect("search");
    assert_eq!(none.threads, 0);
    // Undo: the same messages unread again.
    fx.engine
        .set_flags(
            &[],
            &marked.message_ids,
            &FlagChange {
                seen: Some(false),
                flagged: None,
            },
        )
        .expect("undo");
    assert_eq!(fx.unread(people), 2);
}

#[tokio::test]
async fn follow_ups_can_be_done_or_come_back_later() {
    let fx = fixture();
    let (_, thread) = fx.message(
        "Sent",
        1,
        "out@x",
        None,
        "me@example.com",
        "Proposal",
        1_000,
        1,
    );
    let account = fx.account;
    fx.engine
        .db
        .write(|tx| {
            misc::followup_add(tx, "out@x", account, "Proposal", 1_000, 2_000)?;
            misc::followup_mark_notified(tx, "out@x")
        })
        .expect("follow-up");
    assert_eq!(fx.list(Mailbox::FollowUp), [thread]);

    fx.engine
        .set_follow_up(&[thread], &FollowUpChange::Done)
        .expect("done");
    assert!(fx.list(Mailbox::FollowUp).is_empty());
    // Done stays done: no new reminder, and nothing to list.
    let due = fx
        .engine
        .db
        .read(|c| misc::followups_to_check(c, i64::MAX))
        .expect("check");
    assert!(due.iter().all(|f| f.notified));

    fx.engine
        .set_follow_up(&[thread], &FollowUpChange::Due)
        .expect("undo");
    assert_eq!(fx.list(Mailbox::FollowUp), [thread]);

    let later = crate::db::now_ms() + 86_400_000;
    fx.engine
        .set_follow_up(&[thread], &FollowUpChange::RemindAt { at: later })
        .expect("later");
    assert!(fx.list(Mailbox::FollowUp).is_empty());
    let waiting = fx
        .engine
        .db
        .read(|c| misc::followups_to_check(c, later - 1))
        .expect("check");
    assert!(waiting.is_empty(), "not due before then");
    let due = fx
        .engine
        .db
        .read(|c| misc::followups_to_check(c, later))
        .expect("check");
    assert!(
        due.iter()
            .any(|f| f.thread_id == Some(thread) && !f.notified)
    );
}

#[tokio::test]
async fn not_spam_moves_back_to_the_inbox() {
    let fx = fixture();
    let (_, thread) = fx.message(
        "Junk",
        5,
        "j@x",
        None,
        "friend@example.com",
        "Photos",
        1_000,
        0,
    );
    fx.engine
        .move_threads(&[thread], Some(fx.id("Junk")), &MoveTarget::Inbox)
        .expect("not spam");
    assert_eq!(fx.list(Mailbox::Folder { id: fx.id("INBOX") }), [thread]);
    let op = fx.ops().into_iter().next();
    assert!(
        matches!(&op, Some(PendingOp::Move { dest_folder, items }) if dest_folder == "INBOX" && items[0].folder == "Junk"),
        "{op:?}"
    );
}

fn rule(json: &str) -> MailRule {
    serde_json::from_str(json).expect("rule")
}

#[tokio::test]
async fn rules_run_on_existing_mail_and_undo() {
    let fx = fixture();
    let inbox = fx.id("INBOX");
    let (_, receipt) = fx.message(
        "INBOX",
        1,
        "r1@x",
        None,
        "orders@shop.example",
        "Your receipt",
        3_000,
        0,
    );
    let (_, old) = fx.message(
        "Archive",
        1,
        "r2@x",
        None,
        "orders@shop.example",
        "Older receipt",
        1_000,
        0,
    );
    let (_, spam) = fx.message(
        "Junk",
        1,
        "r3@x",
        None,
        "orders@shop.example",
        "Spammy receipt",
        2_000,
        0,
    );
    let (_, other) = fx.message(
        "INBOX",
        2,
        "o@x",
        None,
        "ann@example.com",
        "Hello",
        4_000,
        0,
    );
    // Written by the user: never matched, as it never arrives through rules.
    fx.message(
        "INBOX",
        3,
        "mine@x",
        None,
        "me@example.com",
        "Order question to shop.example",
        5_000,
        1,
    );
    let receipts = rule(&format!(
        r#"{{"name":"Receipts","enabled":false,"conditions":[{{"field":"any","op":"contains","value":"shop.example"}}],
            "actions":[{{"kind":"read"}},{{"kind":"move","folderId":{}}}]}}"#,
        fx.id("Work")
    ));
    assert!(crate::rules::acts_on_existing(&receipts));
    let found = fx.engine.rule_matches(&receipts, 100).expect("matches");
    assert_eq!(
        found.thread_ids,
        [receipt, old],
        "newest first; not Spam, not mine"
    );
    assert_eq!(found.total, 2);
    assert_eq!(
        fx.engine
            .rule_matches(&receipts, 1)
            .expect("limit")
            .thread_ids,
        [receipt]
    );

    let undo = fx
        .engine
        .apply_rule(&receipts, &found.thread_ids)
        .expect("apply");
    assert_eq!(fx.unread(receipt), 0);
    let work = fx.list(Mailbox::Folder { id: fx.id("Work") });
    assert!(work.contains(&receipt) && work.contains(&old));
    assert!(!fx.list(Mailbox::Folder { id: inbox }).contains(&receipt));
    assert!(fx.list(Mailbox::Folder { id: inbox }).contains(&other));
    assert!(
        fx.list(Mailbox::Folder { id: fx.id("Junk") })
            .contains(&spam)
    );

    fx.engine.undo_rule(&undo).expect("undo");
    assert!(fx.list(Mailbox::Folder { id: inbox }).contains(&receipt));
    assert!(
        fx.list(Mailbox::Folder {
            id: fx.id("Archive")
        })
        .contains(&old)
    );
    assert!(fx.list(Mailbox::Folder { id: fx.id("Work") }).is_empty());
    assert_eq!(fx.unread(receipt), 1);

    let quiet = rule(
        r#"{"name":"Quiet","conditions":[{"field":"from","op":"is","value":"x@y"}],"actions":[{"kind":"mute"}]}"#,
    );
    assert!(!crate::rules::acts_on_existing(&quiet));
}
