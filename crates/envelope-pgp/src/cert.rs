//! Certificates (transferable public keys) and secret keys.
//!
//! Validity is judged leniently but safely: a user id or subkey counts when
//! at least one of its self-signatures verifies (the newest valid one decides
//! flags and expiry), so keys with an old, broken or third-party signature
//! still work while forged components are ignored.

use std::io::Cursor;

use pgp::composed::{
    ArmorOptions, EncryptionCaps, KeyType, PublicOrSecret, SecretKeyParamsBuilder, SignedPublicKey,
    SignedPublicSubKey, SignedSecretKey, SubkeyParamsBuilder,
};
use pgp::crypto::ecc_curve::ECCCurve;
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::packet::{self, KeyFlags, Signature, SignatureType};
use pgp::ser::Serialize;
use pgp::types::{
    CompressionAlgorithm, EcdhPublicParams, KeyDetails, Password, PublicParams, SignedUser, Tag,
};
use smallvec::smallvec;

use crate::{Error, Result};

/// A certificate, with its secret key when this is one of the user's own.
#[derive(Debug, Clone)]
pub struct Cert {
    pub public: SignedPublicKey,
    pub secret: Option<SignedSecretKey>,
}

/// What Envelope shows and stores about a certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Upper-case hex fingerprint of the primary key.
    pub fingerprint: String,
    /// Upper-case hex key ids of the primary key and all subkeys.
    pub key_ids: Vec<String>,
    pub user_ids: Vec<String>,
    /// Lower-cased e-mail addresses of the valid user ids.
    pub emails: Vec<String>,
    /// e.g. `Ed25519`, `RSA 3072`.
    pub algorithm: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds; `None` when the key does not expire.
    pub expires_at: Option<i64>,
    pub revoked: bool,
    /// Has a usable encryption (sub)key right now.
    pub can_encrypt: bool,
    /// Has a usable signing (sub)key right now.
    pub can_sign: bool,
    /// The secret key is included.
    pub secret: bool,
    /// The secret key material is protected by a passphrase.
    pub protected: bool,
}

impl Cert {
    pub fn from_public(public: SignedPublicKey) -> Cert {
        Cert {
            public,
            secret: None,
        }
    }

    pub fn from_secret(secret: SignedSecretKey) -> Cert {
        Cert {
            public: secret.to_public_key(),
            secret: Some(secret),
        }
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }

    pub fn info(&self) -> CertInfo {
        info_at(&self.public, self.secret.as_ref(), crate::now())
    }
}

/// Upper-case hex fingerprint of a certificate.
pub fn fingerprint(cert: &SignedPublicKey) -> String {
    cert.primary_key.fingerprint().to_string().to_uppercase()
}

/// Key id (upper-case hex) belonging to a fingerprint in hex: the low 64
/// bits for v4 keys, the high 64 bits for v6 keys.
pub fn key_id_of_fingerprint(fingerprint: &str) -> String {
    let fp = fingerprint.to_uppercase();
    if fp.len() == 64 {
        fp[..16].to_string()
    } else if fp.len() >= 16 {
        fp[fp.len() - 16..].to_string()
    } else {
        fp
    }
}

// ---------------------------------------------------------------------------
// Parsing

/// Reads every certificate and secret key in `data`: binary or ASCII armored,
/// one or many, several armored blocks in one text included.
pub fn parse(data: &[u8]) -> Result<Vec<Cert>> {
    let mut out = Vec::new();
    let text = std::str::from_utf8(data).ok();
    let blocks: Vec<&[u8]> = match text {
        Some(t) if t.contains("-----BEGIN PGP") => {
            armor_blocks(t).into_iter().map(|b| b.as_bytes()).collect()
        }
        _ => vec![data],
    };
    let mut last_error = None;
    for block in blocks {
        match PublicOrSecret::from_reader_many(Cursor::new(block.to_vec())) {
            Ok((keys, _)) => {
                for key in keys {
                    match key {
                        Ok(PublicOrSecret::Public(k)) => out.push(Cert::from_public(k)),
                        Ok(PublicOrSecret::Secret(k)) => out.push(Cert::from_secret(k)),
                        Err(e) => last_error = Some(e.to_string()),
                    }
                }
            }
            Err(e) => last_error = Some(e.to_string()),
        }
    }
    if out.is_empty() {
        return Err(Error::Invalid(match last_error {
            Some(e) => format!("No OpenPGP key found ({e})"),
            None => "No OpenPGP key found".into(),
        }));
    }
    Ok(out)
}

