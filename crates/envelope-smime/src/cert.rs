//! X.509 certificates: reading them, what they say about their owner, and
//! whether they may sign or encrypt mail (RFC 5280, RFC 8550).

use std::ops::Range;

use const_oid::ObjectIdentifier as Oid;
use der::asn1::Any;
use der::{Decode, Encode, Tag, Tagged};
use sha2::{Digest, Sha256};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{
    AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages,
    NameConstraints, SubjectAltName, SubjectKeyIdentifier,
};
use x509_cert::name::Name;

use crate::algs::{PublicKey, SigAlg};
use crate::ber::{self, AlgorithmId, Tlv};
use crate::oid;
use crate::{Error, Result, hex, invalid};

/// A certificate, read.
#[derive(Clone)]
pub struct Certificate {
    der: Vec<u8>,
    pub(crate) x509: x509_cert::Certificate,
    /// Where the signed part (TBSCertificate) is in `der`.
    tbs: Range<usize>,
    pub(crate) key: PublicKey,
    emails: Vec<String>,
    alt_names: Vec<GeneralName>,
    key_usage: Option<KeyUsage>,
    eku: Option<Vec<Oid>>,
    pub(crate) basic: Option<BasicConstraints>,
    ski: Option<Vec<u8>>,
    pub(crate) aki: Option<Vec<u8>>,
    pub(crate) name_constraints: Option<NameConstraints>,
    /// A critical extension Envelope does not understand.
    pub(crate) unknown_critical: Option<Oid>,
    not_before: i64,
    not_after: i64,
}

impl std::fmt::Debug for Certificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Certificate({}, {})", self.subject(), self.fingerprint())
    }
}

impl PartialEq for Certificate {
    fn eq(&self, other: &Self) -> bool {
        self.der == other.der
    }
}

impl Eq for Certificate {}

/// Whether a certificate is valid at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Valid,
    Expired,
    NotYetValid,
}

impl Certificate {
    pub fn from_der(der: &[u8]) -> Result<Certificate> {
        let unreadable = |_| invalid("unreadable certificate");
        let x509 = x509_cert::Certificate::from_der(der).map_err(unreadable)?;
        // The signed part as it was encoded, to check the issuer's signature.
        let outer = Tlv::parse(der)?;
        let tbs = outer.children().read(ber::SEQUENCE)?;
        let start = der.len() - outer.value.len();
        let tbs = start..start + tbs.raw.len();

        let tbs_cert = &x509.tbs_certificate;
        let key = PublicKey::from_spki(&tbs_cert.subject_public_key_info);
        let mut emails: Vec<String> = Vec::new();
        fn add(emails: &mut Vec<String>, e: &str) {
            let e = e.trim().to_lowercase();
            if e.contains('@') && !emails.contains(&e) {
                emails.push(e);
            }
        }
        let extensions = tbs_cert.extensions.as_deref().unwrap_or(&[]);
        // RFC 5280 allows each extension once; a second subject alternative
        // name could slip past name constraints.
        for (i, ext) in extensions.iter().enumerate() {
            if extensions[..i].iter().any(|e| e.extn_id == ext.extn_id) {
                return Err(invalid("a certificate extension appears twice"));
            }
        }
        let mut alt_names = Vec::new();
        let mut key_usage = None;
        let mut eku = None;
        let mut basic = None;
        let mut ski = None;
        let mut aki = None;
        let mut name_constraints = None;
        let mut unknown_critical = None;
        for ext in extensions {
            let value = ext.extn_value.as_bytes();
            let bad = |_| invalid("unreadable certificate extension");
            match ext.extn_id {
                oid::SUBJECT_ALT_NAME => {
                    alt_names = SubjectAltName::from_der(value).map_err(bad)?.0;
                    for name in &alt_names {
                        if let GeneralName::Rfc822Name(email) = name {
                            add(&mut emails, email.as_str());
                        }
                    }
                }
                oid::KEY_USAGE => key_usage = Some(KeyUsage::from_der(value).map_err(bad)?),
                oid::EXT_KEY_USAGE => eku = Some(ExtendedKeyUsage::from_der(value).map_err(bad)?.0),
                oid::BASIC_CONSTRAINTS => {
                    basic = Some(BasicConstraints::from_der(value).map_err(bad)?)
                }
                oid::SUBJECT_KEY_ID => {
                    ski = Some(
                        SubjectKeyIdentifier::from_der(value)
                            .map_err(bad)?
                            .0
                            .as_bytes()
                            .to_vec(),
                    )
                }
                oid::AUTHORITY_KEY_ID => {
                    aki = AuthorityKeyIdentifier::from_der(value)
                        .map_err(bad)?
                        .key_identifier
                        .map(|k| k.as_bytes().to_vec())
                }
                oid::NAME_CONSTRAINTS => {
                    name_constraints = Some(NameConstraints::from_der(value).map_err(bad)?)
                }
                // Understood well enough: they only add information, or (the
                // policies) constrain nothing Envelope relies on.
                oid::CERTIFICATE_POLICIES
                | oid::ISSUER_ALT_NAME
                | oid::CRL_DISTRIBUTION_POINTS
                | oid::AUTHORITY_INFO_ACCESS
                | oid::SUBJECT_DIRECTORY_ATTRIBUTES => {}
                other if ext.critical && unknown_critical.is_none() => {
                    unknown_critical = Some(other)
                }
                _ => {}
            }
        }
        // Older certificates carry the address in the subject only. It counts
        // only then: name constraints check it only then (RFC 5280, 4.2.1.10).
        if emails.is_empty() {
            for value in name_values(&tbs_cert.subject, oid::EMAIL_ADDRESS) {
                add(&mut emails, &value);
            }
        }
        Ok(Certificate {
            der: der.to_vec(),
            tbs,
            key,
            emails,
            alt_names,
            key_usage,
            eku,
            basic,
            ski,
            aki,
            name_constraints,
            unknown_critical,
            not_before: tbs_cert.validity.not_before.to_unix_duration().as_secs() as i64,
            not_after: tbs_cert.validity.not_after.to_unix_duration().as_secs() as i64,
            x509,
        })
    }

