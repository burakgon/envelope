//! Lists sorted oldest first or by sender, and lists of messages one by one
//! with the actions on single messages, against a real temporary database.

use std::collections::BTreeSet;

use super::mailbox_tests::{Fixture, fixture};
use super::*;
use crate::db::mail::MessageMeta;

/// Stores a message from someone with a name; returns (message id, thread id).
#[allow(clippy::too_many_arguments)]
fn from_named(
    fx: &Fixture,
    path: &str,
    uid: u32,
    message_id: &str,
    reply_to: Option<&str>,
    (name, email): (&str, &str),
    date: i64,
    flags: i64,
) -> (i64, i64) {
    let meta = MessageMeta {
        message_id: Some(message_id.into()),
        in_reply_to: reply_to.map(Into::into),
        references: reply_to.map(|r| vec![r.to_string()]).unwrap_or_default(),
        subject: "Plans".into(),
        from: Some(Address::new(Some(name.into()), email)),
        to: vec![Address::new(None, "me@example.com")],
        date,
        flags,
        ..Default::default()
    };
    let (account, folder) = (fx.account, fx.id(path));
    fx.engine
        .db
        .write(|tx| {
            let stored = mail::upsert_message(tx, account, folder, Some(uid), &meta)?;
            let touched = mail::refresh_threads(tx, &[stored.thread_id].into())?;
            mail::refresh_folder_counts(tx, &touched)?;
            Ok((stored.message_id, stored.thread_id))
        })
        .expect("message")
}

fn query(mailbox: Mailbox, sort: SortOrder, messages: bool) -> ThreadQuery {
    ThreadQuery {
        mailbox,
        search: None,
        filter: ListFilter::default(),
        sort,
        messages,
        offset: 0,
        limit: 100,
    }
}

/// The rows of a list, in order.
fn ids(fx: &Fixture, q: &ThreadQuery) -> Vec<i64> {
    fx.engine
        .threads(q)
        .expect("list")
        .items
        .into_iter()
        .map(|t| t.id)
        .collect()
}

#[tokio::test]
async fn conversations_sort_newest_oldest_and_by_whom_they_are_with() {
    let fx = fixture();
    let inbox = Mailbox::Folder { id: fx.id("INBOX") };
    let (_, zoe) = from_named(
        &fx,
        "INBOX",
        1,
        "a@x",
        None,
        ("Zoë Adams", "zoe@example.com"),
        1_000,
        0,
    );
    // Émile wrote first and the user answered last: it stays among the E's.
    let (_, emile) = from_named(
        &fx,
        "INBOX",
        2,
        "b@x",
        None,
        ("émile Brun", "emile@example.com"),
        2_000,
        0,
    );
    from_named(
        &fx,
        "INBOX",
        3,
        "b2@x",
        Some("b@x"),
        ("Me", "me@example.com"),
        5_000,
        Flags::SEEN,
    );
    // The user started it, Bob (no name) answered: it is with Bob.
    let (_, bob) = from_named(
        &fx,
        "INBOX",
        4,
        "c@x",
        None,
        ("Me", "me@example.com"),
        3_000,
        Flags::SEEN,
    );
    from_named(
        &fx,
        "INBOX",
        5,
        "c2@x",
        Some("c@x"),
        ("", "bob@example.com"),
        4_000,
        0,
    );

    for mailbox in [inbox, Mailbox::Everything] {
        assert_eq!(
            ids(&fx, &query(mailbox.clone(), SortOrder::Newest, false)),
            [emile, bob, zoe]
        );
        assert_eq!(
            ids(&fx, &query(mailbox.clone(), SortOrder::Oldest, false)),
            [zoe, bob, emile]
        );
        assert_eq!(
            ids(&fx, &query(mailbox.clone(), SortOrder::Sender, false)),
            [bob, emile, zoe],
            "{mailbox:?}"
        );
    }
}

