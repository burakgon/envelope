//! Private keys: RSA, and ECDSA / ECDH on P-256 and P-384.

use const_oid::ObjectIdentifier as Oid;
use pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rsa::RsaPrivateKey;
use zeroize::Zeroizing;

use crate::algs::{Hash, PublicKey, SigAlg};
use crate::cert::Certificate;
use crate::oid;
use crate::{Error, Result, invalid};

/// A private key, as kept in the keyring (PKCS #8).
#[derive(Clone)]
pub enum PrivateKey {
    Rsa(Box<RsaPrivateKey>),
    P256(p256::SecretKey),
    P384(p384::SecretKey),
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrivateKey({})", self.public_key().describe())
    }
}

impl PrivateKey {
    /// Reads an unencrypted PKCS #8 key.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<PrivateKey> {
        let info = pkcs8::PrivateKeyInfo::try_from(der).map_err(unreadable)?;
        match info.algorithm.oid {
            oid::RSA_ENCRYPTION => Ok(PrivateKey::Rsa(Box::new(
                RsaPrivateKey::from_pkcs8_der(der).map_err(unreadable)?,
            ))),
            oid::EC_PUBLIC_KEY => match info.algorithm.parameters_oid().ok() {
                Some(oid::SECP256R1) => Ok(PrivateKey::P256(
                    p256::SecretKey::from_pkcs8_der(der).map_err(unreadable)?,
                )),
                Some(oid::SECP384R1) => Ok(PrivateKey::P384(
                    p384::SecretKey::from_pkcs8_der(der).map_err(unreadable)?,
                )),
                Some(curve) => Err(Error::Unsupported(format!(
                    "the curve {}",
                    curve_name(curve)
                ))),
                None => Err(invalid("an EC key without its curve")),
            },
            other => Err(Error::Unsupported(format!("keys of type {other}"))),
        }
    }

    /// Reads a PEM key: PKCS #8, or the older RSA (PKCS #1) and EC (SEC 1)
    /// forms.
    pub fn from_pem(pem: &str) -> Result<PrivateKey> {
        use rsa::pkcs1::DecodeRsaPrivateKey;
        if pem.contains("-----BEGIN RSA PRIVATE KEY-----") {
            return Ok(PrivateKey::Rsa(Box::new(
                RsaPrivateKey::from_pkcs1_pem(pem).map_err(unreadable)?,
            )));
        }
        if pem.contains("-----BEGIN EC PRIVATE KEY-----") {
            if let Ok(k) = p256::SecretKey::from_sec1_pem(pem) {
                return Ok(PrivateKey::P256(k));
            }
            return Ok(PrivateKey::P384(
                p384::SecretKey::from_sec1_pem(pem).map_err(unreadable)?,
            ));
        }
        let (label, der) = pem_rfc7468_decode(pem)?;
        if label != "PRIVATE KEY" {
            return Err(invalid("not a private key"));
        }
        PrivateKey::from_pkcs8_der(&der)
    }

    /// The key as unencrypted PKCS #8 PEM, for the keyring.
    pub fn to_pem(&self) -> Result<Zeroizing<String>> {
        let failed = |e: pkcs8::Error| Error::Crypto(format!("could not encode the key: {e}"));
        match self {
            PrivateKey::Rsa(k) => k.to_pkcs8_pem(LineEnding::LF).map_err(failed),
            PrivateKey::P256(k) => k.to_pkcs8_pem(LineEnding::LF).map_err(failed),
            PrivateKey::P384(k) => k.to_pkcs8_pem(LineEnding::LF).map_err(failed),
        }
    }

    pub(crate) fn public_key(&self) -> PublicKey {
        match self {
            PrivateKey::Rsa(k) => PublicKey::Rsa(k.to_public_key()),
            PrivateKey::P256(k) => PublicKey::P256(k.public_key()),
            PrivateKey::P384(k) => PublicKey::P384(k.public_key()),
        }
    }

    /// Whether this is the private key of the certificate.
    pub fn matches(&self, cert: &Certificate) -> bool {
        self.public_key().same_as(&cert.key)
    }

    /// The digest to sign with: as strong as the key.
    pub(crate) fn hash(&self) -> Hash {
        match self {
            PrivateKey::P384(_) => Hash::Sha384,
            _ => Hash::Sha256,
        }
    }

    /// Signs a digest made with `self.hash()`.
    pub(crate) fn sign(&self, digest: &[u8]) -> Result<(Vec<u8>, SigAlg)> {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        let hash = self.hash();
        let failed = |e: &dyn std::fmt::Display| Error::Crypto(format!("signing failed: {e}"));
        match self {
            PrivateKey::Rsa(k) => {
                let scheme = rsa::Pkcs1v15Sign::new::<sha2::Sha256>();
                let sig = k.sign(scheme, digest).map_err(|e| failed(&e))?;
                Ok((sig, SigAlg::RsaPkcs1(hash)))
            }
            PrivateKey::P256(k) => {
                let sig: p256::ecdsa::Signature = p256::ecdsa::SigningKey::from(k)
                    .sign_prehash(digest)
                    .map_err(|e| failed(&e))?;
                Ok((sig.to_der().as_bytes().to_vec(), SigAlg::Ecdsa(hash)))
            }
            PrivateKey::P384(k) => {
                let sig: p384::ecdsa::Signature = p384::ecdsa::SigningKey::from(k)
                    .sign_prehash(digest)
                    .map_err(|e| failed(&e))?;
                Ok((sig.to_der().as_bytes().to_vec(), SigAlg::Ecdsa(hash)))
            }
        }
    }

    /// The shared secret of an ECDH key agreement with `point`, an encoded
    /// public point of the same curve.
    pub(crate) fn ecdh(&self, point: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let bad = |_| invalid("bad public key in the message");
        Ok(Zeroizing::new(match self {
            PrivateKey::P256(k) => {
                let public = p256::PublicKey::from_sec1_bytes(point).map_err(bad)?;
                p256::ecdh::diffie_hellman(k.to_nonzero_scalar(), public.as_affine())
                    .raw_secret_bytes()
                    .to_vec()
            }
            PrivateKey::P384(k) => {
                let public = p384::PublicKey::from_sec1_bytes(point).map_err(bad)?;
                p384::ecdh::diffie_hellman(k.to_nonzero_scalar(), public.as_affine())
                    .raw_secret_bytes()
                    .to_vec()
            }
            PrivateKey::Rsa(_) => return Err(invalid("not an EC key")),
        }))
    }
}

fn unreadable<E>(_: E) -> Error {
    invalid("unreadable private key")
}

/// Names of the curves Envelope does not support, for messages.
fn curve_name(curve: Oid) -> String {
    match curve {
        oid::SECP521R1 => "P-521".into(),
        other => other.to_string(),
    }
}

/// The label and content of the first PEM block.
pub(crate) fn pem_rfc7468_decode(pem: &str) -> Result<(String, Zeroizing<Vec<u8>>)> {
    use base64::Engine;
    let start = pem
        .find("-----BEGIN ")
        .ok_or_else(|| invalid("no PEM block"))?;
    let rest = &pem[start + 11..];
    let label_end = rest.find("-----").ok_or_else(|| invalid("bad PEM block"))?;
    let label = rest[..label_end].to_string();
    let body = &rest[label_end + 5..];
    let end = body
        .find(&format!("-----END {label}-----"))
        .ok_or_else(|| invalid("unterminated PEM block"))?;
    let text: String = body[..end]
        .lines()
        .filter(|l| !l.contains(':'))
        .flat_map(|l| l.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| invalid("bad PEM block"))?;
    Ok((label, Zeroizing::new(der)))
}
