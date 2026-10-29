//! Interoperability with certificates, PKCS #12 files and messages made by
//! OpenSSL, shaped like Outlook's and Thunderbird's (see
//! `fixtures/generate.py`), and checks that fail as they should.

use std::path::{Path, PathBuf};
use std::process::Command;

use envelope_smime::cms::{self, Envelope, SignWith, Signed, Verifier};
use envelope_smime::mime::{self, Kind, Protect};
use envelope_smime::{Certificate, Error, Integrity, PrivateKey, Trust, TrustStore, Verdict};

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn cert(name: &str) -> Certificate {
    Certificate::parse_all(&fixture(&format!("{name}.pem")))
        .unwrap()
        .remove(0)
}

fn key(name: &str) -> PrivateKey {
    PrivateKey::from_pem(&String::from_utf8(fixture(&format!("{name}.key"))).unwrap()).unwrap()
}

fn trust() -> TrustStore {
    TrustStore::new([cert("root").der().to_vec()])
}

/// Checks a signed fixture as the reader does, for the address in From.
fn verify(name: &str, sender: &str, trust: &TrustStore, known: &[Certificate]) -> Signed {
    let raw = fixture(name);
    let v = Verifier {
        trust,
        known,
        sender: Some(sender),
        now: envelope_smime::now(),
    };
    match mime::detect(&raw) {
        Some(Kind::Signed) => {
            let parts = mime::signed_parts(&raw).unwrap();
            cms::verify_detached(&parts.data, &parts.signature, &v).unwrap()
        }
        Some(Kind::Opaque) => cms::verify_opaque(&mime::payload(&raw).unwrap().cms, &v).unwrap(),
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn reads_certificates() {
    let alice = cert("alice");
    assert_eq!(alice.name(), "Alice Example");
    assert_eq!(alice.emails(), ["alice@example.com"]);
    assert_eq!(alice.issuer_name(), "Envelope Test Mail CA");
    assert_eq!(alice.algorithm(), "RSA 2048");
    assert!(alice.can_sign_mail() && alice.can_encrypt_mail() && !alice.is_ca());
    assert_eq!(alice.fingerprint().len(), 64);
    assert!(alice.subject().contains("CN=Alice Example"));
    let bob = cert("bob");
    assert_eq!(bob.algorithm(), "EC P-256");
    assert!(bob.can_sign_mail() && bob.can_encrypt_mail());
    assert_eq!(cert("carol").algorithm(), "EC P-384");
    // An address in the subject only, as older certificates have it.
    let dave = cert("dave");
    assert_eq!(dave.emails(), ["dave@example.com"]);
    assert_eq!(
        dave.validity_at(envelope_smime::now()),
        envelope_smime::Validity::Expired
    );
    assert!(!cert("erin").can_sign_mail(), "a TLS server certificate");
    assert!(cert("mail-ca").is_ca());
    // A PEM chain, and the same certificate as DER.
    let mut both = fixture("alice.pem");
    both.extend(fixture("mail-ca.pem"));
    assert_eq!(Certificate::parse_all(&both).unwrap().len(), 2);
    assert_eq!(
        Certificate::parse_all(alice.der()).unwrap()[0].fingerprint(),
        alice.fingerprint()
    );
    assert!(alice.to_pem().starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(Certificate::parse_all(b"not a certificate").is_err());
}

#[test]
fn reads_pkcs12_files() {
    for (file, password, name) in [
        ("alice.p12", "alice-pass", "alice"),
        ("alice-legacy.p12", "alice-pass", "alice"),
        ("bob.p12", "bob-pass", "bob"),
    ] {
        let data = fixture(file);
        assert!(envelope_smime::pkcs12::is_pkcs12(&data));
        let bundle = envelope_smime::pkcs12::parse(&data, password)
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(bundle.identities.len(), 1, "{file}");
        let (k, c) = &bundle.identities[0];
        assert_eq!(c.fingerprint(), cert(name).fingerprint(), "{file}");
        assert!(k.matches(c));
        assert_eq!(
            bundle.certificates[0].fingerprint(),
            cert("mail-ca").fingerprint(),
            "{file}: the chain"
        );
        assert_eq!(
            envelope_smime::pkcs12::parse(&data, "wrong").unwrap_err(),
            Error::BadPassword,
            "{file}"
        );
        // The key survives a trip through the keyring's PEM form.
        let pem = k.to_pem().unwrap();
        assert!(PrivateKey::from_pem(&pem).unwrap().matches(c));
    }
    assert!(!envelope_smime::pkcs12::is_pkcs12(&fixture("alice.pem")));
}

#[test]
fn good_signatures_are_verified() {
    let trust = trust();
    for (name, sender, signer, algorithm) in [
        (
            "signed-alice.eml",
            "alice@example.com",
            "alice",
            "RSA, SHA-256",
        ),
        ("signed-bob.eml", "bob@example.org", "bob", "ECDSA, SHA-256"),
        (
            "signed-carol.eml",
            "carol@example.net",
            "carol",
            "ECDSA, SHA-384",
        ),
        (
            "signed-pss.eml",
            "alice@example.com",
            "alice",
            "RSA-PSS, SHA-256",
        ),
        (
            "signed-sha1.eml",
            "ALICE@example.com",
            "alice",
            "RSA, SHA-1",
        ),
        (
            "opaque-alice.eml",
            "alice@example.com",
            "alice",
            "RSA, SHA-256",
        ),
        (
            "outlook-signed.eml",
            "alice@example.com",
            "alice",
            "RSA, SHA-256",
        ),
        (
            "constrained-ok.eml",
            "olga@example.com",
            "olga",
            "RSA, SHA-256",
        ),
    ] {
        let signed = verify(name, sender, &trust, &[]);
        assert_eq!(signed.signatures.len(), 1, "{name}");
        let s = &signed.signatures[0];
        assert_eq!(s.verdict(), Verdict::Verified, "{name}: {s:?}");
        assert_eq!(s.integrity, Integrity::Intact);
        assert_eq!(s.algorithm, algorithm, "{name}");
        assert_eq!(
            s.signer.as_ref().unwrap().fingerprint(),
            cert(signer).fingerprint()
        );
        assert!(s.signed_at.is_some(), "{name}: signing time");
        assert_eq!(
            s.chain.last().unwrap().fingerprint(),
            cert("root").fingerprint(),
            "{name}: chain"
        );
        assert!(!signed.certificates.is_empty());
    }
    // The content of opaque signed data comes back.
    let signed = verify("opaque-alice.eml", "alice@example.com", &trust, &[]);
    let content = String::from_utf8(signed.content.unwrap()).unwrap();
    assert!(content.contains("the figures are below"));
}

#[test]
fn failing_checks_are_named() {
    let trust = trust();
    let verdict =
        |name: &str, sender: &str| verify(name, sender, &trust, &[]).signatures[0].clone();
    let tampered = verdict("tampered.eml", "alice@example.com");
    assert_eq!(tampered.verdict(), Verdict::Bad);
    assert_eq!(tampered.integrity, Integrity::ContentChanged);
    let damaged = verdict("bad-signature.eml", "alice@example.com");
    assert_eq!(damaged.verdict(), Verdict::Bad);
    assert_eq!(damaged.integrity, Integrity::SignatureMismatch);
    let expired = verdict("expired.eml", "dave@example.com");
    assert_eq!(expired.verdict(), Verdict::Expired);
    assert_eq!(expired.integrity, Integrity::Intact, "the text is intact");
    let untrusted = verdict("untrusted.eml", "mallory@example.com");
    assert_eq!(untrusted.verdict(), Verdict::Untrusted);
    assert_eq!(untrusted.trust, Trust::UnknownIssuer);
    let mismatch = verdict("mismatch.eml", "eve@example.com");
    assert_eq!(mismatch.verdict(), Verdict::Mismatch);
    assert!(!mismatch.sender_matches);
    assert_eq!(mismatch.trust, Trust::Trusted);
    let server = verdict("not-for-email.eml", "erin@example.com");
    assert_eq!(server.verdict(), Verdict::NotForEmail);
    let constrained = verdict("constrained-bad.eml", "oscar@example.net");
    assert_eq!(constrained.verdict(), Verdict::Untrusted);
    assert!(
        matches!(&constrained.trust, Trust::Invalid(why) if why.contains("oscar@example.net")),
        "{:?}",
        constrained.trust
    );
    // Without the certificate the signature can't be checked; with it known,
    // it can.
    let missing = verdict("no-certs.eml", "alice@example.com");
    assert_eq!(missing.verdict(), Verdict::NoCertificate);
    let known = verify(
        "no-certs.eml",
        "alice@example.com",
        &trust,
        &[cert("alice"), cert("mail-ca")],
    );
    assert_eq!(known.signatures[0].verdict(), Verdict::Verified);
    // Nothing is trusted when no authority is.
    let alone = verify(
        "signed-alice.eml",
        "alice@example.com",
        &TrustStore::default(),
        &[],
    );
    assert_eq!(alone.signatures[0].verdict(), Verdict::Untrusted);
}

#[test]
fn detects_smime_content() {
    for (name, kind) in [
        ("signed-alice.eml", Kind::Signed),
        ("outlook-signed.eml", Kind::Signed),
        ("opaque-alice.eml", Kind::Opaque),
        ("encrypted-alice.eml", Kind::Encrypted),
        ("encrypted-carol.eml", Kind::Encrypted),
        ("authenc-alice.eml", Kind::Encrypted),
        ("outlook-encrypted.eml", Kind::Encrypted),
    ] {
        assert_eq!(mime::detect(&fixture(name)), Some(kind), "{name}");
    }
    let parts = mime::signed_parts(&fixture("signed-alice.eml")).unwrap();
    assert!(parts.whole);
    assert!(parts.data.starts_with(b"Content-Type: text/plain"));
    assert!(
        mime::payload(&fixture("encrypted-alice.eml"))
            .unwrap()
            .whole
    );
}

/// Decrypts a fixture with a person's certificate and key.
fn decrypt(name: &str, who: &str) -> Result<Vec<u8>, Error> {
    let payload = mime::payload(&fixture(name)).unwrap();
    assert_eq!(payload.kind, Kind::Encrypted);
    let envelope = Envelope::read(&payload.cms)?;
    envelope.decrypt(&cert(who), &key(who))
}

#[test]
fn decrypts_what_others_encrypted() {
    let trust = trust();
    for (name, who) in [
        ("encrypted-carol.eml", "carol"),
        ("authenc-alice.eml", "alice"),
        ("des3-alice.eml", "alice"),
        ("oaep-alice.eml", "alice"),
    ] {
        let plain = decrypt(name, who).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            String::from_utf8_lossy(&plain).contains("the figures are below"),
            "{name}"
        );
    }
    // Streamed BER (Thunderbird and `openssl -stream`), RSA, signed inside.
    let inner = decrypt("encrypted-alice.eml", "alice").unwrap();
    assert_eq!(mime::detect(&inner), Some(Kind::Signed));
    let parts = mime::signed_parts(&inner).unwrap();
    let v = Verifier {
        trust: &trust,
        known: &[],
        sender: Some("bob@example.org"),
        now: envelope_smime::now(),
    };
    let signed = cms::verify_detached(&parts.data, &parts.signature, &v).unwrap();
    assert_eq!(signed.signatures[0].verdict(), Verdict::Verified);
    // Bob could read it too.
    assert!(decrypt("encrypted-alice.eml", "bob").is_ok());
    // ECDH P-256 with an opaque signature inside.
    let inner = decrypt("encrypted-bob.eml", "bob").unwrap();
    assert_eq!(mime::detect(&inner), Some(Kind::Opaque));
    let v = Verifier {
        sender: Some("alice@example.com"),
        ..v
    };
    let signed = cms::verify_opaque(&mime::payload(&inner).unwrap().cms, &v).unwrap();
    assert_eq!(signed.signatures[0].verdict(), Verdict::Verified);
    // Outlook's form, Outlook's signed message inside.
    let inner = decrypt("outlook-encrypted.eml", "alice").unwrap();
    let parts = mime::signed_parts(&inner).unwrap();
    let v = Verifier {
        sender: Some("bob@example.org"),
        ..v
    };
    let signed = cms::verify_detached(&parts.data, &parts.signature, &v).unwrap();
    assert_eq!(signed.signatures[0].verdict(), Verdict::Verified);
    // Not encrypted to Carol.
    let payload = mime::payload(&fixture("encrypted-alice.eml")).unwrap();
    let envelope = Envelope::read(&payload.cms).unwrap();
    assert!(envelope.is_for(&cert("alice")) && !envelope.is_for(&cert("carol")));
    assert_eq!(envelope.cipher().as_deref(), Some("AES-256-CBC"));
    assert_eq!(
        envelope.decrypt(&cert("carol"), &key("carol")).unwrap_err(),
        Error::NoKey
    );
}

const MESSAGE: &[u8] = b"From: Alice Example <alice@example.com>\r\n\
To: Bob Example <bob@example.org>, Carol <carol@example.net>\r\n\
Subject: =?utf-8?q?Planlar_=E2=98=95?=\r\n\
Message-ID: <round-trip@example.com>\r\n\
Date: Wed, 28 Oct 2026 10:00:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=\"utf-8\"\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Merhaba, g=C3=B6r=C3=BC=C5=9F=C3=BCr=C3=BCz.\r\n";

#[test]
fn signs_and_encrypts_round_trip() {
    let trust = trust();
    let (alice, alice_key) = (cert("alice"), key("alice"));
    let chain = [cert("mail-ca")];
    let with = || SignWith {
        cert: &alice,
        key: &alice_key,
        chain: &chain,
        encryption: None,
        time: envelope_smime::now(),
    };
    // Signed only: the text stays readable as it is.
    let signed = mime::protect(
        MESSAGE,
        &Protect {
            sign: Some(with()),
            ..Default::default()
        },
    )
    .unwrap();
    let text = String::from_utf8_lossy(&signed);
    assert!(
        text.contains("Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\";")
    );
    assert!(text.contains("micalg=sha-256"));
    assert!(text.contains("Subject: =?utf-8?q?Planlar_=E2=98=95?="));
    assert_eq!(mime::detect(&signed), Some(Kind::Signed));
    let parts = mime::signed_parts(&signed).unwrap();
    let v = Verifier {
        trust: &trust,
        known: &[],
        sender: Some("alice@example.com"),
        now: envelope_smime::now(),
    };
    let checked = cms::verify_detached(&parts.data, &parts.signature, &v).unwrap();
    let s = &checked.signatures[0];
    assert_eq!(s.verdict(), Verdict::Verified);
    assert_eq!(
        s.encryption_cert.as_ref(),
        Some(&alice),
        "encryption key preference"
    );
    // Still verifies after a trip through a mail store with bare LF.
    let lf = String::from_utf8(signed.clone())
        .unwrap()
        .replace("\r\n", "\n");
    let parts = mime::signed_parts(lf.as_bytes()).unwrap();
    let checked = cms::verify_detached(&parts.data, &parts.signature, &v).unwrap();
    assert_eq!(checked.signatures[0].verdict(), Verdict::Verified);

    // Signed and encrypted to an RSA, a P-256 and a P-384 certificate.
    let (bob, carol) = (cert("bob"), cert("carol"));
    let sealed = mime::protect(
        MESSAGE,
        &Protect {
            sign: Some(with()),
            encrypt_to: vec![&bob, &carol, &alice],
        },
    )
    .unwrap();
    let text = String::from_utf8_lossy(&sealed);
    assert!(text.contains("smime-type=enveloped-data"));
    assert!(!text.contains("Merhaba") && !text.contains("C3=B6r"));
    assert!(
        text.contains("Subject: =?utf-8?q?Planlar_=E2=98=95?="),
        "S/MIME keeps the subject"
    );
    assert_eq!(mime::detect(&sealed), Some(Kind::Encrypted));
    let envelope = Envelope::read(&mime::payload(&sealed).unwrap().cms).unwrap();
    for who in ["bob", "carol", "alice"] {
        let inner = envelope.decrypt(&cert(who), &key(who)).unwrap();
        let parts = mime::signed_parts(&inner).unwrap();
        let checked = cms::verify_detached(&parts.data, &parts.signature, &v).unwrap();
        assert_eq!(checked.signatures[0].verdict(), Verdict::Verified, "{who}");
        assert!(String::from_utf8_lossy(&parts.data).contains("C3=B6r"));
    }
    // An EC key signs too.
    let (bob_key, bob_cert) = (key("bob"), cert("bob"));
    let from_bob = mime::protect(
        MESSAGE,
        &Protect {
            sign: Some(SignWith {
                cert: &bob_cert,
                key: &bob_key,
                chain: &chain,
                encryption: None,
                time: envelope_smime::now(),
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let parts = mime::signed_parts(&from_bob).unwrap();
    let v = Verifier {
        sender: Some("bob@example.org"),
        ..v
    };
    let checked = cms::verify_detached(&parts.data, &parts.signature, &v).unwrap();
    assert_eq!(checked.signatures[0].verdict(), Verdict::Verified);
    assert_eq!(checked.signatures[0].algorithm, "ECDSA, SHA-256");
}

/// A scratch directory for OpenSSL, when it is installed.
fn openssl_dir() -> Option<PathBuf> {
    let ok = Command::new("openssl")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipping: openssl not installed");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("envelope-smime-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[test]
fn openssl_reads_what_envelope_writes() {
    let Some(dir) = openssl_dir() else {
        return;
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let (alice, alice_key) = (cert("alice"), key("alice"));
    let (bob, bob_key) = (cert("bob"), key("bob"));
    let chain = [cert("mail-ca")];
    let signed = mime::protect(
        MESSAGE,
        &Protect {
            sign: Some(SignWith {
                cert: &bob,
                key: &bob_key,
                chain: &chain,
                encryption: None,
                time: envelope_smime::now(),
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let file = dir.join("signed.eml");
    std::fs::write(&file, &signed).unwrap();
    let out = Command::new("openssl")
        .args(["cms", "-verify", "-purpose", "smimesign", "-in"])
        .arg(&file)
        .arg("-CAfile")
        .arg(fixtures.join("root.pem"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Merhaba"));

    // Encrypted to Alice (RSA) and Bob (ECDH): both open it with OpenSSL.
    let sealed = mime::protect(
        MESSAGE,
        &Protect {
            sign: Some(SignWith {
                cert: &alice,
                key: &alice_key,
                chain: &chain,
                encryption: None,
                time: envelope_smime::now(),
            }),
            encrypt_to: vec![&alice, &bob],
        },
    )
    .unwrap();
    let file = dir.join("sealed.eml");
    std::fs::write(&file, &sealed).unwrap();
    for who in ["alice", "bob"] {
        let out = Command::new("openssl")
            .args(["cms", "-decrypt", "-in"])
            .arg(&file)
            .arg("-recip")
            .arg(fixtures.join(format!("{who}.pem")))
            .arg("-inkey")
            .arg(fixtures.join(format!("{who}.key")))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{who}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let inner = dir.join(format!("inner-{who}.eml"));
        std::fs::write(&inner, &out.stdout).unwrap();
        let verified = Command::new("openssl")
            .args(["cms", "-verify", "-purpose", "smimesign", "-in"])
            .arg(&inner)
            .arg("-CAfile")
            .arg(fixtures.join("root.pem"))
            .output()
            .unwrap();
        assert!(
            verified.status.success(),
            "{who}: {}",
            String::from_utf8_lossy(&verified.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
