//! The algorithms behind S/MIME: digests, signatures, RSA key transport,
//! ECDH key agreement and content encryption. SHA-1, 3DES and RC2 are only
//! ever used to read old mail and old PKCS #12 files.

use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use const_oid::ObjectIdentifier as Oid;
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, Pkcs1v15Encrypt, Pkcs1v15Sign, Pss, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};
use zeroize::Zeroizing;

use crate::ber::{self, AlgorithmId};
use crate::oid;
use crate::{Error, Result, invalid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hash {
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub fn from_oid(id: Oid) -> Option<Hash> {
        Some(match id {
            oid::SHA1 => Hash::Sha1,
            oid::SHA224 => Hash::Sha224,
            oid::SHA256 => Hash::Sha256,
            oid::SHA384 => Hash::Sha384,
            oid::SHA512 => Hash::Sha512,
            _ => return None,
        })
    }

    pub fn oid(self) -> Oid {
        match self {
            Hash::Sha1 => oid::SHA1,
            Hash::Sha224 => oid::SHA224,
            Hash::Sha256 => oid::SHA256,
            Hash::Sha384 => oid::SHA384,
            Hash::Sha512 => oid::SHA512,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha224 => "SHA-224",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
        }
    }

    /// The `micalg` parameter of `multipart/signed` (RFC 8551).
    pub fn micalg(self) -> &'static str {
        match self {
            Hash::Sha1 => "sha-1",
            Hash::Sha224 => "sha-224",
            Hash::Sha256 => "sha-256",
            Hash::Sha384 => "sha-384",
            Hash::Sha512 => "sha-512",
        }
    }

    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => Sha1::digest(data).to_vec(),
            Hash::Sha224 => Sha224::digest(data).to_vec(),
            Hash::Sha256 => Sha256::digest(data).to_vec(),
            Hash::Sha384 => Sha384::digest(data).to_vec(),
            Hash::Sha512 => Sha512::digest(data).to_vec(),
        }
    }
}

/// The hash named by an AlgorithmIdentifier inside other parameters, SHA-1
/// when absent (the default of RSASSA-PSS, RSAES-OAEP and PBKDF2).
fn hash_param(tlv: Option<ber::Tlv<'_>>) -> Result<Hash> {
    let Some(tlv) = tlv else {
        return Ok(Hash::Sha1);
    };
    let alg = AlgorithmId::read(&mut tlv.children())?;
    Hash::from_oid(alg.oid).ok_or_else(|| Error::Unsupported(format!("the hash {}", alg.oid)))
}

/// The hash of an MGF1 mask generation function parameter.
fn mgf_param(tlv: Option<ber::Tlv<'_>>) -> Result<Hash> {
    let Some(tlv) = tlv else {
        return Ok(Hash::Sha1);
    };
    let alg = AlgorithmId::read(&mut tlv.children())?;
    if alg.oid != oid::MGF1 {
        return Err(Error::Unsupported(format!("the mask function {}", alg.oid)));
    }
    let params = alg.params.ok_or_else(|| invalid("MGF1 without its hash"))?;
    let hash = AlgorithmId::from_tlv(params)?;
    Hash::from_oid(hash.oid).ok_or_else(|| Error::Unsupported(format!("the hash {}", hash.oid)))
}

// ---------------------------------------------------------------------------
// Public keys and signatures

/// A public key from a certificate.
#[derive(Debug, Clone)]
pub(crate) enum PublicKey {
    Rsa(RsaPublicKey),
    P256(p256::PublicKey),
    P384(p384::PublicKey),
    /// A key Envelope cannot use, by name.
    Other(String),
}

