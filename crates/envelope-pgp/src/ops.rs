//! Encrypting, decrypting, signing and verifying.

use std::io::Cursor;

use pgp::composed::{
    ArmorOptions, CleartextSignedMessage, Deserializable, DetachedSignature, Esk, Message,
    MessageBuilder, SignedPublicKey, SignedPublicSubKey, SignedSecretKey, TheRing,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::packet::{PublicKeyEncryptedSessionKey, Signature};
use pgp::types::{KeyDetails, Password, VerifyingKey};

use crate::cert::{self, EncryptionKey, SigningKey};
use crate::{Error, Result};

/// The secret key that signs, and its passphrase when it has one.
#[derive(Debug, Clone, Copy)]
pub struct Signer<'a> {
    pub key: &'a SignedSecretKey,
    pub passphrase: Option<&'a str>,
}

/// Who made a signature, as named inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issuer {
    /// Upper-case hex fingerprints (of the signing key or subkey).
    pub fingerprints: Vec<String>,
    /// Upper-case hex key ids, including those derived from fingerprints.
    pub key_ids: Vec<String>,
}

impl Issuer {
    fn of(sig: &Signature) -> Issuer {
        let fingerprints: Vec<String> = sig
            .issuer_fingerprint()
            .iter()
            .map(|f| f.to_string().to_uppercase())
            .collect();
        let mut key_ids: Vec<String> = sig
            .issuer_key_id()
            .iter()
            .map(|k| k.to_string().to_uppercase())
            .collect();
        for fp in &fingerprints {
            let id = cert::key_id_of_fingerprint(fp);
            if !key_ids.contains(&id) {
                key_ids.push(id);
            }
        }
        Issuer {
            fingerprints,
            key_ids,
        }
    }

    /// The most precise name of the issuer.
    pub fn label(&self) -> Option<String> {
        self.fingerprints
            .first()
            .or_else(|| self.key_ids.first())
            .cloned()
    }
}

/// Outcome of checking one signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigStatus {
    /// Made by the named certificate, over exactly this content.
    Good,
    /// The signer's key is known, but the signature does not match.
    Bad,
    /// No known certificate carries the signing key.
    UnknownKey,
}

/// One checked signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigResult {
    pub status: SigStatus,
    /// Fingerprint of the signer's certificate (primary key), when known.
    pub signer: Option<String>,
    pub issuer: Issuer,
    /// Unix seconds.
    pub created: Option<i64>,
}

/// Finds certificates that may have made a signature.
pub type Lookup<'a> = &'a dyn Fn(&Issuer) -> Vec<SignedPublicKey>;

/// A public key a signature may have been made with.
#[derive(Clone, Copy)]
enum Candidate<'a> {
    Primary(&'a pgp::packet::PublicKey),
    Subkey(&'a SignedPublicSubKey),
}

impl Candidate<'_> {
    fn as_dyn(&self) -> &dyn VerifyingKey {
        match self {
            Candidate::Primary(k) => *k,
            Candidate::Subkey(k) => *k,
        }
    }

    fn verify_detached(&self, sig: &Signature, data: &[u8]) -> bool {
        match self {
            Candidate::Primary(k) => sig.verify(*k, data).is_ok(),
            Candidate::Subkey(k) => sig.verify(*k, data).is_ok(),
        }
    }
}

fn check_signature(
    sig: &Signature,
    lookup: Lookup<'_>,
    verify: &dyn Fn(Candidate<'_>) -> bool,
) -> SigResult {
    let issuer = Issuer::of(sig);
    let created = sig.created().map(|t| i64::from(t.as_secs()));
    let certs = lookup(&issuer);
    let mut known = None;
    for c in &certs {
        known.get_or_insert_with(|| cert::fingerprint(c));
        let ok = verify(Candidate::Primary(&c.primary_key))
            || c.public_subkeys
                .iter()
                .any(|s| verify(Candidate::Subkey(s)));
        if ok {
            return SigResult {
                status: SigStatus::Good,
                signer: Some(cert::fingerprint(c)),
                issuer,
                created,
            };
        }
    }
    SigResult {
        status: if known.is_some() {
            SigStatus::Bad
        } else {
            SigStatus::UnknownKey
        },
        signer: known,
        issuer,
        created,
    }
}

fn read_message(data: &[u8]) -> Result<Message<'static>> {
    let cursor = Cursor::new(data.to_vec());
    let msg = if is_armored(data) {
        Message::from_armor(cursor)?.0
    } else {
        Message::from_bytes(cursor)?
    };
    Ok(msg)
}

