//! S/MIME through the whole pipeline: certificates imported and collected,
//! messages from the OpenSSL fixtures of envelope-smime "received" and
//! opened, and messages composed, rendered, received and opened again. The
//! engine trusts the fixtures' root in tests.

use envelope_smime::Certificate;
use envelope_smime::cms::Envelope;
use envelope_smime::mime::{self as smime_mime, Kind};

use super::tests::{Fixture, draft};
use crate::db::mail;
use crate::error::Error;
use crate::model::*;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../envelope-smime/tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn people() -> Fixture {
    Fixture::new(&[
        ("Alice Example", "alice@example.com"),
        ("Bob Example", "bob@example.org"),
    ])
}

fn verdict(opened: &super::OpenedMessage) -> String {
    opened.signatures[0].smime.as_ref().unwrap().verdict.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smime_mail_opens_through_the_pipeline() {
    // Alice's side only: Bob is someone else here.
    let f = Fixture::new(&[("Alice Example", "alice@example.com")]);
    let e = &f.engine;
    let mine = e
        .import_smime(&fixture("alice.p12"), Some("alice-pass"), None)
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);
    assert!(mine[0].secret && mine[0].can_sign && mine[0].can_encrypt);
    assert_eq!(mine[0].source, "own");
    assert_eq!(mine[0].emails, vec!["alice@example.com"]);
    assert!(matches!(
        e.import_smime(&fixture("alice.p12"), Some("nope"), None)
            .await,
        Err(Error::BadPassword)
    ));

    // Signed by Bob (P-256): shown as it is, the signature hidden, Bob's
    // certificate kept for encrypting to him.
    let id = f.deliver("alice@example.com", &fixture("signed-bob.eml"));
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.crypto.as_deref(), Some("smime-signed"));
    assert!(m.attachments.is_empty(), "smime.p7s is not an attachment");
    assert!(
        m.body
            .unwrap()
            .text
            .unwrap()
            .contains("the figures are below")
    );
    let bob = e.smime_certificates("bob").unwrap();
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0].source, "message");
    assert!(!bob[0].secret && bob[0].can_encrypt);
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok");
    assert_eq!(opened.kind, "smime-signed");
    let sig = &opened.signatures[0];
    assert_eq!(sig.status, "good");
    assert!(sig.sender_matches);
    let smime = sig.smime.as_ref().unwrap();
    assert_eq!(smime.verdict, "verified");
    assert_eq!(smime.certificate.as_ref().unwrap().name, "Bob Example");
    assert_eq!(
        smime.chain,
        vec!["Envelope Test Mail CA", "Envelope Test Root CA"]
    );
    assert_eq!(smime.algorithm, "ECDSA, SHA-256");

    // Encrypted to Alice, signed by Bob inside (streamed BER).
    let id = f.deliver("alice@example.com", &fixture("encrypted-alice.eml"));
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.crypto.as_deref(), Some("smime-encrypted"));
    assert_eq!(m.snippet, super::ENCRYPTED_SNIPPET);
    assert!(m.attachments.is_empty());
    let body = m.body.unwrap();
    assert!(body.html.is_none() && body.text.is_none());
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok", "{:?}", opened.error);
    assert!(opened.encrypted);
    assert!(
        opened
            .body
            .as_ref()
            .unwrap()
            .text
            .as_deref()
            .unwrap()
            .contains("Hello Alice")
    );
    assert_eq!(verdict(&opened), "verified");
    assert!(
        opened.attachments.is_empty(),
        "the inner signature is hidden"
    );

    // Outlook's encrypted form, with a PDF inside, decrypted on demand.
    let id = f.deliver("alice@example.com", &fixture("outlook-encrypted.eml"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok", "{:?}", opened.error);
    assert_eq!(verdict(&opened), "verified");
    assert_eq!(opened.attachments.len(), 1);
    let att = &opened.attachments[0];
    assert!(att.id < 0);
    assert_eq!(att.filename, "secret.pdf");
    let path = e.attachment_file(att.id).await.unwrap();
    assert!(std::fs::read(path).unwrap().starts_with(b"%PDF"));

    // AES-GCM.
    let id = f.deliver("alice@example.com", &fixture("authenc-alice.eml"));
    assert_eq!(e.open_message(id).await.unwrap().status, "ok");

    // Opaque signed: not secret, so its text is stored and searchable.
    let id = f.deliver("alice@example.com", &fixture("opaque-alice.eml"));
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.crypto.as_deref(), Some("smime-opaque"));
    assert!(m.snippet.contains("the figures are below"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(verdict(&opened), "verified");
    assert!(opened.body.unwrap().text.unwrap().contains("Q2: 47"));

    // Not encrypted to a certificate here: a notice, not an empty message.
    let id = f.deliver("alice@example.com", &fixture("encrypted-carol.eml"));
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "noKey");
    assert!(opened.error.unwrap().contains("certificate"));

    // Checks that fail are named, and nothing is learned from them.
    for (name, expected, reason) in [
        ("tampered.eml", "bad", Some("content")),
        ("bad-signature.eml", "bad", Some("signature")),
        ("expired.eml", "expired", None),
        ("untrusted.eml", "untrusted", Some("unknownIssuer")),
        ("mismatch.eml", "mismatch", None),
        ("not-for-email.eml", "notForEmail", None),
        ("constrained-bad.eml", "untrusted", Some("invalid")),
    ] {
        let id = f.deliver("alice@example.com", &fixture(name));
        let opened = e.open_message(id).await.unwrap();
        let smime = opened.signatures[0].smime.as_ref().unwrap();
        assert_eq!(smime.verdict, expected, "{name}");
        assert_eq!(smime.reason.as_deref(), reason, "{name}");
        if expected == "bad" {
            assert_eq!(opened.signatures[0].status, "bad");
        }
    }
    for who in ["mallory", "dave", "erin", "oscar"] {
        assert!(e.smime_certificates(who).unwrap().is_empty(), "{who}");
    }
    assert!(
        e.smime_certificates("eve").unwrap().is_empty(),
        "a mismatched sender gets nothing"
    );

    // Replying to Bob's encrypted message: encrypted, with S/MIME.
    let encrypted = f.deliver("alice@example.com", &fixture("encrypted-alice.eml"));
    e.open_message(encrypted).await.unwrap();
    let reply = e
        .compose_reply(encrypted, "reply", &QuoteText::default())
        .unwrap();
    assert_eq!(reply.encrypt, Some(true));
    assert!(reply.html.contains("Hello Alice"));
    let (alice, _) = f.account("alice@example.com");
    let status = e
        .compose_crypto(
            alice,
            None,
            None,
            &["bob@example.org".into()],
            Some(encrypted),
        )
        .unwrap();
    assert_eq!(status.standard, "smime");
    assert!(status.smime.can_encrypt && status.smime.can_sign);
    assert!(!status.can_encrypt, "no OpenPGP keys here");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smime_mail_is_signed_and_encrypted() {
    let f = people();
    let e = &f.engine;
    let (alice, _) = f.account("alice@example.com");
    e.import_smime(&fixture("alice.p12"), Some("alice-pass"), None)
        .await
        .unwrap();
    // Bob's certificate came with a .p12 for Bob's account (both people's
    // certificates live in this one engine).
    e.import_smime(&fixture("bob.p12"), Some("bob-pass"), None)
        .await
        .unwrap();

    // With a certificate and no OpenPGP key, S/MIME is the default.
    let status = e
        .compose_crypto(alice, None, None, &["bob@example.org".into()], None)
        .unwrap();
    assert_eq!(status.standard, "smime");
    assert!(status.smime.can_encrypt);
    assert_eq!(
        status.smime.recipients[0]
            .certificate
            .as_ref()
            .unwrap()
            .name,
        "Bob Example"
    );

    let file = f.dir.join("plan.txt");
    std::fs::write(&file, "Q1: 42\n").unwrap();
    let mut d = draft(
        alice,
        &["bob@example.org"],
        "Quarterly ☕",
        "<p>Merhaba Bob, görüşürüz.</p>",
    );
    d.encrypt = Some(true);
    d.sign = Some(true);
    d.attachments.push(DraftAttachment {
        filename: "plan.txt".into(),
        mime_type: "text/plain".into(),
        size: 7,
        path: Some(file.to_string_lossy().into_owned()),
        attachment_id: None,
        message_id: None,
        content_id: None,
        inline: false,
    });
    let raw = f.render(&d, false).await;
    let text = String::from_utf8_lossy(&raw);
    assert_eq!(smime_mime::detect(&raw), Some(Kind::Encrypted));
    assert!(!text.contains("Merhaba") && !text.contains("plan.txt"));
    assert!(!text.contains("Autocrypt:"), "no OpenPGP key was made");
    assert!(e.own_key_for("alice@example.com").unwrap().is_none());
    // Encrypted to Bob and to Alice, for her Sent copy.
    let envelope = Envelope::read(&smime_mime::payload(&raw).unwrap().cms).unwrap();
    let cert = |name: &str| {
        Certificate::parse_all(&fixture(&format!("{name}.pem")))
            .unwrap()
            .remove(0)
    };
    assert!(envelope.is_for(&cert("alice")) && envelope.is_for(&cert("bob")));

    let id = f.deliver("bob@example.org", &raw);
    let opened = e.open_message(id).await.unwrap();
    assert_eq!(opened.status, "ok", "{:?}", opened.error);
    assert!(
        opened
            .body
            .as_ref()
            .unwrap()
            .html
            .as_deref()
            .unwrap()
            .contains("görüşürüz")
    );
    assert_eq!(verdict(&opened), "verified");
    assert_eq!(opened.attachments.len(), 1);
    let path = e.attachment_file(opened.attachments[0].id).await.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "Q1: 42\n");
    // The subject is not hidden: S/MIME does not protect it.
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert_eq!(m.subject, "Quarterly ☕");

    // Signed only: readable everywhere, verified here.
    let mut d = draft(alice, &["bob@example.org"], "Signed", "<p>signed text</p>");
    d.sign = Some(true);
    let raw = f.render(&d, false).await;
    assert_eq!(smime_mime::detect(&raw), Some(Kind::Signed));
    let id = f.deliver("bob@example.org", &raw);
    let m = e.db.read(|c| mail::get_message(c, id)).unwrap();
    assert!(m.body.unwrap().html.unwrap().contains("signed text"));
    assert_eq!(verdict(&e.open_message(id).await.unwrap()), "verified");

    // A server copy of an encrypted draft is for Alice alone, unsigned.
    let mut d = draft(alice, &["bob@example.org"], "Draft", "<p>draft text</p>");
    d.encrypt = Some(true);
    d.sign = Some(true);
    let stored = f.render(&d, true).await;
    let envelope = Envelope::read(&smime_mime::payload(&stored).unwrap().cms).unwrap();
    assert!(envelope.is_for(&cert("alice")) && !envelope.is_for(&cert("bob")));
    let inner = envelope
        .decrypt(
            &cert("alice"),
            &e.load_smime_key(&cert("alice").fingerprint())
                .await
                .unwrap(),
        )
        .unwrap();
    assert_eq!(smime_mime::detect(&inner), None, "not signed");

    // No certificate for a recipient: encryption is refused, saying whose.
    let mut d = draft(alice, &["carol@example.net"], "No", "<p>x</p>");
    d.encrypt = Some(true);
    d.smime = Some(true);
    let err = crate::sync::render_draft(e, &d, false).await.unwrap_err();
    assert!(err.to_string().contains("carol@example.net"), "{err}");

    // An OpenPGP key for both: OpenPGP again, unless replying to S/MIME.
    e.generate_key("alice@example.com", "Alice", None)
        .await
        .unwrap();
    e.generate_key("bob@example.org", "Bob", None)
        .await
        .unwrap();
    let status = e
        .compose_crypto(alice, None, None, &["bob@example.org".into()], None)
        .unwrap();
    assert_eq!(status.standard, "openpgp");
    let status = e
        .compose_crypto(alice, None, None, &["bob@example.org".into()], Some(id))
        .unwrap();
    assert_eq!(status.standard, "smime");

    // Export and delete.
    let fp = cert("alice").fingerprint();
    assert!(
        e.export_smime_certificate(&fp)
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert_eq!(e.smime_certificate_der(&fp).unwrap(), cert("alice").der());
    e.delete_smime_certificate(&fp).await.unwrap();
    assert!(
        e.load_smime_key(&fp).await.is_err(),
        "removed from the keyring"
    );
    assert!(!e.has_own_certificate("alice@example.com").unwrap());
}

#[tokio::test]
async fn others_certificates_are_imported_and_files_open() {
    let f = people();
    let e = &f.engine;
    // A correspondent's certificate, with its authority's.
    let mut pem = fixture("carol.pem");
    pem.extend(fixture("mail-ca.pem"));
    let imported = e.import_smime(&pem, None, None).await.unwrap();
    assert_eq!(imported.len(), 1, "the authority is not listed");
    assert_eq!(imported[0].source, "manual");
    assert_eq!(imported[0].algorithm, "EC P-384");
    let (alice, _) = f.account("alice@example.com");
    e.import_smime(&fixture("alice-legacy.p12"), Some("alice-pass"), None)
        .await
        .unwrap();
    let status = e
        .compose_crypto(alice, None, None, &["carol@example.net".into()], None)
        .unwrap();
    assert!(status.smime.can_encrypt);
    assert!(e.import_smime(b"nothing here", None, None).await.is_err());

    // An encrypted message saved as a file opens decrypted.
    let saved = f.dir.join("secret.eml");
    std::fs::write(&saved, fixture("outlook-encrypted.eml")).unwrap();
    let crate::files::OpenedFile::Message(m) = e.open_file(&saved).await.unwrap() else {
        panic!("not a message");
    };
    assert!(m.encrypted);
    assert!(m.text.unwrap_or_default().contains("encrypted report"));
    assert_eq!(m.attachments.len(), 1);
    assert_eq!(m.attachments[0].filename, "secret.pdf");
}