impl PublicKey {
    pub fn from_spki(spki: &spki::SubjectPublicKeyInfoOwned) -> PublicKey {
        use rsa::pkcs1::DecodeRsaPublicKey;
        let bits = spki.subject_public_key.raw_bytes();
        match spki.algorithm.oid {
            oid::RSA_ENCRYPTION | oid::RSASSA_PSS => RsaPublicKey::from_pkcs1_der(bits)
                .map(PublicKey::Rsa)
                .unwrap_or_else(|_| PublicKey::Other("RSA".into())),
            oid::EC_PUBLIC_KEY => {
                let curve = spki
                    .algorithm
                    .parameters
                    .as_ref()
                    .and_then(|p| p.decode_as::<Oid>().ok());
                match curve {
                    Some(oid::SECP256R1) => p256::PublicKey::from_sec1_bytes(bits)
                        .map(PublicKey::P256)
                        .unwrap_or_else(|_| PublicKey::Other("EC P-256".into())),
                    Some(oid::SECP384R1) => p384::PublicKey::from_sec1_bytes(bits)
                        .map(PublicKey::P384)
                        .unwrap_or_else(|_| PublicKey::Other("EC P-384".into())),
                    Some(oid::SECP521R1) => PublicKey::Other("EC P-521".into()),
                    _ => PublicKey::Other("EC".into()),
                }
            }
            oid::ED25519 => PublicKey::Other("Ed25519".into()),
            other => PublicKey::Other(other.to_string()),
        }
    }

    /// "RSA 2048", "EC P-256", …
    pub fn describe(&self) -> String {
        match self {
            PublicKey::Rsa(k) => format!("RSA {}", k.n().bits()),
            PublicKey::P256(_) => "EC P-256".into(),
            PublicKey::P384(_) => "EC P-384".into(),
            PublicKey::Other(name) => name.clone(),
        }
    }

    pub fn is_usable(&self) -> bool {
        !matches!(self, PublicKey::Other(_))
    }

    pub fn is_rsa(&self) -> bool {
        matches!(self, PublicKey::Rsa(_))
    }

    pub fn same_as(&self, other: &PublicKey) -> bool {
        match (self, other) {
            (PublicKey::Rsa(a), PublicKey::Rsa(b)) => a == b,
            (PublicKey::P256(a), PublicKey::P256(b)) => a == b,
            (PublicKey::P384(a), PublicKey::P384(b)) => a == b,
            _ => false,
        }
    }

    /// Checks a signature over a digest.
    pub fn verify(&self, alg: SigAlg, digest: &[u8], signature: &[u8]) -> bool {
        use p256::ecdsa::signature::hazmat::PrehashVerifier;
        match (self, alg) {
            // Keys this small are broken.
            (PublicKey::Rsa(k), _) if k.n().bits() < 1024 => false,
            (PublicKey::Rsa(k), SigAlg::RsaPkcs1(hash)) => {
                k.verify(pkcs1_scheme(hash), digest, signature).is_ok()
            }
            (PublicKey::Rsa(k), SigAlg::RsaPss { hash, salt }) => {
                k.verify(pss_scheme(hash, salt), digest, signature).is_ok()
            }
            (PublicKey::P256(k), SigAlg::Ecdsa(_)) => {
                let Ok(sig) = p256::ecdsa::Signature::from_der(signature) else {
                    return false;
                };
                p256::ecdsa::VerifyingKey::from(k)
                    .verify_prehash(digest, &sig)
                    .is_ok()
            }
            (PublicKey::P384(k), SigAlg::Ecdsa(_)) => {
                let Ok(sig) = p384::ecdsa::Signature::from_der(signature) else {
                    return false;
                };
                p384::ecdsa::VerifyingKey::from(k)
                    .verify_prehash(digest, &sig)
                    .is_ok()
            }
            _ => false,
        }
    }
}

fn pkcs1_scheme(hash: Hash) -> Pkcs1v15Sign {
    match hash {
        Hash::Sha1 => Pkcs1v15Sign::new::<Sha1>(),
        Hash::Sha224 => Pkcs1v15Sign::new::<Sha224>(),
        Hash::Sha256 => Pkcs1v15Sign::new::<Sha256>(),
        Hash::Sha384 => Pkcs1v15Sign::new::<Sha384>(),
        Hash::Sha512 => Pkcs1v15Sign::new::<Sha512>(),
    }
}

