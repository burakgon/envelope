//! Drafts and replies inside one engine with a real (temporary) database and
//! no server: drafts written elsewhere, drafts only on this computer, server
//! copies that are replaced or deleted, and quote headers in other languages.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::compose::{FilePart, Outgoing};
use crate::db::mail::{self, FolderUpsert};
use crate::db::misc::{self, PendingOp};
use crate::engine::{Engine, EventSink, Paths};
use crate::model::*;

struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _: EngineEvent) {}
}

struct Fixture {
    engine: Engine,
    dir: PathBuf,
    account: i64,
    inbox: i64,
    drafts: i64,
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
        let dir = std::env::temp_dir().join(format!("envelope-drafts-{}", uuid::Uuid::new_v4()));
        let engine = Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            NoEvents,
        )
        .unwrap();
        let (account, inbox, drafts) = engine
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
                        folder("Drafts", FolderRole::Drafts),
                    ],
                )?;
                Ok((account, folders["INBOX"], folders["Drafts"]))
            })
            .unwrap();
        Fixture {
            engine,
            dir,
            account,
            inbox,
            drafts,
        }
    }

    /// Stores a raw message in a folder as sync would; `draft` flags it as one.
    fn store(&self, folder: i64, uid: u32, raw: &[u8], draft: bool) -> i64 {
        let (account, raw) = (self.account, raw.to_vec());
        self.engine
            .db
            .write(|tx| {
                let (mut meta, body) = crate::mime::parse_full(&raw).unwrap();
                if draft {
                    meta.flags |= Flags::DRAFT | Flags::SEEN;
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

    /// Puts an attachment's content where a download would leave it.
    fn downloaded(&self, attachment: &AttachmentInfo, data: &[u8]) {
        let dir = self
            .dir
            .join("cache")
            .join(format!("account-{}", self.account))
            .join(format!("att-{}", attachment.id));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&attachment.filename), data).unwrap();
    }

    fn ops(&self) -> Vec<PendingOp> {
        self.engine
            .db
            .read(|c| misc::queued_ops(c, self.account))
            .unwrap()
    }

    fn exists(&self, message_id: i64) -> bool {
        self.engine
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM messages WHERE id = ?",
                    [message_id],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap()
            > 0
    }
}

fn mime(from: &str, to: &str, subject: &str, html: &str, id: &str) -> Outgoing {
    Outgoing {
        from: Address::new(Some("Alex Morgan".into()), from),
        to: vec![Address::new(Some("Elif Yılmaz".into()), to)],
        subject: subject.into(),
        html: html.into(),
        message_id: Some(id.into()),
        allow_no_recipients: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn opens_a_draft_written_on_another_computer() {
    let f = Fixture::new();
    let original = crate::compose::render(&Outgoing {
        from: Address::new(Some("Elif Yılmaz".into()), "elif@example.com"),
        to: vec![Address::new(None, "alex@example.com")],
        subject: "Hafta sonu".into(),
        html: "<p>Cumartesi?</p>".into(),
        message_id: Some("question@example.com".into()),
        ..Default::default()
    })
    .unwrap();
    let asked = f.store(f.inbox, 1, &original.mime, false);
    let mut out = mime(
        "alex@example.com",
        "elif@example.com",
        "Re: Hafta sonu",
        "<p>Olur, <b>11</b> gibi.</p>",
        "reply-draft@example.com",
    );
    out.in_reply_to = Some("question@example.com".into());
    out.attachments.push(FilePart {
        filename: "plan.txt".into(),
        mime_type: "text/plain".into(),
        data: b"kahve".to_vec(),
        content_id: None,
    });
    let rendered = crate::compose::render(&out).unwrap();
    let server = f.store(f.drafts, 7, &rendered.mime, true);
    let attachment = f
        .engine
        .db
        .read(|c| mail::get_message(c, server))
        .unwrap()
        .attachments
        .remove(0);
    f.downloaded(&attachment, b"kahve");

    let draft = f.engine.draft_for_message(server).await.unwrap();
    assert_eq!(draft.data.subject, "Re: Hafta sonu");
    assert_eq!(draft.data.to[0].email, "elif@example.com");
    assert!(draft.data.html.contains("<b>11</b>"), "{}", draft.data.html);
    assert_eq!(draft.data.reply_to_message_id, Some(asked));
    assert_eq!(draft.data.mode.as_deref(), Some("reply"));
    // Sent from the account's own address: its identity, not a fixed From.
    assert!(draft.data.identity_id.is_some() && draft.data.from.is_none());
    // The attachment is kept next to the draft: the server copy goes once it is saved again.
    let kept = draft.data.attachments[0].path.clone().expect("a file");
    assert_eq!(std::fs::read(&kept).unwrap(), b"kahve");
    assert_eq!(draft.server_message_id, Some(server));
    assert!(!draft.local_only);

    // Opening it again finds the same draft.
    let again = f.engine.draft_for_message(server).await.unwrap();
    assert_eq!(again.id, draft.id);

    // Discarding it deletes the server copy, here at once.
    f.engine.delete_draft(draft.id).unwrap();
    assert!(!f.exists(server));
    assert!(f.ops().iter().any(|op| matches!(
        op,
        PendingOp::DeleteDraft { remote_uid: Some(7), message_id: Some(id), .. } if id == "reply-draft@example.com"
    )));
    assert!(!std::path::Path::new(&kept).exists());
    assert!(f.engine.drafts().unwrap().is_empty());
}

#[tokio::test]
async fn lists_drafts_that_are_only_on_this_computer() {
    let f = Fixture::new();
    let data = DraftInput {
        account_id: f.account,
        subject: "Notes".into(),
        html: "<p>Half written</p>".into(),
        ..Default::default()
    };
    let id = f.engine.save_draft(None, &data, false).unwrap();
    let listed = f.engine.drafts().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].local_only && listed[0].server_message_id.is_none());

    // Stored on the server and synced back: listed through its server copy.
    let version = f.engine.draft(id).unwrap().updated_at;
    let copy = crate::compose::render(&mime(
        "alex@example.com",
        "x@example.com",
        "Notes",
        "<p>Half written</p>",
        "one@example.com",
    ))
    .unwrap();
    f.engine
        .db
        .write(|tx| misc::draft_uploaded(tx, id, Some(3), None, "one@example.com", version))
        .unwrap();
    let server = f.store(f.drafts, 3, &copy.mime, true);
    let listed = f.engine.drafts().unwrap();
    assert_eq!(
        (listed[0].server_message_id, listed[0].local_only),
        (Some(server), false)
    );

    // Saved again: the old copy leaves Drafts with the upload of the new one.
    let newer = crate::compose::render(&mime(
        "alex@example.com",
        "x@example.com",
        "Notes",
        "<p>Done</p>",
        "two@example.com",
    ))
    .unwrap();
    let threads = f
        .engine
        .db
        .write(|tx| misc::draft_uploaded(tx, id, Some(4), None, "two@example.com", version))
        .unwrap();
    assert!(!threads.is_empty());
    assert!(!f.exists(server));
    let server = f.store(f.drafts, 4, &newer.mime, true);
    assert_eq!(
        f.engine.drafts().unwrap()[0].server_message_id,
        Some(server)
    );

    // Deleted elsewhere: nothing to show, as this computer has no changes of its own...
    f.engine
        .db
        .write(|tx| mail::forget_message(tx, server).map(|_| ()))
        .unwrap();
    assert!(!f.engine.drafts().unwrap()[0].local_only);
    // ...until it does.
    std::thread::sleep(std::time::Duration::from_millis(5));
    f.engine.save_draft(Some(id), &data, false).unwrap();
    assert!(f.engine.drafts().unwrap()[0].local_only);

    // Queued to be sent: not a draft any more.
    f.engine
        .send(
            Some(id),
            &DraftInput {
                to: vec![Address::new(None, "x@example.com")],
                ..data.clone()
            },
            60_000,
            None,
        )
        .await
        .unwrap();
    assert!(f.engine.drafts().unwrap().is_empty());
}

#[tokio::test]
async fn a_draft_moved_to_another_account_leaves_the_old_server() {
    let f = Fixture::new();
    let other = f
        .engine
        .db
        .write(|tx| {
            mail::insert_account(
                tx,
                AccountKind::Imap,
                "generic",
                "alex@home.example",
                "Alex Morgan",
                "#111",
                &AccountConfig::default(),
            )
        })
        .unwrap();
    let data = DraftInput {
        account_id: f.account,
        subject: "Plans".into(),
        ..Default::default()
    };
    let id = f.engine.save_draft(None, &data, false).unwrap();
    f.engine
        .db
        .write(|tx| misc::draft_uploaded(tx, id, Some(9), None, "plans@example.com", 0))
        .unwrap();
    let copy = crate::compose::render(&mime(
        "alex@example.com",
        "x@example.com",
        "Plans",
        "",
        "plans@example.com",
    ))
    .unwrap();
    let server = f.store(f.drafts, 9, &copy.mime, true);

    let moved = DraftInput {
        account_id: other,
        ..data
    };
    assert_eq!(f.engine.save_draft(Some(id), &moved, false).unwrap(), id);
    assert!(!f.exists(server));
    assert!(f.ops().iter().any(|op| matches!(
        op,
        PendingOp::DeleteDraft {
            remote_uid: Some(9),
            ..
        }
    )));
    let row = f.engine.db.read(|c| misc::draft_get(c, id)).unwrap();
    assert_eq!(
        (row.account_id, row.remote_uid, row.message_id),
        (other, None, None)
    );
}

#[tokio::test]
async fn saving_a_draft_that_is_gone_starts_a_new_one() {
    let f = Fixture::new();
    let data = DraftInput {
        account_id: f.account,
        subject: "Still writing".into(),
        ..Default::default()
    };
    let id = f.engine.save_draft(None, &data, false).unwrap();
    f.engine.delete_draft(id).unwrap();
    assert!(f.engine.draft(id).is_err());
    let again = f.engine.save_draft(Some(id), &data, false).unwrap();
    assert_eq!(f.engine.draft(again).unwrap().data.subject, "Still writing");
    assert!(f.engine.drafts().unwrap()[0].local_only);
}

#[tokio::test]
async fn quotes_in_the_interface_language() {
    let f = Fixture::new();
    let original = crate::compose::render(&Outgoing {
        from: Address::new(Some("Elif <Y>".into()), "elif@example.com"),
        to: vec![Address::new(None, "alex@example.com")],
        subject: "Plan".into(),
        html: "<p>Merhaba</p>".into(),
        ..Default::default()
    })
    .unwrap();
    let m = f.store(f.inbox, 1, &original.mime, false);
    let german = QuoteText {
        wrote: Some("Am {date} schrieb {sender}:".into()),
        forwarded: Some("---------- Weitergeleitete Nachricht ---------".into()),
        from: Some("Von:".into()),
        date: Some("Datum:".into()),
        subject: Some("Betreff:".into()),
        to: Some("An:".into()),
        when: Some("Do., 29. Okt. 2026, 14:32".into()),
    };
    let reply = f.engine.compose_reply(m, "reply", &german).unwrap();
    assert!(
        reply.html.contains(
            "Am Do., 29. Okt. 2026, 14:32 schrieb Elif &lt;Y&gt; &lt;elif@example.com&gt;:"
        ),
        "{}",
        reply.html
    );
    let forward = f.engine.compose_reply(m, "forward", &german).unwrap();
    for part in [
        "Weitergeleitete Nachricht",
        "Von: Elif",
        "Datum: Do., 29. Okt. 2026",
        "Betreff: Plan",
        "An: alex@example.com",
    ] {
        assert!(forward.html.contains(part), "{part} in {}", forward.html);
    }
    // Without texts: English, as before.
    let english = f
        .engine
        .compose_reply(m, "reply", &QuoteText::default())
        .unwrap();
    assert!(english.html.contains(" wrote:</div>"), "{}", english.html);
    let forward = f
        .engine
        .compose_reply(m, "forward", &QuoteText::default())
        .unwrap();
    assert!(
        forward
            .html
            .contains("---------- Forwarded message ---------<br>From: ")
    );
    // Translations are text, not markup.
    let odd = QuoteText {
        wrote: Some("<b>{sender}</b> wrote:".into()),
        ..Default::default()
    };
    let reply = f.engine.compose_reply(m, "reply", &odd).unwrap();
    assert!(reply.html.contains("&lt;b&gt;Elif"), "{}", reply.html);
}

#[tokio::test]
async fn a_draft_edited_elsewhere_opens_as_the_server_has_it() {
    let f = Fixture::new();
    let data = DraftInput {
        account_id: f.account,
        subject: "Plan".into(),
        html: "<p>Written here</p>".into(),
        ..Default::default()
    };
    let id = f.engine.save_draft(None, &data, false).unwrap();
    let version = f.engine.draft(id).unwrap().updated_at;
    f.engine
        .db
        .write(|tx| misc::draft_uploaded(tx, id, Some(5), None, "plan@example.com", version))
        .unwrap();
    let copy = crate::compose::render(&mime(
        "alex@example.com",
        "x@example.com",
        "Plan",
        "<p>Written here</p>",
        "plan@example.com",
    ))
    .unwrap();
    let server = f.store(f.drafts, 5, &copy.mime, true);
    let draft = f.engine.draft_for_message(server).await.unwrap();
    assert_eq!(
        (draft.id, draft.data.html.as_str()),
        (id, "<p>Written here</p>")
    );

    // Changed in place on another device (Graph and Exchange drafts are).
    let (mut meta, _) = crate::mime::parse_full(&copy.mime).unwrap();
    meta.subject = "Plan, edited".into();
    f.engine
        .db
        .write(|tx| {
            mail::refresh_draft(tx, server, &meta)?;
            mail::store_body(
                tx,
                server,
                Some("<p>Edited on the phone</p>"),
                None,
                "",
                None,
            )
        })
        .unwrap();
    let draft = f.engine.draft_for_message(server).await.unwrap();
    assert_eq!(draft.id, id, "still the same draft, tied to the same copy");
    assert!(
        draft.data.html.contains("Edited on the phone"),
        "{}",
        draft.data.html
    );
    assert_eq!(draft.data.subject, "Plan, edited");
    assert!(!draft.local_only);
}

/// Records what the engine tells the interface.
#[derive(Clone, Default)]
struct Captured(std::sync::Arc<parking_lot::Mutex<Vec<EngineEvent>>>);

impl EventSink for Captured {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

#[tokio::test]
async fn new_mail_notifies_as_its_account_says() {
    let dir = std::env::temp_dir().join(format!("envelope-notify-{}", uuid::Uuid::new_v4()));
    let events = Captured::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .unwrap();
    let account = |notify: NotifyMode| {
        engine
            .db
            .write(|tx| {
                let id = mail::insert_account(
                    tx,
                    AccountKind::Imap,
                    "generic",
                    &format!("{notify:?}@example.com"),
                    "Alex Morgan",
                    "#000",
                    &AccountConfig {
                        notify,
                        ..AccountConfig::default()
                    },
                )?;
                tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [id])?;
                Ok(id)
            })
            .unwrap()
    };
    let thread = |id: i64, category: Option<&str>| ThreadSummary {
        id,
        account_id: 0,
        subject: format!("Thread {id}"),
        snippet: String::new(),
        last_date: 0,
        message_count: 1,
        unread_count: 1,
        flagged: false,
        has_attachments: false,
        draft_count: 0,
        participants: Vec::new(),
        last_sender: None,
        folder_ids: Vec::new(),
        category: category.map(str::to_string),
        is_invite: false,
        muted: false,
        snoozed_until: None,
        thread_id: None,
    };
    let arrived = || {
        vec![
            thread(1, Some("people")),
            thread(2, Some("newsletters")),
            thread(3, Some("notifications")),
            thread(4, None),
        ]
    };
    let notified = |account_id: i64| -> Option<Vec<i64>> {
        events.0.lock().iter().rev().find_map(|e| match e {
            EngineEvent::NewMail {
                account_id: a,
                threads,
            } if *a == account_id => Some(threads.iter().map(|t| t.id).collect()),
            _ => None,
        })
    };
    for (mode, expected) in [
        (NotifyMode::All, Some(vec![1, 2, 3, 4])),
        // Unsorted conversations count as people's.
        (NotifyMode::People, Some(vec![1, 4])),
        (NotifyMode::None, None),
    ] {
        let id = account(mode);
        engine.emit(EngineEvent::NewMail {
            account_id: id,
            threads: arrived(),
        });
        assert_eq!(notified(id), expected, "{mode:?}");
    }
    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