#[tokio::test]
async fn messages_are_listed_one_by_one() {
    let fx = fixture();
    let inbox = Mailbox::Folder { id: fx.id("INBOX") };
    let (m1, t) = from_named(
        &fx,
        "INBOX",
        1,
        "a@x",
        None,
        ("Ann", "ann@example.com"),
        1_000,
        0,
    );
    let (m2, _) = from_named(
        &fx,
        "INBOX",
        2,
        "a2@x",
        Some("a@x"),
        ("Bob", "bob@example.com"),
        2_000,
        0,
    );
    let (m3, u) = from_named(
        &fx,
        "INBOX",
        3,
        "c@x",
        None,
        ("Carl", "carl@example.com"),
        1_500,
        Flags::FLAGGED,
    );
    // Elsewhere: not in the inbox's list.
    let (m4, _) = from_named(
        &fx,
        "Work",
        1,
        "w@x",
        None,
        ("Dora", "dora@example.com"),
        3_000,
        0,
    );

    let page = fx
        .engine
        .threads(&query(inbox.clone(), SortOrder::Newest, true))
        .expect("list");
    assert_eq!(page.total, 3);
    assert_eq!(
        page.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [m2, m3, m1]
    );
    // Each row is one message, and knows its conversation.
    let row = &page.items[0];
    assert_eq!(
        (row.thread_id, row.message_count, row.unread_count),
        (Some(t), 1, 1)
    );
    assert_eq!(
        row.last_sender.as_ref().map(|a| a.email.as_str()),
        Some("bob@example.com")
    );
    assert!(page.items[1].flagged && !page.items[0].flagged);
    assert_eq!(
        ids(&fx, &query(inbox.clone(), SortOrder::Oldest, true)),
        [m1, m3, m2]
    );
    assert_eq!(
        ids(&fx, &query(inbox.clone(), SortOrder::Sender, true)),
        [m1, m2, m3]
    );
    assert_eq!(
        ids(&fx, &query(Mailbox::Everything, SortOrder::Newest, true)),
        [m4, m2, m3, m1]
    );

    // Filters take messages, not conversations.
    fx.engine
        .set_flags(
            &[],
            &[m2],
            &FlagChange {
                seen: Some(true),
                flagged: None,
            },
        )
        .expect("read");
    let unread = ThreadQuery {
        filter: ListFilter {
            unread: true,
            ..Default::default()
        },
        ..query(inbox.clone(), SortOrder::Newest, true)
    };
    assert_eq!(ids(&fx, &unread), [m3, m1]);
    let starred = ThreadQuery {
        filter: ListFilter {
            starred: true,
            ..Default::default()
        },
        ..query(inbox.clone(), SortOrder::Newest, true)
    };
    assert_eq!(ids(&fx, &starred), [m3]);
    assert_eq!(
        ids(&fx, &query(Mailbox::Starred, SortOrder::Newest, true)),
        [m3]
    );
    let search = ThreadQuery {
        search: Some("from:carl".into()),
        ..query(Mailbox::Everything, SortOrder::Newest, true)
    };
    assert_eq!(ids(&fx, &search), [m3]);
    // VIPs: their messages alone, not the others of their conversations.
    fx.engine
        .set_vip("bob@example.com", Some("Bob"), true)
        .expect("vip");
    assert_eq!(
        ids(&fx, &query(Mailbox::Vip, SortOrder::Newest, true)),
        [m2]
    );
    assert_eq!(
        ids(&fx, &query(Mailbox::Vip, SortOrder::Newest, false)),
        [t]
    );

    // Opened, a row is that message alone, with its conversation beside it.
    let opened = fx.engine.message(m1).expect("message");
    assert_eq!((opened.thread.id, opened.thread.thread_id), (m1, Some(t)));
    assert_eq!(
        opened.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        [m1]
    );
    let conversation = opened.conversation.expect("conversation");
    assert_eq!((conversation.id, conversation.message_count), (t, 2));
    assert!(fx.engine.thread(t).expect("thread").conversation.is_none());

    // A snoozed conversation's messages leave the inbox with it.
    fx.engine.snooze(&[u], Some(i64::MAX)).expect("snooze");
    let page = fx
        .engine
        .threads(&query(inbox.clone(), SortOrder::Newest, true))
        .expect("list");
    assert_eq!(
        (
            page.items.iter().map(|r| r.id).collect::<Vec<_>>(),
            page.total
        ),
        (vec![m2, m1], 2)
    );
    assert_eq!(
        ids(&fx, &query(Mailbox::Snoozed, SortOrder::Newest, true)),
        [m3]
    );
    // The conversations around them are listed as before.
    assert_eq!(ids(&fx, &query(inbox, SortOrder::Newest, false)), [t]);
}

