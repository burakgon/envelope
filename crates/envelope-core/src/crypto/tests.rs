//! The OpenPGP pipeline end to end inside one engine: keys, composing,
//! rendering, "receiving" the rendered message, opening it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use envelope_pgp::ops;
use envelope_pgp::pgpmime::{self, Kind};
use parking_lot::Mutex;

use super::CryptoSettings;
use crate::db::crypto as store;
use crate::db::mail::{self, FolderUpsert};
use crate::engine::{Engine, EventSink, Paths};
use crate::model::*;

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().push(e);
    }
}

struct Fixture {
    engine: Engine,
    dir: PathBuf,
    /// (account id, inbox folder id) per address.
    accounts: Vec<(String, i64, i64)>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(addresses: &[(&str, &str)]) -> Fixture {
        // Secrets go to a file in the test directory, not the desktop keyring.
        unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
        let dir = std::env::temp_dir().join(format!("envelope-pgp-{}", uuid::Uuid::new_v4()));
        let engine = Engine::start(
            Paths {
                data: dir.join("data"),
                cache: dir.join("cache"),
            },
            Events::default(),
        )
        .unwrap();
        let mut accounts = Vec::new();
        for (name, email) in addresses {
            let (account, inbox) = engine
                .db
                .write(|tx| {
                    let account = mail::insert_account(
                        tx,
                        AccountKind::Imap,
                        "generic",
                        email,
                        name,
                        "#000",
                        &AccountConfig::default(),
                    )?;
                    let folders = mail::sync_folders(
                        tx,
                        account,
                        &[FolderUpsert {
                            remote_id: "INBOX".into(),
                            name: "Inbox".into(),
                            path: "INBOX".into(),
                            parent_remote_id: None,
                            role: Some(FolderRole::Inbox),
                            kind: "folder".into(),
                            delimiter: None,
                            selectable: true,
                            sort_order: 0,
                            color: None,
                        }],
                    )?;
                    Ok((account, folders["INBOX"]))
                })
                .unwrap();
            accounts.push((email.to_string(), account, inbox));
        }
        Fixture {
            engine,
            dir,
            accounts,
        }
    }

    fn account(&self, email: &str) -> (i64, i64) {
        let a = self.accounts.iter().find(|a| a.0 == email).unwrap();
        (a.1, a.2)
    }

