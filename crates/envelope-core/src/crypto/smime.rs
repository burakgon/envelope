//! S/MIME: certificates — the user's own, whose private keys live in the
//! keyring, and other people's, kept from the signed mail they send — and
//! the S/MIME side of opening and sending messages. The cryptography is in
//! `envelope-smime`; this decides where certificates live, which
//! authorities are trusted and which certificate to use.
//!
//! - Private keys are in the system keyring (`smime:secret:<fingerprint>`,
//!   unencrypted PKCS #8: the keyring protects them, as it does OpenPGP keys
//!   Envelope creates), never in the database. Certificates are in
//!   `smime_certs`.
//! - Trusted authorities are the ones TLS trusts on this computer (the
//!   system certificate store, as rustls-native-certs reads it) and the
//!   distribution's e-mail bundle where it has one.
//! - When a signed message verifies, its sender's certificate is kept (with
//!   the one they ask to be encrypted to, and the authorities between them
//!   and the root), so mail to them can be encrypted.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use envelope_smime::cms::{self, Envelope, SignWith, Signed, Verifier};
use envelope_smime::mime::{self as smime_mime, Kind, Protect};
use envelope_smime::{
    Certificate, Integrity, PrivateKey, Trust, TrustStore, Validity, Verdict, chain, pkcs12,
};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use super::incoming::{Failure, OpenedMessage, SignatureView};
use crate::db::crypto as crypto_store;
use crate::db::now_ms;
use crate::db::smime::{self as store, CertRow};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::DraftInput;

/// Name of a private key in the keyring.
pub(crate) fn secret_name(fingerprint: &str) -> String {
    format!("smime:secret:{fingerprint}")
}

/// A certificate as the interface shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateInfo {
    /// SHA-256 of the certificate, upper-case hex.
    pub fingerprint: String,
    /// Its owner's name (common name, organization or address).
    pub name: String,
    /// The subject, written out.
    pub subject: String,
    pub emails: Vec<String>,
    /// The issuer's name, and written out.
    pub issuer: String,
    pub issuer_subject: String,
    /// Hex, colon separated.
    pub serial: String,
    pub algorithm: String,
    /// Unix ms.
    pub not_before: i64,
    pub not_after: i64,
    pub expired: bool,
    pub not_yet_valid: bool,
    /// It may sign mail, and receive encrypted mail.
    pub can_sign: bool,
    pub can_encrypt: bool,
    /// One of the user's own: the private key is in the keyring.
    pub secret: bool,
    /// `own`, `message` (from their signed mail) or `manual` (imported).
    pub source: String,
    pub added_at: i64,
    pub updated_at: i64,
}

impl CertificateInfo {
    pub(crate) fn new(cert: &Certificate, row: Option<&CertRow>, now: i64) -> CertificateInfo {
        let validity = cert.validity_at(now / 1000);
        CertificateInfo {
            fingerprint: cert.fingerprint(),
            name: cert.name(),
            subject: cert.subject(),
            emails: row.map_or_else(|| cert.emails().to_vec(), |r| r.emails.clone()),
            issuer: cert.issuer_name(),
            issuer_subject: cert.issuer(),
            serial: cert.serial(),
            algorithm: cert.algorithm(),
            not_before: cert.not_before() * 1000,
            not_after: cert.not_after() * 1000,
            expired: validity == Validity::Expired,
            not_yet_valid: validity == Validity::NotYetValid,
            can_sign: cert.can_sign_mail(),
            can_encrypt: cert.can_encrypt_mail(),
            secret: row.is_some_and(|r| r.secret),
            source: row.map_or_else(|| "message".to_string(), |r| r.source.clone()),
            added_at: row.map_or(0, |r| r.added_at),
            updated_at: row.map_or(0, |r| r.updated_at),
        }
    }
}

/// An S/MIME signature, check by check, for the interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmimeSignature {
    /// `verified` when every check passed; otherwise the first that failed:
    /// `bad`, `noCertificate`, `unsupported`, `expired`, `notYetValid`,
    /// `notForEmail`, `untrusted` or `mismatch`.
    pub verdict: String,
    /// Why: for `bad`, the `content` changed or the `signature` does not
    /// fit the certificate; for `untrusted`, `unknownIssuer`,
    /// `issuerExpired` or `invalid`.
    pub reason: Option<String>,
    /// More about it, in English: the authority that expired, the rule the
    /// chain breaks, the algorithm that is not supported.
    pub detail: Option<String>,
    /// The signer's certificate.
    pub certificate: Option<CertificateInfo>,
    /// The authorities from the certificate's issuer up to the trusted root.
    pub chain: Vec<String>,
    /// "RSA, SHA-256" and the like.
    pub algorithm: String,
}

