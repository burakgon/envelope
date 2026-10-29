//! PKCS #12 (RFC 7292): the `.p12` / `.pfx` files a certificate and its
//! private key are exported in by Windows, macOS, browsers and OpenSSL.
//!
//! Both kinds are read: PBES2 with PBKDF2 and AES (OpenSSL 3, recent
//! Windows) and the older PKCS #12 encryption with SHA-1 and 3DES or RC2
//! (older Windows and OpenSSL). The password is checked against the file's
//! MAC first.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};
use zeroize::Zeroizing;

use crate::algs::{Hash, cbc_decrypt};
use crate::ber::{self, AlgorithmId, Tlv};
use crate::cert::Certificate;
use crate::key::PrivateKey;
use crate::oid;
use crate::{Error, Result, invalid};

/// What a PKCS #12 file holds.
#[derive(Debug, Clone, Default)]
pub struct Bundle {
    /// Private keys with their certificates.
    pub identities: Vec<(PrivateKey, Certificate)>,
    /// The other certificates (usually the issuers' chain).
    pub certificates: Vec<Certificate>,
}

/// Whether data looks like a PKCS #12 file (a version 3 PFX).
pub fn is_pkcs12(data: &[u8]) -> bool {
    Tlv::parse(data)
        .ok()
        .filter(|t| t.tag == ber::SEQUENCE)
        .and_then(|t| t.children().read(ber::INTEGER).ok()?.small_uint().ok())
        == Some(3)
}

/// Reads a PKCS #12 file with its password.
pub fn parse(data: &[u8], password: &str) -> Result<Bundle> {
    let pfx = Tlv::parse(data)?;
    let mut r = pfx.children();
    if r.read(ber::INTEGER)?.small_uint()? != 3 {
        return Err(invalid("not a PKCS #12 file"));
    }
    let auth_safe = r.read(ber::SEQUENCE)?;
    let mut a = auth_safe.children();
    if a.read(ber::OID)?.oid()? != oid::DATA {
        return Err(Error::Unsupported(
            "PKCS #12 files protected by a public key".into(),
        ));
    }
    let contents = a
        .read(ber::context_constructed(0))?
        .children()
        .any()?
        .octets()?;
    let passwords = Passwords::new(password);
    if let Some(mac) = r.optional(ber::SEQUENCE)? {
        check_mac(mac, &contents, &passwords)?;
    }
    let mut bags = Bags::default();
    let mut safes = Tlv::parse(&contents)?.children();
    while let Some(info) = safes.next()? {
        let mut c = info.children();
        let kind = c.read(ber::OID)?.oid()?;
        let content = c.read(ber::context_constructed(0))?.children().any()?;
        match kind {
            oid::DATA => bags.read(&content.octets()?, &passwords, 0)?,
            oid::ENCRYPTED_DATA => {
                // EncryptedData: the version, then EncryptedContentInfo.
                let mut e = content.children();
                e.read(ber::INTEGER)?;
                let mut info = e.read(ber::SEQUENCE)?.children();
                info.read(ber::OID)?;
                let alg = AlgorithmId::read(&mut info)?;
                let encrypted = info.any()?.octets()?;
                let plain = decrypt(&alg, &encrypted, &passwords)?;
                bags.read(&plain, &passwords, 0)?;
            }
            _ => {
                return Err(Error::Unsupported(
                    "PKCS #12 contents encrypted to a public key".into(),
                ));
            }
        }
    }
    bags.into_bundle()
}

/// The forms a password takes: UTF-8 for PBES2 (RFC 9579), BMPString with
/// a terminating zero for PKCS #12's own key derivation. An empty password
/// may also have been left out entirely.
struct Passwords {
    utf8: Zeroizing<Vec<u8>>,
    bmp: Vec<Zeroizing<Vec<u8>>>,
}

impl Passwords {
    fn new(password: &str) -> Passwords {
        let mut bmp: Vec<u8> = password.encode_utf16().flat_map(u16::to_be_bytes).collect();
        bmp.extend_from_slice(&[0, 0]);
        let mut forms = vec![Zeroizing::new(bmp)];
        if password.is_empty() {
            forms.push(Zeroizing::new(Vec::new()));
        }
        Passwords {
            utf8: Zeroizing::new(password.as_bytes().to_vec()),
            bmp: forms,
        }
    }
}