    /// Delivers a raw message into the inbox of `to`, as body sync would.
    fn deliver(&self, to: &str, raw: &[u8]) -> i64 {
        let (account, inbox) = self.account(to);
        let raw = raw.to_vec();
        self.engine
            .db
            .write(|tx| {
                let (meta, mut body) = crate::mime::parse_full(&raw).unwrap();
                let uid: u32 =
                    tx.query_row("SELECT count(*) + 1 FROM messages", [], |r| r.get(0))?;
                let stored = mail::upsert_message(tx, account, inbox, Some(uid), &meta)?;
                super::on_source(tx, stored.message_id, &raw, &meta, &mut body)?;
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

    async fn render(&self, draft: &DraftInput, for_draft: bool) -> Vec<u8> {
        crate::sync::render_draft(&self.engine, draft, for_draft)
            .await
            .unwrap()
            .mime
    }
}

fn draft(from_account: i64, to: &[&str], subject: &str, html: &str) -> DraftInput {
    DraftInput {
        account_id: from_account,
        to: to.iter().map(|e| Address::new(None, *e)).collect(),
        subject: subject.into(),
        html: html.into(),
        mode: Some("new".into()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_and_signed_mail_round_trip() {
    let f = Fixture::new(&[
        ("Alex Morgan", "alex@envelope.test"),
        ("Elif Yılmaz", "elif@example.com"),
    ]);
    let e = &f.engine;
    let (alex, _) = f.account("alex@envelope.test");
    let alex_key = e
        .generate_key("alex@envelope.test", "Alex Morgan", None)
        .await
        .unwrap();
    let elif_key = e
        .generate_key("elif@example.com", "Elif Yılmaz", None)
        .await
        .unwrap();
    assert!(alex_key.can_encrypt && alex_key.can_sign && alex_key.secret);
    assert_eq!(alex_key.source, "own");

    // The composer sees keys for both sides.
    let status = e
        .compose_crypto(alex, None, None, &["Elif@Example.com".into()])
        .unwrap();
    assert!(status.can_encrypt && status.can_sign && !status.own_locked);
    assert_eq!(
        status.recipients[0].key.as_ref().unwrap().fingerprint,
        elif_key.fingerprint
    );
    assert!(status.missing.is_empty());

    // An attachment travels inside the encryption.
    let file = f.dir.join("plan.txt");
    std::fs::write(&file, "Q1: 42\n").unwrap();
    let mut d = draft(
        alex,
        &["elif@example.com"],
        "Gizli plan ☕",
        "<p>Merhaba Elif, sır aramızda.</p>",
    );
    d.encrypt = Some(true);
    d.sign = Some(true);
    d.attachments.push(DraftAttachment {
        filename: "plan.txt".into(),
        mime_type: "text/plain".into(),
        size: 7,
        path: Some(file.to_string_lossy().into_owned()),
        attachment_id: None,
        content_id: None,
        inline: false,
    });
    let raw = f.render(&d, false).await;
    let text = String::from_utf8_lossy(&raw);
    assert_eq!(pgpmime::detect(&raw), Some(Kind::Encrypted));
    assert!(text.contains("Subject: ...\r\n"), "subject hidden");
    assert!(!text.contains("Merhaba") && !text.contains("plan.txt"));
    assert!(text.contains("Autocrypt: addr=alex@envelope.test;"));
    // Encrypted to Elif and to Alex (for the Sent copy).
    let ids = ops::recipients(&pgpmime::encrypted_payload(&raw).unwrap()).unwrap();
    let key_ids = |fp: &str| e.key_row(fp).unwrap().key_ids;
    assert!(
        ids.iter()
            .any(|id| key_ids(&elif_key.fingerprint).contains(id))
    );
    assert!(
        ids.iter()
            .any(|id| key_ids(&alex_key.fingerprint).contains(id))
    );

    // Elif receives it: nothing readable is stored, until it is opened.
    let id = f.deliver("elif@example.com", &raw);
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.crypto.as_deref(), Some("encrypted"));
    assert_eq!(m.snippet, super::ENCRYPTED_SNIPPET);
    assert!(m.attachments.is_empty());
    let body = m.body.unwrap();
    assert!(body.html.is_none() && body.text.is_none());

    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok", "{:?}", opened.error);
    assert!(opened.encrypted);
    assert_eq!(opened.subject.as_deref(), Some("Gizli plan ☕"));
    let shown = opened.body.as_ref().unwrap();
    assert!(shown.html.as_deref().unwrap().contains("sır aramızda"));
    assert_eq!(opened.signatures.len(), 1);
    let sig = &opened.signatures[0];
    assert_eq!(sig.status, "good");
    assert!(sig.sender_matches);
    assert_eq!(
        sig.signer.as_ref().unwrap().fingerprint,
        alex_key.fingerprint
    );
    // The attachment is decrypted on demand.
    assert_eq!(opened.attachments.len(), 1);
    let att = &opened.attachments[0];
    assert!(att.id < 0);
    assert_eq!(att.filename, "plan.txt");
    let path = e.attachment_file(att.id).await.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "Q1: 42\n");

    // Nothing decrypted reached the database or the search index.
    let found = e
        .threads(&ThreadQuery {
            mailbox: Mailbox::Everything,
            search: Some("aramızda".into()),
            filter: ListFilter::default(),
            offset: 0,
            limit: 10,
        })
        .unwrap();
    assert_eq!(found.total, 0);

    // Replying quotes the decrypted text, keeps the real subject and
    // encrypts again.
    let reply = e.compose_reply(id, "reply").unwrap();
    assert_eq!(reply.encrypt, Some(true));
    assert_eq!(reply.subject, "Re: Gizli plan ☕");
    assert!(reply.html.contains("sır aramızda"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passphrases_bcc_drafts_and_kept_copies() {
    let f = Fixture::new(&[
        ("Alex", "alex@envelope.test"),
        ("Bob", "bob@example.com"),
        ("Carol", "carol@example.com"),
    ]);
    let e = &f.engine;
    let (alex, _) = f.account("alex@envelope.test");
    e.generate_key("alex@envelope.test", "Alex", None)
        .await
        .unwrap();
    let bob = e
        .generate_key("bob@example.com", "Bob", Some("bob pass"))
        .await
        .unwrap();
    let carol = e
        .generate_key("carol@example.com", "Carol", None)
        .await
        .unwrap();
    assert!(bob.protected);

    // Bob to Carol, with Alex as a blind copy.
    let mut d = draft(
        alex,
        &["bob@example.com"],
        "Hidden",
        "<p>to bob, carol in bcc</p>",
    );
    d.bcc = vec![Address::new(None, "carol@example.com")];
    d.encrypt = Some(true);
    let raw = f.render(&d, false).await;
    let ids = ops::recipients(&pgpmime::encrypted_payload(&raw).unwrap()).unwrap();
    let carol_ids = e.key_row(&carol.fingerprint).unwrap().key_ids;
    assert!(
        ids.iter().all(|id| !carol_ids.contains(id)),
        "Bcc not named"
    );
    assert!(!String::from_utf8_lossy(&raw).contains("carol@"));
    // Carol's key opens it all the same.
    let carol_secret = e.load_secret(&carol.fingerprint).await.unwrap();
    let payload = pgpmime::encrypted_payload(&raw).unwrap();
    assert!(ops::decrypt(&payload, &[(&carol_secret, None)], &|_| Vec::new()).is_ok());

    // Bob's key is protected: a message only for him asks for the
    // passphrase (all three keys live in this one engine, so it must not be
    // encrypted to the others).
    let bob_cert = super::keys::public_of(&e.key_row(&bob.fingerprint).unwrap()).unwrap();
    let raw_bob = pgpmime::protect(
        b"From: alex@envelope.test\r\nTo: bob@example.com\r\nSubject: x\r\n\r\nonly bob\r\n",
        &pgpmime::Protect {
            encrypt_to: vec![&bob_cert],
            ..Default::default()
        },
    )
    .unwrap();
    let id = f.deliver("bob@example.com", &raw_bob);
    // The passphrase typed when the key was made is remembered; forget it.
    e.crypto_lock();
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "locked");
    assert_eq!(
        opened.locked_key.as_ref().unwrap().fingerprint,
        bob.fingerprint
    );
    assert!(e.unlock_key(&bob.fingerprint, "wrong").await.is_err());
    e.unlock_key(&bob.fingerprint, "bob pass").await.unwrap();
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok");
    assert!(opened.body.unwrap().text.unwrap().contains("only bob"));

    // A server copy of an encrypted draft is encrypted to the sender only.
    let mut draft_copy = draft(alex, &["bob@example.com"], "Draft", "<p>draft text</p>");
    draft_copy.encrypt = Some(true);
    let stored = f.render(&draft_copy, true).await;
    let text = String::from_utf8_lossy(&stored);
    assert!(!text.contains("draft text") && !text.contains("Autocrypt:"));
    let ids = ops::recipients(&pgpmime::encrypted_payload(&stored).unwrap()).unwrap();
    let alex_row = e.own_key_for("alex@envelope.test").unwrap().unwrap();
    assert_eq!(ids.len(), 1);
    assert!(alex_row.key_ids.contains(&ids[0]));

    // With decrypted copies kept, encrypted mail becomes searchable.
    e.set_crypto_settings(&CryptoSettings {
        keep_decrypted: true,
        ..Default::default()
    })
    .unwrap();
    let mut d = draft(alex, &["carol@example.com"], "Searchable", "<p>kumquat</p>");
    d.encrypt = Some(true);
    let raw = f.render(&d, false).await;
    let id = f.deliver("carol@example.com", &raw);
    e.open_message(id).await.unwrap();
    let search = |q: &str| {
        e.threads(&ThreadQuery {
            mailbox: Mailbox::Everything,
            search: Some(q.into()),
            filter: ListFilter::default(),
            offset: 0,
            limit: 10,
        })
        .unwrap()
    };
    assert_eq!(search("kumquat").total, 1);
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.subject, "Searchable");
    // Turning the setting off forgets them again.
    e.set_crypto_settings(&CryptoSettings::default()).unwrap();
    assert_eq!(search("kumquat").total, 0);
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.subject, pgpmime::HIDDEN_SUBJECT);
    assert_eq!(m.snippet, super::ENCRYPTED_SNIPPET);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autocrypt_signatures_and_key_management() {
    let f = Fixture::new(&[("Alex", "alex@envelope.test"), ("Dana", "dana@example.org")]);
    let e = &f.engine;
    let (alex, _) = f.account("alex@envelope.test");
    let (dana, _) = f.account("dana@example.org");
    e.generate_key("alex@envelope.test", "Alex", None)
        .await
        .unwrap();

    // Plain mail from Dana carries her key (created on first send) in an
    // Autocrypt header, signed as asked.
    let mut d = draft(dana, &["alex@envelope.test"], "Hello", "<p>hi Alex</p>");
    d.sign = Some(true);
    let raw = f.render(&d, false).await;
    assert_eq!(pgpmime::detect(&raw), Some(Kind::Signed));
    assert!(String::from_utf8_lossy(&raw).contains("Autocrypt: addr=dana@example.org;"));
    let dana_fp = e
        .own_key_for("dana@example.org")
        .unwrap()
        .unwrap()
        .fingerprint;

    // Alex's side: Dana's key is someone else's key there. Simulate a
    // separate engine by forgetting that it is Dana's own key.
    e.db.write(|tx| {
        tx.execute("DELETE FROM pgp_own WHERE email = 'dana@example.org'", [])?;
        tx.execute("DELETE FROM pgp_keys WHERE fingerprint = ?", [&dana_fp])?;
        tx.execute(
            "DELETE FROM identities WHERE email = 'dana@example.org'",
            [],
        )?;
        tx.execute(
            "UPDATE accounts SET email = 'dana-old@example.org' WHERE id = ?",
            [dana],
        )?;
        Ok(())
    })
    .unwrap();
    let id = f.deliver("alex@envelope.test", &raw);
    let peer =
        e.db.read(|c| store::peer_get(c, "dana@example.org"))
            .unwrap()
            .unwrap();
    assert_eq!(peer.fingerprint.as_deref(), Some(dana_fp.as_str()));
    let key = e.key_row(&dana_fp).unwrap();
    assert_eq!(key.source, "autocrypt");
    assert!(!key.secret);

    // The signature checks out against the Autocrypt key.
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.crypto.as_deref(), Some("signed"));
    assert!(m.body.unwrap().html.unwrap().contains("hi Alex"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.signatures[0].status, "good");
    assert!(opened.signatures[0].sender_matches);
    assert!(!opened.signatures[0].signer.as_ref().unwrap().verified);

    // Alex can now encrypt to Dana.
    let status = e
        .compose_crypto(alex, None, None, &["dana@example.org".into()])
        .unwrap();
    assert!(status.can_encrypt);
    assert!(!status.mutual, "Dana did not ask for encryption");

    // Verifying by fingerprint.
    e.set_key_verified(&dana_fp, true).unwrap();
    assert!(e.keys("dana").unwrap()[0].verified);

    // Export and import: the public key, and Alex's secret key protected
    // for the export.
    let public = e.export_public_key(&dana_fp).unwrap();
    assert!(public.starts_with("-----BEGIN PGP PUBLIC KEY BLOCK-----"));
    let alex_fp = e
        .own_key_for("alex@envelope.test")
        .unwrap()
        .unwrap()
        .fingerprint;
    let secret = e
        .export_secret_key(&alex_fp, Some("export pass"))
        .await
        .unwrap();
    assert!(secret.starts_with("-----BEGIN PGP PRIVATE KEY BLOCK-----"));
    e.delete_key(&alex_fp).await.unwrap();
    assert!(e.own_key_for("alex@envelope.test").unwrap().is_none());
    assert!(
        e.load_secret(&alex_fp).await.is_err(),
        "removed from the keyring"
    );
    assert!(
        e.import_keys(secret.as_bytes(), Some("nope"), None)
            .await
            .is_err()
    );
    let imported = e
        .import_keys(secret.as_bytes(), Some("export pass"), None)
        .await
        .unwrap();
    assert_eq!(imported[0].fingerprint, alex_fp);
    assert!(imported[0].protected && imported[0].secret);
    // It is Alex's key again, already unlocked for this session.
    let own = e.own_keys().unwrap();
    let alex_own = own
        .iter()
        .find(|o| o.email == "alex@envelope.test")
        .unwrap();
    assert_eq!(alex_own.key.as_ref().unwrap().fingerprint, alex_fp);
    assert!(alex_own.unlocked);

    // A tampered signed message is reported as bad.
    let tampered = String::from_utf8_lossy(&raw).replace("hi Alex", "hi Eve!");
    let id = f.deliver("alex@envelope.test", tampered.as_bytes());
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.signatures[0].status, "bad");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gnupg_messages_open_with_imported_keys() {
    let fixture = |name: &str| {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../envelope-pgp/tests/fixtures")
                .join(name),
        )
        .unwrap()
    };
    let f = Fixture::new(&[("Rita", "rita@rsa.example")]);
    let e = &f.engine;
    let rita = e
        .import_keys(&fixture("rita.sec.asc"), None, None)
        .await
        .unwrap();
    assert!(rita[0].protected && rita[0].secret);
    assert_eq!(rita[0].algorithm, "RSA 3072");
    let gina = e
        .import_keys(&fixture("gina.pub.asc"), None, None)
        .await
        .unwrap();
    assert_eq!(gina[0].source, "manual");
    assert!(!gina[0].secret);

    let id = f.deliver("rita@rsa.example", &fixture("tb-encrypted-signed.eml"));
    assert_eq!(e.open_message(id).await.unwrap().status, "locked");
    e.unlock_key(&rita[0].fingerprint, "rita-pass")
        .await
        .unwrap();
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok");
    assert_eq!(opened.subject.as_deref(), Some("Quarterly numbers ☕"));
    let text = opened.body.unwrap().text.unwrap();
    assert!(
        text.starts_with("Hi Rita,"),
        "legacy display part hidden: {text}"
    );
    assert_eq!(opened.signatures[0].status, "good");
    assert!(opened.signatures[0].sender_matches);
    assert_eq!(opened.attachments.len(), 1);
    let path = e.attachment_file(opened.attachments[0].id).await.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "Q1: 42\nQ2: 47\n");

    let id = f.deliver("rita@rsa.example", &fixture("enigmail-signed.eml"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.kind, "signed");
    assert_eq!(opened.signatures[0].status, "good");
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert!(
        m.attachments.is_empty(),
        "signature.asc is not an attachment"
    );

    let id = f.deliver("rita@rsa.example", &fixture("inline-encrypted.eml"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(
        opened.body.unwrap().text.unwrap().trim(),
        "Inline secret: şifre 1234"
    );

    let id = f.deliver("rita@rsa.example", &fixture("clearsigned.eml"));
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert!(
        !m.body.unwrap().text.unwrap().contains("BEGIN PGP"),
        "armor removed"
    );
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.signatures[0].status, "good");

    let id = f.deliver("rita@rsa.example", &fixture("nested-signed-encrypted.eml"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok");
    assert_eq!(opened.signatures[0].status, "good");
    assert!(opened.body.unwrap().text.unwrap().contains("signed first"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_found_in_directories() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let f = Fixture::new(&[("Alex", "alex@envelope.test")]);
    let e = &f.engine;
    let zoe = envelope_pgp::cert::generate("Zoe", "zoe@example.net", None).unwrap();
    let other = envelope_pgp::cert::generate("Other", "other@example.net", None).unwrap();
    let mut both = envelope_pgp::cert::public_bytes(&zoe.to_public_key()).unwrap();
    both.extend(envelope_pgp::cert::public_bytes(&other.to_public_key()).unwrap());

    // A Web Key Directory answering with the key (and one it should not).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/openpgpkey/hu/zoe"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(both.clone()))
        .mount(&server)
        .await;
    let client = reqwest::Client::new();
    let data = super::keys::fetch_key(
        &client,
        &format!("{}/.well-known/openpgpkey/hu/zoe", server.uri()),
    )
    .await
    .expect("served");
    assert!(
        super::keys::fetch_key(&client, &format!("{}/missing", server.uri()))
            .await
            .is_none()
    );

    // Only Zoe's key is taken for Zoe's address.
    let found = e.take_found_keys("zoe@example.net", &data, "wkd").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].emails, vec!["zoe@example.net"]);
    assert_eq!(found[0].source, "wkd");
    assert!(e.keys("other@example.net").unwrap().is_empty());
    let (alex, _) = f.account("alex@envelope.test");
    e.generate_key("alex@envelope.test", "Alex", None)
        .await
        .unwrap();
    let status = e
        .compose_crypto(
            alex,
            None,
            None,
            &["zoe@example.net".into(), "nobody@example.net".into()],
        )
        .unwrap();
    assert!(!status.can_encrypt);
    assert_eq!(status.missing, vec!["nobody@example.net"]);
    assert_eq!(status.recipients[0].key.as_ref().unwrap().source, "wkd");

    // Automatic lookups ask each address at most once an hour.
    assert!(e.crypto.may_look_up("nobody@example.net", 1_000));
    assert!(!e.crypto.may_look_up("nobody@example.net", 2_000));
    assert!(
        e.crypto
            .may_look_up("nobody@example.net", 1_000 + 3_600_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_and_imported_keys_win() {
    let f = Fixture::new(&[("Alex", "alex@envelope.test")]);
    let e = &f.engine;
    let now = crate::db::now_ms();
    let a = envelope_pgp::cert::generate("Kim", "kim@example.org", None).unwrap();
    let b = envelope_pgp::cert::generate("Kim", "kim@example.org", None).unwrap();
    let c = envelope_pgp::cert::generate("Kim", "kim@example.org", None).unwrap();
    let keep = |k: &envelope_pgp::pgp::composed::SignedSecretKey, source: &str| {
        let cert = envelope_pgp::Cert::from_public(k.to_public_key());
        e.db.write(|tx| super::keys::store_cert(tx, &cert, source, &[]))
            .unwrap()
            .fingerprint
    };
    let fa = keep(&a, "autocrypt");
    let fb = keep(&b, "wkd");
    let pick = || {
        e.db.read(|c| super::outgoing::select_key(c, "kim@example.org", now))
            .unwrap()
            .unwrap()
            .fingerprint
    };
    // A key from the person's domain beats an Autocrypt key that is not
    // their current one; their current Autocrypt key beats it.
    assert_eq!(pick(), fb);
    e.db.write(|tx| {
        store::peer_save(
            tx,
            &store::Peer {
                email: "kim@example.org".into(),
                fingerprint: Some(fa.clone()),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert_eq!(pick(), fa);
    // A key the user imported beats that, and a verified one beats all.
    let fc = keep(&c, "manual");
    assert_eq!(pick(), fc);
    e.set_key_verified(&fb, true).unwrap();
    assert_eq!(pick(), fb);
}
