//! Access-token plumbing.

use std::fmt;

use async_trait::async_trait;

/// Supplies OAuth 2.0 access tokens for Google API requests.
///
/// Implementations usually wrap the account's refresh token and cache the
/// current access token until it expires. The same source can be shared with
/// the IMAP/SMTP layer (XOAUTH2) as long as the token carries every scope in
/// [`scopes`](crate::scopes).
///
/// When Google rejects a token with HTTP `401`, the client calls
/// [`invalidate`](TokenSource::invalidate) and retries the request once with
/// a freshly obtained token.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Returns a currently valid access token (without the `Bearer ` prefix).
    ///
    /// The error string is surfaced as [`Error::Auth`](crate::Error::Auth).
    async fn access_token(&self) -> Result<String, String>;

    /// Forgets the cached access token so that the next
    /// [`access_token`](TokenSource::access_token) call obtains a new one.
    async fn invalidate(&self);
}

/// A fixed access token. Useful for tests and short-lived tools; it cannot
/// be refreshed, so [`invalidate`](TokenSource::invalidate) is a no-op.
#[derive(Clone)]
pub struct StaticToken(String);

impl StaticToken {
    /// Wraps an access token.
    pub fn new(token: impl Into<String>) -> Self {
        StaticToken(token.into())
    }
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticToken(<redacted>)")
    }
}

#[async_trait]
impl TokenSource for StaticToken {
    async fn access_token(&self) -> Result<String, String> {
        Ok(self.0.clone())
    }

    async fn invalidate(&self) {}
}