/// The `-----BEGIN PGP …-----` … `-----END PGP …-----` blocks of a text.
pub fn armor_blocks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN PGP") {
        let after = &rest[start..];
        let Some(end_marker) = after.find("-----END PGP") else {
            break;
        };
        // The END line runs to its closing dashes.
        let end_line = &after[end_marker + 12..];
        let close = end_line
            .find("-----")
            .map(|i| end_marker + 12 + i + 5)
            .unwrap_or(after.len());
        out.push(&after[..close]);
        rest = &after[close..];
    }
    out
}

/// Parses one public certificate (binary or armored).
pub fn parse_public(data: &[u8]) -> Result<SignedPublicKey> {
    parse(data)?
        .into_iter()
        .next()
        .map(|c| c.public)
        .ok_or_else(|| Error::Invalid("No OpenPGP key found".into()))
}

/// Parses one secret key (binary or armored).
pub fn parse_secret(data: &[u8]) -> Result<SignedSecretKey> {
    parse(data)?
        .into_iter()
        .find_map(|c| c.secret)
        .ok_or_else(|| Error::Invalid("This is not a secret key".into()))
}

// ---------------------------------------------------------------------------
// Serialization

pub fn public_bytes(cert: &SignedPublicKey) -> Result<Vec<u8>> {
    Ok(cert.to_bytes()?)
}

pub fn public_armored(cert: &SignedPublicKey) -> Result<String> {
    Ok(cert.to_armored_string(ArmorOptions::default())?)
}

pub fn secret_armored(key: &SignedSecretKey) -> Result<String> {
    Ok(key.to_armored_string(ArmorOptions::default())?)
}

// ---------------------------------------------------------------------------
// Generation and passphrases

/// Generates a modern key for an address: an Ed25519 primary key that
/// certifies and signs, with a Curve25519 encryption subkey (the shape GnuPG
/// creates by default, readable by every current OpenPGP implementation).
pub fn generate(name: &str, email: &str, passphrase: Option<&str>) -> Result<SignedSecretKey> {
    let email = email.trim();
    if !email.contains('@') {
        return Err(Error::Invalid(format!(
            "“{email}” is not an e-mail address"
        )));
    }
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '\r' | '\n'))
        .collect();
    let user_id = if name.trim().is_empty() {
        format!("<{email}>")
    } else {
        format!("{} <{email}>", name.trim())
    };
    let passphrase = passphrase.filter(|p| !p.is_empty()).map(str::to_string);
    let encryption = SubkeyParamsBuilder::default()
        .key_type(KeyType::ECDH(ECCCurve::Curve25519Legacy))
        .can_encrypt(EncryptionCaps::All)
        .passphrase(passphrase.clone())
        .build()
        .map_err(|e| Error::Pgp(e.to_string()))?;
    let params = SecretKeyParamsBuilder::default()
        .key_type(KeyType::Ed25519Legacy)
        .can_certify(true)
        .can_sign(true)
        .primary_user_id(user_id)
        .passphrase(passphrase)
        .preferred_symmetric_algorithms(smallvec![
            SymmetricKeyAlgorithm::AES256,
            SymmetricKeyAlgorithm::AES192,
            SymmetricKeyAlgorithm::AES128,
        ])
        .preferred_hash_algorithms(smallvec![
            HashAlgorithm::Sha512,
            HashAlgorithm::Sha384,
            HashAlgorithm::Sha256,
            HashAlgorithm::Sha224,
        ])
        .preferred_compression_algorithms(smallvec![
            CompressionAlgorithm::ZLIB,
            CompressionAlgorithm::ZIP,
            CompressionAlgorithm::Uncompressed,
        ])
        .subkey(encryption)
        .build()
        .map_err(|e| Error::Pgp(e.to_string()))?;
    Ok(params.generate(rand::thread_rng())?)
}

/// Whether any secret key material is protected by a passphrase.
pub fn is_protected(key: &SignedSecretKey) -> bool {
    key.primary_key.secret_params().is_encrypted()
        || key
            .secret_subkeys
            .iter()
            .any(|s| s.key.secret_params().is_encrypted())
}

/// Checks a passphrase against the secret key material.
pub fn check_passphrase(key: &SignedSecretKey, passphrase: &str) -> Result<()> {
    remove_passphrase(key, passphrase).map(|_| ())
}

