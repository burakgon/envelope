//! S/MIME for Envelope (RFC 8551), built on the RustCrypto crates:
//!
//! - [`cert`]: X.509 certificates — reading them, what they say about their
//!   owner, and whether they may sign or encrypt mail.
//! - [`key`]: private keys (RSA, ECDSA P-256 and P-384).
//! - [`pkcs12`]: `.p12` / `.pfx` files, a certificate with its private key.
//! - [`chain`]: certificate chains up to trusted authorities.
//! - [`cms`]: CMS signatures and encryption (RFC 5652, 5083, 5753).
//! - [`mime`]: S/MIME in e-mail — finding it in messages and protecting
//!   outgoing ones.
//!
//! CMS and PKCS #12 data are read with a small BER reader of their own
//! ([`ber`]): mail programs and Windows write them streamed, with
//! indefinite lengths, which DER readers refuse. Certificates are DER and go
//! to `x509-cert`; what is written goes out as DER through `cms` and `der`.
//!
//! Nothing here touches the network, the disk or the keyring; the engine
//! decides where certificates and keys live and which authorities are
//! trusted.

pub(crate) mod algs;
pub(crate) mod ber;
pub mod cert;
pub mod chain;
pub mod cms;
pub mod key;
pub mod mime;
pub(crate) mod oid;
pub mod pkcs12;

pub use cert::{Certificate, Validity};
pub use chain::{Trust, TrustStore};
pub use cms::{Envelope, Integrity, SignWith, Signature, Signed, Verdict, Verifier};
pub use key::PrivateKey;
pub use mime::Kind;

/// Errors of S/MIME operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The input is not what was expected (not a certificate, not CMS, …).
    #[error("{0}")]
    Invalid(String),
    /// The password of a PKCS #12 file is wrong.
    #[error("wrong password")]
    BadPassword,
    /// Made with an algorithm Envelope does not support.
    #[error("{0} is not supported")]
    Unsupported(String),
    /// None of the given certificates can decrypt the message.
    #[error("this message was not encrypted to any of your certificates")]
    NoKey,
    /// Decryption failed although the certificate matched.
    #[error("{0}")]
    Crypto(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn invalid(what: &str) -> Error {
    Error::Invalid(what.to_string())
}

/// Current time in unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Upper-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}