/// A recipient and the certificate mail to them would be encrypted to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientCertificate {
    pub email: String,
    pub certificate: Option<CertificateInfo>,
}

/// What S/MIME can do for a message being written.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmimeCompose {
    /// The sender's certificate that signs.
    pub own: Option<CertificateInfo>,
    pub recipients: Vec<RecipientCertificate>,
    /// Recipients without a usable certificate.
    pub missing: Vec<String>,
    /// Every recipient has a certificate, and the sender one to keep a
    /// readable copy.
    pub can_encrypt: bool,
    pub can_sign: bool,
}

// ---------------------------------------------------------------------------
// Certificates

fn source_rank(source: &str) -> u8 {
    match source {
        "own" => 3,
        "manual" => 2,
        "message" => 1,
        _ => 0,
    }
}

/// Stores a certificate, keeping what is already known about it.
pub(crate) fn store_certificate(
    tx: &Transaction<'_>,
    cert: &Certificate,
    source: &str,
    extra_emails: &[String],
    secret: bool,
) -> Result<CertRow> {
    let fingerprint = cert.fingerprint();
    let existing = store::cert_get(tx, &fingerprint)?;
    let mut emails = cert.emails().to_vec();
    for e in extra_emails
        .iter()
        .chain(existing.iter().flat_map(|o| o.emails.iter()))
    {
        let e = e.trim().to_lowercase();
        if e.contains('@') && !emails.contains(&e) {
            emails.push(e);
        }
    }
    let now = now_ms();
    let source = match &existing {
        Some(old) if source_rank(&old.source) >= source_rank(source) => old.source.clone(),
        _ => source.to_string(),
    };
    let row = CertRow {
        fingerprint,
        data: cert.der().to_vec(),
        emails,
        ca: cert.is_ca(),
        secret: secret || existing.as_ref().is_some_and(|o| o.secret),
        source,
        added_at: existing.as_ref().map_or(now, |o| o.added_at),
        updated_at: now,
    };
    store::cert_save(tx, &row)?;
    Ok(row)
}

fn parsed(rows: Vec<CertRow>) -> Vec<(CertRow, Certificate)> {
    rows.into_iter()
        .filter_map(|r| Certificate::from_der(&r.data).ok().map(|c| (r, c)))
        .collect()
}

/// Certificates that may complete a chain or stand for a signer a message
/// leaves out: the authorities kept, and the sender's.
fn known_certificates(c: &Connection, sender: Option<&str>) -> Result<Vec<Certificate>> {
    let mut rows = store::certs_ca(c)?;
    if let Some(sender) = sender {
        rows.extend(store::certs_for_email(c, sender)?);
    }
    Ok(parsed(rows).into_iter().map(|(_, c)| c).collect())
}

/// The newest valid certificate among `rows` that passes `fits`.
fn newest(
    rows: Vec<CertRow>,
    now: i64,
    fits: impl Fn(&Certificate) -> bool,
) -> Option<(CertRow, Certificate)> {
    parsed(rows)
        .into_iter()
        .filter(|(_, c)| c.validity_at(now / 1000) == Validity::Valid && fits(c))
        .max_by_key(|(r, c)| (source_rank(&r.source), c.not_before()))
}

/// The certificate to encrypt to for an address: one the user imported,
/// else the newest from their signed mail; valid and meant for encrypting
/// mail.
pub(crate) fn recipient_certificate(
    c: &Connection,
    email: &str,
    now: i64,
) -> Result<Option<(CertRow, Certificate)>> {
    Ok(newest(store::certs_for_email(c, email)?, now, |cert| {
        cert.can_encrypt_mail()
    }))
}

/// Where distributions keep the authorities trusted for e-mail, besides
/// the ones for TLS (p11-kit's extracted bundles).
const EMAIL_BUNDLES: [&str; 2] = [
    "/etc/ca-certificates/extracted/email-ca-bundle.pem",
    "/etc/pki/ca-trust/extracted/pem/email-ca-bundle.pem",
];