fn is_armored(data: &[u8]) -> bool {
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    data[start..].starts_with(b"-----BEGIN PGP")
}

/// Decrypted content larger than this is refused (a compressed message can
/// expand enormously).
pub const MAX_PLAINTEXT: u64 = 256 * 1024 * 1024;

/// Reads a message's content to its end, at most [`MAX_PLAINTEXT`] bytes.
fn read_limited(msg: &mut Message<'_>) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::new();
    msg.take(MAX_PLAINTEXT + 1)
        .read_to_end(&mut data)
        .map_err(|e| Error::Pgp(format!("decryption failed: {e}")))?;
    if data.len() as u64 > MAX_PLAINTEXT {
        return Err(Error::Invalid("The message is too large to open".into()));
    }
    Ok(data)
}

/// `micalg` name of a hash algorithm for `multipart/signed`.
pub fn micalg(hash: HashAlgorithm) -> &'static str {
    match hash {
        HashAlgorithm::Sha1 => "pgp-sha1",
        HashAlgorithm::Sha224 => "pgp-sha224",
        HashAlgorithm::Sha384 => "pgp-sha384",
        HashAlgorithm::Sha512 => "pgp-sha512",
        HashAlgorithm::Sha3_256 => "pgp-sha3-256",
        HashAlgorithm::Sha3_512 => "pgp-sha3-512",
        _ => "pgp-sha256",
    }
}

fn unlock_signer<'a>(signer: &Signer<'a>, now: i64) -> Result<(SigningKey<'a>, Password)> {
    let key = cert::signing_key(signer.key, now)
        .ok_or_else(|| Error::Invalid("Your key cannot sign (expired or revoked)".into()))?;
    let password = match signer.passphrase {
        Some(p) => Password::from(p),
        None if key.is_protected() => {
            return Err(Error::Locked(cert::fingerprint(
                &signer.key.to_public_key(),
            )));
        }
        None => Password::empty(),
    };
    // A wrong passphrase is reported as such, before anything is signed.
    if key.is_protected() {
        let unlocked = match key {
            SigningKey::Primary(k) => k.unlock(&password, |_, _| Ok(())).is_ok_and(|r| r.is_ok()),
            SigningKey::Subkey(k) => k.unlock(&password, |_, _| Ok(())).is_ok_and(|r| r.is_ok()),
        };
        if !unlocked {
            return Err(Error::BadPassphrase);
        }
    }
    Ok((key, password))
}

fn hash_of(key: &SigningKey<'_>) -> HashAlgorithm {
    match key {
        SigningKey::Primary(k) => k.public_params().hash_alg(),
        SigningKey::Subkey(k) => k.public_params().hash_alg(),
    }
}

