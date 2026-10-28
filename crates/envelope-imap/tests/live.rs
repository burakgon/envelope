//! Live tests against the development server (`scripts/devserver/run.sh`).
//! Run with: ENVELOPE_TEST_IMAP=127.0.0.1:1993 cargo test -p envelope-imap --test live -- --nocapture

use std::time::Duration;

use envelope_imap::{Auth, Client, Config, IdleEnd, Security};

fn server() -> Option<(String, u16)> {
    let v = std::env::var("ENVELOPE_TEST_IMAP").ok()?;
    let (h, p) = v.rsplit_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

async fn connect(security: Security, port_override: Option<u16>) -> Option<Client> {
    let (host, port) = server()?;
    let mut cfg = Config::new(host, port_override.unwrap_or(port), security);
    cfg.accept_invalid_certs = true;
    let mut c = Client::connect(&cfg).await.expect("connect");
    c.authenticate(&Auth::Password {
        user: "alex@envelope.test".into(),
        password: "password".into(),
    })
    .await
    .expect("login");
    Some(c)
}

#[tokio::test]
async fn full_session() {
    let Some(mut c) = connect(Security::Tls, None).await else {
        eprintln!("ENVELOPE_TEST_IMAP not set; skipping");
        return;
    };
    println!("caps: {:?}", c.capabilities());
    let enabled = c.enable(&["QRESYNC", "UTF8=ACCEPT"]).await.expect("enable");
    println!("enabled: {enabled:?}");
    c.id("Envelope", "test").await.expect("id");

    let (list, statuses) = c.list(true).await.expect("list");
    for l in &list {
        println!(
            "mailbox {:?} attrs {:?} special {:?}",
            l.name,
            l.attributes,
            l.special_use()
        );
    }
    println!("statuses: {}", statuses.len());
    assert!(list.iter().any(|l| l.name == "INBOX"));
    assert!(list.iter().any(|l| l.special_use() == Some("\\Sent")));

    let sel = c.select("INBOX", None).await.expect("select");
    println!(
        "selected: exists={} uidvalidity={:?} uidnext={:?} modseq={:?}",
        sel.exists, sel.uid_validity, sel.uid_next, sel.highest_modseq
    );
    assert!(sel.exists > 50);

    let t = std::time::Instant::now();
    let items = c
        .uid_fetch(
            "1:*",
            "UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (DATE FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES LIST-UNSUBSCRIBE CONTENT-TYPE)]",
        )
        .await
        .expect("fetch");
    println!("fetched {} headers in {:?}", items.len(), t.elapsed());
    assert_eq!(items.len() as u32, sel.exists);
    let with_header = items.iter().filter(|i| i.header.is_some()).count();
    assert_eq!(with_header, items.len(), "every item has header data");
    let with_att = items
        .iter()
        .filter(|i| {
            i.bodystructure.as_ref().is_some_and(|b| {
                b.leaves()
                    .iter()
                    .any(|p| p.disposition.as_deref() == Some("attachment"))
            })
        })
        .count();
    println!("with attachments: {with_att}");
    assert!(with_att >= 2);

    // Full body of the newest message.
    let last = items.iter().map(|i| i.uid.unwrap()).max().unwrap();
    let body = c
        .uid_fetch(&last.to_string(), "UID BODY.PEEK[]")
        .await
        .expect("body");
    assert!(body[0].body.as_ref().is_some_and(|b| b.len() > 100));

    // Append + move + expunge round trip in a fresh mailbox.
    let _ = c.delete("Envelope Test/Ünïcödé").await;
    let _ = c.delete("Envelope Test").await;
    c.create("Envelope Test/Ünïcödé")
        .await
        .expect("create unicode mailbox");
    let raw = b"From: a@b.test\r\nTo: alex@envelope.test\r\nSubject: append test\r\n\r\nhello\r\n";
    let appended = c
        .append("Envelope Test/Ünïcödé", &["\\Seen"], None, raw)
        .await
        .expect("append");
    println!("appended: {appended:?}");
    let s2 = c
        .select("Envelope Test/Ünïcödé", None)
        .await
        .expect("select unicode");
    assert_eq!(s2.exists, 1);
    let uids = c.uid_search("ALL").await.expect("search");
    assert_eq!(uids.len(), 1);
    c.uid_store(&uids, "+FLAGS.SILENT", &["\\Flagged"])
        .await
        .expect("store");
    let f = c
        .uid_fetch(&uids[0].to_string(), "UID FLAGS")
        .await
        .expect("flags");
    assert!(f[0].has_flag("\\Flagged"));
    let moved = c.uid_move(&uids, "Trash").await.expect("move");
    println!("moved pairs: {moved:?}");
    c.select("INBOX", None).await.expect("reselect");
    c.delete("Envelope Test/Ünïcödé").await.expect("delete");
    let _ = c.delete("Envelope Test").await;

    // QRESYNC select with stale modseq returns changes.
    if c.is_enabled("QRESYNC") {
        let q = envelope_imap::QresyncParams {
            uid_validity: sel.uid_validity.unwrap(),
            modseq: 1,
            known_uids: None,
        };
        let s3 = c.select("INBOX", Some(&q)).await.expect("qresync select");
        println!(
            "qresync changed={} vanished={}",
            s3.changed.len(),
            s3.vanished.len()
        );
    }

    // IDLE: interrupt after a short time.
    let (end, _) = c
        .idle(
            Duration::from_secs(30),
            tokio::time::sleep(Duration::from_millis(300)),
        )
        .await
        .expect("idle");
    assert_eq!(end, IdleEnd::Interrupted);
    c.logout().await.unwrap();
}

#[tokio::test]
async fn starttls_and_idle_push() {
    let Some((_, _)) = server() else { return };
    let Some(mut a) = connect(Security::StartTls, Some(1143)).await else {
        return;
    };
    let Some(mut b) = connect(Security::Tls, None).await else {
        return;
    };
    a.select("INBOX", None).await.unwrap();
    let pusher = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        b.append(
            "INBOX",
            &[],
            None,
            b"From: x@y.test\r\nSubject: push\r\n\r\nhi\r\n",
        )
        .await
        .unwrap();
        b
    });
    let (end, untagged) = a
        .idle(Duration::from_secs(10), std::future::pending::<()>())
        .await
        .unwrap();
    println!("idle end {end:?} {untagged:?}");
    assert_eq!(end, IdleEnd::Changed);
    let mut b = pusher.await.unwrap();
    // clean up the pushed message
    b.select("INBOX", None).await.unwrap();
    let uids = b.uid_search("SUBJECT push").await.unwrap();
    b.uid_store(&uids, "+FLAGS.SILENT", &["\\Deleted"])
        .await
        .unwrap();
    b.uid_expunge(&uids).await.unwrap();
}