/// A copy of the key without passphrase protection.
pub fn remove_passphrase(key: &SignedSecretKey, passphrase: &str) -> Result<SignedSecretKey> {
    let pw = Password::from(passphrase);
    let mut out = key.clone();
    out.primary_key
        .remove_password(&pw)
        .map_err(|_| Error::BadPassphrase)?;
    for sub in &mut out.secret_subkeys {
        sub.key
            .remove_password(&pw)
            .map_err(|_| Error::BadPassphrase)?;
    }
    Ok(out)
}

/// A copy of an unprotected key protected by `passphrase`.
pub fn set_passphrase(key: &SignedSecretKey, passphrase: &str) -> Result<SignedSecretKey> {
    if is_protected(key) {
        return Err(Error::Invalid(
            "Remove the current passphrase before setting a new one".into(),
        ));
    }
    let pw = Password::from(passphrase);
    let mut rng = rand::thread_rng();
    let mut out = key.clone();
    out.primary_key.set_password(&mut rng, &pw)?;
    for sub in &mut out.secret_subkeys {
        sub.key.set_password(&mut rng, &pw)?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Validity

fn ts(t: pgp::types::Timestamp) -> i64 {
    i64::from(t.as_secs())
}

fn sig_time(sig: &Signature) -> i64 {
    sig.created().map(ts).unwrap_or(0)
}

/// Valid self-signatures over a user id, newest first.
fn user_self_sigs<'a>(cert: &SignedPublicKey, user: &'a SignedUser) -> Vec<&'a Signature> {
    let mut sigs: Vec<&Signature> = user
        .signatures
        .iter()
        .filter(|s| {
            s.verify_certification(&cert.primary_key, Tag::UserId, &user.id)
                .is_ok()
        })
        .collect();
    sigs.sort_by_key(|s| std::cmp::Reverse(sig_time(s)));
    sigs
}

/// Valid user ids with their newest self-signature (revoked ones left out).
fn valid_users(cert: &SignedPublicKey) -> Vec<(&SignedUser, &Signature)> {
    cert.details
        .users
        .iter()
        .filter_map(|u| {
            let newest = user_self_sigs(cert, u).into_iter().next()?;
            (newest.typ() != Some(SignatureType::CertRevocation)).then_some((u, newest))
        })
        .collect()
}

/// The self-signature that describes the primary key: a direct key
/// signature, else the primary user id's newest self-signature.
fn primary_sig(cert: &SignedPublicKey) -> Option<&Signature> {
    let users = valid_users(cert);
    let from_user = users
        .iter()
        .find(|(_, s)| s.is_primary())
        .or_else(|| users.first())
        .map(|(_, s)| *s);
    let direct = cert
        .details
        .direct_signatures
        .iter()
        .filter(|s| s.verify_key(&cert.primary_key).is_ok())
        .max_by_key(|s| sig_time(s));
    match (direct, from_user) {
        // Flags and expiry usually live on the user id signature; prefer it
        // unless only a direct key signature carries them.
        (Some(d), Some(u)) if u.key_flags() == KeyFlags::default() => Some(d),
        (_, Some(u)) => Some(u),
        (Some(d), None) => Some(d),
        (None, None) => None,
    }
}

fn key_revoked(cert: &SignedPublicKey) -> bool {
    cert.details
        .revocation_signatures
        .iter()
        .any(|s| s.verify_key(&cert.primary_key).is_ok())
}

fn expiry(created: i64, sig: Option<&Signature>) -> Option<i64> {
    let secs = sig?.key_expiration_time()?.as_secs();
    (secs > 0).then(|| created + i64::from(secs))
}

fn primary_expiry(cert: &SignedPublicKey) -> Option<i64> {
    expiry(ts(cert.primary_key.created_at()), primary_sig(cert))
}

fn flags_or_default(flags: KeyFlags, alg: PublicKeyAlgorithm) -> (bool, bool) {
    if flags == KeyFlags::default() {
        // No key flags: fall back to what the algorithm can do.
        (alg.can_sign(), alg.can_encrypt())
    } else {
        (
            flags.sign(),
            flags.encrypt_comms() || flags.encrypt_storage(),
        )
    }
}