    /// Reads certificates: PEM (one or several), DER, or a PKCS #7
    /// certificate bundle (`.p7b`, `.p7c`).
    pub fn parse_all(data: &[u8]) -> Result<Vec<Certificate>> {
        let text = String::from_utf8_lossy(data);
        if text.contains("-----BEGIN ") {
            let mut out = Vec::new();
            let mut rest: &str = &text;
            while let Some(start) = rest.find("-----BEGIN ") {
                let block = &rest[start..];
                let (label, der) = crate::key::pem_rfc7468_decode(block)?;
                match label.as_str() {
                    "CERTIFICATE" | "X509 CERTIFICATE" | "TRUSTED CERTIFICATE" => {
                        out.push(Certificate::from_der(&der)?)
                    }
                    "PKCS7" | "CMS" => out.extend(crate::cms::certificates_of(&der)?),
                    _ => {}
                }
                let end = block
                    .find(&format!("-----END {label}-----"))
                    .map(|e| e + label.len() + 14)
                    .unwrap_or(block.len());
                rest = &block[end.min(block.len())..];
            }
            if out.is_empty() {
                return Err(invalid("no certificate found"));
            }
            return Ok(out);
        }
        match Certificate::from_der(data) {
            Ok(c) => Ok(vec![c]),
            Err(e) => crate::cms::certificates_of(data)
                .ok()
                .filter(|c| !c.is_empty())
                .ok_or(e),
        }
    }

    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// The certificate as PEM.
    pub fn to_pem(&self) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&self.der);
        let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            out.push_str(&String::from_utf8_lossy(line));
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
        out
    }

    /// SHA-256 of the certificate, upper-case hex.
    pub fn fingerprint(&self) -> String {
        hex(&Sha256::digest(&self.der))
    }

    /// The name to show: the subject's common name, else its organization,
    /// else its address.
    pub fn name(&self) -> String {
        display_name(&self.x509.tbs_certificate.subject)
            .or_else(|| self.emails.first().cloned())
            .unwrap_or_else(|| self.subject())
    }

    /// The subject, written out (RFC 4514).
    pub fn subject(&self) -> String {
        self.x509.tbs_certificate.subject.to_string()
    }

    /// The issuer's name to show.
    pub fn issuer_name(&self) -> String {
        display_name(&self.x509.tbs_certificate.issuer).unwrap_or_else(|| self.issuer())
    }

    /// The issuer, written out (RFC 4514).
    pub fn issuer(&self) -> String {
        self.x509.tbs_certificate.issuer.to_string()
    }

    /// The serial number in hex, colon separated.
    pub fn serial(&self) -> String {
        let bytes = ber::strip_zeros(self.x509.tbs_certificate.serial_number.as_bytes());
        let bytes = if bytes.is_empty() { &[0u8][..] } else { bytes };
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    /// The addresses the certificate is for (lower case).
    pub fn emails(&self) -> &[String] {
        &self.emails
    }

    pub fn has_email(&self, email: &str) -> bool {
        let email = email.trim().to_lowercase();
        self.emails.contains(&email)
    }

    /// Unix seconds.
    pub fn not_before(&self) -> i64 {
        self.not_before
    }

    /// Unix seconds.
    pub fn not_after(&self) -> i64 {
        self.not_after
    }

    pub fn validity_at(&self, time: i64) -> Validity {
        if time < self.not_before {
            Validity::NotYetValid
        } else if time > self.not_after {
            Validity::Expired
        } else {
            Validity::Valid
        }
    }

    /// "RSA 2048", "EC P-256", …
    pub fn algorithm(&self) -> String {
        self.key.describe()
    }

    /// A certificate authority's certificate.
    pub fn is_ca(&self) -> bool {
        self.basic.as_ref().is_some_and(|b| b.ca)
    }

    fn usage_allows(&self, uses: &[KeyUsages]) -> bool {
        self.key_usage
            .as_ref()
            .is_none_or(|ku| uses.iter().any(|u| ku.0.contains(*u)))
    }

    /// The extended key usage, if any, includes e-mail.
    pub(crate) fn eku_allows_email(&self) -> bool {
        self.eku.as_ref().is_none_or(|list| {
            list.contains(&oid::EMAIL_PROTECTION) || list.contains(&oid::ANY_EXTENDED_KEY_USAGE)
        })
    }

    /// The certificate may sign mail (RFC 8550, 4.4): an end entity's key
    /// Envelope can check, the digital signature or non-repudiation key
    /// usage, and e-mail protection among its extended key usages, when the
    /// certificate restricts them.
    pub fn can_sign_mail(&self) -> bool {
        self.key.is_usable()
            && !self.is_ca()
            && self.usage_allows(&[KeyUsages::DigitalSignature, KeyUsages::NonRepudiation])
            && self.eku_allows_email()
    }

    /// The certificate may receive encrypted mail: key encipherment for RSA,
    /// key agreement for EC keys, and e-mail protection when the extended
    /// key usages are restricted.
    pub fn can_encrypt_mail(&self) -> bool {
        let usage = if self.key.is_rsa() {
            KeyUsages::KeyEncipherment
        } else {
            KeyUsages::KeyAgreement
        };
        self.key.is_usable()
            && !self.is_ca()
            && self.usage_allows(&[usage])
            && self.eku_allows_email()
    }

    /// An authority may issue certificates with this key.
    pub(crate) fn may_certify(&self) -> bool {
        self.is_ca() && self.usage_allows(&[KeyUsages::KeyCertSign])
    }

    /// The subject, encoded, for matching issuers.
    pub(crate) fn subject_der(&self) -> Vec<u8> {
        self.x509
            .tbs_certificate
            .subject
            .to_der()
            .unwrap_or_default()
    }

    pub(crate) fn issuer_der(&self) -> Vec<u8> {
        self.x509
            .tbs_certificate
            .issuer
            .to_der()
            .unwrap_or_default()
    }

    pub(crate) fn subject_key_id(&self) -> Option<&[u8]> {
        self.ski.as_deref()
    }

    /// Whether this certificate is the one named by an IssuerAndSerialNumber.
    pub(crate) fn is_issuer_and_serial(&self, issuer: &[u8], serial: &[u8]) -> bool {
        ber::strip_zeros(self.x509.tbs_certificate.serial_number.as_bytes())
            == ber::strip_zeros(serial)
            && self.issuer_der() == issuer
    }

    /// Checks the signature an issuer's key made over this certificate.
    /// SHA-1 signatures are refused, as in browsers: they can be forged.
    pub(crate) fn signed_by(&self, issuer: &PublicKey) -> Result<()> {
        let alg_der = self
            .x509
            .signature_algorithm
            .to_der()
            .map_err(|_| invalid("unreadable signature algorithm"))?;
        let alg = SigAlg::from_id(&AlgorithmId::from_tlv(Tlv::parse(&alg_der)?)?, None)?;
        if alg.hash() == crate::algs::Hash::Sha1 {
            return Err(Error::Unsupported("SHA-1 certificate signatures".into()));
        }
        let digest = alg.hash().digest(&self.der[self.tbs.clone()]);
        let signature = self
            .x509
            .signature
            .as_bytes()
            .ok_or_else(|| invalid("unreadable signature"))?;
        if issuer.verify(alg, &digest, signature) {
            Ok(())
        } else {
            Err(Error::Crypto("signature does not match".into()))
        }
    }

    pub(crate) fn is_self_issued(&self) -> bool {
        self.subject_der() == self.issuer_der()
    }

    /// The names a name constraint applies to: the subject (unless empty)
    /// and the alternative names, with the subject's e-mail attributes when
    /// there is no rfc822Name.
    pub(crate) fn constrained_names(&self) -> Vec<GeneralName> {
        let tbs = &self.x509.tbs_certificate;
        let mut names = Vec::new();
        if !tbs.subject.0.is_empty() {
            names.push(GeneralName::DirectoryName(tbs.subject.clone()));
        }
        let has_rfc822 = self
            .alt_names
            .iter()
            .any(|n| matches!(n, GeneralName::Rfc822Name(_)));
        names.extend(self.alt_names.iter().cloned());
        if !has_rfc822 {
            for value in name_values(&tbs.subject, oid::EMAIL_ADDRESS) {
                if let Ok(ia5) = der::asn1::Ia5String::new(&value) {
                    names.push(GeneralName::Rfc822Name(ia5));
                }
            }
        }
        names
    }
}

