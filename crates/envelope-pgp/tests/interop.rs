//! Interoperability with messages and keys made by other OpenPGP software.
//! The fixtures come from GnuPG 2.4 (see `fixtures/generate.py`), shaped
//! like mail from Thunderbird, Enigmail and Mutt.

use std::path::{Path, PathBuf};
use std::process::Command;

use envelope_pgp::cert::{self, Cert};
use envelope_pgp::ops::{self, Issuer, SigStatus, Signer};
use envelope_pgp::pgp::composed::{SignedPublicKey, SignedSecretKey};
use envelope_pgp::pgpmime::{self, Kind};
use envelope_pgp::{Error, autocrypt};

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn gina() -> SignedSecretKey {
    cert::parse_secret(&fixture("gina.sec.asc")).unwrap()
}

fn rita() -> SignedSecretKey {
    cert::parse_secret(&fixture("rita.sec.asc")).unwrap()
}

fn lookup(certs: Vec<SignedPublicKey>) -> impl Fn(&Issuer) -> Vec<SignedPublicKey> {
    move |issuer: &Issuer| {
        certs
            .iter()
            .filter(|c| {
                cert::key_ids(c)
                    .iter()
                    .any(|id| issuer.key_ids.contains(id))
            })
            .cloned()
            .collect()
    }
}

#[test]
fn reads_gnupg_keys() {
    let gina = Cert::from_secret(gina()).info();
    assert_eq!(gina.user_ids, vec!["Gina GnuPG <gina@gnupg.example>"]);
    assert_eq!(gina.algorithm, "Ed25519");
    assert!(gina.can_encrypt && gina.can_sign && gina.secret && !gina.protected);
    let rita = Cert::from_secret(rita()).info();
    assert_eq!(rita.emails, vec!["rita@rsa.example"]);
    assert_eq!(rita.algorithm, "RSA 3072");
    assert!(rita.can_encrypt && rita.can_sign && rita.protected);
    // The public halves match the exported public keys.
    let public = cert::parse_public(&fixture("rita.pub.asc")).unwrap();
    assert_eq!(cert::fingerprint(&public), rita.fingerprint);
    assert!(Cert::from_public(public).info().can_encrypt);
    // The wrong passphrase is refused, the right one accepted.
    assert_eq!(
        cert::check_passphrase(&self::rita(), "nope").unwrap_err(),
        Error::BadPassphrase
    );
    cert::check_passphrase(&self::rita(), "rita-pass").unwrap();
}

#[test]
fn thunderbird_style_encrypted_and_signed() {
    let raw = fixture("tb-encrypted-signed.eml");
    assert_eq!(pgpmime::detect(&raw), Some(Kind::Encrypted));
    let payload = pgpmime::encrypted_payload(&raw).unwrap();
    let rita = rita();
    let rita_ids = cert::key_ids(&rita.to_public_key());
    let wanted = ops::recipients(&payload).unwrap();
    assert!(wanted.iter().any(|id| rita_ids.contains(id)));
    let signers = lookup(vec![gina().to_public_key()]);
    // Rita's key is protected: it needs her passphrase.
    assert!(matches!(
        ops::decrypt(&payload, &[(&rita, None)], &signers),
        Err(Error::Locked(_))
    ));
    assert_eq!(
        ops::decrypt(&payload, &[(&rita, Some("wrong"))], &signers).unwrap_err(),
        Error::BadPassphrase
    );
    let out = ops::decrypt(&payload, &[(&rita, Some("rita-pass"))], &signers).unwrap();
    assert_eq!(out.signatures.len(), 1);
    assert_eq!(out.signatures[0].status, SigStatus::Good);
    assert_eq!(
        out.signatures[0].signer.as_deref(),
        Some(cert::fingerprint(&gina().to_public_key()).as_str())
    );
    assert!(pgpmime::has_protected_headers(&out.data));
    let inner = mail_parser::MessageParser::new()
        .parse(&out.data[..])
        .unwrap();
    assert_eq!(inner.subject(), Some("Quarterly numbers ☕"));
    assert!(inner.body_text(0).unwrap().contains("Görüşürüz!"));
    assert!(
        inner
            .attachments()
            .any(|a| a.attachment_name() == Some("numbers.txt"))
    );
    // The legacy display part (the subject repeated in the body for clients
    // without protected headers) goes away; the rest stays.
    let shown = pgpmime::strip_legacy_display(&out.data);
    assert!(shown.len() < out.data.len());
    let shown = mail_parser::MessageParser::new().parse(&shown[..]).unwrap();
    assert!(shown.body_text(0).unwrap().starts_with("Hi Rita,"));
    assert_eq!(shown.subject(), Some("Quarterly numbers ☕"));
    assert_eq!(shown.attachments().count(), 1);
    assert!(
        !String::from_utf8_lossy(&pgpmime::strip_legacy_display(&out.data))
            .contains("rfc822-headers")
    );
    // Gina encrypted to herself as well (her copy in Sent).
    let gina = gina();
    assert!(ops::decrypt(&payload, &[(&gina, None)], &signers).is_ok());
}