/// The newest valid binding signature of a subkey, `None` when the subkey is
/// unbound or revoked.
fn subkey_binding<'a>(
    cert: &SignedPublicKey,
    sub: &'a SignedPublicSubKey,
) -> Option<&'a Signature> {
    let revoked = sub.signatures.iter().any(|s| {
        s.typ() == Some(SignatureType::SubkeyRevocation)
            && s.verify_subkey_binding(&cert.primary_key, &sub.key).is_ok()
    });
    if revoked {
        return None;
    }
    sub.signatures
        .iter()
        .filter(|s| s.typ() == Some(SignatureType::SubkeyBinding))
        .filter(|s| s.verify_subkey_binding(&cert.primary_key, &sub.key).is_ok())
        .filter(|s| {
            // Signing subkeys must sign back.
            !s.key_flags().sign()
                || s.embedded_signature().is_some_and(|b| {
                    b.verify_primary_key_binding(&sub.key, &cert.primary_key)
                        .is_ok()
                })
        })
        .max_by_key(|s| sig_time(s))
}

/// What a subkey may be used for at time `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Usage {
    sign: bool,
    encrypt: bool,
}

fn subkey_usage(cert: &SignedPublicKey, sub: &SignedPublicSubKey, now: i64) -> Option<Usage> {
    let binding = subkey_binding(cert, sub)?;
    if expiry(ts(sub.key.created_at()), Some(binding)).is_some_and(|e| e <= now) {
        return None;
    }
    let (sign, encrypt) = flags_or_default(binding.key_flags(), sub.key.algorithm());
    Some(Usage {
        sign: sign && sub.key.algorithm().can_sign(),
        encrypt: encrypt && sub.key.algorithm().can_encrypt(),
    })
}

/// Whether the certificate as a whole is usable at `now` (not revoked, not
/// expired, with a valid user id or direct key signature).
pub fn is_usable(cert: &SignedPublicKey, now: i64) -> bool {
    !key_revoked(cert)
        && primary_sig(cert).is_some()
        && !primary_expiry(cert).is_some_and(|e| e <= now)
}

fn primary_usage(cert: &SignedPublicKey) -> Usage {
    let alg = cert.primary_key.algorithm();
    let flags = primary_sig(cert).map(|s| s.key_flags()).unwrap_or_default();
    let (sign, encrypt) = flags_or_default(flags, alg);
    Usage {
        sign: sign && alg.can_sign(),
        encrypt: encrypt && alg.can_encrypt(),
    }
}

/// A key to encrypt to.
#[derive(Debug, Clone, Copy)]
pub enum EncryptionKey<'a> {
    Primary(&'a SignedPublicKey),
    Subkey(&'a SignedPublicSubKey),
}

/// The keys to encrypt to: every valid encryption subkey (the primary key
/// when it is the only one able to encrypt). Empty when the certificate is
/// unusable.
pub fn encryption_keys(cert: &SignedPublicKey, now: i64) -> Vec<EncryptionKey<'_>> {
    if !is_usable(cert, now) {
        return Vec::new();
    }
    let subs: Vec<EncryptionKey<'_>> = cert
        .public_subkeys
        .iter()
        .filter(|s| subkey_usage(cert, s, now).is_some_and(|u| u.encrypt))
        .map(EncryptionKey::Subkey)
        .collect();
    if !subs.is_empty() {
        return subs;
    }
    if primary_usage(cert).encrypt {
        return vec![EncryptionKey::Primary(cert)];
    }
    Vec::new()
}

/// A secret key to sign with.
#[derive(Debug, Clone, Copy)]
pub enum SigningKey<'a> {
    Primary(&'a packet::SecretKey),
    Subkey(&'a packet::SecretSubkey),
}

impl SigningKey<'_> {
    pub fn is_protected(&self) -> bool {
        match self {
            SigningKey::Primary(k) => k.secret_params().is_encrypted(),
            SigningKey::Subkey(k) => k.secret_params().is_encrypted(),
        }
    }

    pub fn fingerprint(&self) -> String {
        match self {
            SigningKey::Primary(k) => k.fingerprint().to_string().to_uppercase(),
            SigningKey::Subkey(k) => k.fingerprint().to_string().to_uppercase(),
        }
    }
}

