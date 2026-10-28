//! Access-token plumbing.

use std::fmt;

use async_trait::async_trait;

/// Supplies OAuth 2.0 access tokens for Microsoft Graph.
///
/// Implementations usually cache a token and refresh it shortly before it
/// expires. When Graph rejects a token with `401 Unauthorized`,
/// [`GraphClient`](crate::GraphClient) calls [`invalidate`](Self::invalidate)
/// and retries the request once with a freshly obtained token.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Returns a bearer access token (without the `Bearer ` prefix).
    ///
    /// The error text is surfaced to callers as
    /// [`GraphError::Auth`](crate::GraphError::Auth).
    async fn access_token(&self) -> Result<String, String>;

    /// Discards the cached token so that the next
    /// [`access_token`](Self::access_token) call obtains a new one.
    async fn invalidate(&self);
}

/// A [`TokenSource`] that always hands out the same token.
///
/// Useful for tests and short-lived tools; it cannot refresh, so
/// [`invalidate`](TokenSource::invalidate) is a no-op.
#[derive(Clone)]
pub struct StaticToken {
    token: String,
}

impl StaticToken {
    /// Wraps a fixed access token.
    pub fn new(token: impl Into<String>) -> Self {
        StaticToken {
            token: token.into(),
        }
    }
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticToken")
            .field("token", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl TokenSource for StaticToken {
    async fn access_token(&self) -> Result<String, String> {
        Ok(self.token.clone())
    }

    async fn invalidate(&self) {}
}