/// The authorities this computer trusts, read once.
pub(crate) fn trust_store() -> Arc<TrustStore> {
    static STORE: OnceLock<Arc<TrustStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            #[cfg(test)]
            {
                Arc::new(test_trust())
            }
            #[cfg(not(test))]
            {
                Arc::new(system_trust())
            }
        })
        .clone()
}

#[cfg(not(test))]
fn system_trust() -> TrustStore {
    let found = rustls_native_certs::load_native_certs();
    for e in &found.errors {
        tracing::debug!("system certificate store: {e}");
    }
    let mut certs: Vec<Vec<u8>> = found.certs.iter().map(|c| c.to_vec()).collect();
    for path in EMAIL_BUNDLES {
        if let Ok(pem) = std::fs::read(path) {
            for cert in Certificate::parse_all(&pem).unwrap_or_default() {
                certs.push(cert.der().to_vec());
            }
        }
    }
    certs.sort();
    certs.dedup();
    let store = TrustStore::new(certs);
    tracing::debug!("{} trusted authorities for S/MIME", store.len());
    store
}

/// Tests trust the root of the S/MIME fixtures.
#[cfg(test)]
fn test_trust() -> TrustStore {
    let _ = EMAIL_BUNDLES;
    let pem = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../envelope-smime/tests/fixtures/root.pem"),
    )
    .unwrap_or_default();
    TrustStore::new(
        Certificate::parse_all(&pem)
            .unwrap_or_default()
            .iter()
            .map(|c| c.der().to_vec()),
    )
}

// ---------------------------------------------------------------------------
// Receiving

/// Checks the signature of S/MIME signed content (detached or opaque) in a
/// message. `None` for encrypted content.
pub(crate) fn verify(
    c: &Connection,
    kind: Kind,
    raw: &[u8],
    sender: Option<&str>,
) -> Result<Option<(Signed, bool)>> {
    let trust = trust_store();
    let known = known_certificates(c, sender)?;
    let v = Verifier {
        trust: &trust,
        known: &known,
        sender,
        now: envelope_smime::now(),
    };
    let unreadable = |what: &str| Error::Invalid(format!("The {what} of this message is missing."));
    Ok(Some(match kind {
        Kind::Signed => {
            let parts = smime_mime::signed_parts(raw).ok_or_else(|| unreadable("signed part"))?;
            (
                cms::verify_detached(&parts.data, &parts.signature, &v)?,
                parts.whole,
            )
        }
        Kind::Opaque => {
            let payload = smime_mime::payload(raw).ok_or_else(|| unreadable("signed part"))?;
            (cms::verify_opaque(&payload.cms, &v)?, payload.whole)
        }
        Kind::Encrypted => return Ok(None),
    }))
}

/// Keeps the certificates of a sender whose signature verified: theirs,
/// the one they ask to be encrypted to, and the authorities in between.
/// The user's own addresses are left alone.
pub(crate) fn collect(
    tx: &Transaction<'_>,
    signed: &Signed,
    sender: &str,
    mine: &HashSet<String>,
) -> Result<()> {
    let sender = sender.trim().to_lowercase();
    if mine.contains(&sender) {
        return Ok(());
    }
    let trust = trust_store();
    for sig in &signed.signatures {
        let Some(cert) = sig
            .signer
            .as_ref()
            .filter(|_| sig.verdict() == Verdict::Verified)
        else {
            continue;
        };
        store_certificate(tx, cert, "message", &[], false)?;
        for ca in sig.chain.iter().filter(|c| !c.is_self_issued()) {
            store_certificate(tx, ca, "chain", &[], false)?;
        }
        if let Some(other) = &sig.encryption_cert
            && other != cert
            && other.has_email(&sender)
            && other.can_encrypt_mail()
        {
            let pool: Vec<&Certificate> = signed.certificates.iter().collect();
            if chain::verify(other, &pool, &trust, envelope_smime::now()).is_ok() {
                store_certificate(tx, other, "message", &[], false)?;
            }
        }
    }
    Ok(())
}

