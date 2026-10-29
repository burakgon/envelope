//! Object identifiers used by S/MIME, CMS, PKCS #12 and X.509.

use const_oid::ObjectIdentifier as Oid;

const fn oid(s: &str) -> Oid {
    Oid::new_unwrap(s)
}

// Content types (RFC 5652, RFC 5083).
pub const DATA: Oid = oid("1.2.840.113549.1.7.1");
pub const SIGNED_DATA: Oid = oid("1.2.840.113549.1.7.2");
pub const ENVELOPED_DATA: Oid = oid("1.2.840.113549.1.7.3");
pub const ENCRYPTED_DATA: Oid = oid("1.2.840.113549.1.7.6");
pub const AUTH_ENVELOPED_DATA: Oid = oid("1.2.840.113549.1.9.16.1.23");

// Signed attributes.
pub const CONTENT_TYPE: Oid = oid("1.2.840.113549.1.9.3");
pub const MESSAGE_DIGEST: Oid = oid("1.2.840.113549.1.9.4");
pub const SIGNING_TIME: Oid = oid("1.2.840.113549.1.9.5");
pub const SMIME_CAPABILITIES: Oid = oid("1.2.840.113549.1.9.15");
pub const ENCRYPTION_KEY_PREFERENCE: Oid = oid("1.2.840.113549.1.9.16.2.11");
pub const MS_ENCRYPTION_KEY_PREFERENCE: Oid = oid("1.3.6.1.4.1.311.16.4");

// PKCS #12.
pub const LOCAL_KEY_ID: Oid = oid("1.2.840.113549.1.9.21");
pub const KEY_BAG: Oid = oid("1.2.840.113549.1.12.10.1.1");
pub const SHROUDED_KEY_BAG: Oid = oid("1.2.840.113549.1.12.10.1.2");
pub const CERT_BAG: Oid = oid("1.2.840.113549.1.12.10.1.3");
pub const SAFE_CONTENTS_BAG: Oid = oid("1.2.840.113549.1.12.10.1.6");
pub const X509_CERTIFICATE: Oid = oid("1.2.840.113549.1.9.22.1");
pub const PBE_SHA1_3DES: Oid = oid("1.2.840.113549.1.12.1.3");
pub const PBE_SHA1_2DES: Oid = oid("1.2.840.113549.1.12.1.4");
pub const PBE_SHA1_RC2_128: Oid = oid("1.2.840.113549.1.12.1.5");
pub const PBE_SHA1_RC2_40: Oid = oid("1.2.840.113549.1.12.1.6");
pub const PBES2: Oid = oid("1.2.840.113549.1.5.13");
pub const PBKDF2: Oid = oid("1.2.840.113549.1.5.12");
pub const HMAC_SHA1: Oid = oid("1.2.840.113549.2.7");
pub const HMAC_SHA224: Oid = oid("1.2.840.113549.2.8");
pub const HMAC_SHA256: Oid = oid("1.2.840.113549.2.9");
pub const HMAC_SHA384: Oid = oid("1.2.840.113549.2.10");
pub const HMAC_SHA512: Oid = oid("1.2.840.113549.2.11");

// Digests.
pub const SHA1: Oid = oid("1.3.14.3.2.26");
pub const SHA224: Oid = oid("2.16.840.1.101.3.4.2.4");
pub const SHA256: Oid = oid("2.16.840.1.101.3.4.2.1");
pub const SHA384: Oid = oid("2.16.840.1.101.3.4.2.2");
pub const SHA512: Oid = oid("2.16.840.1.101.3.4.2.3");

// Keys and signatures.
pub const RSA_ENCRYPTION: Oid = oid("1.2.840.113549.1.1.1");
pub const RSAES_OAEP: Oid = oid("1.2.840.113549.1.1.7");
pub const MGF1: Oid = oid("1.2.840.113549.1.1.8");
pub const RSASSA_PSS: Oid = oid("1.2.840.113549.1.1.10");
pub const SHA1_WITH_RSA: Oid = oid("1.2.840.113549.1.1.5");
pub const SHA224_WITH_RSA: Oid = oid("1.2.840.113549.1.1.14");
pub const SHA256_WITH_RSA: Oid = oid("1.2.840.113549.1.1.11");
pub const SHA384_WITH_RSA: Oid = oid("1.2.840.113549.1.1.12");
pub const SHA512_WITH_RSA: Oid = oid("1.2.840.113549.1.1.13");
pub const EC_PUBLIC_KEY: Oid = oid("1.2.840.10045.2.1");
pub const SECP256R1: Oid = oid("1.2.840.10045.3.1.7");
pub const SECP384R1: Oid = oid("1.3.132.0.34");
pub const SECP521R1: Oid = oid("1.3.132.0.35");
pub const ECDSA_SHA1: Oid = oid("1.2.840.10045.4.1");
pub const ECDSA_SHA224: Oid = oid("1.2.840.10045.4.3.1");
pub const ECDSA_SHA256: Oid = oid("1.2.840.10045.4.3.2");
pub const ECDSA_SHA384: Oid = oid("1.2.840.10045.4.3.3");
pub const ECDSA_SHA512: Oid = oid("1.2.840.10045.4.3.4");
pub const ED25519: Oid = oid("1.3.101.112");

