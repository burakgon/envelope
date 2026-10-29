//! CMS (RFC 5652): signed data checked and made; enveloped data (RFC 5652,
//! ECDH from RFC 5753) and authenticated enveloped data (RFC 5083) opened
//! and made.

use std::borrow::Cow;

use const_oid::ObjectIdentifier as Oid;
use der::asn1::{Any, BitString, OctetString, SetOfVec};
use der::{Decode, Encode};
use rand::RngCore;
use spki::AlgorithmIdentifierOwned;
use zeroize::Zeroizing;

use crate::algs::{self, ContentAlg, Hash, PublicKey, SigAlg};
use crate::ber::{self, AlgorithmId, Tlv};
use crate::cert::{Certificate, Validity};
use crate::chain::{self, Trust, TrustStore};
use crate::key::PrivateKey;
use crate::oid;
use crate::{Error, Result, invalid};

/// The content type and the content of a ContentInfo.
fn content_info(data: &[u8]) -> Result<(Oid, Tlv<'_>)> {
    let tlv = Tlv::parse(data)?;
    if tlv.tag != ber::SEQUENCE {
        return Err(invalid("not CMS data"));
    }
    let mut r = tlv.children();
    let kind = r.read(ber::OID)?.oid()?;
    let content = r.read(ber::context_constructed(0))?.children().any()?;
    Ok((kind, content))
}

/// The type of CMS content: signed, enveloped, …
pub fn content_type(data: &[u8]) -> Option<Oid> {
    let tlv = Tlv::parse(data).ok()?;
    tlv.children().read(ber::OID).ok()?.oid().ok()
}

pub fn is_signed_data(kind: Oid) -> bool {
    kind == oid::SIGNED_DATA
}

pub fn is_enveloped_data(kind: Oid) -> bool {
    kind == oid::ENVELOPED_DATA || kind == oid::AUTH_ENVELOPED_DATA
}

/// How a signer or a recipient is named: by issuer and serial number, or by
/// subject key identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CertId {
    IssuerSerial { issuer: Vec<u8>, serial: Vec<u8> },
    KeyId(Vec<u8>),
}

impl CertId {
    fn issuer_serial(tlv: Tlv<'_>) -> Result<CertId> {
        let mut r = tlv.children();
        let issuer = r.read(ber::SEQUENCE)?.to_der()?;
        let serial = r.read(ber::INTEGER)?.value.to_vec();
        Ok(CertId::IssuerSerial { issuer, serial })
    }

    /// A SignerIdentifier or RecipientIdentifier.
    fn read(tlv: Tlv<'_>) -> Result<CertId> {
        match tlv.tag {
            ber::SEQUENCE => CertId::issuer_serial(tlv),
            t if t == ber::context(0) || t == ber::context_constructed(0) => {
                Ok(CertId::KeyId(tlv.octets()?.into_owned()))
            }
            _ => Err(invalid("unknown certificate identifier")),
        }
    }

    fn matches(&self, cert: &Certificate) -> bool {
        match self {
            CertId::IssuerSerial { issuer, serial } => cert.is_issuer_and_serial(issuer, serial),
            CertId::KeyId(id) => cert.subject_key_id() == Some(id.as_slice()),
        }
    }
}

// ---------------------------------------------------------------------------
// Signed data

struct SignerInfo<'a> {
    sid: CertId,
    digest: AlgorithmId<'a>,
    attrs: Option<Tlv<'a>>,
    signature_alg: AlgorithmId<'a>,
    signature: Cow<'a, [u8]>,
}

struct SignedData<'a> {
    content_type: Oid,
    content: Option<Cow<'a, [u8]>>,
    certificates: Vec<&'a [u8]>,
    signers: Vec<SignerInfo<'a>>,
}