/// The key to sign with: the primary key when it may sign, else the newest
/// valid signing subkey.
pub fn signing_key(key: &SignedSecretKey, now: i64) -> Option<SigningKey<'_>> {
    let public = key.to_public_key();
    if !is_usable(&public, now) {
        return None;
    }
    if primary_usage(&public).sign {
        return Some(SigningKey::Primary(&key.primary_key));
    }
    key.secret_subkeys
        .iter()
        .filter(|s| {
            let fp = s.key.fingerprint();
            public
                .public_subkeys
                .iter()
                .find(|p| p.key.fingerprint() == fp)
                .and_then(|p| subkey_usage(&public, p, now))
                .is_some_and(|u| u.sign)
        })
        .max_by_key(|s| ts(s.key.created_at()))
        .map(|s| SigningKey::Subkey(&s.key))
}

/// Upper-case hex key ids of the primary key and its subkeys.
pub fn key_ids(cert: &SignedPublicKey) -> Vec<String> {
    std::iter::once(cert.primary_key.legacy_key_id())
        .chain(cert.public_subkeys.iter().map(|s| s.key.legacy_key_id()))
        .map(|id| id.to_string().to_uppercase())
        .collect()
}

/// The e-mail address inside a user id (`Name <a@b>` or a bare address).
pub fn user_id_email(uid: &str) -> Option<String> {
    let addr = match (uid.rfind('<'), uid.rfind('>')) {
        (Some(start), Some(end)) if start < end => &uid[start + 1..end],
        _ => uid,
    };
    let addr = addr.trim();
    (addr.contains('@') && !addr.contains(char::is_whitespace)).then(|| addr.to_lowercase())
}

/// Readable name of a key algorithm (`Ed25519`, `RSA 3072`, `Cv25519`, …).
pub fn algorithm_name(alg: PublicKeyAlgorithm, params: &PublicParams) -> String {
    match params {
        PublicParams::RSA(_) => {
            // The modulus is the first MPI of the serialized parameters.
            let bits = params
                .to_bytes()
                .ok()
                .filter(|b| b.len() >= 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]));
            match bits {
                Some(bits) => format!("RSA {}", (u32::from(bits).div_ceil(1024)) * 1024),
                None => "RSA".into(),
            }
        }
        PublicParams::EdDSALegacy(_) | PublicParams::Ed25519(_) => "Ed25519".into(),
        PublicParams::Ed448(_) => "Ed448".into(),
        PublicParams::X25519(_) => "Cv25519".into(),
        PublicParams::X448(_) => "X448".into(),
        PublicParams::ECDH(EcdhPublicParams::Curve25519Legacy { .. }) => "Cv25519".into(),
        PublicParams::ECDH(_) => "ECDH".into(),
        PublicParams::ECDSA(_) => "ECDSA".into(),
        PublicParams::DSA(_) => "DSA".into(),
        PublicParams::Elgamal(_) => "ElGamal".into(),
        _ => format!("{alg:?}"),
    }
}

/// Inspects a certificate at time `now` (unix seconds).
pub fn info_at(cert: &SignedPublicKey, secret: Option<&SignedSecretKey>, now: i64) -> CertInfo {
    let users = valid_users(cert);
    let mut user_ids: Vec<String> = users
        .iter()
        .map(|(u, _)| String::from_utf8_lossy(u.id.id()).into_owned())
        .collect();
    // The primary user id first.
    if let Some(i) = users.iter().position(|(_, s)| s.is_primary())
        && i > 0
    {
        let primary = user_ids.remove(i);
        user_ids.insert(0, primary);
    }
    let mut emails: Vec<String> = Vec::new();
    for uid in &user_ids {
        if let Some(e) = user_id_email(uid)
            && !emails.contains(&e)
        {
            emails.push(e);
        }
    }
    let usable = is_usable(cert, now);
    let can_sign = usable
        && (primary_usage(cert).sign
            || cert
                .public_subkeys
                .iter()
                .any(|s| subkey_usage(cert, s, now).is_some_and(|u| u.sign)));
    CertInfo {
        fingerprint: fingerprint(cert),
        key_ids: key_ids(cert),
        user_ids,
        emails,
        algorithm: algorithm_name(
            cert.primary_key.algorithm(),
            cert.primary_key.public_params(),
        ),
        created_at: ts(cert.primary_key.created_at()),
        expires_at: primary_expiry(cert),
        revoked: key_revoked(cert),
        can_encrypt: !encryption_keys(cert, now).is_empty(),
        can_sign,
        secret: secret.is_some(),
        protected: secret.is_some_and(is_protected),
    }
}