#[test]
fn enigmail_style_signed() {
    let raw = fixture("enigmail-signed.eml");
    assert_eq!(pgpmime::detect(&raw), Some(Kind::Signed));
    let parts = pgpmime::signed_parts(&raw).unwrap();
    assert!(parts.whole);
    let signers = lookup(vec![gina().to_public_key()]);
    let results = ops::verify_detached(&parts.data, &parts.signature, &signers).unwrap();
    assert_eq!(results[0].status, SigStatus::Good);
    // Tampering with one character breaks it.
    let mut tampered = parts.data.clone();
    let i = tampered.iter().position(|&b| b == b'S').unwrap();
    tampered[i] = b's';
    assert_eq!(
        ops::verify_detached(&tampered, &parts.signature, &signers).unwrap()[0].status,
        SigStatus::Bad
    );
    // Without Gina's key the signer is unknown, but named.
    let nobody = |_: &Issuer| Vec::new();
    let unknown = ops::verify_detached(&parts.data, &parts.signature, &nobody).unwrap();
    assert_eq!(unknown[0].status, SigStatus::UnknownKey);
    assert!(unknown[0].issuer.label().is_some());
}

#[test]
fn nested_signature_inside_encryption() {
    let raw = fixture("nested-signed-encrypted.eml");
    let payload = pgpmime::encrypted_payload(&raw).unwrap();
    let rita = rita();
    let nobody = |_: &Issuer| Vec::new();
    let out = ops::decrypt(&payload, &[(&rita, Some("rita-pass"))], &nobody).unwrap();
    assert!(
        out.signatures.is_empty(),
        "no signature in the OpenPGP layer"
    );
    assert_eq!(pgpmime::detect(&out.data), Some(Kind::Signed));
    let parts = pgpmime::signed_parts(&out.data).unwrap();
    let signers = lookup(vec![gina().to_public_key()]);
    assert_eq!(
        ops::verify_detached(&parts.data, &parts.signature, &signers).unwrap()[0].status,
        SigStatus::Good
    );
}

#[test]
fn hidden_recipient() {
    let raw = fixture("hidden-recipient.eml");
    let payload = pgpmime::encrypted_payload(&raw).unwrap();
    assert_eq!(
        ops::recipients(&payload).unwrap(),
        vec!["0000000000000000".to_string()]
    );
    let rita = rita();
    let gina = gina();
    let nobody = |_: &Issuer| Vec::new();
    // Gina's key is tried and fails; Rita's works.
    let out = ops::decrypt(
        &payload,
        &[(&gina, None), (&rita, Some("rita-pass"))],
        &nobody,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&out.data).contains("For your eyes only."));
}

#[test]
fn inline_pgp() {
    let raw = fixture("inline-encrypted.eml");
    assert_eq!(pgpmime::detect(&raw), Some(Kind::InlineEncrypted));
    let text = String::from_utf8(raw).unwrap();
    let range = pgpmime::find_block(&text, pgpmime::MESSAGE_BEGIN, pgpmime::MESSAGE_END).unwrap();
    let rita = rita();
    let signers = lookup(vec![gina().to_public_key()]);
    let out = ops::decrypt(
        text[range].as_bytes(),
        &[(&rita, Some("rita-pass"))],
        &signers,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out.data).unwrap(),
        "Inline secret: şifre 1234\n"
    );
    assert_eq!(out.signatures[0].status, SigStatus::Good);

    let raw = fixture("clearsigned.eml");
    assert_eq!(pgpmime::detect(&raw), Some(Kind::InlineSigned));
    let text = String::from_utf8(raw).unwrap();
    let range = pgpmime::find_block(&text, pgpmime::SIGNED_BEGIN, pgpmime::SIGNATURE_END).unwrap();
    let (clear, results) = ops::verify_cleartext(&text[range], &signers).unwrap();
    assert!(clear.contains("Clear and signed.\n- a list item\nTschüss"));
    assert_eq!(results[0].status, SigStatus::Good);
}