fn signed_data(content: Tlv<'_>) -> Result<SignedData<'_>> {
    let mut r = content.children();
    r.read(ber::INTEGER)?;
    r.read(ber::SET)?;
    let encap = r.read(ber::SEQUENCE)?;
    let mut e = encap.children();
    let content_type = e.read(ber::OID)?.oid()?;
    let content = match e.optional(ber::context_constructed(0))? {
        Some(t) => Some(t.children().any()?.octets()?),
        None => None,
    };
    let mut certificates = Vec::new();
    if let Some(set) = r.optional(ber::context_constructed(0))? {
        let mut c = set.children();
        while let Some(cert) = c.next()? {
            // Attribute and other certificate formats are skipped.
            if cert.tag == ber::SEQUENCE {
                certificates.push(cert.raw);
            }
        }
    }
    r.optional(ber::context_constructed(1))?;
    let mut signers = Vec::new();
    let mut s = r.read(ber::SET)?.children();
    while let Some(si) = s.next()? {
        let mut r = si.children();
        r.read(ber::INTEGER)?;
        let sid = CertId::read(r.any()?)?;
        let digest = AlgorithmId::read(&mut r)?;
        let attrs = r.optional(ber::context_constructed(0))?;
        let signature_alg = AlgorithmId::read(&mut r)?;
        let signature = r.any()?.octets()?;
        signers.push(SignerInfo {
            sid,
            digest,
            attrs,
            signature_alg,
            signature,
        });
    }
    Ok(SignedData {
        content_type,
        content,
        certificates,
        signers,
    })
}

/// The certificates of CMS signed data (also a `.p7b` bundle).
pub fn certificates_of(data: &[u8]) -> Result<Vec<Certificate>> {
    let (kind, content) = content_info(data)?;
    if kind != oid::SIGNED_DATA {
        return Err(invalid("not a certificate bundle"));
    }
    signed_data(content)?
        .certificates
        .iter()
        .map(|der| Certificate::from_der(der))
        .collect()
}

/// What the signed attributes say.
#[derive(Default)]
struct Attributes {
    content_type: Option<Oid>,
    digest: Option<Vec<u8>>,
    signing_time: Option<i64>,
    encryption_cert: Option<CertId>,
    /// The attributes as signed: as they were sent, and sorted as DER wants
    /// them (signers differ in which one they hash).
    encodings: Vec<Vec<u8>>,
}

fn attributes(set: Tlv<'_>) -> Result<Attributes> {
    let mut out = Attributes::default();
    let mut list: Vec<Vec<u8>> = Vec::new();
    let mut seen: Vec<Oid> = Vec::new();
    let mut r = set.children();
    while let Some(attr) = r.next()? {
        list.push(attr.to_der()?);
        let mut a = attr.children();
        let kind = a.read(ber::OID)?.oid()?;
        let values = a.read(ber::SET)?;
        let mut v = values.children();
        let Some(value) = v.next()? else {
            continue;
        };
        let single = v.next()?.is_none();
        let essential = matches!(
            kind,
            oid::CONTENT_TYPE | oid::MESSAGE_DIGEST | oid::SIGNING_TIME
        );
        // These must appear once, with one value: two digests to choose
        // from would let a signature cover another text.
        if essential && (!single || seen.contains(&kind)) {
            return Err(invalid("a signed attribute appears twice"));
        }
        seen.push(kind);
        match kind {
            oid::CONTENT_TYPE => out.content_type = Some(value.oid()?),
            oid::MESSAGE_DIGEST => out.digest = Some(value.octets()?.into_owned()),
            oid::SIGNING_TIME => out.signing_time = ber::time(&value).ok(),
            oid::ENCRYPTION_KEY_PREFERENCE if value.tag == ber::context_constructed(0) => {
                out.encryption_cert = CertId::issuer_serial(value).ok()
            }
            oid::MS_ENCRYPTION_KEY_PREFERENCE if out.encryption_cert.is_none() => {
                out.encryption_cert = CertId::issuer_serial(value).ok()
            }
            _ => {}
        }
    }
    let joined = |l: &[Vec<u8>]| ber::tlv(ber::SET, &l.concat());
    out.encodings.push(joined(&list));
    list.sort();
    let sorted = joined(&list);
    if sorted != out.encodings[0] {
        out.encodings.push(sorted);
    }
    Ok(out)
}

/// What a signature is checked against.
pub struct Verifier<'a> {
    pub trust: &'a TrustStore,
    /// Certificates known besides the message's: the signer's (when the
    /// message leaves it out) and intermediate authorities.
    pub known: &'a [Certificate],
    /// The address the message is from.
    pub sender: Option<&'a str>,
    /// Unix seconds.
    pub now: i64,
}

/// Whether the signature fits the content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    Intact,
    /// The content (or what the signature says about it) changed after
    /// signing.
    ContentChanged,
    /// The signature was not made by the certificate's key over this.
    SignatureMismatch,
    /// The signer's certificate is neither in the message nor known.
    NoCertificate,
    /// Made with an algorithm Envelope cannot check (named).
    Unsupported(String),
}

