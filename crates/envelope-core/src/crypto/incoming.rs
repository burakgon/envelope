//! Receiving: Autocrypt headers, noting OpenPGP and S/MIME content when
//! messages are downloaded, and decrypting or verifying them when they are
//! opened.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use envelope_pgp::autocrypt;
use envelope_pgp::cert::{self, Cert};
use envelope_pgp::ops::{self, Issuer, SigResult, SigStatus};
use envelope_pgp::pgp::composed::{SignedPublicKey, SignedSecretKey};
use envelope_pgp::pgpmime::{self, Kind};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use super::keys::{KeyInfo, public_of, store_cert};
use super::smime::{self, SmimeSignature};
use super::{ENCRYPTED_SNIPPET, Opened, Protection};
use crate::db::crypto::{self as store, Peer};
use crate::db::mail::{self, MessageMeta};
use crate::db::now_ms;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mime::{self, ParsedBody};
use crate::model::{AttachmentInfo, EngineEvent, MessageBody};

/// One checked signature, for the interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureView {
    /// `good`, `bad` or `unknownKey` (for S/MIME: the signature fits, it
    /// does not, or it cannot be checked).
    pub status: String,
    /// The signer's OpenPGP key, when known.
    pub signer: Option<KeyInfo>,
    /// Fingerprint or key id named in the signature.
    pub issuer: Option<String>,
    /// Unix ms.
    pub created: Option<i64>,
    /// The signing key carries the sender's address.
    pub sender_matches: bool,
    /// An S/MIME signature: its certificate and how each check went.
    pub smime: Option<SmimeSignature>,
}

/// An encrypted or signed message, opened.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedMessage {
    pub message_id: i64,
    /// `encrypted`, `signed`, `inline-encrypted` or `inline-signed`; for
    /// S/MIME `smime-encrypted`, `smime-signed` or `smime-opaque`.
    pub kind: String,
    pub encrypted: bool,
    /// `ok`, `locked` (a passphrase is needed), `noKey` (not encrypted to any
    /// of the user's keys or certificates) or `error`.
    pub status: String,
    pub error: Option<String>,
    /// The key whose passphrase is needed (`locked`).
    pub locked_key: Option<KeyInfo>,
    /// The decrypted content, or the text of an inline signed message.
    /// `None` when the stored body is the content (PGP/MIME signed).
    pub body: Option<MessageBody>,
    /// Attachments inside the encryption (or the opaque signature).
    pub attachments: Vec<AttachmentInfo>,
    /// The real subject (protected headers).
    pub subject: Option<String>,
    pub signatures: Vec<SignatureView>,
    /// The signature covers only part of the message (for example, a
    /// mailing list added a footer).
    pub partially_signed: bool,
    /// The message has unencrypted parts besides the encrypted one (they
    /// are not shown).
    pub partially_encrypted: bool,
}