/// Adapts what is stored for a downloaded S/MIME message: encrypted ones
/// keep no readable body until they are opened, signatures are not
/// attachments, and opaque signed content (not secret) is stored as the
/// body. Certificates of senders whose signature verifies are kept, except
/// from spam.
pub(crate) fn on_source(
    tx: &Transaction<'_>,
    kind: Kind,
    raw: &[u8],
    sender: Option<&str>,
    junk: bool,
    body: &mut crate::mime::ParsedBody,
) -> Result<()> {
    match kind {
        Kind::Encrypted => {
            body.html = None;
            body.text = None;
            body.snippet = super::ENCRYPTED_SNIPPET.into();
            body.attachments.clear();
            body.calendar = None;
            return Ok(());
        }
        Kind::Signed => body.attachments.retain(|a| {
            !smime_mime::is_smime_type(&a.mime_type)
                && !a.filename.to_ascii_lowercase().ends_with(".p7s")
        }),
        Kind::Opaque => {
            body.html = None;
            body.text = None;
            body.snippet = String::new();
            body.attachments.clear();
        }
    }
    let signed = match verify(tx, kind, raw, sender) {
        Ok(Some((signed, _))) => signed,
        Ok(None) => return Ok(()),
        Err(e) => {
            tracing::debug!("S/MIME signature not checked: {e}");
            return Ok(());
        }
    };
    if kind == Kind::Opaque
        && let Some((_, inner)) = signed.content.as_deref().and_then(crate::mime::parse_full)
    {
        body.html = inner.html;
        body.text = inner.text;
        body.snippet = inner.snippet;
        body.calendar = inner.calendar;
    }
    if !junk && let Some(sender) = sender {
        let mine = super::my_emails(tx)?;
        collect(tx, &signed, sender, &mine)?;
    }
    Ok(())
}

/// The interface's view of checked S/MIME signatures.
fn views(c: &Connection, signed: &Signed) -> Vec<SignatureView> {
    let now = now_ms();
    signed
        .signatures
        .iter()
        .map(|s| {
            let verdict = s.verdict();
            let (status, bad) = match &s.integrity {
                Integrity::Intact => ("good", None),
                Integrity::ContentChanged => ("bad", Some("content")),
                Integrity::SignatureMismatch => ("bad", Some("signature")),
                Integrity::NoCertificate | Integrity::Unsupported(_) => ("unknownKey", None),
            };
            let (reason, detail) = match verdict {
                Verdict::Bad => (bad.map(str::to_string), None),
                Verdict::Untrusted => match &s.trust {
                    Trust::UnknownIssuer => (Some("unknownIssuer".to_string()), None),
                    Trust::IssuerExpired(name) => {
                        (Some("issuerExpired".to_string()), Some(name.clone()))
                    }
                    Trust::Invalid(why) => (Some("invalid".to_string()), Some(why.clone())),
                    Trust::Trusted => (None, None),
                },
                Verdict::Unsupported => match &s.integrity {
                    Integrity::Unsupported(name) => (None, Some(name.clone())),
                    _ => (None, None),
                },
                _ => (None, None),
            };
            let certificate = s.signer.as_ref().map(|cert| {
                let row = store::cert_get(c, &cert.fingerprint()).ok().flatten();
                CertificateInfo::new(cert, row.as_ref(), now)
            });
            SignatureView {
                status: status.into(),
                signer: None,
                issuer: None,
                created: s.signed_at.map(|t| t * 1000),
                sender_matches: s.sender_matches,
                smime: Some(SmimeSignature {
                    verdict: verdict.as_str().into(),
                    reason,
                    detail,
                    certificate,
                    chain: s.chain.iter().map(Certificate::name).collect(),
                    algorithm: s.algorithm.clone(),
                }),
            }
        })
        .collect()
}