/// The overall result of a signature, the first check that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every check passed.
    Verified,
    Bad,
    NoCertificate,
    Unsupported,
    Expired,
    NotYetValid,
    NotForEmail,
    Untrusted,
    Mismatch,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::Bad => "bad",
            Verdict::NoCertificate => "noCertificate",
            Verdict::Unsupported => "unsupported",
            Verdict::Expired => "expired",
            Verdict::NotYetValid => "notYetValid",
            Verdict::NotForEmail => "notForEmail",
            Verdict::Untrusted => "untrusted",
            Verdict::Mismatch => "mismatch",
        }
    }
}

/// One signer's signature, check by check.
#[derive(Debug, Clone)]
pub struct Signature {
    /// The certificate that made it, when the message or the caller had it.
    pub signer: Option<Certificate>,
    /// The authorities above the signer's certificate, the trusted root
    /// last (empty unless a trusted chain was found).
    pub chain: Vec<Certificate>,
    /// When the signer says it signed (unix seconds).
    pub signed_at: Option<i64>,
    /// "RSA, SHA-256" and the like.
    pub algorithm: String,
    pub integrity: Integrity,
    /// The certificate's validity now.
    pub validity: Validity,
    /// The certificate may sign mail (key usage, extended key usage).
    pub for_email: bool,
    pub trust: Trust,
    /// One of the certificate's addresses is the sender's.
    pub sender_matches: bool,
    /// The certificate the signer wants mail encrypted to, when the message
    /// names one it carries.
    pub encryption_cert: Option<Certificate>,
}

impl Signature {
    /// Verified only when every check passed; otherwise the first check that
    /// failed, in the order the interface explains them.
    pub fn verdict(&self) -> Verdict {
        match self.integrity {
            Integrity::Intact => {}
            Integrity::ContentChanged | Integrity::SignatureMismatch => return Verdict::Bad,
            Integrity::NoCertificate => return Verdict::NoCertificate,
            Integrity::Unsupported(_) => return Verdict::Unsupported,
        }
        match self.validity {
            Validity::Valid => {}
            Validity::Expired => return Verdict::Expired,
            Validity::NotYetValid => return Verdict::NotYetValid,
        }
        if !self.for_email {
            return Verdict::NotForEmail;
        }
        if self.trust != Trust::Trusted {
            return Verdict::Untrusted;
        }
        if !self.sender_matches {
            return Verdict::Mismatch;
        }
        Verdict::Verified
    }
}

/// A signed message, checked.
#[derive(Debug, Clone)]
pub struct Signed {
    /// The content of opaque signed data (a MIME entity).
    pub content: Option<Vec<u8>>,
    pub signatures: Vec<Signature>,
    /// Every certificate the message carries.
    pub certificates: Vec<Certificate>,
}

/// Checks a detached signature (`application/pkcs7-signature`) over
/// `content`, the first part of a `multipart/signed` in canonical form.
pub fn verify_detached(content: &[u8], signature: &[u8], v: &Verifier<'_>) -> Result<Signed> {
    let (kind, tlv) = content_info(signature)?;
    if kind != oid::SIGNED_DATA {
        return Err(invalid("the signature is not CMS signed data"));
    }
    let sd = signed_data(tlv)?;
    check(&sd, content, v, None)
}

/// Checks opaque signed data (`application/pkcs7-mime;
/// smime-type=signed-data`) and returns its content.
pub fn verify_opaque(cms: &[u8], v: &Verifier<'_>) -> Result<Signed> {
    let (kind, tlv) = content_info(cms)?;
    if kind != oid::SIGNED_DATA {
        return Err(invalid("not CMS signed data"));
    }
    let sd = signed_data(tlv)?;
    let content = sd
        .content
        .clone()
        .ok_or_else(|| invalid("the signed content is missing"))?;
    check(&sd, &content, v, Some(content.to_vec()))
}

fn check(
    sd: &SignedData<'_>,
    content: &[u8],
    v: &Verifier<'_>,
    keep: Option<Vec<u8>>,
) -> Result<Signed> {
    let certificates: Vec<Certificate> = sd
        .certificates
        .iter()
        .filter_map(|der| Certificate::from_der(der).ok())
        .collect();
    let pool: Vec<&Certificate> = certificates.iter().chain(v.known).collect();
    let signatures = sd
        .signers
        .iter()
        .map(|si| check_signer(si, sd.content_type, content, &pool, v))
        .collect();
    Ok(Signed {
        content: keep,
        signatures,
        certificates,
    })
}