// Content encryption and key wrapping.
pub const AES128_CBC: Oid = oid("2.16.840.1.101.3.4.1.2");
pub const AES192_CBC: Oid = oid("2.16.840.1.101.3.4.1.22");
pub const AES256_CBC: Oid = oid("2.16.840.1.101.3.4.1.42");
pub const AES128_GCM: Oid = oid("2.16.840.1.101.3.4.1.6");
pub const AES192_GCM: Oid = oid("2.16.840.1.101.3.4.1.26");
pub const AES256_GCM: Oid = oid("2.16.840.1.101.3.4.1.46");
pub const AES128_WRAP: Oid = oid("2.16.840.1.101.3.4.1.5");
pub const AES192_WRAP: Oid = oid("2.16.840.1.101.3.4.1.25");
pub const AES256_WRAP: Oid = oid("2.16.840.1.101.3.4.1.45");
pub const DES_EDE3_CBC: Oid = oid("1.2.840.113549.3.7");
pub const RC2_CBC: Oid = oid("1.2.840.113549.3.2");

// Ephemeral-static ECDH (RFC 5753, SEC 1).
pub const DH_STD_SHA1_KDF: Oid = oid("1.3.133.16.840.63.0.2");
pub const DH_STD_SHA224_KDF: Oid = oid("1.3.132.1.11.0");
pub const DH_STD_SHA256_KDF: Oid = oid("1.3.132.1.11.1");
pub const DH_STD_SHA384_KDF: Oid = oid("1.3.132.1.11.2");
pub const DH_STD_SHA512_KDF: Oid = oid("1.3.132.1.11.3");
pub const DH_COFACTOR_SHA1_KDF: Oid = oid("1.3.133.16.840.63.0.3");
pub const DH_COFACTOR_SHA224_KDF: Oid = oid("1.3.132.1.14.0");
pub const DH_COFACTOR_SHA256_KDF: Oid = oid("1.3.132.1.14.1");
pub const DH_COFACTOR_SHA384_KDF: Oid = oid("1.3.132.1.14.2");
pub const DH_COFACTOR_SHA512_KDF: Oid = oid("1.3.132.1.14.3");

// Certificate extensions.
pub const SUBJECT_DIRECTORY_ATTRIBUTES: Oid = oid("2.5.29.9");
pub const SUBJECT_KEY_ID: Oid = oid("2.5.29.14");
pub const KEY_USAGE: Oid = oid("2.5.29.15");
pub const SUBJECT_ALT_NAME: Oid = oid("2.5.29.17");
pub const ISSUER_ALT_NAME: Oid = oid("2.5.29.18");
pub const BASIC_CONSTRAINTS: Oid = oid("2.5.29.19");
pub const NAME_CONSTRAINTS: Oid = oid("2.5.29.30");
pub const CRL_DISTRIBUTION_POINTS: Oid = oid("2.5.29.31");
pub const CERTIFICATE_POLICIES: Oid = oid("2.5.29.32");
pub const AUTHORITY_KEY_ID: Oid = oid("2.5.29.35");
pub const EXT_KEY_USAGE: Oid = oid("2.5.29.37");
pub const AUTHORITY_INFO_ACCESS: Oid = oid("1.3.6.1.5.5.7.1.1");

// Extended key usages.
pub const EMAIL_PROTECTION: Oid = oid("1.3.6.1.5.5.7.3.4");
pub const ANY_EXTENDED_KEY_USAGE: Oid = oid("2.5.29.37.0");

// Name attributes.
pub const EMAIL_ADDRESS: Oid = oid("1.2.840.113549.1.9.1");
pub const COMMON_NAME: Oid = oid("2.5.4.3");
pub const ORGANIZATION: Oid = oid("2.5.4.10");