fn pss_scheme(hash: Hash, salt: usize) -> Pss {
    match hash {
        Hash::Sha1 => Pss::new_with_salt::<Sha1>(salt),
        Hash::Sha224 => Pss::new_with_salt::<Sha224>(salt),
        Hash::Sha256 => Pss::new_with_salt::<Sha256>(salt),
        Hash::Sha384 => Pss::new_with_salt::<Sha384>(salt),
        Hash::Sha512 => Pss::new_with_salt::<Sha512>(salt),
    }
}

/// A signature algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SigAlg {
    RsaPkcs1(Hash),
    RsaPss { hash: Hash, salt: usize },
    Ecdsa(Hash),
}

impl SigAlg {
    /// The algorithm of a certificate's or a CMS signer's signature. The
    /// bare key algorithms some signers name (`rsaEncryption`,
    /// `id-ecPublicKey`) take `digest`, the signer's digest algorithm.
    pub fn from_id(alg: &AlgorithmId<'_>, digest: Option<Hash>) -> Result<SigAlg> {
        let needs = |h: Option<Hash>| h.ok_or_else(|| invalid("a signature without its hash"));
        Ok(match alg.oid {
            oid::RSA_ENCRYPTION => SigAlg::RsaPkcs1(needs(digest)?),
            oid::SHA1_WITH_RSA => SigAlg::RsaPkcs1(Hash::Sha1),
            oid::SHA224_WITH_RSA => SigAlg::RsaPkcs1(Hash::Sha224),
            oid::SHA256_WITH_RSA => SigAlg::RsaPkcs1(Hash::Sha256),
            oid::SHA384_WITH_RSA => SigAlg::RsaPkcs1(Hash::Sha384),
            oid::SHA512_WITH_RSA => SigAlg::RsaPkcs1(Hash::Sha512),
            oid::EC_PUBLIC_KEY => SigAlg::Ecdsa(needs(digest)?),
            oid::ECDSA_SHA1 => SigAlg::Ecdsa(Hash::Sha1),
            oid::ECDSA_SHA224 => SigAlg::Ecdsa(Hash::Sha224),
            oid::ECDSA_SHA256 => SigAlg::Ecdsa(Hash::Sha256),
            oid::ECDSA_SHA384 => SigAlg::Ecdsa(Hash::Sha384),
            oid::ECDSA_SHA512 => SigAlg::Ecdsa(Hash::Sha512),
            oid::RSASSA_PSS => {
                // RSASSA-PSS-params: [0] hash, [1] mask generation, [2] salt
                // length, [3] trailer field, each with a default.
                let params = alg
                    .params
                    .ok_or_else(|| invalid("RSA-PSS without parameters"))?;
                let mut r = params.children();
                let hash = hash_param(r.optional(ber::context_constructed(0))?)?;
                let mgf = mgf_param(r.optional(ber::context_constructed(1))?)?;
                let salt = match r.optional(ber::context_constructed(2))? {
                    Some(t) => t.children().read(ber::INTEGER)?.small_uint()? as usize,
                    None => 20,
                };
                if let Some(t) = r.optional(ber::context_constructed(3))?
                    && t.children().read(ber::INTEGER)?.small_uint()? != 1
                {
                    return Err(Error::Unsupported("an RSA-PSS trailer field".into()));
                }
                if mgf != hash {
                    return Err(Error::Unsupported(format!(
                        "RSA-PSS with {} and MGF1 with {}",
                        hash.name(),
                        mgf.name()
                    )));
                }
                SigAlg::RsaPss { hash, salt }
            }
            oid::ED25519 => return Err(Error::Unsupported("Ed25519".into())),
            other => {
                return Err(Error::Unsupported(format!(
                    "the signature algorithm {other}"
                )));
            }
        })
    }

