use std::io;

/// Errors returned by the IMAP client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Io(#[from] io::Error),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("connection timed out")]
    Timeout,
    #[error("connection closed by server")]
    Closed,
    #[error("server said goodbye: {0}")]
    Bye(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    /// The server answered a command with `NO`.
    #[error("{command} failed: {text}")]
    No { command: String, text: String },
    /// The server answered a command with `BAD`.
    #[error("{command} rejected by server: {text}")]
    Bad { command: String, text: String },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("server does not support {0}")]
    Unsupported(&'static str),
}

impl Error {
    /// Whether retrying on a fresh connection may succeed.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Error::Io(_) | Error::Timeout | Error::Closed | Error::Bye(_) | Error::Tls(_)
        )
    }

    /// Whether the error indicates rejected credentials.
    pub fn is_auth(&self) -> bool {
        matches!(self, Error::AuthFailed(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