/// Encrypts `data` to the recipients (and to `hidden` ones whose key ids are
/// left out of the message), signing inside the encryption when a signer is
/// given. Returns an ASCII armored OpenPGP message.
pub fn encrypt(
    data: &[u8],
    recipients: &[&SignedPublicKey],
    hidden: &[&SignedPublicKey],
    signer: Option<&Signer<'_>>,
) -> Result<String> {
    let now = crate::now();
    let all: Vec<&SignedPublicKey> = recipients.iter().chain(hidden).copied().collect();
    if all.is_empty() {
        return Err(Error::Invalid("No one to encrypt to".into()));
    }
    let mut rng = rand::thread_rng();
    let alg = cert::common_symmetric(&all);
    let mut builder = MessageBuilder::from_bytes("", data.to_vec()).seipd_v1(&mut rng, alg);
    for (cert, anonymous) in recipients
        .iter()
        .map(|c| (c, false))
        .chain(hidden.iter().map(|c| (c, true)))
    {
        let keys = cert::encryption_keys(cert, now);
        if keys.is_empty() {
            return Err(Error::Invalid(format!(
                "The key {} cannot be used for encryption (expired, revoked or without an encryption subkey)",
                cert::fingerprint(cert)
            )));
        }
        for key in keys {
            match (key, anonymous) {
                (EncryptionKey::Primary(k), false) => builder.encrypt_to_key(&mut rng, k)?,
                (EncryptionKey::Subkey(k), false) => builder.encrypt_to_key(&mut rng, k)?,
                (EncryptionKey::Primary(k), true) => {
                    builder.encrypt_to_key_anonymous(&mut rng, k)?
                }
                (EncryptionKey::Subkey(k), true) => {
                    builder.encrypt_to_key_anonymous(&mut rng, k)?
                }
            };
        }
    }
    if let Some(signer) = signer {
        let (key, password) = unlock_signer(signer, now)?;
        let hash = hash_of(&key);
        match key {
            SigningKey::Primary(k) => builder.sign(k, password, hash),
            SigningKey::Subkey(k) => builder.sign(k, password, hash),
        };
    }
    Ok(builder.to_armored_string(&mut rng, ArmorOptions::default())?)
}

/// A detached signature over `data` (ASCII armored) and the hash used.
pub fn sign_detached(data: &[u8], signer: &Signer<'_>) -> Result<(String, HashAlgorithm)> {
    let (key, password) = unlock_signer(signer, crate::now())?;
    let hash = hash_of(&key);
    let rng = rand::thread_rng();
    let sig = match key {
        SigningKey::Primary(k) => {
            DetachedSignature::sign_binary_data(rng, k, &password, hash, data)
        }
        SigningKey::Subkey(k) => DetachedSignature::sign_binary_data(rng, k, &password, hash, data),
    }?;
    Ok((sig.to_armored_string(ArmorOptions::default())?, hash))
}

/// Checks the detached signature(s) in `signature` (armored or binary) over
/// `data`.
pub fn verify_detached(
    data: &[u8],
    signature: &[u8],
    lookup: Lookup<'_>,
) -> Result<Vec<SigResult>> {
    let (sigs, _) = DetachedSignature::from_reader_many(Cursor::new(signature.to_vec()))
        .map_err(|e| Error::Invalid(format!("Unreadable signature: {e}")))?;
    let mut out = Vec::new();
    for sig in sigs {
        let sig = sig.map_err(|e| Error::Invalid(format!("Unreadable signature: {e}")))?;
        let s = &sig.signature;
        out.push(check_signature(s, lookup, &|key| {
            key.verify_detached(s, data)
        }));
    }
    if out.is_empty() {
        return Err(Error::Invalid("No signature found".into()));
    }
    Ok(out)
}

/// Checks a cleartext signed message (`-----BEGIN PGP SIGNED MESSAGE-----`)
/// and returns its text (dash-unescaped, `\n` line ends) with the results.
pub fn verify_cleartext(block: &str, lookup: Lookup<'_>) -> Result<(String, Vec<SigResult>)> {
    let (msg, _) = CleartextSignedMessage::from_string(block)
        .map_err(|e| Error::Invalid(format!("Unreadable signed message: {e}")))?;
    let signed = msg.signed_text();
    let results = msg
        .signatures()
        .iter()
        .map(|s| check_signature(s, lookup, &|key| key.verify_detached(s, signed.as_bytes())))
        .collect();
    Ok((signed.replace("\r\n", "\n"), results))
}