/// The symmetric algorithms all recipients prefer, strongest first; AES-128
/// (which every OpenPGP implementation supports) when they agree on none.
pub fn common_symmetric(certs: &[&SignedPublicKey]) -> SymmetricKeyAlgorithm {
    const CHOICES: [SymmetricKeyAlgorithm; 3] = [
        SymmetricKeyAlgorithm::AES256,
        SymmetricKeyAlgorithm::AES192,
        SymmetricKeyAlgorithm::AES128,
    ];
    CHOICES
        .into_iter()
        .find(|alg| {
            certs.iter().all(|c| {
                let prefs = primary_sig(c)
                    .map(|s| s.preferred_symmetric_algs().to_vec())
                    .unwrap_or_default();
                // No preferences: modern implementations all do AES-256.
                prefs.is_empty() || prefs.contains(alg)
            })
        })
        .unwrap_or(SymmetricKeyAlgorithm::AES128)
}

/// A copy of the certificate as Autocrypt wants it in headers: the primary
/// key, the user id for `email` (or the primary one), and the subkeys that
/// can encrypt or sign now.
pub fn minimal(cert: &SignedPublicKey, email: &str, now: i64) -> SignedPublicKey {
    let email = email.to_lowercase();
    let users = valid_users(cert);
    let user = users
        .iter()
        .find(|(u, _)| {
            user_id_email(&String::from_utf8_lossy(u.id.id())).as_deref() == Some(&email)
        })
        .or_else(|| users.iter().find(|(_, s)| s.is_primary()))
        .or_else(|| users.first())
        .map(|(u, s)| SignedUser::new(u.id.clone(), vec![(*s).clone()]));
    let subkeys: Vec<SignedPublicSubKey> = cert
        .public_subkeys
        .iter()
        .filter_map(|s| {
            let usage = subkey_usage(cert, s, now)?;
            if !usage.encrypt && !usage.sign {
                return None;
            }
            let binding = subkey_binding(cert, s)?.clone();
            Some(SignedPublicSubKey::new(s.key.clone(), vec![binding]))
        })
        .collect();
    let mut details = cert.details.clone();
    details.users = user.into_iter().collect();
    details.user_attributes.clear();
    details.revocation_signatures.clear();
    details
        .direct_signatures
        .retain(|s| s.verify_key(&cert.primary_key).is_ok());
    SignedPublicKey::new(cert.primary_key.clone(), details, subkeys)
}