fn check_signer(
    si: &SignerInfo<'_>,
    content_type: Oid,
    content: &[u8],
    pool: &[&Certificate],
    v: &Verifier<'_>,
) -> Signature {
    let signer = pool
        .iter()
        .find(|c| si.sid.matches(c))
        .map(|c| (*c).clone());
    let mut sig = Signature {
        signer: signer.clone(),
        chain: Vec::new(),
        signed_at: None,
        algorithm: String::new(),
        integrity: Integrity::NoCertificate,
        validity: Validity::Valid,
        for_email: false,
        trust: Trust::UnknownIssuer,
        sender_matches: false,
        encryption_cert: None,
    };
    let hash = Hash::from_oid(si.digest.oid);
    sig.algorithm = match SigAlg::from_id(&si.signature_alg, hash) {
        Ok(alg) => alg.name(),
        Err(_) => si.signature_alg.oid.to_string(),
    };
    sig.integrity = match si.attrs.map(attributes).transpose() {
        Ok(attrs) => {
            if let Some(a) = &attrs {
                sig.signed_at = a.signing_time;
                sig.encryption_cert = a
                    .encryption_cert
                    .as_ref()
                    .and_then(|id| pool.iter().find(|c| id.matches(c)).map(|c| (*c).clone()));
            }
            integrity(si, attrs.as_ref(), content_type, content, signer.as_ref())
        }
        // Broken or doubled signed attributes.
        Err(_) => Integrity::ContentChanged,
    };
    if let Some(cert) = &signer {
        sig.validity = cert.validity_at(v.now);
        sig.for_email = cert.can_sign_mail();
        sig.sender_matches = v.sender.is_some_and(|s| cert.has_email(s));
        match chain::verify(cert, pool, v.trust, v.now) {
            Ok(path) => {
                sig.trust = Trust::Trusted;
                sig.chain = path;
            }
            Err(problem) => sig.trust = problem,
        }
    }
    sig
}

/// Whether the signature fits the content and the signer's certificate.
fn integrity(
    si: &SignerInfo<'_>,
    attrs: Option<&Attributes>,
    content_type: Oid,
    content: &[u8],
    signer: Option<&Certificate>,
) -> Integrity {
    let Some(hash) = Hash::from_oid(si.digest.oid) else {
        return Integrity::Unsupported(format!("the digest {}", si.digest.oid));
    };
    let digest = hash.digest(content);
    let signed: Vec<Vec<u8>> = match attrs {
        Some(a) => {
            // The digest is checked first: a changed text is "changed",
            // whoever signed it.
            if a.digest.as_deref() != Some(digest.as_slice())
                || a.content_type != Some(content_type)
            {
                return Integrity::ContentChanged;
            }
            a.encodings.iter().map(|e| hash.digest(e)).collect()
        }
        // Without signed attributes the signature covers the content itself,
        // which S/MIME only allows for plain data.
        None if content_type == oid::DATA => vec![digest],
        None => return Integrity::ContentChanged,
    };
    let Some(cert) = signer else {
        return Integrity::NoCertificate;
    };
    let alg = match SigAlg::from_id(&si.signature_alg, Some(hash)) {
        Ok(alg) => alg,
        Err(Error::Unsupported(name)) => return Integrity::Unsupported(name),
        Err(_) => return Integrity::SignatureMismatch,
    };
    if alg.hash() != hash {
        return Integrity::Unsupported(format!(
            "a {} signature over a {} digest",
            alg.hash().name(),
            hash.name()
        ));
    }
    if !cert.key.is_usable() {
        return Integrity::Unsupported(format!("{} keys", cert.algorithm()));
    }
    if signed
        .iter()
        .any(|d| cert.key.verify(alg, d, &si.signature))
    {
        Integrity::Intact
    } else {
        Integrity::SignatureMismatch
    }
}

// ---------------------------------------------------------------------------
// Enveloped data

#[derive(Debug, Clone)]
enum Recipient {
    KeyTrans {
        id: CertId,
        /// The key transport AlgorithmIdentifier (DER).
        alg: Vec<u8>,
        key: Vec<u8>,
    },
    KeyAgree {
        /// The originator's ephemeral public point.
        point: Vec<u8>,
        ukm: Option<Vec<u8>>,
        scheme: Oid,
        /// The key wrap AlgorithmIdentifier (DER) and its algorithm.
        wrap: Vec<u8>,
        wrap_oid: Oid,
        keys: Vec<(CertId, Vec<u8>)>,
    },
    /// Passwords, pre-shared keys, …
    Other,
}

