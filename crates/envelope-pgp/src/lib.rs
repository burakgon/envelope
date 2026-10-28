//! OpenPGP for Envelope, built on rPGP (the pure Rust `pgp` crate):
//!
//! - [`cert`]: certificates (public keys) and secret keys — generation,
//!   import, export, validity, and which subkeys to encrypt or sign with.
//! - [`ops`]: encrypting, decrypting, signing and verifying.
//! - [`pgpmime`]: PGP/MIME (RFC 3156) and inline PGP in e-mail, including
//!   protected headers.
//! - [`autocrypt`]: `Autocrypt` and `Autocrypt-Gossip` headers.
//! - [`wkd`]: Web Key Directory addresses.
//!
//! Nothing here touches the network, the disk or the keyring; the engine
//! decides where keys live and when to look them up.

pub mod autocrypt;
pub mod cert;
pub mod ops;
pub mod pgpmime;
pub mod wkd;

pub use cert::{Cert, CertInfo};
pub use ops::{SigResult, SigStatus};

/// Re-exported so the engine can name rPGP types without its own dependency.
pub use pgp;

/// Errors of OpenPGP operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The input is not what was expected (not a key, not a message, …).
    #[error("{0}")]
    Invalid(String),
    /// A passphrase is needed to use the secret key with this fingerprint.
    #[error("the key {0} is protected by a passphrase")]
    Locked(String),
    /// The passphrase does not unlock the key.
    #[error("wrong passphrase")]
    BadPassphrase,
    /// None of the available secret keys can decrypt the message.
    #[error("this message was not encrypted to any of your keys")]
    NoKey,
    /// The OpenPGP library rejected the operation.
    #[error("OpenPGP error: {0}")]
    Pgp(String),
}

impl From<pgp::errors::Error> for Error {
    fn from(e: pgp::errors::Error) -> Self {
        Error::Pgp(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Current time in unix seconds.
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