    pub fn hash(self) -> Hash {
        match self {
            SigAlg::RsaPkcs1(h) | SigAlg::Ecdsa(h) | SigAlg::RsaPss { hash: h, .. } => h,
        }
    }

    /// "RSA, SHA-256" and the like.
    pub fn name(self) -> String {
        match self {
            SigAlg::RsaPkcs1(h) => format!("RSA, {}", h.name()),
            SigAlg::RsaPss { hash, .. } => format!("RSA-PSS, {}", hash.name()),
            SigAlg::Ecdsa(h) => format!("ECDSA, {}", h.name()),
        }
    }
}

// ---------------------------------------------------------------------------
// Key transport and key agreement

/// Decrypts a content-encryption key sent with RSA (PKCS #1 v1.5 or OAEP).
pub(crate) fn rsa_decrypt(
    key: &RsaPrivateKey,
    alg: &AlgorithmId<'_>,
    encrypted: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let failed = |_| Error::Crypto("the message key could not be decrypted".into());
    match alg.oid {
        oid::RSA_ENCRYPTION => Ok(Zeroizing::new(
            key.decrypt(Pkcs1v15Encrypt, encrypted).map_err(failed)?,
        )),
        oid::RSAES_OAEP => {
            let (mut hash, mut mgf) = (Hash::Sha1, Hash::Sha1);
            if let Some(params) = alg.params {
                let mut r = params.children();
                hash = hash_param(r.optional(ber::context_constructed(0))?)?;
                mgf = mgf_param(r.optional(ber::context_constructed(1))?)?;
                if let Some(source) = r.optional(ber::context_constructed(2))? {
                    let label = AlgorithmId::read(&mut source.children())?;
                    let empty = label
                        .params
                        .map(|p| p.octets().map(|o| o.is_empty()))
                        .transpose()?
                        .unwrap_or(true);
                    if !empty {
                        return Err(Error::Unsupported("RSA-OAEP with a label".into()));
                    }
                }
            }
            let scheme = match (hash, mgf) {
                (Hash::Sha1, Hash::Sha1) => Oaep::new::<Sha1>(),
                (Hash::Sha224, Hash::Sha224) => Oaep::new::<Sha224>(),
                (Hash::Sha256, Hash::Sha256) => Oaep::new::<Sha256>(),
                (Hash::Sha384, Hash::Sha384) => Oaep::new::<Sha384>(),
                (Hash::Sha512, Hash::Sha512) => Oaep::new::<Sha512>(),
                (Hash::Sha256, Hash::Sha1) => Oaep::new_with_mgf_hash::<Sha256, Sha1>(),
                (h, m) => {
                    return Err(Error::Unsupported(format!(
                        "RSA-OAEP with {} and MGF1 with {}",
                        h.name(),
                        m.name()
                    )));
                }
            };
            Ok(Zeroizing::new(
                key.decrypt(scheme, encrypted).map_err(failed)?,
            ))
        }
        other => Err(Error::Unsupported(format!("the key transport {other}"))),
    }
}

/// Encrypts a content-encryption key to an RSA key (PKCS #1 v1.5, what
/// every S/MIME client reads).
pub(crate) fn rsa_encrypt(key: &RsaPublicKey, cek: &[u8]) -> Result<Vec<u8>> {
    key.encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, cek)
        .map_err(|e| Error::Crypto(format!("RSA encryption failed: {e}")))
}

/// The hash of an ECDH key agreement scheme (RFC 5753). The cofactor and
/// standard schemes agree on P-256 and P-384, whose cofactor is 1.
pub(crate) fn ecdh_kdf_hash(scheme: Oid) -> Result<Hash> {
    Ok(match scheme {
        oid::DH_STD_SHA1_KDF | oid::DH_COFACTOR_SHA1_KDF => Hash::Sha1,
        oid::DH_STD_SHA224_KDF | oid::DH_COFACTOR_SHA224_KDF => Hash::Sha224,
        oid::DH_STD_SHA256_KDF | oid::DH_COFACTOR_SHA256_KDF => Hash::Sha256,
        oid::DH_STD_SHA384_KDF | oid::DH_COFACTOR_SHA384_KDF => Hash::Sha384,
        oid::DH_STD_SHA512_KDF | oid::DH_COFACTOR_SHA512_KDF => Hash::Sha512,
        other => return Err(Error::Unsupported(format!("the key agreement {other}"))),
    })
}