/// Encrypted CMS content, read.
#[derive(Debug, Clone)]
pub struct Envelope {
    recipients: Vec<Recipient>,
    cipher: std::result::Result<ContentAlg, Error>,
    encrypted: Vec<u8>,
    /// AuthEnvelopedData: the authenticated attributes and the tag.
    aad: Vec<u8>,
    mac: Option<Vec<u8>>,
}

impl Envelope {
    /// Reads enveloped or authenticated enveloped data.
    pub fn read(cms: &[u8]) -> Result<Envelope> {
        let (kind, content) = content_info(cms)?;
        let auth = match kind {
            oid::ENVELOPED_DATA => false,
            oid::AUTH_ENVELOPED_DATA => true,
            _ => return Err(invalid("not encrypted CMS data")),
        };
        let mut r = content.children();
        r.read(ber::INTEGER)?;
        r.optional(ber::context_constructed(0))?;
        let mut recipients = Vec::new();
        let mut infos = r.read(ber::SET)?.children();
        while let Some(info) = infos.next()? {
            recipients.push(recipient(info)?);
        }
        let mut e = r.read(ber::SEQUENCE)?.children();
        e.read(ber::OID)?;
        let alg = AlgorithmId::read(&mut e)?;
        let cipher = ContentAlg::from_id(&alg);
        let encrypted = match e.next()? {
            Some(t) if t.tag == ber::context(0) || t.tag == ber::context_constructed(0) => {
                t.octets()?.into_owned()
            }
            _ => return Err(invalid("the encrypted content is missing")),
        };
        let (mut aad, mut mac) = (Vec::new(), None);
        if auth {
            if let Some(attrs) = r.optional(ber::context_constructed(1))? {
                let mut list = Vec::new();
                let mut a = attrs.children();
                while let Some(attr) = a.next()? {
                    list.push(attr.to_der()?);
                }
                aad = ber::tlv(ber::SET, &list.concat());
            }
            mac = Some(r.any()?.octets()?.into_owned());
        }
        if let Ok(c) = &cipher
            && c.is_aead() != auth
        {
            return Err(invalid("the cipher does not fit the kind of encryption"));
        }
        Ok(Envelope {
            recipients,
            cipher,
            encrypted,
            aad,
            mac,
        })
    }

    /// Whether the content was encrypted to this certificate.
    pub fn is_for(&self, cert: &Certificate) -> bool {
        self.recipients.iter().any(|r| match r {
            Recipient::KeyTrans { id, .. } => id.matches(cert),
            Recipient::KeyAgree { keys, .. } => keys.iter().any(|(id, _)| id.matches(cert)),
            Recipient::Other => false,
        })
    }

    /// The content cipher, as a name ("AES-256-CBC"), when supported.
    pub fn cipher(&self) -> Option<String> {
        self.cipher.as_ref().ok().map(ContentAlg::name)
    }

    /// Decrypts the content with the private key of `cert`.
    pub fn decrypt(&self, cert: &Certificate, key: &PrivateKey) -> Result<Vec<u8>> {
        let cipher = self.cipher.clone()?;
        let mut failure = Error::NoKey;
        for r in &self.recipients {
            let cek = match r {
                Recipient::KeyTrans {
                    id,
                    alg,
                    key: wrapped,
                } if id.matches(cert) => {
                    let PrivateKey::Rsa(rsa) = key else {
                        continue;
                    };
                    let alg_tlv = Tlv::parse(alg)?;
                    algs::rsa_decrypt(rsa, &AlgorithmId::from_tlv(alg_tlv)?, wrapped)
                }
                Recipient::KeyAgree {
                    point,
                    ukm,
                    scheme,
                    wrap,
                    wrap_oid,
                    keys,
                } => {
                    let Some((_, wrapped)) = keys.iter().find(|(id, _)| id.matches(cert)) else {
                        continue;
                    };
                    (|| {
                        let hash = algs::ecdh_kdf_hash(*scheme)?;
                        let kek_len = algs::wrap_key_len(*wrap_oid)?;
                        let shared = key.ecdh(point)?;
                        let kek = algs::ecdh_kek(hash, &shared, wrap, ukm.as_deref(), kek_len);
                        algs::aes_unwrap(&kek, wrapped)
                    })()
                }
                _ => continue,
            };
            let tag = self.mac.as_deref().unwrap_or_default();
            match cek.and_then(|cek| cipher.decrypt(&cek, &self.encrypted, &self.aad, tag)) {
                Ok(plain) => return Ok(plain),
                Err(e) => failure = e,
            }
        }
        Err(failure)
    }
}