/// Key ids a message is encrypted to (upper-case hex; all zeros for
/// anonymous recipients).
pub fn recipients(ciphertext: &[u8]) -> Result<Vec<String>> {
    let msg = read_message(ciphertext)?;
    let Message::Encrypted { esk, .. } = &msg else {
        return Err(Error::Invalid("This is not an encrypted message".into()));
    };
    Ok(esk
        .iter()
        .filter_map(|e| match e {
            Esk::PublicKeyEncryptedSessionKey(p) => Some(pkesk_key_id(p)),
            Esk::SymKeyEncryptedSessionKey(_) => None,
        })
        .collect())
}

fn pkesk_key_id(p: &PublicKeyEncryptedSessionKey) -> String {
    if let Ok(id) = p.id() {
        return id.to_string().to_uppercase();
    }
    match p.fingerprint() {
        Ok(Some(fp)) => cert::key_id_of_fingerprint(&fp.to_string()),
        _ => "0000000000000000".into(),
    }
}

/// A decrypted message.
#[derive(Debug, Clone)]
pub struct Decrypted {
    pub data: Vec<u8>,
    /// Signatures found inside the encryption, checked.
    pub signatures: Vec<SigResult>,
}

/// Decrypts a message (armored or binary) with the given secret keys and
/// checks the signatures inside it. Each key comes with its passphrase when
/// it needs one; a protected key without one makes this fail with
/// [`Error::Locked`] if it is the only key the message is for.
pub fn decrypt(
    ciphertext: &[u8],
    keys: &[(&SignedSecretKey, Option<&str>)],
    lookup: Lookup<'_>,
) -> Result<Decrypted> {
    let wanted = recipients(ciphertext)?;
    let anonymous = wanted.iter().any(|id| id == "0000000000000000");
    // The keys this message is for, by key id (any of them for anonymous
    // recipients).
    let matching: Vec<&(&SignedSecretKey, Option<&str>)> = keys
        .iter()
        .filter(|(k, _)| {
            anonymous
                || cert::key_ids(&k.to_public_key())
                    .iter()
                    .any(|id| wanted.contains(id))
        })
        .collect();
    if matching.is_empty() {
        return Err(Error::NoKey);
    }
    let usable: Vec<&(&SignedSecretKey, Option<&str>)> = matching
        .iter()
        .copied()
        .filter(|(k, p)| p.is_some() || !cert::is_protected(k))
        .collect();
    if usable.is_empty() {
        let first = matching[0].0;
        return Err(Error::Locked(cert::fingerprint(&first.to_public_key())));
    }
    let passwords: Vec<Password> = usable
        .iter()
        .filter_map(|(_, p)| p.map(Password::from))
        .collect();
    let ring = TheRing {
        secret_keys: usable.iter().map(|(k, _)| *k).collect(),
        key_passwords: passwords.iter().collect(),
        ..Default::default()
    };
    let msg = read_message(ciphertext)?;
    let mut msg = match msg.decrypt_the_ring(ring, true) {
        Ok((m, _)) => m,
        Err(e) => {
            let had_passwords = usable
                .iter()
                .any(|(k, p)| p.is_some() && cert::is_protected(k));
            return Err(match e {
                pgp::errors::Error::MissingKey if had_passwords => Error::BadPassphrase,
                pgp::errors::Error::MissingKey => Error::NoKey,
                e => e.into(),
            });
        }
    };
    while msg.is_compressed() {
        msg = msg.decompress()?;
    }
    let data = read_limited(&mut msg)?;
    let mut signatures = Vec::new();
    if let Message::Signed { reader, .. } = &msg {
        for i in 0..reader.num_signatures() {
            let Some(sig) = reader.signature(i) else {
                continue;
            };
            signatures.push(check_signature(sig, lookup, &|key| {
                msg.verify_nested_explicit(i, key.as_dyn()).is_ok()
            }));
        }
    }
    Ok(Decrypted { data, signatures })
}