impl Engine {
    /// Opens an S/MIME message: checks its signature, or decrypts it (and
    /// checks the signature inside). Returns the entity the attachment ids
    /// of the view refer to, when the content was inside the protection.
    pub(crate) async fn open_smime(
        &self,
        message_id: i64,
        date: i64,
        kind: Kind,
        raw: &[u8],
        sender: Option<&str>,
        view: &mut OpenedMessage,
    ) -> Result<Option<Vec<u8>>> {
        match kind {
            Kind::Signed => {
                match self.db.read(|c| verify(c, kind, raw, sender)) {
                    Ok(Some((signed, whole))) => {
                        view.signatures = self.db.read(|c| Ok(views(c, &signed)))?;
                        view.partially_signed = !whole;
                        self.keep_certificates(&signed, sender);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        view.status = "error".into();
                        view.error = Some(e.to_string());
                    }
                }
                Ok(None)
            }
            Kind::Opaque => match self.db.read(|c| verify(c, kind, raw, sender)) {
                Ok(Some((signed, whole))) => {
                    let content = signed.content.clone().unwrap_or_default();
                    let signatures = self.db.read(|c| Ok(views(c, &signed)))?;
                    view.partially_signed = !whole;
                    self.keep_certificates(&signed, sender);
                    let shown =
                        self.show_entity(message_id, date, &content, signatures, sender, view)?;
                    Ok(Some(shown))
                }
                Ok(None) => Ok(None),
                Err(e) => {
                    view.status = "error".into();
                    view.error = Some(e.to_string());
                    Ok(None)
                }
            },
            Kind::Encrypted => {
                let payload = smime_mime::payload(raw).ok_or_else(|| {
                    Error::Invalid("The encrypted part of this message is missing.".into())
                })?;
                view.partially_encrypted = !payload.whole;
                match self.smime_decrypt(&payload.cms).await {
                    Ok(content) => {
                        let (shown, signatures) = self.smime_inner(content, sender)?;
                        let shown =
                            self.show_entity(message_id, date, &shown, signatures, sender, view)?;
                        Ok(Some(shown))
                    }
                    Err(f) => {
                        self.failed(view, f);
                        Ok(None)
                    }
                }
            }
        }
    }

    /// What decrypted S/MIME content shows, and the signatures inside it
    /// (signed, then encrypted: a signed entity inside).
    pub(crate) fn smime_inner(
        &self,
        content: Vec<u8>,
        sender: Option<&str>,
    ) -> Result<(Vec<u8>, Vec<SignatureView>)> {
        let Some(kind) = smime_mime::detect(&content).filter(|k| !k.is_encrypted()) else {
            return Ok((content, Vec::new()));
        };
        match self.db.read(|c| verify(c, kind, &content, sender)) {
            Ok(Some((signed, _))) => {
                let signatures = self.db.read(|c| Ok(views(c, &signed)))?;
                self.keep_certificates(&signed, sender);
                let shown = match kind {
                    Kind::Opaque => signed.content.unwrap_or(content),
                    _ => content,
                };
                Ok((shown, signatures))
            }
            _ => Ok((content, Vec::new())),
        }
    }

    fn keep_certificates(&self, signed: &Signed, sender: Option<&str>) {
        let Some(sender) = sender else {
            return;
        };
        let kept = self.db.write(|tx| {
            let mine = super::my_emails(tx)?;
            collect(tx, signed, sender, &mine)
        });
        if let Err(e) = kept {
            tracing::debug!("certificates not kept: {e}");
        }
    }

    /// Decrypts S/MIME content with the user's own certificates.
    pub(crate) async fn smime_decrypt(&self, cms: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
        let envelope =
            Envelope::read(cms).map_err(|e| Failure::Error(Error::from(e).to_string()))?;
        let own = self
            .db
            .read(|c| Ok(parsed(store::certs_secret(c)?)))
            .map_err(|e| Failure::Error(e.to_string()))?;
        let candidates: Vec<(CertRow, Certificate)> = own
            .into_iter()
            .filter(|(_, cert)| envelope.is_for(cert))
            .collect();
        if candidates.is_empty() {
            return Err(Failure::NoCertificate);
        }
        let mut failure = Failure::Error("Your private key is missing from the keyring.".into());
        for (row, cert) in candidates {
            let key = match self.load_smime_key(&row.fingerprint).await {
                Ok(k) => k,
                Err(e) => {
                    tracing::warn!("private key {} unavailable: {e}", row.fingerprint);
                    continue;
                }
            };
            let envelope = envelope.clone();
            let result = tokio::task::spawn_blocking(move || envelope.decrypt(&cert, &key))
                .await
                .map_err(|e| Failure::Error(e.to_string()))?;
            match result {
                Ok(plain) => return Ok(plain),
                Err(e) => failure = Failure::Error(Error::from(e).to_string()),
            }
        }
        Err(failure)
    }