fn recipient(info: Tlv<'_>) -> Result<Recipient> {
    if info.tag == ber::SEQUENCE {
        let mut r = info.children();
        r.read(ber::INTEGER)?;
        let id = CertId::read(r.any()?)?;
        let alg = r.read(ber::SEQUENCE)?.to_der()?;
        let key = r.any()?.octets()?.into_owned();
        return Ok(Recipient::KeyTrans { id, alg, key });
    }
    if info.tag != ber::context_constructed(1) {
        return Ok(Recipient::Other);
    }
    let mut r = info.children();
    r.read(ber::INTEGER)?;
    // The originator's key: only an ephemeral public key is supported.
    let originator = r.read(ber::context_constructed(0))?.children().any()?;
    if originator.tag != ber::context_constructed(1) {
        return Err(Error::Unsupported(
            "key agreement with the sender's certificate".into(),
        ));
    }
    let mut o = originator.children();
    AlgorithmId::read(&mut o)?;
    let bits = o.read(ber::BIT_STRING)?.to_der()?;
    let bits = Tlv::parse(&bits)?.value;
    let point = bits.get(1..).ok_or_else(|| invalid("empty key"))?.to_vec();
    let ukm = match r.optional(ber::context_constructed(1))? {
        Some(t) => Some(t.children().any()?.octets()?.into_owned()),
        None => None,
    };
    let alg = AlgorithmId::read(&mut r)?;
    let wrap_tlv = alg
        .params
        .ok_or_else(|| invalid("key agreement without its key wrap"))?;
    let wrap = wrap_tlv.to_der()?;
    let wrap_oid = AlgorithmId::from_tlv(wrap_tlv)?.oid;
    let mut keys = Vec::new();
    let mut list = r.read(ber::SEQUENCE)?.children();
    while let Some(entry) = list.next()? {
        let mut e = entry.children();
        let rid = e.any()?;
        let id = if rid.tag == ber::context_constructed(0) {
            // RecipientKeyIdentifier: the subject key identifier first.
            CertId::KeyId(rid.children().any()?.octets()?.into_owned())
        } else {
            CertId::read(rid)?
        };
        keys.push((id, e.any()?.octets()?.into_owned()));
    }
    Ok(Recipient::KeyAgree {
        point,
        ukm,
        scheme: alg.oid,
        wrap,
        wrap_oid,
        keys,
    })
}

// ---------------------------------------------------------------------------
// Making signed and enveloped data

fn der_error(e: der::Error) -> Error {
    Error::Invalid(format!("could not encode: {e}"))
}

fn issuer_and_serial(cert: &Certificate) -> cms::cert::IssuerAndSerialNumber {
    cms::cert::IssuerAndSerialNumber {
        issuer: cert.x509.tbs_certificate.issuer.clone(),
        serial_number: cert.x509.tbs_certificate.serial_number.clone(),
    }
}

fn attribute(kind: Oid, value: Any) -> Result<x509_cert::attr::Attribute> {
    let mut values = SetOfVec::new();
    values.insert(value).map_err(der_error)?;
    Ok(x509_cert::attr::Attribute { oid: kind, values })
}

fn any(der: &[u8]) -> Result<Any> {
    Any::from_der(der).map_err(der_error)
}

/// The ciphers Envelope reads, strongest first, for senders to choose from
/// (SMIMECapabilities).
fn capabilities() -> Vec<u8> {
    let list: Vec<u8> = [
        oid::AES256_CBC,
        oid::AES192_CBC,
        oid::AES128_CBC,
        oid::AES256_GCM,
        oid::AES128_GCM,
    ]
    .iter()
    .flat_map(|o| ber::tlv(ber::SEQUENCE, &ber::tlv(ber::OID, o.as_bytes())))
    .collect();
    ber::tlv(ber::SEQUENCE, &list)
}

/// What signing needs.
pub struct SignWith<'a> {
    pub cert: &'a Certificate,
    pub key: &'a PrivateKey,
    /// Authorities to send along, so recipients can build the chain.
    pub chain: &'a [Certificate],
    /// The certificate to encrypt replies to, if not `cert`.
    pub encryption: Option<&'a Certificate>,
    /// Unix seconds.
    pub time: i64,
}