/// The scheme to use for a curve when sending.
pub(crate) fn ecdh_scheme_for(key: &PublicKey) -> (Oid, Hash) {
    match key {
        PublicKey::P384(_) => (oid::DH_STD_SHA384_KDF, Hash::Sha384),
        _ => (oid::DH_STD_SHA256_KDF, Hash::Sha256),
    }
}

/// The key-encryption key of an ECDH key agreement: ANSI X9.63 KDF over the
/// shared secret and the ECC-CMS-SharedInfo of RFC 5753.
pub(crate) fn ecdh_kek(
    hash: Hash,
    shared: &[u8],
    wrap_alg_der: &[u8],
    ukm: Option<&[u8]>,
    kek_len: usize,
) -> Zeroizing<Vec<u8>> {
    let mut info = Vec::new();
    info.extend_from_slice(wrap_alg_der);
    if let Some(ukm) = ukm {
        info.extend(ber::tlv(
            ber::context_constructed(0),
            &ber::tlv(ber::OCTET_STRING, ukm),
        ));
    }
    let bits = ((kek_len * 8) as u32).to_be_bytes();
    info.extend(ber::tlv(
        ber::context_constructed(2),
        &ber::tlv(ber::OCTET_STRING, &bits),
    ));
    let info = ber::tlv(ber::SEQUENCE, &info);
    let mut out = Zeroizing::new(Vec::with_capacity(kek_len + 64));
    let mut counter: u32 = 1;
    while out.len() < kek_len {
        let mut input = Zeroizing::new(Vec::with_capacity(shared.len() + 4 + info.len()));
        input.extend_from_slice(shared);
        input.extend_from_slice(&counter.to_be_bytes());
        input.extend_from_slice(&info);
        out.extend_from_slice(&hash.digest(&input));
        counter += 1;
    }
    out.truncate(kek_len);
    out
}

/// The length of the key an AES key wrap algorithm takes.
pub(crate) fn wrap_key_len(wrap: Oid) -> Result<usize> {
    Ok(match wrap {
        oid::AES128_WRAP => 16,
        oid::AES192_WRAP => 24,
        oid::AES256_WRAP => 32,
        other => return Err(Error::Unsupported(format!("the key wrap {other}"))),
    })
}

pub(crate) fn aes_unwrap(kek: &[u8], wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let failed = |_| Error::Crypto("the message key could not be unwrapped".into());
    let key = match kek.len() {
        16 => aes_kw::KekAes128::try_from(kek)
            .map_err(|_| invalid("bad key length"))?
            .unwrap_vec(wrapped)
            .map_err(failed)?,
        24 => aes_kw::KekAes192::try_from(kek)
            .map_err(|_| invalid("bad key length"))?
            .unwrap_vec(wrapped)
            .map_err(failed)?,
        32 => aes_kw::KekAes256::try_from(kek)
            .map_err(|_| invalid("bad key length"))?
            .unwrap_vec(wrapped)
            .map_err(failed)?,
        _ => return Err(invalid("bad key length")),
    };
    Ok(Zeroizing::new(key))
}

pub(crate) fn aes256_wrap(kek: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    aes_kw::KekAes256::try_from(kek)
        .map_err(|_| invalid("bad key length"))?
        .wrap_vec(key)
        .map_err(|_| Error::Crypto("the message key could not be wrapped".into()))
}

// ---------------------------------------------------------------------------
// Content encryption

