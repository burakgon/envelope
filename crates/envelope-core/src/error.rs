/// Errors produced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Imap(#[from] envelope_imap::Error),
    #[error("sending failed: {0}")]
    Smtp(String),
    #[error("{0}")]
    Auth(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// An OpenPGP secret key (fingerprint) needs its passphrase.
    #[error("Your OpenPGP key is protected by a passphrase. Unlock it to continue.")]
    Locked(String),
    /// The password of a certificate file (PKCS #12) is wrong.
    #[error("Wrong password")]
    BadPassword,
    #[error("{0}")]
    Other(String),
    #[error("invalid data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Whether the error is caused by rejected credentials.
    pub fn is_auth(&self) -> bool {
        match self {
            Error::Auth(_) => true,
            Error::Imap(e) => e.is_auth(),
            _ => false,
        }
    }

    /// Whether the operation may succeed if retried later (network trouble).
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Imap(e) => e.is_transient(),
            Error::Network(_) => true,
            _ => false,
        }
    }
}

impl From<envelope_pgp::Error> for Error {
    fn from(e: envelope_pgp::Error) -> Self {
        use envelope_pgp::Error as P;
        match e {
            P::Locked(fingerprint) => Error::Locked(fingerprint),
            P::BadPassphrase => Error::Invalid("Wrong passphrase".into()),
            P::NoKey | P::Invalid(_) => Error::Invalid(e.to_string()),
            P::Pgp(m) => Error::Other(format!("OpenPGP: {m}")),
        }
    }
}

impl From<envelope_smime::Error> for Error {
    fn from(e: envelope_smime::Error) -> Self {
        use envelope_smime::Error as S;
        match e {
            S::BadPassword => Error::BadPassword,
            S::Invalid(_) | S::Unsupported(_) | S::NoKey => {
                Error::Invalid(sentence(&e.to_string()))
            }
            S::Crypto(m) => Error::Other(format!("S/MIME: {m}")),
        }
    }
}

/// A message starting with a capital letter.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