/// The PKCS #12 key derivation (RFC 7292, appendix B).
fn derive(
    hash: Hash,
    password: &[u8],
    salt: &[u8],
    id: u8,
    rounds: u64,
    len: usize,
) -> Zeroizing<Vec<u8>> {
    use pkcs12::kdf::{Pkcs12KeyType, derive_key};
    let kind = match id {
        1 => Pkcs12KeyType::EncryptionKey,
        2 => Pkcs12KeyType::Iv,
        _ => Pkcs12KeyType::Mac,
    };
    let rounds = rounds.min(i32::MAX as u64) as i32;
    Zeroizing::new(match hash {
        Hash::Sha1 => derive_key::<Sha1>(password, salt, kind, rounds, len),
        Hash::Sha224 => derive_key::<Sha224>(password, salt, kind, rounds, len),
        Hash::Sha256 => derive_key::<Sha256>(password, salt, kind, rounds, len),
        Hash::Sha384 => derive_key::<Sha384>(password, salt, kind, rounds, len),
        Hash::Sha512 => derive_key::<Sha512>(password, salt, kind, rounds, len),
    })
}

fn hmac(hash: Hash, key: &[u8], data: &[u8]) -> Vec<u8> {
    macro_rules! mac {
        ($d:ty) => {{
            let mut m = <Hmac<$d> as Mac>::new_from_slice(key).expect("any key length");
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }};
    }
    match hash {
        Hash::Sha1 => mac!(Sha1),
        Hash::Sha224 => mac!(Sha224),
        Hash::Sha256 => mac!(Sha256),
        Hash::Sha384 => mac!(Sha384),
        Hash::Sha512 => mac!(Sha512),
    }
}