impl OpenedMessage {
    fn new(message_id: i64, kind: Protection) -> OpenedMessage {
        OpenedMessage {
            message_id,
            kind: kind.as_str().to_string(),
            encrypted: kind.is_encrypted(),
            status: "ok".into(),
            error: None,
            locked_key: None,
            body: None,
            attachments: Vec::new(),
            subject: None,
            signatures: Vec::new(),
            partially_signed: false,
            partially_encrypted: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Attachments inside encrypted messages get ids of their own: negative, with
// the message id and the part index packed in.

const PART_BITS: i64 = 12;
const PART_MASK: i64 = (1 << PART_BITS) - 1;

pub(crate) fn pgp_attachment_id(message_id: i64, index: usize) -> Option<i64> {
    let index = i64::try_from(index).ok()?;
    (index <= PART_MASK && message_id > 0).then(|| -((message_id << PART_BITS) | index))
}

pub(crate) fn split_attachment_id(id: i64) -> Option<(i64, usize)> {
    if id >= 0 {
        return None;
    }
    let v = id.checked_neg()?;
    Some((v >> PART_BITS, (v & PART_MASK) as usize))
}

// ---------------------------------------------------------------------------
// Autocrypt

/// Addresses that belong to the user (identities and accounts).
pub(crate) fn my_emails(c: &Connection) -> Result<HashSet<String>> {
    let mut st = c.prepare_cached(
        "SELECT lower(trim(email)) FROM identities UNION SELECT lower(trim(email)) FROM accounts",
    )?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Updates the Autocrypt state of the sender of a message (Autocrypt Level
/// 1, "Updating Autocrypt Peer State"). Messages from the user and messages
/// older than what is known change nothing.
pub(crate) fn autocrypt_seen(
    tx: &Transaction<'_>,
    from: &str,
    date: i64,
    header: Option<&str>,
    mine: &HashSet<String>,
) -> Result<()> {
    let from = from.trim().to_lowercase();
    if !from.contains('@') || mine.contains(&from) {
        return Ok(());
    }
    let effective = date.min(now_ms());
    let existing = store::peer_get(tx, &from)?;
    let parsed = header
        .and_then(autocrypt::parse)
        .filter(|h| h.addr.trim().eq_ignore_ascii_case(&from));
    // Only addresses that sent a key are tracked.
    if existing.is_none() && parsed.is_none() {
        return Ok(());
    }
    let mut peer = existing.unwrap_or_else(|| Peer {
        email: from.clone(),
        ..Default::default()
    });
    if effective < peer.autocrypt_at {
        return Ok(());
    }
    let mut changed = false;
    if effective > peer.last_seen {
        peer.last_seen = effective;
        changed = true;
    }
    if let Some(h) = parsed
        && let Ok(public) = cert::parse_public(&h.keydata)
        && cert::is_usable(&public, effective / 1000)
    {
        let fingerprint = cert::fingerprint(&public);
        let same = peer.fingerprint.as_deref() == Some(fingerprint.as_str())
            && store::key_get(tx, &fingerprint)?
                .is_some_and(|k| k.data == h.keydata || k.source != "autocrypt");
        if !same {
            store_cert(
                tx,
                &Cert::from_public(public),
                "autocrypt",
                std::slice::from_ref(&from),
            )?;
        }
        peer.fingerprint = Some(fingerprint);
        peer.autocrypt_at = effective;
        peer.prefer_encrypt = h.prefer_encrypt;
        changed = true;
    }
    if changed {
        store::peer_save(tx, &peer)?;
    }
    Ok(())
}

/// Autocrypt for a message whose headers were just stored.
pub(crate) fn on_headers(
    tx: &Transaction<'_>,
    meta: &MessageMeta,
    mine: &HashSet<String>,
) -> Result<()> {
    match &meta.from {
        Some(from) => autocrypt_seen(tx, &from.email, meta.date, meta.autocrypt.as_deref(), mine),
        None => Ok(()),
    }
}

/// Notes the OpenPGP or S/MIME content of a downloaded message and adapts
/// what is stored: encrypted messages keep no readable body until they are
/// opened, signatures are not listed as attachments, inline signed text
/// loses its armor. Returns the kind of protection.
pub(crate) fn on_source(
    tx: &Transaction<'_>,
    message_id: i64,
    raw: &[u8],
    meta: &MessageMeta,
    body: &mut ParsedBody,
) -> Result<Option<Protection>> {
    let mine = my_emails(tx)?;
    // Keys from spam are not taken.
    let junk: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
           WHERE mf.message_id = ? AND f.role = 'junk')",
        [message_id],
        |r| r.get(0),
    )?;
    if let Some(from) = &meta.from
        && !junk
    {
        autocrypt_seen(tx, &from.email, meta.date, meta.autocrypt.as_deref(), &mine)?;
    }
    let Some(protection) = Protection::detect(raw) else {
        store::message_crypto_delete(tx, message_id)?;
        return Ok(None);
    };
    store::message_crypto_set(tx, message_id, protection.as_str(), raw)?;
    let kind = match protection {
        Protection::Pgp(kind) => kind,
        Protection::Smime(kind) => {
            let sender = meta.from.as_ref().map(|f| f.email.trim().to_lowercase());
            smime::on_source(tx, kind, raw, sender.as_deref(), junk, body)?;
            return Ok(Some(protection));
        }
    };
    match kind {
        Kind::Encrypted => {
            body.html = None;
            body.text = None;
            body.snippet = ENCRYPTED_SNIPPET.into();
            body.attachments.clear();
            body.calendar = None;
        }
        // The text stays (armored ciphertext, perhaps with words around it),
        // to be shown as it is when it cannot be decrypted.
        Kind::InlineEncrypted => body.snippet = ENCRYPTED_SNIPPET.into(),
        Kind::Signed => body
            .attachments
            .retain(|a| a.mime_type != "application/pgp-signature"),
        Kind::InlineSigned => {
            if let Some(text) = &body.text
                && let Some((clear, _, _)) = cleartext(text, &|_| Vec::new())
            {
                body.snippet = mime::make_snippet(&clear);
                body.text = Some(clear);
            }
        }
    }
    Ok(Some(protection))
}

/// The text of a message with its cleartext signed block unwrapped, and the
/// signature results.
fn cleartext(text: &str, lookup: ops::Lookup<'_>) -> Option<(String, Vec<SigResult>, bool)> {
    let range = pgpmime::find_block(text, pgpmime::SIGNED_BEGIN, pgpmime::SIGNATURE_END)?;
    let (clear, results) = ops::verify_cleartext(&text[range.clone()], lookup).ok()?;
    let (before, after) = (&text[..range.start], &text[range.end..]);
    let partial = !before.trim().is_empty() || !after.trim().is_empty();
    Some((format!("{before}{clear}{after}"), results, partial))
}

// ---------------------------------------------------------------------------
// Opening

/// Why decryption did not happen.
pub(crate) enum Failure {
    Locked(String, Option<String>),
    NoKey,
    /// S/MIME: not encrypted to a certificate whose private key is here.
    NoCertificate,
    Error(String),
}

impl Engine {
    /// The decrypted content (a MIME entity) of a PGP/MIME or S/MIME
    /// message read from a file (also the content of S/MIME opaque signed
    /// data), or `None` when it is not encrypted.
    pub(crate) async fn decrypt_file_message(&self, raw: &[u8]) -> Result<Option<Vec<u8>>> {
        if let Some(Protection::Smime(kind)) = Protection::detect(raw) {
            let Some(payload) = envelope_smime::mime::payload(raw) else {
                return Ok(None);
            };
            return match kind {
                envelope_smime::Kind::Signed => Ok(None),
                envelope_smime::Kind::Opaque => {
                    let (shown, _) = self.smime_inner(raw.to_vec(), None)?;
                    Ok((shown != raw).then_some(shown))
                }
                envelope_smime::Kind::Encrypted => match self.smime_decrypt(&payload.cms).await {
                    Ok(content) => Ok(Some(self.smime_inner(content, None)?.0)),
                    Err(Failure::NoCertificate) => Err(Error::Invalid(
                        "This message is encrypted to a certificate whose private key is not on this computer.".into(),
                    )),
                    Err(Failure::Error(e)) => Err(Error::Invalid(e)),
                    Err(_) => Ok(None),
                },
            };
        }
        let Some(payload) = pgpmime::encrypted_payload(raw) else {
            return Ok(None);
        };
        match self.decrypt_payload(&payload).await {
            Ok(d) => Ok(Some(d.data)),
            Err(Failure::NoKey) => Err(Error::Invalid(
                "This message is encrypted to a key that is not on this computer.".into(),
            )),
            Err(Failure::Locked(..)) => Err(Error::Invalid(
                "Your key is locked. Open an encrypted message in Envelope to enter its passphrase, then open this file again.".into(),
            )),
            Err(Failure::Error(e)) => Err(Error::Invalid(e)),
            Err(Failure::NoCertificate) => Ok(None),
        }
    }

    /// Decrypts and/or verifies a message (kept for the session; with the
    /// "remember decrypted messages" setting, also stored for search).
    pub async fn open_message(&self, message_id: i64) -> Result<OpenedMessage> {
        Ok(self.open_inner(message_id).await?.view.clone())
    }

    pub(crate) async fn open_inner(&self, message_id: i64) -> Result<Arc<Opened>> {
        if let Some(o) = self.crypto.opened(message_id) {
            return Ok(o);
        }
        let row = self
            .db
            .read(|c| store::message_crypto_get(c, message_id))?
            .ok_or_else(|| {
                Error::NotFound("This message is neither encrypted nor signed.".into())
            })?;
        let protection = Protection::parse(&row.kind)
            .ok_or_else(|| Error::Invalid(format!("unknown protected content {}", row.kind)))?;
        let raw = match row.raw {
            Some(raw) => raw,
            None => self.message_source_bytes(message_id).await?,
        };
        let message = self.db.read(|c| mail::get_message(c, message_id))?;
        let sender = message.from.as_ref().map(|f| f.email.trim().to_lowercase());
        let sender = sender.as_deref();
        let mut view = OpenedMessage::new(message_id, protection);
        let mut entity = None;
        let kind = match protection {
            Protection::Pgp(kind) => kind,
            Protection::Smime(kind) => {
                entity = self
                    .open_smime(message_id, message.date, kind, &raw, sender, &mut view)
                    .await?
                    .map(Arc::new);
                return self.remember(message_id, view, entity, row.decrypted);
            }
        };
        match kind {
            Kind::Encrypted => {
                let payload = pgpmime::encrypted_payload(&raw).ok_or_else(|| {
                    Error::Invalid("The encrypted part of this message is missing.".into())
                })?;
                view.partially_encrypted = !pgpmime::encrypted_is_whole(&raw);
                match self.decrypt_payload(&payload).await {
                    Ok(d) => {
                        let signatures = self.signature_views(d.signatures, sender);
                        let shown = self.show_entity(
                            message_id,
                            message.date,
                            &d.data,
                            signatures,
                            sender,
                            &mut view,
                        )?;
                        entity = Some(Arc::new(shown));
                    }
                    Err(f) => self.failed(&mut view, f),
                }
            }
            Kind::InlineEncrypted => {
                let text = mime::parse_full(&raw)
                    .and_then(|(_, b)| b.text)
                    .unwrap_or_default();
                let range =
                    pgpmime::find_block(&text, pgpmime::MESSAGE_BEGIN, pgpmime::MESSAGE_END)
                        .ok_or_else(|| Error::Invalid("The encrypted text is missing.".into()))?;
                match self.decrypt_payload(text[range.clone()].as_bytes()).await {
                    Ok(d) => {
                        let plain = String::from_utf8_lossy(&d.data).replace("\r\n", "\n");
                        let (before, after) = (&text[..range.start], &text[range.end..]);
                        let partial = !before.trim().is_empty() || !after.trim().is_empty();
                        view.partially_encrypted = partial;
                        view.partially_signed = !d.signatures.is_empty() && partial;
                        view.body = Some(MessageBody {
                            html: None,
                            text: Some(format!("{before}{plain}{after}")),
                        });
                        view.signatures = self.signature_views(d.signatures, sender);
                    }
                    Err(f) => self.failed(&mut view, f),
                }
            }
            Kind::Signed => {
                let parts = pgpmime::signed_parts(&raw)
                    .ok_or_else(|| Error::Invalid("The signed part is missing.".into()))?;
                match ops::verify_detached(&parts.data, &parts.signature, &|i| self.certs_for(i)) {
                    Ok(results) => {
                        view.signatures = self.signature_views(results, sender);
                        view.partially_signed = !parts.whole;
                    }
                    Err(e) => {
                        view.status = "error".into();
                        view.error = Some(e.to_string());
                    }
                }
            }
            Kind::InlineSigned => {
                let text = mime::parse_full(&raw)
                    .and_then(|(_, b)| b.text)
                    .unwrap_or_default();
                match cleartext(&text, &|i| self.certs_for(i)) {
                    Some((clear, results, partial)) => {
                        view.body = Some(MessageBody {
                            html: None,
                            text: Some(clear),
                        });
                        view.signatures = self.signature_views(results, sender);
                        view.partially_signed = partial;
                    }
                    None => {
                        view.status = "error".into();
                        view.error = Some("The signed text is unreadable.".into());
                    }
                }
            }
        }
        self.remember(message_id, view, entity, row.decrypted)
    }

    /// Keeps an opened message for the session (and, with the setting, its
    /// decrypted text for search).
    fn remember(
        &self,
        message_id: i64,
        view: OpenedMessage,
        entity: Option<Arc<Vec<u8>>>,
        kept: bool,
    ) -> Result<Arc<Opened>> {
        let opened = Arc::new(Opened { view, entity });
        if opened.view.status == "ok" {
            self.crypto.remember_opened(message_id, opened.clone());
            if opened.view.encrypted
                && !kept
                && self.crypto_settings()?.keep_decrypted
                && let Err(e) = self.keep_decrypted_copy(message_id, &opened.view)
            {
                tracing::warn!("could not keep the decrypted copy of message {message_id}: {e}");
            }
        }
        Ok(opened)
    }

    pub(crate) fn failed(&self, view: &mut OpenedMessage, failure: Failure) {
        match failure {
            Failure::Locked(fingerprint, error) => {
                view.status = "locked".into();
                view.error = error;
                view.locked_key = self
                    .key_row(&fingerprint)
                    .ok()
                    .map(|k| KeyInfo::from_row(&k, now_ms()));
            }
            Failure::NoKey => {
                view.status = "noKey".into();
                view.error = Some(
                    "This message was not encrypted to any of your keys. It can only be read where the key it was encrypted to is."
                        .into(),
                );
            }
            Failure::NoCertificate => {
                view.status = "noKey".into();
                view.error = Some(
                    "This message was encrypted to a certificate whose private key is not on this computer. Import that certificate with its private key (a .p12 or .pfx file) in Settings → Encryption to read it."
                        .into(),
                );
            }
            Failure::Error(e) => {
                view.status = "error".into();
                view.error = Some(e);
            }
        }
    }

    /// Decrypts an OpenPGP message with the user's secret keys.
    async fn decrypt_payload(
        &self,
        payload: &[u8],
    ) -> std::result::Result<ops::Decrypted, Failure> {
        let wanted = ops::recipients(payload).map_err(|e| Failure::Error(e.to_string()))?;
        let anonymous = wanted.iter().any(|id| id == "0000000000000000");
        let rows = self
            .db
            .read(store::secret_keys)
            .map_err(|e| Failure::Error(e.to_string()))?;
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|r| anonymous || r.key_ids.iter().any(|id| wanted.contains(id)))
            .collect();
        if rows.is_empty() {
            return Err(Failure::NoKey);
        }
        let mut keys: Vec<(SignedSecretKey, Option<String>)> = Vec::new();
        for r in &rows {
            match self.load_secret(&r.fingerprint).await {
                Ok(k) => keys.push((k, self.crypto.passphrase(&r.fingerprint))),
                Err(e) => tracing::warn!("secret key {} unavailable: {e}", r.fingerprint),
            }
        }
        if keys.is_empty() {
            return Err(Failure::Error(
                "Your secret key is missing from the keyring.".into(),
            ));
        }
        let engine = self.clone();
        let payload = payload.to_vec();
        let fingerprints: Vec<String> = rows.iter().map(|r| r.fingerprint.clone()).collect();
        let result = tokio::task::spawn_blocking(move || {
            let refs: Vec<(&SignedSecretKey, Option<&str>)> =
                keys.iter().map(|(k, p)| (k, p.as_deref())).collect();
            ops::decrypt(&payload, &refs, &|i| engine.certs_for(i))
        })
        .await
        .map_err(|e| Failure::Error(e.to_string()))?;
        match result {
            Ok(d) => Ok(d),
            Err(envelope_pgp::Error::Locked(fingerprint)) => {
                Err(Failure::Locked(fingerprint, None))
            }
            Err(envelope_pgp::Error::BadPassphrase) => {
                // A remembered passphrase no longer works: ask again.
                for f in &fingerprints {
                    self.crypto.forget_passphrase(f);
                }
                let locked = rows
                    .iter()
                    .find(|r| r.protected)
                    .map(|r| r.fingerprint.clone())
                    .unwrap_or_else(|| fingerprints[0].clone());
                Err(Failure::Locked(locked, Some("Wrong passphrase".into())))
            }
            Err(envelope_pgp::Error::NoKey) => Err(Failure::NoKey),
            Err(e) => Err(Failure::Error(e.to_string())),
        }
    }

    /// Certificates that may have made a signature.
    pub(crate) fn certs_for(&self, issuer: &Issuer) -> Vec<SignedPublicKey> {
        self.db
            .read(|c| store::keys_by_key_ids(c, &issuer.key_ids))
            .unwrap_or_default()
            .iter()
            .filter_map(|r| public_of(r).ok())
            .collect()
    }

    fn signature_views(&self, results: Vec<SigResult>, sender: Option<&str>) -> Vec<SignatureView> {
        let now = now_ms();
        results
            .into_iter()
            .map(|r| {
                let signer = r
                    .signer
                    .as_deref()
                    .and_then(|f| self.db.read(|c| store::key_get(c, f)).ok().flatten());
                let sender_matches = match (&signer, sender) {
                    (Some(k), Some(s)) => k.emails.iter().any(|e| e == s),
                    _ => false,
                };
                SignatureView {
                    status: match r.status {
                        SigStatus::Good => "good",
                        SigStatus::Bad => "bad",
                        SigStatus::UnknownKey => "unknownKey",
                    }
                    .into(),
                    signer: signer.map(|k| KeyInfo::from_row(&k, now)),
                    issuer: r.issuer.label(),
                    created: r.created.map(|c| c * 1000),
                    sender_matches,
                    smime: None,
                }
            })
            .collect()
    }

    /// Fills the view from decrypted (or opaque signed) content. Returns
    /// the entity the attachment ids refer to.
    pub(crate) fn show_entity(
        &self,
        message_id: i64,
        date: i64,
        data: &[u8],
        mut signatures: Vec<SignatureView>,
        sender: Option<&str>,
        view: &mut OpenedMessage,
    ) -> Result<Vec<u8>> {
        // Signed, then encrypted: a multipart/signed entity inside.
        if pgpmime::detect(data) == Some(Kind::Signed)
            && let Some(parts) = pgpmime::signed_parts(data)
        {
            if let Ok(results) =
                ops::verify_detached(&parts.data, &parts.signature, &|i| self.certs_for(i))
            {
                signatures.extend(self.signature_views(results, sender));
            }
            view.partially_signed = !parts.whole;
        }
        let shown = pgpmime::strip_legacy_display(data);
        let (meta, body) = mime::parse_full(&shown)
            .ok_or_else(|| Error::Invalid("The decrypted message is unreadable.".into()))?;
        if pgpmime::has_protected_headers(data) && !meta.subject.is_empty() {
            view.subject = Some(meta.subject.clone());
        }
        view.body = Some(MessageBody {
            html: body.html,
            text: body.text,
        });
        view.attachments = body
            .attachments
            .iter()
            .filter(|a| {
                a.mime_type != "application/pgp-signature"
                    && !envelope_smime::mime::is_smime_type(&a.mime_type)
            })
            .filter_map(|a| {
                let index: usize = a.part_id.strip_prefix("mp:")?.parse().ok()?;
                Some(AttachmentInfo {
                    id: pgp_attachment_id(message_id, index)?,
                    filename: a.filename.clone(),
                    mime_type: a.mime_type.clone(),
                    size: a.size,
                    content_id: a.content_id.clone(),
                    inline: a.inline,
                })
            })
            .collect();
        view.signatures = signatures;
        // Keys of the other recipients, vouched for by the signed sender.
        if view
            .signatures
            .iter()
            .any(|s| s.smime.is_none() && s.status == "good" && s.sender_matches)
            && let Err(e) = self.take_gossip(message_id, date, data)
        {
            tracing::debug!("ignoring Autocrypt-Gossip: {e}");
        }
        Ok(shown)
    }

    /// Stores `Autocrypt-Gossip` keys from the protected headers of a
    /// message, for its recipients only.
    fn take_gossip(&self, message_id: i64, date: i64, entity: &[u8]) -> Result<()> {
        let Some(msg) = mail_parser::MessageParser::new().parse(entity) else {
            return Ok(());
        };
        let headers: Vec<String> = msg
            .root()
            .headers()
            .all("Autocrypt-Gossip")
            .map(|h| String::from_utf8_lossy(h.raw_value()).into_owned())
            .collect();
        if headers.is_empty() {
            return Ok(());
        }
        let message = self.db.read(|c| mail::get_message(c, message_id))?;
        let recipients: HashSet<String> = message
            .to
            .iter()
            .chain(&message.cc)
            .map(|a| a.email.trim().to_lowercase())
            .collect();
        self.db.write(|tx| {
            let mine = my_emails(tx)?;
            for value in &headers {
                let Some(g) = autocrypt::parse_gossip(value) else {
                    continue;
                };
                let addr = g.addr.trim().to_lowercase();
                if !recipients.contains(&addr) || mine.contains(&addr) {
                    continue;
                }
                let Ok(public) = cert::parse_public(&g.keydata) else {
                    continue;
                };
                let mut peer = store::peer_get(tx, &addr)?.unwrap_or_else(|| Peer {
                    email: addr.clone(),
                    ..Default::default()
                });
                if date <= peer.gossip_at {
                    continue;
                }
                let row = store_cert(
                    tx,
                    &Cert::from_public(public),
                    "gossip",
                    std::slice::from_ref(&addr),
                )?;
                peer.gossip_at = date;
                peer.gossip_fingerprint = Some(row.fingerprint);
                store::peer_save(tx, &peer)?;
            }
            Ok(())
        })
    }

    /// Writes the decrypted text and subject into the database so the
    /// message shows a preview and can be searched.
    fn keep_decrypted_copy(&self, message_id: i64, view: &OpenedMessage) -> Result<()> {
        let Some(body) = &view.body else {
            return Ok(());
        };
        let text = body
            .text
            .clone()
            .or_else(|| body.html.as_deref().map(mime::html_to_text))
            .unwrap_or_default();
        let snippet = mime::make_snippet(&text);
        let (account_id, thread_id, folders) = self.db.write(|tx| {
            if let Some(subject) = &view.subject {
                tx.execute(
                    "UPDATE messages SET subject = ? WHERE id = ?",
                    rusqlite::params![subject, message_id],
                )?;
            }
            mail::store_body(
                tx,
                message_id,
                body.html.as_deref(),
                Some(&text),
                &snippet,
                None,
            )?;
            store::message_crypto_set_decrypted(tx, message_id, true)?;
            let (account_id, thread_id): (i64, i64) = tx.query_row(
                "SELECT account_id, thread_id FROM messages WHERE id = ?",
                [message_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let folders = mail::refresh_threads(tx, &BTreeSet::from([thread_id]))?;
            mail::refresh_folder_counts(tx, &folders)?;
            Ok((account_id, thread_id, folders))
        })?;
        self.emit(EngineEvent::MailChanged {
            account_id,
            folder_ids: folders.into_iter().collect(),
            thread_ids: vec![thread_id],
        });
        Ok(())
    }

    /// Removes every kept decrypted copy (the setting was turned off).
    pub(crate) fn forget_decrypted_copies(&self) -> Result<()> {
        let ids = self.db.read(store::decrypted_messages)?;
        for id in ids {
            let row = self.db.read(|c| store::message_crypto_get(c, id))?;
            let inline = row.as_ref().is_some_and(|r| r.kind == "inline-encrypted");
            let raw = row.and_then(|r| r.raw);
            let outer_subject = raw
                .as_ref()
                .map(|raw| {
                    let end = raw
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| p + 4)
                        .unwrap_or(raw.len().min(65_536));
                    mime::meta_from_headers(&raw[..end]).subject
                })
                .unwrap_or_else(|| pgpmime::HIDDEN_SUBJECT.to_string());
            // Inline PGP goes back to its armored text.
            let text = raw
                .filter(|_| inline)
                .and_then(|raw| mime::parse_full(&raw))
                .and_then(|(_, body)| body.text);
            self.db.write(|tx| {
                tx.execute(
                    "UPDATE messages SET subject = ?, snippet = ? WHERE id = ?",
                    rusqlite::params![outer_subject, ENCRYPTED_SNIPPET, id],
                )?;
                tx.execute(
                    "UPDATE bodies SET html = NULL, text = ? WHERE message_id = ?",
                    rusqlite::params![text.as_deref().map(crate::db::pack), id],
                )?;
                mail::index_message(tx, id, None)?;
                store::message_crypto_set_decrypted(tx, id, false)?;
                if let Ok(thread) =
                    tx.query_row("SELECT thread_id FROM messages WHERE id = ?", [id], |r| {
                        r.get::<_, i64>(0)
                    })
                {
                    let folders = mail::refresh_threads(tx, &BTreeSet::from([thread]))?;
                    mail::refresh_folder_counts(tx, &folders)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Decrypts newly downloaded encrypted messages in the background when
    /// decrypted copies are kept (so they can be searched), as far as that
    /// works without a passphrase.
    pub(crate) fn decrypt_later(&self, message_ids: Vec<i64>) {
        if message_ids.is_empty()
            || !self
                .crypto_settings()
                .map(|s| s.keep_decrypted)
                .unwrap_or(false)
        {
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            for id in message_ids {
                let _ = engine.open_inner(id).await;
            }
        });
    }

    /// The local file of an attachment inside an encrypted message
    /// (decrypted on demand; removed when Envelope starts again).
    pub(crate) async fn decrypted_attachment_file(&self, attachment_id: i64) -> Result<PathBuf> {
        let (message_id, index) = split_attachment_id(attachment_id)
            .ok_or_else(|| Error::NotFound(format!("attachment {attachment_id}")))?;
        let opened = self.open_inner(message_id).await?;
        if let Some(key) = &opened.view.locked_key {
            return Err(Error::Locked(key.fingerprint.clone()));
        }
        let entity = opened
            .entity
            .clone()
            .ok_or_else(|| Error::NotFound("The message could not be decrypted.".into()))?;
        let att = opened
            .view
            .attachments
            .iter()
            .find(|a| a.id == attachment_id)
            .ok_or_else(|| Error::NotFound(format!("attachment {attachment_id}")))?;
        let dir = self
            .paths
            .cache
            .join("decrypted")
            .join(format!("message-{message_id}"))
            .join(index.to_string());
        let safe: String = att
            .filename
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | '\0') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let path = dir.join(if safe.trim().is_empty() {
            "attachment".to_string()
        } else {
            safe
        });
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(path);
        }
        let data = mime::extract_part(&entity, &format!("mp:{index}"))
            .ok_or_else(|| Error::NotFound(format!("attachment {attachment_id}")))?;
        tokio::fs::create_dir_all(&dir).await?;
        write_private(&path, &data).await?;
        Ok(path)
    }

    /// An inline (`cid:`) image of an encrypted message, if it is one.
    pub(crate) async fn decrypted_inline(
        &self,
        message_id: i64,
        cid: &str,
    ) -> Result<Option<(String, PathBuf)>> {
        let contained = self
            .db
            .read(|c| store::message_crypto_kind(c, message_id))?
            .and_then(|k| Protection::parse(&k))
            .is_some_and(Protection::is_contained);
        if !contained {
            return Ok(None);
        }
        let opened = self.open_inner(message_id).await?;
        let Some(att) = opened
            .view
            .attachments
            .iter()
            .find(|a| a.content_id.as_deref() == Some(cid))
        else {
            return Err(Error::NotFound(format!("inline image {cid}")));
        };
        let path = self.decrypted_attachment_file(att.id).await?;
        Ok(Some((att.mime_type.clone(), path)))
    }
}

/// Writes a file only the user can read.
async fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let tmp = path.with_extension("part");
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut f = options.open(&tmp).await?;
    f.write_all(data).await?;
    f.flush().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_ids_round_trip() {
        let id = pgp_attachment_id(12345, 7).unwrap();
        assert!(id < 0);
        assert_eq!(split_attachment_id(id), Some((12345, 7)));
        assert_eq!(split_attachment_id(5), None);
        assert!(pgp_attachment_id(1, 5000).is_none());
    }
}