/// A detached signature over `content`, and its `micalg`.
pub fn sign_detached(content: &[u8], with: &SignWith<'_>) -> Result<(Vec<u8>, &'static str)> {
    use cms::content_info::{CmsVersion, ContentInfo};
    use cms::signed_data::{
        CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo,
        SignerInfos,
    };
    if !with.key.matches(with.cert) {
        return Err(invalid("the key does not belong to the certificate"));
    }
    let hash = with.key.hash();
    let encryption = with.encryption.unwrap_or(with.cert);
    let preference = issuer_and_serial(encryption).to_der().map_err(der_error)?;
    let mut tagged = preference.clone();
    tagged[0] = ber::context_constructed(0);
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(with.time.max(0) as u64);
    let time = x509_cert::time::Time::try_from(time).map_err(der_error)?;
    let mut attrs = SetOfVec::new();
    for attr in [
        attribute(
            oid::CONTENT_TYPE,
            Any::encode_from(&oid::DATA).map_err(der_error)?,
        )?,
        attribute(
            oid::SIGNING_TIME,
            Any::encode_from(&time).map_err(der_error)?,
        )?,
        attribute(
            oid::MESSAGE_DIGEST,
            Any::encode_from(&OctetString::new(hash.digest(content)).map_err(der_error)?)
                .map_err(der_error)?,
        )?,
        attribute(oid::SMIME_CAPABILITIES, any(&capabilities())?)?,
        attribute(oid::ENCRYPTION_KEY_PREFERENCE, any(&tagged)?)?,
        attribute(oid::MS_ENCRYPTION_KEY_PREFERENCE, any(&preference)?)?,
    ] {
        attrs.insert(attr).map_err(der_error)?;
    }
    let signed = attrs.to_der().map_err(der_error)?;
    let (signature, alg) = with.key.sign(&hash.digest(&signed))?;
    let signature_algorithm = match alg {
        SigAlg::Ecdsa(Hash::Sha384) => AlgorithmIdentifierOwned {
            oid: oid::ECDSA_SHA384,
            parameters: None,
        },
        SigAlg::Ecdsa(_) => AlgorithmIdentifierOwned {
            oid: oid::ECDSA_SHA256,
            parameters: None,
        },
        _ => AlgorithmIdentifierOwned {
            oid: oid::SHA256_WITH_RSA,
            parameters: Some(Any::null()),
        },
    };
    let digest_alg = AlgorithmIdentifierOwned {
        oid: hash.oid(),
        parameters: None,
    };
    let signer = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(issuer_and_serial(with.cert)),
        digest_alg: digest_alg.clone(),
        signed_attrs: Some(attrs),
        signature_algorithm,
        signature: OctetString::new(signature).map_err(der_error)?,
        unsigned_attrs: None,
    };
    let mut certs = SetOfVec::new();
    let mut add = |c: &Certificate| -> Result<()> {
        let choice = cms::cert::CertificateChoices::Certificate(c.x509.clone());
        if !certs.iter().any(|x| x == &choice) {
            certs.insert(choice).map_err(der_error)?;
        }
        Ok(())
    };
    add(with.cert)?;
    add(encryption)?;
    for c in with.chain.iter().filter(|c| !c.is_self_issued()) {
        add(c)?;
    }
    let mut digests = SetOfVec::new();
    digests.insert(digest_alg).map_err(der_error)?;
    let mut signers = SetOfVec::new();
    signers.insert(signer).map_err(der_error)?;
    let sd = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: digests,
        encap_content_info: EncapsulatedContentInfo {
            econtent_type: oid::DATA,
            econtent: None,
        },
        certificates: Some(CertificateSet(certs)),
        crls: None,
        signer_infos: SignerInfos(signers),
    };
    let info = ContentInfo {
        content_type: oid::SIGNED_DATA,
        content: Any::encode_from(&sd).map_err(der_error)?,
    };
    Ok((info.to_der().map_err(der_error)?, hash.micalg()))
}