/// Checks the password against the MAC over the contents.
fn check_mac(mac: Tlv<'_>, contents: &[u8], passwords: &Passwords) -> Result<()> {
    let mut r = mac.children();
    let mut info = r.read(ber::SEQUENCE)?.children();
    let alg = AlgorithmId::read(&mut info)?;
    let expected = info.any()?.octets()?;
    let salt = r.any()?.octets()?;
    let rounds = match r.optional(ber::INTEGER)? {
        Some(t) => t.small_uint()?,
        None => 1,
    };
    let hash = Hash::from_oid(alg.oid)
        .ok_or_else(|| Error::Unsupported(format!("the PKCS #12 MAC {}", alg.oid)))?;
    let len = hash.digest(b"").len();
    for password in &passwords.bmp {
        let key = derive(hash, password, &salt, 3, rounds, len);
        if constant_time_eq(&hmac(hash, &key, contents), &expected) {
            return Ok(());
        }
    }
    Err(Error::BadPassword)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Decrypts what a password-based encryption scheme encrypted.
fn decrypt(
    alg: &AlgorithmId<'_>,
    data: &[u8],
    passwords: &Passwords,
) -> Result<Zeroizing<Vec<u8>>> {
    let wrong = || Error::BadPassword;
    match alg.oid {
        oid::PBES2 => {
            let params = alg
                .params
                .ok_or_else(|| invalid("PBES2 without parameters"))?;
            let mut r = params.children();
            let kdf = AlgorithmId::read(&mut r)?;
            let scheme = AlgorithmId::read(&mut r)?;
            if kdf.oid != oid::PBKDF2 {
                return Err(Error::Unsupported(format!(
                    "the key derivation {}",
                    kdf.oid
                )));
            }
            let mut k = kdf
                .params
                .ok_or_else(|| invalid("PBKDF2 without parameters"))?
                .children();
            let salt = k.any()?.octets()?;
            let rounds = k.read(ber::INTEGER)?.small_uint()?;
            let _key_len = k.optional(ber::INTEGER)?;
            let prf = match k.optional(ber::SEQUENCE)? {
                Some(t) => AlgorithmId::from_tlv(t)?.oid,
                None => oid::HMAC_SHA1,
            };
            if rounds == 0 || rounds > 10_000_000 {
                return Err(invalid("unreasonable PBKDF2 iteration count"));
            }
            let (key_len, iv) = match scheme.oid {
                oid::AES128_CBC => (16, &scheme),
                oid::AES192_CBC => (24, &scheme),
                oid::AES256_CBC => (32, &scheme),
                oid::DES_EDE3_CBC => (24, &scheme),
                other => return Err(Error::Unsupported(format!("the cipher {other}"))),
            };
            let iv = iv
                .params
                .ok_or_else(|| invalid("a cipher without its IV"))?
                .octets()?;
            // UTF-8 as RFC 9579 says; some Windows versions used BMPString.
            let mut candidates: Vec<&[u8]> = vec![&passwords.utf8];
            candidates.extend(passwords.bmp.iter().map(|p| p.as_slice()));
            for password in candidates {
                let mut key = Zeroizing::new(vec![0u8; key_len]);
                let rounds = rounds as u32;
                match prf {
                    oid::HMAC_SHA1 => {
                        pbkdf2::pbkdf2_hmac::<Sha1>(password, &salt, rounds, &mut key)
                    }
                    oid::HMAC_SHA224 => {
                        pbkdf2::pbkdf2_hmac::<Sha224>(password, &salt, rounds, &mut key)
                    }
                    oid::HMAC_SHA256 => {
                        pbkdf2::pbkdf2_hmac::<Sha256>(password, &salt, rounds, &mut key)
                    }
                    oid::HMAC_SHA384 => {
                        pbkdf2::pbkdf2_hmac::<Sha384>(password, &salt, rounds, &mut key)
                    }
                    oid::HMAC_SHA512 => {
                        pbkdf2::pbkdf2_hmac::<Sha512>(password, &salt, rounds, &mut key)
                    }
                    other => return Err(Error::Unsupported(format!("the PRF {other}"))),
                }
                let plain = match (scheme.oid, key_len) {
                    (oid::DES_EDE3_CBC, _) => cbc_decrypt::<des::TdesEde3>(&key, &iv, data),
                    (_, 16) => cbc_decrypt::<aes::Aes128>(&key, &iv, data),
                    (_, 24) => cbc_decrypt::<aes::Aes192>(&key, &iv, data),
                    _ => cbc_decrypt::<aes::Aes256>(&key, &iv, data),
                };
                if let Some(plain) = plain {
                    return Ok(Zeroizing::new(plain));
                }
            }
            Err(wrong())
        }
        oid::PBE_SHA1_3DES | oid::PBE_SHA1_2DES | oid::PBE_SHA1_RC2_128 | oid::PBE_SHA1_RC2_40 => {
            let params = alg
                .params
                .ok_or_else(|| invalid("PBE without parameters"))?;
            let mut r = params.children();
            let salt = r.any()?.octets()?;
            let rounds = r.read(ber::INTEGER)?.small_uint()?;
            if rounds == 0 || rounds > 10_000_000 {
                return Err(invalid("unreasonable PKCS #12 iteration count"));
            }
            for password in &passwords.bmp {
                let iv = derive(Hash::Sha1, password, &salt, 2, rounds, 8);
                let plain = match alg.oid {
                    oid::PBE_SHA1_3DES => {
                        let key = derive(Hash::Sha1, password, &salt, 1, rounds, 24);
                        cbc_decrypt::<des::TdesEde3>(&key, &iv, data)
                    }
                    oid::PBE_SHA1_2DES => {
                        // Two-key 3DES: K1 K2 K1.
                        let key = derive(Hash::Sha1, password, &salt, 1, rounds, 16);
                        let mut full = Zeroizing::new(key.to_vec());
                        full.extend_from_slice(&key[..8]);
                        cbc_decrypt::<des::TdesEde3>(&full, &iv, data)
                    }
                    _ => {
                        let bits = if alg.oid == oid::PBE_SHA1_RC2_40 {
                            40
                        } else {
                            128
                        };
                        let key = derive(Hash::Sha1, password, &salt, 1, rounds, bits / 8);
                        rc2_decrypt(&key, bits, &iv, data)
                    }
                };
                if let Some(plain) = plain {
                    return Ok(Zeroizing::new(plain));
                }
            }
            Err(wrong())
        }
        other => Err(Error::Unsupported(format!(
            "the PKCS #12 encryption {other}"
        ))),
    }
}

fn rc2_decrypt(key: &[u8], bits: usize, iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use cbc::cipher::block_padding::Pkcs7;
    use cbc::cipher::{BlockDecryptMut, InnerIvInit};
    let cipher = rc2::Rc2::new_with_eff_key_len(key, bits);
    cbc::Decryptor::<rc2::Rc2>::inner_iv_slice_init(cipher, iv)
        .ok()?
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .ok()
}

/// The bags found so far.
#[derive(Default)]
struct Bags {
    keys: Vec<(PrivateKey, Option<Vec<u8>>)>,
    certs: Vec<(Certificate, Option<Vec<u8>>)>,
}

impl Bags {
    fn read(&mut self, safe_contents: &[u8], passwords: &Passwords, depth: usize) -> Result<()> {
        if depth > 4 {
            return Err(invalid("nested too deeply"));
        }
        let mut bags = Tlv::parse(safe_contents)?.children();
        while let Some(bag) = bags.next()? {
            let mut b = bag.children();
            let kind = b.read(ber::OID)?.oid()?;
            let value = b.read(ber::context_constructed(0))?.children().any()?;
            let local_id = match b.optional(ber::SET)? {
                Some(attrs) => local_key_id(attrs)?,
                None => None,
            };
            match kind {
                oid::KEY_BAG => self
                    .keys
                    .push((PrivateKey::from_pkcs8_der(&value.to_der()?)?, local_id)),
                oid::SHROUDED_KEY_BAG => {
                    let mut e = value.children();
                    let alg = AlgorithmId::read(&mut e)?;
                    let encrypted = e.any()?.octets()?;
                    let der = decrypt(&alg, &encrypted, passwords)?;
                    self.keys
                        .push((PrivateKey::from_pkcs8_der(&der)?, local_id));
                }
                oid::CERT_BAG => {
                    let mut c = value.children();
                    if c.read(ber::OID)?.oid()? != oid::X509_CERTIFICATE {
                        continue;
                    }
                    let der = c
                        .read(ber::context_constructed(0))?
                        .children()
                        .any()?
                        .octets()?;
                    self.certs.push((Certificate::from_der(&der)?, local_id));
                }
                oid::SAFE_CONTENTS_BAG => self.read(value.raw, passwords, depth + 1)?,
                // CRLs and secrets are of no use here.
                _ => {}
            }
        }
        Ok(())
    }

    /// Pairs keys with their certificates: by local key id, else by key.
    fn into_bundle(self) -> Result<Bundle> {
        let mut certs = self.certs;
        let mut identities = Vec::new();
        for (key, id) in self.keys {
            let by_id = id.as_ref().and_then(|id| {
                certs
                    .iter()
                    .position(|(c, cid)| cid.as_ref() == Some(id) && key.matches(c))
            });
            let found = by_id.or_else(|| certs.iter().position(|(c, _)| key.matches(c)));
            match found {
                Some(i) => {
                    let (cert, _) = certs.remove(i);
                    identities.push((key, cert));
                }
                None => {
                    return Err(invalid(
                        "the file has a private key without its certificate",
                    ));
                }
            }
        }
        Ok(Bundle {
            identities,
            certificates: certs.into_iter().map(|(c, _)| c).collect(),
        })
    }
}

fn local_key_id(attrs: Tlv<'_>) -> Result<Option<Vec<u8>>> {
    let mut r = attrs.children();
    while let Some(attr) = r.next()? {
        let mut a = attr.children();
        if a.read(ber::OID)?.oid()? == oid::LOCAL_KEY_ID
            && let Some(value) = a.read(ber::SET)?.children().next()?
        {
            return Ok(Some(value.octets()?.into_owned()));
        }
    }
    Ok(None)
}