/// How the content of an enveloped message is encrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContentAlg {
    AesCbc {
        key_len: usize,
        iv: Vec<u8>,
    },
    DesEde3Cbc {
        iv: Vec<u8>,
    },
    AesGcm {
        key_len: usize,
        nonce: Vec<u8>,
        tag_len: usize,
    },
}

impl ContentAlg {
    pub fn from_id(alg: &AlgorithmId<'_>) -> Result<ContentAlg> {
        let iv = || -> Result<Vec<u8>> {
            let p = alg
                .params
                .ok_or_else(|| invalid("a cipher without its IV"))?;
            if p.tag != ber::OCTET_STRING && p.tag != 0x24 {
                return Err(invalid("a cipher without its IV"));
            }
            Ok(p.octets()?.into_owned())
        };
        let gcm = |key_len| -> Result<ContentAlg> {
            // GCMParameters: the nonce, and the tag length (12 by default).
            let p = alg
                .params
                .ok_or_else(|| invalid("GCM without parameters"))?;
            let mut r = p.children();
            let nonce = r.any()?.octets()?.into_owned();
            let tag_len = match r.optional(ber::INTEGER)? {
                Some(t) => t.small_uint()? as usize,
                None => 12,
            };
            Ok(ContentAlg::AesGcm {
                key_len,
                nonce,
                tag_len,
            })
        };
        let alg = match alg.oid {
            oid::AES128_CBC => ContentAlg::AesCbc {
                key_len: 16,
                iv: iv()?,
            },
            oid::AES192_CBC => ContentAlg::AesCbc {
                key_len: 24,
                iv: iv()?,
            },
            oid::AES256_CBC => ContentAlg::AesCbc {
                key_len: 32,
                iv: iv()?,
            },
            oid::DES_EDE3_CBC => ContentAlg::DesEde3Cbc { iv: iv()? },
            oid::AES128_GCM => gcm(16)?,
            oid::AES192_GCM => gcm(24)?,
            oid::AES256_GCM => gcm(32)?,
            oid::RC2_CBC => return Err(Error::Unsupported("RC2 encryption".into())),
            other => return Err(Error::Unsupported(format!("the cipher {other}"))),
        };
        match &alg {
            ContentAlg::AesCbc { iv, .. } if iv.len() != 16 => Err(invalid("bad AES IV")),
            ContentAlg::DesEde3Cbc { iv } if iv.len() != 8 => Err(invalid("bad 3DES IV")),
            ContentAlg::AesGcm { nonce, tag_len, .. }
                if nonce.len() != 12 || !(12..=16).contains(tag_len) =>
            {
                Err(Error::Unsupported("these AES-GCM parameters".into()))
            }
            _ => Ok(alg),
        }
    }

    pub fn key_len(&self) -> usize {
        match self {
            ContentAlg::AesCbc { key_len, .. } | ContentAlg::AesGcm { key_len, .. } => *key_len,
            ContentAlg::DesEde3Cbc { .. } => 24,
        }
    }

    pub fn is_aead(&self) -> bool {
        matches!(self, ContentAlg::AesGcm { .. })
    }

    /// "AES-256-CBC" and the like.
    pub fn name(&self) -> String {
        match self {
            ContentAlg::AesCbc { key_len, .. } => format!("AES-{}-CBC", key_len * 8),
            ContentAlg::DesEde3Cbc { .. } => "3DES".into(),
            ContentAlg::AesGcm { key_len, .. } => format!("AES-{}-GCM", key_len * 8),
        }
    }