/// Reads a signed (not encrypted) OpenPGP message and checks it.
pub fn verify_message(data: &[u8], lookup: Lookup<'_>) -> Result<Decrypted> {
    let mut msg = read_message(data)?;
    while msg.is_compressed() {
        msg = msg.decompress()?;
    }
    if msg.is_encrypted() {
        return Err(Error::Invalid("The message is encrypted".into()));
    }
    let content = read_limited(&mut msg)?;
    let mut signatures = Vec::new();
    if let Message::Signed { reader, .. } = &msg {
        for i in 0..reader.num_signatures() {
            let Some(sig) = reader.signature(i) else {
                continue;
            };
            signatures.push(check_signature(sig, lookup, &|key| {
                msg.verify_nested_explicit(i, key.as_dyn()).is_ok()
            }));
        }
    }
    Ok(Decrypted {
        data: content,
        signatures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::generate;

    fn lookup_in(certs: Vec<SignedPublicKey>) -> impl Fn(&Issuer) -> Vec<SignedPublicKey> {
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
    fn encrypt_decrypt_round_trip_with_signature() {
        let alice = generate("Alice", "alice@example.com", None).unwrap();
        let bob = generate("Bob", "bob@example.com", None).unwrap();
        let text = "Grüße — gizli mesaj 🔐".as_bytes();
        let armored = encrypt(
            text,
            &[&bob.to_public_key(), &alice.to_public_key()],
            &[],
            Some(&Signer {
                key: &alice,
                passphrase: None,
            }),
        )
        .unwrap();
        assert!(armored.starts_with("-----BEGIN PGP MESSAGE-----"));
        let ids = recipients(armored.as_bytes()).unwrap();
        assert_eq!(ids.len(), 2);
        let lookup = lookup_in(vec![alice.to_public_key()]);
        // Bob reads it and sees Alice's good signature.
        let out = decrypt(armored.as_bytes(), &[(&bob, None)], &lookup).unwrap();
        assert_eq!(out.data, text);
        assert_eq!(out.signatures.len(), 1);
        assert_eq!(out.signatures[0].status, SigStatus::Good);
        assert_eq!(
            out.signatures[0].signer.as_deref(),
            Some(cert::fingerprint(&alice.to_public_key()).as_str())
        );
        // Alice can read her own copy.
        assert_eq!(
            decrypt(armored.as_bytes(), &[(&alice, None)], &lookup)
                .unwrap()
                .data,
            text
        );
        // Without Alice's key, the signature is from an unknown key.
        let nobody = |_: &Issuer| Vec::new();
        let out = decrypt(armored.as_bytes(), &[(&bob, None)], &nobody).unwrap();
        assert_eq!(out.signatures[0].status, SigStatus::UnknownKey);
        // Someone else cannot.
        let eve = generate("Eve", "eve@example.com", None).unwrap();
        assert_eq!(
            decrypt(armored.as_bytes(), &[(&eve, None)], &lookup).unwrap_err(),
            Error::NoKey
        );
    }

    #[test]
    fn wrong_signer_key_is_reported_bad() {
        let alice = generate("Alice", "alice@example.com", None).unwrap();
        let bob = generate("Bob", "bob@example.com", None).unwrap();
        let armored = encrypt(
            b"hi",
            &[&bob.to_public_key()],
            &[],
            Some(&Signer {
                key: &alice,
                passphrase: None,
            }),
        )
        .unwrap();
        // A lookup that claims another key for Alice's key id.
        let mallory = generate("Alice", "alice@example.com", None).unwrap();
        let liar = |_: &Issuer| vec![mallory.to_public_key()];
        let out = decrypt(armored.as_bytes(), &[(&bob, None)], &liar).unwrap();
        assert_eq!(out.signatures[0].status, SigStatus::Bad);
    }

    #[test]
    fn hidden_recipients_are_anonymous() {
        let alice = generate("Alice", "alice@example.com", None).unwrap();
        let bob = generate("Bob", "bob@example.com", None).unwrap();
        let carol = generate("Carol", "carol@example.com", None).unwrap();
        let armored = encrypt(
            b"for bob, and carol in secret",
            &[&bob.to_public_key(), &alice.to_public_key()],
            &[&carol.to_public_key()],
            None,
        )
        .unwrap();
        let ids = recipients(armored.as_bytes()).unwrap();
        let carol_ids = cert::key_ids(&carol.to_public_key());
        assert!(
            ids.iter().all(|id| !carol_ids.contains(id)),
            "carol not named"
        );
        assert!(ids.contains(&"0000000000000000".to_string()));
        let none = |_: &Issuer| Vec::new();
        for key in [&alice, &bob, &carol] {
            assert_eq!(
                decrypt(armored.as_bytes(), &[(key, None)], &none)
                    .unwrap()
                    .data,
                b"for bob, and carol in secret"
            );
        }
    }

    #[test]
    fn protected_keys_need_their_passphrase() {
        let bob = generate("Bob", "bob@example.com", Some("s3cret")).unwrap();
        let armored = encrypt(b"x", &[&bob.to_public_key()], &[], None).unwrap();
        let none = |_: &Issuer| Vec::new();
        let fp = cert::fingerprint(&bob.to_public_key());
        assert_eq!(
            decrypt(armored.as_bytes(), &[(&bob, None)], &none).unwrap_err(),
            Error::Locked(fp.clone())
        );
        assert_eq!(
            decrypt(armored.as_bytes(), &[(&bob, Some("nope"))], &none).unwrap_err(),
            Error::BadPassphrase
        );
        assert_eq!(
            decrypt(armored.as_bytes(), &[(&bob, Some("s3cret"))], &none)
                .unwrap()
                .data,
            b"x"
        );
        // Signing needs it too.
        assert_eq!(
            sign_detached(
                b"x",
                &Signer {
                    key: &bob,
                    passphrase: None
                }
            )
            .unwrap_err(),
            Error::Locked(fp)
        );
        let wrong = Signer {
            key: &bob,
            passphrase: Some("wrong"),
        };
        assert_eq!(
            sign_detached(b"x", &wrong).unwrap_err(),
            Error::BadPassphrase
        );
        assert_eq!(
            encrypt(b"x", &[&bob.to_public_key()], &[], Some(&wrong)).unwrap_err(),
            Error::BadPassphrase
        );
        let right = Signer {
            key: &bob,
            passphrase: Some("s3cret"),
        };
        assert!(encrypt(b"x", &[&bob.to_public_key()], &[], Some(&right)).is_ok());
        let (sig, _) = sign_detached(b"x", &right).unwrap();
        let lookup = lookup_in(vec![bob.to_public_key()]);
        assert_eq!(
            verify_detached(b"x", sig.as_bytes(), &lookup).unwrap()[0].status,
            SigStatus::Good
        );
    }

    #[test]
    fn detached_signatures() {
        let alice = generate("Alice", "alice@example.com", None).unwrap();
        let data = b"Content-Type: text/plain\r\n\r\nHello\r\n";
        let (sig, hash) = sign_detached(
            data,
            &Signer {
                key: &alice,
                passphrase: None,
            },
        )
        .unwrap();
        assert_eq!(micalg(hash), "pgp-sha256");
        assert!(sig.starts_with("-----BEGIN PGP SIGNATURE-----"));
        let lookup = lookup_in(vec![alice.to_public_key()]);
        let ok = verify_detached(data, sig.as_bytes(), &lookup).unwrap();
        assert_eq!(ok[0].status, SigStatus::Good);
        assert!(ok[0].created.is_some());
        let bad = verify_detached(b"tampered", sig.as_bytes(), &lookup).unwrap();
        assert_eq!(bad[0].status, SigStatus::Bad);
        let none = |_: &Issuer| Vec::new();
        let unknown = verify_detached(data, sig.as_bytes(), &none).unwrap();
        assert_eq!(unknown[0].status, SigStatus::UnknownKey);
        assert!(unknown[0].issuer.label().is_some());
    }
}