/// A string attribute value (DirectoryString or IA5String).
pub(crate) fn string_value(value: &Any) -> Option<String> {
    let bytes = value.value();
    match value.tag() {
        Tag::Utf8String | Tag::PrintableString | Tag::Ia5String | Tag::VisibleString => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        // T.61, read as Latin-1 as everyone does.
        Tag::TeletexString => Some(bytes.iter().map(|b| *b as char).collect()),
        Tag::BmpString => {
            let units: Vec<u16> = bytes
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

/// The values of one attribute type in a name.
pub(crate) fn name_values(name: &Name, attr: Oid) -> Vec<String> {
    name.0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .filter(|atv| atv.oid == attr)
        .filter_map(|atv| string_value(&atv.value))
        .collect()
}

/// A name's common name, else its organization.
fn display_name(name: &Name) -> Option<String> {
    name_values(name, oid::COMMON_NAME)
        .into_iter()
        .last()
        .or_else(|| name_values(name, oid::ORGANIZATION).into_iter().last())
        .filter(|n| !n.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_of_string_types() {
        let utf8 = Any::new(Tag::Utf8String, "Ömer".as_bytes()).unwrap();
        assert_eq!(string_value(&utf8).as_deref(), Some("Ömer"));
        let bmp = Any::new(Tag::BmpString, [0x00, 0x41, 0x01, 0x1f].as_slice()).unwrap();
        assert_eq!(string_value(&bmp).as_deref(), Some("Ağ"));
        let int = Any::new(Tag::Integer, [1u8].as_slice()).unwrap();
        assert_eq!(string_value(&int), None);
    }
}