#[test]
fn autocrypt_keys_from_gnupg() {
    // An Autocrypt header built from a GnuPG key reads back as the same key.
    let public = cert::parse_public(&fixture("gina.pub.asc")).unwrap();
    let small = cert::minimal(
        &public,
        "gina@gnupg.example",
        chrono::Utc::now().timestamp(),
    );
    let keydata = cert::public_bytes(&small).unwrap();
    let header = autocrypt::format("gina@gnupg.example", true, &keydata);
    let parsed = autocrypt::parse(&header).unwrap();
    let back = cert::parse_public(&parsed.keydata).unwrap();
    assert_eq!(cert::fingerprint(&back), cert::fingerprint(&public));
    assert!(Cert::from_public(back).info().can_encrypt);
}

// ---------------------------------------------------------------------------
// The other direction: GnuPG reads what Envelope writes.

fn gpg_home() -> Option<PathBuf> {
    let ok = Command::new("gpg")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipping: gpg not installed");
        return None;
    }
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("gnupg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    Some(home)
}

fn gpg(home: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new("gpg")
        .arg("--homedir")
        .arg(home)
        .args([
            "--batch",
            "--yes",
            "--pinentry-mode",
            "loopback",
            "--trust-model",
            "always",
            "--status-fd",
            "2",
        ])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn gnupg_reads_envelope_messages() {
    let Some(home) = gpg_home() else {
        return;
    };
    let alex = cert::generate("Alex Morgan", "alex@envelope.test", None).unwrap();
    let alex_pub = cert::public_armored(&alex.to_public_key()).unwrap();
    // GnuPG holds Gina's secret key and knows Alex's public key. Without a
    // working gpg-agent (some containers) there is nothing to test.
    let imported = gpg(&home, &["--import"], &fixture("gina.sec.asc"));
    if !imported.status.success() {
        eprintln!(
            "skipping: gpg cannot import a secret key here: {}",
            String::from_utf8_lossy(&imported.stderr)
        );
        return;
    }
    assert!(
        gpg(&home, &["--import"], alex_pub.as_bytes())
            .status
            .success()
    );

    let message = b"From: Alex Morgan <alex@envelope.test>\r\n\
To: Gina GnuPG <gina@gnupg.example>\r\n\
Subject: Hello Gina\r\n\
Message-ID: <x1@envelope.test>\r\n\
Date: Wed, 28 Oct 2026 10:00:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=\"utf-8\"\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
Encrypted by Envelope.\r\n";
    let gina_pub = gina().to_public_key();
    let encrypted = pgpmime::protect(
        message,
        &pgpmime::Protect {
            encrypt_to: vec![&gina_pub, &alex.to_public_key()],
            signer: Some(Signer {
                key: &alex,
                passphrase: None,
            }),
            protect_headers: true,
            hide_subject: true,
            ..Default::default()
        },
    )
    .unwrap();
    let payload = pgpmime::encrypted_payload(&encrypted).unwrap();
    let out = gpg(&home, &["--decrypt"], &payload);
    let status = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "gpg --decrypt failed: {status}");
    assert!(status.contains("GOODSIG"), "{status}");
    let plain = String::from_utf8_lossy(&out.stdout);
    assert!(plain.contains("Subject: Hello Gina"));
    assert!(plain.contains("protected-headers=\"v1\""));
    assert!(plain.contains("Encrypted by Envelope."));

    // A multipart/signed message verifies with `gpg --verify`.
    let signed = pgpmime::protect(
        message,
        &pgpmime::Protect {
            signer: Some(Signer {
                key: &alex,
                passphrase: None,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let parts = pgpmime::signed_parts(&signed).unwrap();
    let dir = home.join("verify");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("data"), &parts.data).unwrap();
    std::fs::write(dir.join("data.asc"), &parts.signature).unwrap();
    let out = gpg(
        &home,
        &[
            "--verify",
            dir.join("data.asc").to_str().unwrap(),
            dir.join("data").to_str().unwrap(),
        ],
        b"",
    );
    let status = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && status.contains("GOODSIG"),
        "{status}"
    );

    // A hidden recipient can still decrypt.
    let bcc = pgpmime::protect(
        message,
        &pgpmime::Protect {
            encrypt_to: vec![&alex.to_public_key()],
            hidden: vec![&gina_pub],
            ..Default::default()
        },
    )
    .unwrap();
    let out = gpg(
        &home,
        &["--decrypt"],
        &pgpmime::encrypted_payload(&bcc).unwrap(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = Command::new("gpgconf")
        .arg("--homedir")
        .arg(&home)
        .args(["--kill", "gpg-agent"])
        .status();
    let _ = std::fs::remove_dir_all(&home);
}
