//! OpenPGP end to end against the development server
//! (scripts/devserver/run.sh): an encrypted and signed message with an
//! attachment goes out through SMTP, comes back through IMAP, and opens
//! decrypted and verified.
//!
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_pgp -- --nocapture

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_signed_mail_to_self() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        eprintln!("skipping: ENVELOPE_TEST_IMAP not set");
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let dir = std::env::temp_dir().join(format!("envelope-pgp-test-{}", uuid::Uuid::new_v4()));
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        Events::default(),
    )
    .unwrap();
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

    let key = engine
        .generate_key("alex@envelope.test", "Alex Morgan", None)
        .await
        .unwrap();
    let status = engine
        .compose_crypto(account.id, None, None, &["alex@envelope.test".into()])
        .unwrap();
    assert!(status.can_encrypt && status.can_sign, "{status:?}");

    let marker = uuid::Uuid::new_v4().simple().to_string();
    let file = dir.join("numbers.txt");
    std::fs::write(&file, format!("secret numbers {marker}\n")).unwrap();
    let subject = format!("Encrypted {marker}");
    let draft = DraftInput {
        account_id: account.id,
        to: vec![Address::new(Some("Alex".into()), "alex@envelope.test")],
        subject: subject.clone(),
        html: format!("<p>Gizli mesaj {marker} — görüşürüz.</p>"),
        attachments: vec![DraftAttachment {
            filename: "numbers.txt".into(),
            mime_type: "text/plain".into(),
            size: 40,
            path: Some(file.to_string_lossy().into_owned()),
            attachment_id: None,
            content_id: None,
            inline: false,
        }],
        mode: Some("new".into()),
        encrypt: Some(true),
        sign: Some(true),
        ..Default::default()
    };
    let t0 = Instant::now();
    engine.send(None, &draft, 0, None).await.expect("queue");

    // Wait for it to arrive: an encrypted message whose decrypted text
    // carries the marker.
    let inbox = ThreadQuery {
        mailbox: Mailbox::Unified {
            role: FolderRole::Inbox,
        },
        search: None,
        filter: ListFilter::default(),
        offset: 0,
        limit: 30,
    };
    let mut found = None;
    while found.is_none() && t0.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Ok(page) = engine.threads(&inbox) else {
            continue;
        };
        for t in page.items.iter().filter(|t| t.subject == "...") {
            let Ok(detail) = engine.thread(t.id) else {
                continue;
            };
            let ids: Vec<i64> = detail.messages.iter().map(|m| m.id).collect();
            let Ok(messages) = engine.load_bodies(ids).await else {
                continue;
            };
            for m in messages
                .iter()
                .filter(|m| m.crypto.as_deref() == Some("encrypted"))
            {
                if let Ok(opened) = engine.open_message(m.id).await
                    && opened
                        .body
                        .as_ref()
                        .and_then(|b| b.html.as_deref())
                        .is_some_and(|h| h.contains(&marker))
                {
                    found = Some((m.clone(), opened));
                }
            }
        }
    }
    let (message, opened) = found.expect("the encrypted message arrived and decrypted");
    println!("round trip in {:?}", t0.elapsed());

    // Stored and shown as the server has it: no readable content.
    assert_eq!(message.subject, "...");
    assert!(message.attachments.is_empty());
    let source = engine.message_source(message.id).await.unwrap();
    assert!(source.contains("multipart/encrypted"));
    assert!(source.contains("Autocrypt: addr=alex@envelope.test"));
    assert!(!source.contains(&marker), "nothing readable on the server");

    // Opened: decrypted, the real subject, a good signature by the sender.
    assert_eq!(opened.status, "ok");
    assert!(opened.encrypted);
    assert_eq!(opened.subject.as_deref(), Some(subject.as_str()));
    assert_eq!(opened.signatures.len(), 1);
    assert_eq!(opened.signatures[0].status, "good");
    assert!(opened.signatures[0].sender_matches);
    assert_eq!(
        opened.signatures[0].signer.as_ref().unwrap().fingerprint,
        key.fingerprint
    );
    let html = opened.body.as_ref().unwrap().html.as_deref().unwrap();
    assert!(html.contains("görüşürüz"));
    // The attachment is decrypted on demand.
    let att = opened
        .attachments
        .iter()
        .find(|a| a.filename == "numbers.txt")
        .expect("attachment inside the encryption");
    let path = engine.attachment_file(att.id).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        format!("secret numbers {marker}\n")
    );

    // A reply starts encrypted, quoting the decrypted text.
    let reply = engine.compose_reply(message.id, "reply").unwrap();
    assert_eq!(reply.encrypt, Some(true));
    assert_eq!(reply.subject, format!("Re: {subject}"));
    assert!(reply.html.contains(&marker));

    engine.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