#[tokio::test]
async fn actions_on_one_message_leave_the_rest_of_its_conversation() {
    let fx = fixture();
    let inbox = fx.id("INBOX");
    let (m1, t) = from_named(
        &fx,
        "INBOX",
        1,
        "a@x",
        None,
        ("Ann", "ann@example.com"),
        1_000,
        0,
    );
    let (m2, _) = from_named(
        &fx,
        "INBOX",
        2,
        "a2@x",
        Some("a@x"),
        ("Bob", "bob@example.com"),
        2_000,
        0,
    );
    let messages = |mailbox: Mailbox| ids(&fx, &query(mailbox, SortOrder::Newest, true));

    // Archiving the newer message moves it alone.
    let token = fx
        .engine
        .move_messages(&[m2], Some(inbox), &MoveTarget::Archive)
        .expect("archive");
    assert_eq!(messages(Mailbox::Folder { id: inbox }), [m1]);
    assert_eq!(
        messages(Mailbox::Folder {
            id: fx.id("Archive")
        }),
        [m2]
    );
    assert_eq!(fx.list(Mailbox::Folder { id: inbox }), [t]);
    let op = fx.ops().into_iter().next();
    assert!(
        matches!(&op, Some(PendingOp::Move { dest_folder, items }) if dest_folder == "Archive" && items.len() == 1 && items[0].message_id == m2),
        "{op:?}"
    );
    // Undo puts it back.
    fx.engine.undo(&token).expect("undo");
    assert_eq!(messages(Mailbox::Folder { id: inbox }), [m2, m1]);
    assert!(fx.ops().is_empty(), "the move never reached the server");

    // Moving to a folder (a label on IMAP) and deleting forever take one message too.
    fx.engine
        .label_messages(&[m1], fx.id("Work"), true)
        .expect("label");
    assert_eq!(messages(Mailbox::Folder { id: fx.id("Work") }), [m1]);
    assert_eq!(messages(Mailbox::Folder { id: inbox }), [m2]);
    fx.engine
        .move_messages(&[m1], None, &MoveTarget::Trash)
        .expect("trash");
    fx.engine.delete_messages_forever(&[m1]).expect("delete");
    assert!(
        fx.engine
            .thread(t)
            .expect("thread")
            .messages
            .iter()
            .all(|m| m.id != m1)
    );
    assert_eq!(messages(Mailbox::Folder { id: inbox }), [m2]);

    // Mark all as read and select all take the messages the list shows.
    let (m3, _) = from_named(
        &fx,
        "Work",
        2,
        "a3@x",
        Some("a2@x"),
        ("Ann", "ann@example.com"),
        3_000,
        0,
    );
    let refs = fx
        .engine
        .thread_refs(
            &Mailbox::Folder { id: inbox },
            &ListFilter::default(),
            None,
            true,
        )
        .expect("refs");
    assert_eq!(
        refs,
        [ThreadRef {
            id: m2,
            account_id: fx.account
        }]
    );
    let marked = fx
        .engine
        .mark_read(
            &Mailbox::Folder { id: inbox },
            &ListFilter::default(),
            None,
            true,
        )
        .expect("mark");
    assert_eq!((marked.threads, marked.message_ids), (1, vec![m2]));
    let unread: BTreeSet<i64> = fx
        .engine
        .thread(t)
        .expect("thread")
        .messages
        .iter()
        .filter(|m| !m.flags.seen)
        .map(|m| m.id)
        .collect();
    assert_eq!(
        unread,
        BTreeSet::from([m3]),
        "the message in Work stays unread"
    );
}

#[test]
fn senders_sort_without_case_or_accents() {
    use crate::db::mail::sort_key;
    assert_eq!(sort_key(Some("Émile Zola"), Some("e@x")), "emile zola");
    assert_eq!(sort_key(Some(" \"Zoë\" "), None), "zoe");
    assert_eq!(sort_key(Some("İlker Şahin"), None), "ilker sahin");
    assert_eq!(sort_key(Some("Łukasz Żak"), None), "lukasz zak");
    assert_eq!(sort_key(Some("Æsa Straße"), None), "aesa strasse");
    assert_eq!(
        sort_key(Some(""), Some("Bob@Example.com")),
        "bob@example.com"
    );
    assert_eq!(sort_key(None, None), "");
}

#[tokio::test]
async fn folder_lists_come_in_order_from_an_index() {
    let fx = fixture();
    from_named(
        &fx,
        "INBOX",
        1,
        "a@x",
        None,
        ("Ann", "ann@example.com"),
        1_000,
        0,
    );
    // The inbox, with its snoozed conversations left out, and another folder.
    for mailbox in [
        Mailbox::Folder { id: fx.id("INBOX") },
        Mailbox::Folder { id: fx.id("Work") },
    ] {
        for sort in [SortOrder::Newest, SortOrder::Oldest, SortOrder::Sender] {
            for messages in [false, true] {
                let plan = fx
                    .engine
                    .db
                    .read(|c| {
                        let sql = mail::list_sql(
                            c,
                            &mailbox,
                            &ListFilter::default(),
                            None,
                            sort,
                            messages,
                        )?;
                        let mut values = sql.values.clone();
                        values.push(100.into());
                        values.push(0.into());
                        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {}", sql.page()))?;
                        let rows = st
                            .query_map(rusqlite::params_from_iter(values), |r| {
                                r.get::<_, String>(3)
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        Ok(rows.join("\n"))
                    })
                    .expect("plan");
                assert!(
                    !plan.contains("TEMP B-TREE FOR ORDER BY"),
                    "{mailbox:?} {sort:?} messages={messages}: sorted on every page\n{plan}"
                );
            }
        }
    }
}