    /// Decrypts the content (`aad` and `tag` for AES-GCM).
    pub fn decrypt(&self, key: &[u8], data: &[u8], aad: &[u8], tag: &[u8]) -> Result<Vec<u8>> {
        if key.len() != self.key_len() {
            return Err(Error::Crypto("the message key has the wrong length".into()));
        }
        let failed = || Error::Crypto("the message could not be decrypted".into());
        match self {
            ContentAlg::AesCbc { key_len, iv } => match key_len {
                16 => cbc_decrypt::<aes::Aes128>(key, iv, data),
                24 => cbc_decrypt::<aes::Aes192>(key, iv, data),
                _ => cbc_decrypt::<aes::Aes256>(key, iv, data),
            }
            .ok_or_else(failed),
            ContentAlg::DesEde3Cbc { iv } => {
                cbc_decrypt::<des::TdesEde3>(key, iv, data).ok_or_else(failed)
            }
            ContentAlg::AesGcm {
                key_len,
                nonce,
                tag_len,
            } => {
                if tag.len() != *tag_len {
                    return Err(failed());
                }
                gcm_decrypt(*key_len, key, nonce, data, aad, tag).ok_or_else(failed)
            }
        }
    }
}

pub(crate) fn cbc_decrypt<C>(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>>
where
    C: cbc::cipher::BlockDecryptMut + cbc::cipher::BlockCipher + cbc::cipher::KeyInit,
{
    cbc::Decryptor::<C>::new_from_slices(key, iv)
        .ok()?
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .ok()
}

/// AES-256-CBC, for what is sent.
pub(crate) fn aes256_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    Ok(cbc::Encryptor::<aes::Aes256>::new_from_slices(key, iv)
        .map_err(|_| invalid("bad key length"))?
        .encrypt_padded_vec_mut::<Pkcs7>(data))
}

fn gcm_decrypt(
    key_len: usize,
    key: &[u8],
    nonce: &[u8],
    data: &[u8],
    aad: &[u8],
    tag: &[u8],
) -> Option<Vec<u8>> {
    use aes_gcm::aead::consts::{U12, U13, U14, U15, U16};
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{AesGcm, Nonce};
    let mut msg = Vec::with_capacity(data.len() + tag.len());
    msg.extend_from_slice(data);
    msg.extend_from_slice(tag);
    let payload = Payload { msg: &msg, aad };
    let nonce = Nonce::<U12>::from_slice(nonce);
    macro_rules! open {
        ($aes:ty, $tag:ty) => {
            AesGcm::<$aes, U12, $tag>::new_from_slice(key)
                .ok()?
                .decrypt(nonce, payload)
                .ok()
        };
    }
    macro_rules! by_tag {
        ($aes:ty) => {
            match tag.len() {
                12 => open!($aes, U12),
                13 => open!($aes, U13),
                14 => open!($aes, U14),
                15 => open!($aes, U15),
                _ => open!($aes, U16),
            }
        };
    }
    match key_len {
        16 => by_tag!(aes::Aes128),
        24 => by_tag!(aes::Aes192),
        _ => by_tag!(aes::Aes256),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x963_kdf_matches_a_known_answer() {
        // ANSI X9.63 KDF with SHA-256 and no shared info beyond the
        // structure: the counter starts at 1.
        let kek = ecdh_kek(Hash::Sha256, &[0x11; 32], &[0x30, 0x00], None, 16);
        let mut input = vec![0x11; 32];
        input.extend_from_slice(&[0, 0, 0, 1]);
        input.extend_from_slice(&[
            0x30, 0x0a, 0x30, 0x00, 0xa2, 0x06, 0x04, 0x04, 0x00, 0x00, 0x00, 0x80,
        ]);
        assert_eq!(&kek[..], &Hash::Sha256.digest(&input)[..16]);
    }

    #[test]
    fn content_round_trip() {
        let key = [7u8; 32];
        let iv = [9u8; 16];
        let sealed = aes256_cbc_encrypt(&key, &iv, b"hello").unwrap();
        let alg = ContentAlg::AesCbc {
            key_len: 32,
            iv: iv.to_vec(),
        };
        assert_eq!(alg.decrypt(&key, &sealed, &[], &[]).unwrap(), b"hello");
        assert!(alg.decrypt(&[8u8; 32], &sealed, &[], &[]).is_err());
    }
}
