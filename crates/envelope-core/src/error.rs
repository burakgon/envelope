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

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