/// Encrypts `content` (AES-256-CBC) to every certificate: RSA keys by key
/// transport (PKCS #1 v1.5), EC keys by ephemeral-static ECDH with AES key
/// wrap.
pub fn encrypt(content: &[u8], recipients: &[&Certificate]) -> Result<Vec<u8>> {
    use cms::content_info::{CmsVersion, ContentInfo};
    use cms::enveloped_data::{
        EncryptedContentInfo, EnvelopedData, KeyAgreeRecipientIdentifier, KeyAgreeRecipientInfo,
        KeyTransRecipientInfo, OriginatorIdentifierOrKey, OriginatorPublicKey,
        RecipientEncryptedKey, RecipientIdentifier, RecipientInfo, RecipientInfos,
    };
    if recipients.is_empty() {
        return Err(invalid("no recipients"));
    }
    let mut rng = rand::rngs::OsRng;
    let mut cek = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(cek.as_mut());
    let mut iv = [0u8; 16];
    rng.fill_bytes(&mut iv);
    let encrypted = algs::aes256_cbc_encrypt(cek.as_ref(), &iv, content)?;
    let mut infos = SetOfVec::new();
    let mut only_rsa = true;
    for cert in recipients {
        let info = match &cert.key {
            PublicKey::Rsa(key) => RecipientInfo::Ktri(KeyTransRecipientInfo {
                version: CmsVersion::V0,
                rid: RecipientIdentifier::IssuerAndSerialNumber(issuer_and_serial(cert)),
                key_enc_alg: AlgorithmIdentifierOwned {
                    oid: oid::RSA_ENCRYPTION,
                    parameters: Some(Any::null()),
                },
                enc_key: OctetString::new(algs::rsa_encrypt(key, cek.as_ref())?)
                    .map_err(der_error)?,
            }),
            PublicKey::P256(_) | PublicKey::P384(_) => {
                only_rsa = false;
                let (scheme, hash) = algs::ecdh_scheme_for(&cert.key);
                let wrap = AlgorithmIdentifierOwned {
                    oid: oid::AES256_WRAP,
                    parameters: None,
                };
                let wrap_der = wrap.to_der().map_err(der_error)?;
                let (point, shared) = ephemeral(&cert.key)?;
                let kek = algs::ecdh_kek(hash, &shared, &wrap_der, None, 32);
                let wrapped = algs::aes256_wrap(&kek, cek.as_ref())?;
                RecipientInfo::Kari(KeyAgreeRecipientInfo {
                    version: CmsVersion::V3,
                    originator: OriginatorIdentifierOrKey::OriginatorKey(OriginatorPublicKey {
                        algorithm: AlgorithmIdentifierOwned {
                            oid: oid::EC_PUBLIC_KEY,
                            parameters: None,
                        },
                        public_key: BitString::from_bytes(&point).map_err(der_error)?,
                    }),
                    ukm: None,
                    key_enc_alg: AlgorithmIdentifierOwned {
                        oid: scheme,
                        parameters: Some(any(&wrap_der)?),
                    },
                    recipient_enc_keys: vec![RecipientEncryptedKey {
                        rid: KeyAgreeRecipientIdentifier::IssuerAndSerialNumber(issuer_and_serial(
                            cert,
                        )),
                        enc_key: OctetString::new(wrapped).map_err(der_error)?,
                    }],
                })
            }
            PublicKey::Other(name) => {
                return Err(Error::Unsupported(format!("encrypting to {name} keys")));
            }
        };
        infos.insert(info).map_err(der_error)?;
    }
    let env = EnvelopedData {
        version: if only_rsa {
            CmsVersion::V0
        } else {
            CmsVersion::V2
        },
        originator_info: None,
        recip_infos: RecipientInfos(infos),
        encrypted_content: EncryptedContentInfo {
            content_type: oid::DATA,
            content_enc_alg: AlgorithmIdentifierOwned {
                oid: oid::AES256_CBC,
                parameters: Some(
                    Any::encode_from(&OctetString::new(iv.to_vec()).map_err(der_error)?)
                        .map_err(der_error)?,
                ),
            },
            encrypted_content: Some(OctetString::new(encrypted).map_err(der_error)?),
        },
        unprotected_attrs: None,
    };
    let info = ContentInfo {
        content_type: oid::ENVELOPED_DATA,
        content: Any::encode_from(&env).map_err(der_error)?,
    };
    info.to_der().map_err(der_error)
}

/// An ephemeral key on the recipient's curve: its public point and the
/// shared secret with the recipient.
fn ephemeral(recipient: &PublicKey) -> Result<(Vec<u8>, Zeroizing<Vec<u8>>)> {
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let mut rng = rand::rngs::OsRng;
    match recipient {
        PublicKey::P256(peer) => {
            let secret = p256::ecdh::EphemeralSecret::random(&mut rng);
            let point = secret
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec();
            let shared = secret.diffie_hellman(peer);
            Ok((point, Zeroizing::new(shared.raw_secret_bytes().to_vec())))
        }
        PublicKey::P384(peer) => {
            let secret = p384::ecdh::EphemeralSecret::random(&mut rng);
            let point = secret
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec();
            let shared = secret.diffie_hellman(peer);
            Ok((point, Zeroizing::new(shared.raw_secret_bytes().to_vec())))
        }
        _ => Err(invalid("not an EC key")),
    }
}