/// Combines what is known about the same certificate from two sources (a
/// newer copy with new subkeys, user ids or a revocation).
pub fn merge(old: &SignedPublicKey, new: &SignedPublicKey) -> SignedPublicKey {
    if old.primary_key != new.primary_key {
        return new.clone();
    }
    let mut out = old.clone();
    for sig in &new.details.revocation_signatures {
        if !out.details.revocation_signatures.contains(sig) {
            out.details.revocation_signatures.push(sig.clone());
        }
    }
    for sig in &new.details.direct_signatures {
        if !out.details.direct_signatures.contains(sig) {
            out.details.direct_signatures.push(sig.clone());
        }
    }
    for user in &new.details.users {
        match out.details.users.iter_mut().find(|u| u.id == user.id) {
            Some(existing) => {
                for sig in &user.signatures {
                    if !existing.signatures.contains(sig) {
                        existing.signatures.push(sig.clone());
                    }
                }
            }
            None => out.details.users.push(user.clone()),
        }
    }
    for sub in &new.public_subkeys {
        match out.public_subkeys.iter_mut().find(|s| s.key == sub.key) {
            Some(existing) => {
                for sig in &sub.signatures {
                    if !existing.signatures.contains(sig) {
                        existing.signatures.push(sig.clone());
                    }
                }
            }
            None => out.public_subkeys.push(sub.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_modern_key() {
        let key = generate("Alex Morgan", "alex@envelope.test", None).unwrap();
        key.verify_bindings().unwrap();
        let cert = Cert::from_secret(key.clone());
        let info = cert.info();
        assert_eq!(info.fingerprint.len(), 40);
        assert_eq!(info.user_ids, vec!["Alex Morgan <alex@envelope.test>"]);
        assert_eq!(info.emails, vec!["alex@envelope.test"]);
        assert_eq!(info.algorithm, "Ed25519");
        assert!(info.can_encrypt && info.can_sign && info.secret);
        assert!(!info.protected && !info.revoked);
        assert_eq!(info.expires_at, None);
        assert_eq!(info.key_ids.len(), 2);
        assert!(info.fingerprint.ends_with(&info.key_ids[0]));
        // The encryption subkey is Curve25519.
        let enc = encryption_keys(&cert.public, crate::now());
        assert_eq!(enc.len(), 1);
        match enc[0] {
            EncryptionKey::Subkey(s) => assert_eq!(
                algorithm_name(s.key.algorithm(), s.key.public_params()),
                "Cv25519"
            ),
            EncryptionKey::Primary(_) => panic!("expected a subkey"),
        }
        assert!(matches!(
            signing_key(&key, crate::now()),
            Some(SigningKey::Primary(_))
        ));
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(generate("x", "not an address", None).is_err());
    }

    #[test]
    fn armor_round_trips() {
        let key = generate("", "sam@envelope.test", None).unwrap();
        let armored = secret_armored(&key).unwrap();
        assert!(armored.starts_with("-----BEGIN PGP PRIVATE KEY BLOCK-----"));
        let back = parse_secret(armored.as_bytes()).unwrap();
        assert_eq!(back, key);
        let public = public_armored(&key.to_public_key()).unwrap();
        let parsed = parse(public.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].secret.is_none());
        assert_eq!(
            parsed[0].fingerprint(),
            Cert::from_secret(key.clone()).fingerprint()
        );
        assert_eq!(parsed[0].info().user_ids, vec!["<sam@envelope.test>"]);
        // Binary works too.
        let binary = public_bytes(&key.to_public_key()).unwrap();
        assert_eq!(parse_public(&binary).unwrap(), key.to_public_key());
        // Several armored blocks with text around them.
        let other = generate("Elif", "elif@example.com", None).unwrap();
        let text = format!(
            "my keys:\n{public}\nand\n{}\nbye",
            public_armored(&other.to_public_key()).unwrap()
        );
        assert_eq!(parse(text.as_bytes()).unwrap().len(), 2);
        assert!(parse(b"hello").is_err());
    }

    #[test]
    fn passphrases() {
        let key = generate("P", "p@example.com", Some("correct horse")).unwrap();
        assert!(is_protected(&key));
        assert!(Cert::from_secret(key.clone()).info().protected);
        assert_eq!(
            check_passphrase(&key, "wrong").unwrap_err(),
            Error::BadPassphrase
        );
        check_passphrase(&key, "correct horse").unwrap();
        let open = remove_passphrase(&key, "correct horse").unwrap();
        assert!(!is_protected(&open));
        assert_eq!(open.to_public_key(), key.to_public_key());
        let locked = set_passphrase(&open, "new one").unwrap();
        assert!(is_protected(&locked));
        check_passphrase(&locked, "new one").unwrap();
        // Still a valid key after the round trip through armor.
        let back = parse_secret(secret_armored(&locked).unwrap().as_bytes()).unwrap();
        back.verify_bindings().unwrap();
        check_passphrase(&back, "new one").unwrap();
    }

    #[test]
    fn user_id_addresses() {
        assert_eq!(
            user_id_email("Alex Morgan <Alex@Envelope.test>").as_deref(),
            Some("alex@envelope.test")
        );
        assert_eq!(
            user_id_email("bob@example.com").as_deref(),
            Some("bob@example.com")
        );
        assert_eq!(user_id_email("Just a name"), None);
        assert_eq!(
            key_id_of_fingerprint("0123456789ABCDEF0123456789ABCDEF01234567"),
            "89ABCDEF01234567"
        );
    }

    #[test]
    fn minimal_keeps_one_user_id() {
        let mut key = generate("A", "a@example.com", None).unwrap();
        let other = generate("A", "a.other@example.com", None).unwrap();
        // Add a second (unsigned → ignored) user id from another key: it is
        // not self-signed, so it must not survive.
        key.details.users.push(other.details.users[0].clone());
        let cert = key.to_public_key();
        let info = info_at(&cert, None, crate::now());
        assert_eq!(info.emails, vec!["a@example.com"]);
        let small = minimal(&cert, "a@example.com", crate::now());
        assert_eq!(small.details.users.len(), 1);
        assert_eq!(small.public_subkeys.len(), 1);
        small.verify_bindings().unwrap();
        // Merging the full certificate back gives the same key material.
        let merged = merge(&small, &cert);
        assert_eq!(merged.primary_key, cert.primary_key);
        assert_eq!(merged.public_subkeys.len(), 1);
    }
}