    /// A private key from the keyring.
    pub(crate) async fn load_smime_key(&self, fingerprint: &str) -> Result<PrivateKey> {
        let pem = self
            .secrets
            .get(&secret_name(fingerprint))
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "The private key of the certificate {fingerprint} is missing from the keyring."
                ))
            })?;
        Ok(PrivateKey::from_pem(&pem)?)
    }

    // -----------------------------------------------------------------------
    // Managing certificates

    /// People's certificates (the user's own first) matching a name,
    /// address or fingerprint.
    pub fn smime_certificates(&self, query: &str) -> Result<Vec<CertificateInfo>> {
        let q = query.trim().to_lowercase();
        let compact = q.replace([' ', ':'], "");
        let now = now_ms();
        Ok(parsed(self.db.read(store::certs_list)?)
            .into_iter()
            .map(|(row, cert)| CertificateInfo::new(&cert, Some(&row), now))
            .filter(|i| {
                q.is_empty()
                    || i.name.to_lowercase().contains(&q)
                    || i.emails.iter().any(|e| e.contains(&q))
                    || i.issuer.to_lowercase().contains(&q)
                    || (!compact.is_empty() && i.fingerprint.to_lowercase().contains(&compact))
            })
            .collect())
    }

    /// Imports certificates: a PKCS #12 file (`.p12`, `.pfx`) with its
    /// password, whose certificates become the user's own (with `own_email`
    /// as one more address), or certificates of other people (PEM, DER or
    /// PKCS #7). Authorities' certificates are kept to complete chains.
    pub async fn import_smime(
        &self,
        data: &[u8],
        password: Option<&str>,
        own_email: Option<&str>,
    ) -> Result<Vec<CertificateInfo>> {
        let own_email: Vec<String> = own_email
            .map(|e| e.trim().to_lowercase())
            .filter(|e| e.contains('@'))
            .into_iter()
            .collect();
        let now = now_ms();
        let mut out = Vec::new();
        if pkcs12::is_pkcs12(data) {
            let (data, password) = (data.to_vec(), password.unwrap_or_default().to_string());
            let bundle = tokio::task::spawn_blocking(move || pkcs12::parse(&data, &password))
                .await
                .map_err(|e| Error::Other(format!("import failed: {e}")))??;
            if bundle.identities.is_empty() {
                return Err(Error::Invalid(
                    "This file has no private key. Import other people's certificates under Other people's certificates.".into(),
                ));
            }
            for (key, cert) in &bundle.identities {
                self.secrets
                    .set(&secret_name(&cert.fingerprint()), &key.to_pem()?)
                    .await?;
            }
            let rows = self.db.write(|tx| {
                let mut rows = Vec::new();
                for (_, cert) in &bundle.identities {
                    rows.push((
                        store_certificate(tx, cert, "own", &own_email, true)?,
                        cert.clone(),
                    ));
                }
                for cert in bundle.certificates.iter().filter(|c| c.is_ca()) {
                    store_certificate(tx, cert, "chain", &[], false)?;
                }
                Ok(rows)
            })?;
            out.extend(
                rows.iter()
                    .map(|(row, cert)| CertificateInfo::new(cert, Some(row), now)),
            );
        } else {
            let certs = Certificate::parse_all(data).map_err(|_| {
                Error::Invalid(
                    "No certificate found. Import a certificate (.pem, .cer, .crt, .der, .p7b), or a .p12 or .pfx file with your own."
                        .into(),
                )
            })?;
            let rows = self.db.write(|tx| {
                let mut rows = Vec::new();
                for cert in &certs {
                    if cert.is_ca() {
                        store_certificate(tx, cert, "chain", &[], false)?;
                    } else {
                        rows.push((store_certificate(tx, cert, "manual", &[], false)?, cert));
                    }
                }
                Ok(rows
                    .into_iter()
                    .map(|(row, cert)| CertificateInfo::new(cert, Some(&row), now))
                    .collect::<Vec<_>>())
            })?;
            if rows.is_empty() {
                return Err(Error::Invalid(
                    "This file only has certificates of authorities.".into(),
                ));
            }
            out = rows;
        }
        self.crypto.forget_opened();
        Ok(out)
    }

    fn certificate_row(&self, fingerprint: &str) -> Result<(CertRow, Certificate)> {
        let fingerprint = fingerprint.trim().replace([' ', ':'], "").to_uppercase();
        let row = self
            .db
            .read(|c| store::cert_get(c, &fingerprint))?
            .ok_or_else(|| Error::NotFound(format!("certificate {fingerprint}")))?;
        let cert = Certificate::from_der(&row.data)?;
        Ok((row, cert))
    }

    /// The certificate as PEM (no private key).
    pub fn export_smime_certificate(&self, fingerprint: &str) -> Result<String> {
        Ok(self.certificate_row(fingerprint)?.1.to_pem())
    }

    /// The certificate as DER (`.cer`, `.der`).
    pub fn smime_certificate_der(&self, fingerprint: &str) -> Result<Vec<u8>> {
        Ok(self.certificate_row(fingerprint)?.1.der().to_vec())
    }

    /// Deletes a certificate (and, for the user's own, its private key in
    /// the keyring).
    pub async fn delete_smime_certificate(&self, fingerprint: &str) -> Result<()> {
        let (row, _) = self.certificate_row(fingerprint)?;
        if row.secret {
            self.secrets.delete(&secret_name(&row.fingerprint)).await?;
        }
        self.db
            .write(|tx| store::cert_delete(tx, &row.fingerprint))?;
        self.crypto.forget_opened();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Sending

    /// The user's certificate for an address that signs, and the one that
    /// receives encrypted mail (often the same).
    fn own_certificates(
        &self,
        email: &str,
    ) -> Result<(
        Option<(CertRow, Certificate)>,
        Option<(CertRow, Certificate)>,
    )> {
        let email = email.trim().to_lowercase();
        let rows: Vec<CertRow> = self
            .db
            .read(store::certs_secret)?
            .into_iter()
            .filter(|r| r.emails.contains(&email))
            .collect();
        let now = now_ms();
        Ok((
            newest(rows.clone(), now, Certificate::can_sign_mail),
            newest(rows, now, Certificate::can_encrypt_mail),
        ))
    }

    /// Whether the user has a certificate for an address (to sign or to
    /// receive encrypted mail).
    pub(crate) fn has_own_certificate(&self, email: &str) -> Result<bool> {
        let (sign, encrypt) = self.own_certificates(email)?;
        Ok(sign.is_some() || encrypt.is_some())
    }

    /// Whether mail can be encrypted to the user's own certificate for an
    /// address (encrypted drafts and Sent copies).
    pub(crate) fn can_encrypt_to_self(&self, email: &str) -> Result<bool> {
        Ok(self.own_certificates(email)?.1.is_some())
    }

    /// What S/MIME can do for a message from `from` to `recipients`.
    pub(crate) fn smime_compose(&self, from: &str, recipients: &[String]) -> Result<SmimeCompose> {
        let now = now_ms();
        let (sign, encrypt) = self.own_certificates(from)?;
        let mut out = SmimeCompose {
            own: sign
                .as_ref()
                .or(encrypt.as_ref())
                .map(|(row, cert)| CertificateInfo::new(cert, Some(row), now)),
            can_sign: sign.is_some(),
            ..Default::default()
        };
        let mut seen = HashSet::new();
        for r in recipients {
            let email = r.trim().to_lowercase();
            if !email.contains('@') || !seen.insert(email.clone()) {
                continue;
            }
            let found = self.db.read(|c| recipient_certificate(c, &email, now))?;
            if found.is_none() {
                out.missing.push(email.clone());
            }
            out.recipients.push(RecipientCertificate {
                email,
                certificate: found.map(|(row, cert)| CertificateInfo::new(&cert, Some(&row), now)),
            });
        }
        out.can_encrypt = encrypt.is_some() && !out.recipients.is_empty() && out.missing.is_empty();
        Ok(out)
    }

    /// The standard to use when a message does not choose: S/MIME for an
    /// answer to S/MIME mail (with or without a certificate of one's own, so
    /// that importing one is offered rather than an OpenPGP key), and when
    /// the sender has a certificate and either no OpenPGP key or a way to
    /// encrypt to everyone with S/MIME but not with OpenPGP.
    pub(crate) fn default_standard(
        &self,
        from: &str,
        reply_to: Option<i64>,
        smime: &SmimeCompose,
        pgp_can_encrypt: bool,
    ) -> Result<&'static str> {
        let replied = match reply_to {
            Some(id) => self.db.read(|c| crypto_store::message_crypto_kind(c, id))?,
            None => None,
        };
        if let Some(kind) = &replied
            && kind.starts_with("smime-")
        {
            return Ok("smime");
        }
        if smime.own.is_none() || replied.is_some() {
            return Ok("openpgp");
        }
        if self.own_key_for(&from.trim().to_lowercase())?.is_none() {
            return Ok("smime");
        }
        Ok(if smime.can_encrypt && !pgp_can_encrypt {
            "smime"
        } else {
            "openpgp"
        })
    }

    /// Whether a draft goes out with S/MIME: as the composer chose, else by
    /// the default for its sender, recipients and conversation.
    pub(crate) fn uses_smime(&self, draft: &DraftInput, from: &str) -> Result<bool> {
        if let Some(choice) = draft.smime {
            return Ok(choice);
        }
        let recipients: Vec<String> = draft
            .to
            .iter()
            .chain(&draft.cc)
            .chain(&draft.bcc)
            .map(|a| a.email.clone())
            .collect();
        let smime = self.smime_compose(from, &recipients)?;
        let pgp_can_encrypt = self.pgp_can_encrypt(from, &recipients)?;
        Ok(
            self.default_standard(from, draft.reply_to_message_id, &smime, pgp_can_encrypt)?
                == "smime",
        )
    }

    /// Plans signing and encryption with S/MIME. For server copies of
    /// drafts (`for_draft`), an encrypted draft is encrypted to the sender
    /// alone and nothing is signed.
    pub(crate) async fn plan_smime(
        &self,
        draft: &DraftInput,
        email: &str,
        for_draft: bool,
    ) -> Result<SmimePlan> {
        let encrypt = draft.encrypt == Some(true);
        let sign = draft.sign == Some(true) && !for_draft;
        let (own_sign, own_encrypt) = self.own_certificates(email)?;
        let mut plan = SmimePlan::default();
        if sign {
            let (row, cert) = own_sign.ok_or_else(|| {
                Error::Invalid(format!(
                    "Import your certificate for {email} (a .p12 or .pfx file) in Settings → Encryption to sign with S/MIME."
                ))
            })?;
            let key = self.load_smime_key(&row.fingerprint).await?;
            let authorities: Vec<Certificate> = self
                .db
                .read(|c| Ok(parsed(store::certs_ca(c)?)))?
                .into_iter()
                .map(|(_, c)| c)
                .collect();
            let chain = chain::issuers_of(&cert, &authorities);
            plan.sign = Some(SignPlan {
                encryption: own_encrypt
                    .as_ref()
                    .map(|(_, c)| c.clone())
                    .filter(|c| *c != cert),
                cert,
                key,
                chain,
            });
        }
        if encrypt {
            let (_, own) = own_encrypt.ok_or_else(|| {
                Error::Invalid(format!(
                    "Import your certificate for {email} (a .p12 or .pfx file) in Settings → Encryption to send encrypted S/MIME mail."
                ))
            })?;
            plan.encrypt_to.push(own);
            if !for_draft {
                let now = now_ms();
                let mut seen: HashSet<String> = HashSet::from([email.to_string()]);
                let mut missing = Vec::new();
                for a in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
                    let e = a.email.trim().to_lowercase();
                    if !seen.insert(e.clone()) {
                        continue;
                    }
                    match self.db.read(|c| recipient_certificate(c, &e, now))? {
                        Some((_, cert)) => plan.encrypt_to.push(cert),
                        None => missing.push(e),
                    }
                }
                if !missing.is_empty() {
                    return Err(Error::Invalid(format!(
                        "Can't encrypt: there is no certificate for {}. Turn encryption off, or ask them for a signed message.",
                        missing.join(", ")
                    )));
                }
            }
        }
        Ok(plan)
    }
}

/// How an outgoing message is protected with S/MIME.
#[derive(Default)]
pub(crate) struct SmimePlan {
    sign: Option<SignPlan>,
    encrypt_to: Vec<Certificate>,
}

struct SignPlan {
    cert: Certificate,
    key: PrivateKey,
    chain: Vec<Certificate>,
    encryption: Option<Certificate>,
}

impl SmimePlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.sign.is_none() && self.encrypt_to.is_empty()
    }

    /// Signs and/or encrypts a rendered message.
    pub(crate) fn apply(&self, mime: &[u8]) -> Result<Vec<u8>> {
        let protect = Protect {
            sign: self.sign.as_ref().map(|s| SignWith {
                cert: &s.cert,
                key: &s.key,
                chain: &s.chain,
                encryption: s.encryption.as_ref(),
                time: envelope_smime::now(),
            }),
            encrypt_to: self.encrypt_to.iter().collect(),
        };
        Ok(smime_mime::protect(mime, &protect)?)
    }
}
