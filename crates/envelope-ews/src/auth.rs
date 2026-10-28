//! Credentials: HTTP Basic, NTLM (NTLMv2) and OAuth 2 bearer tokens.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;

/// Supplies OAuth 2 access tokens for [`Auth::Bearer`].
///
/// Implementations usually cache a token and refresh it shortly before it
/// expires. When Exchange rejects a token with `401 Unauthorized`, the
/// client calls [`invalidate`](Self::invalidate) and retries the request
/// once with a freshly obtained token.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Returns a bearer access token (without the `Bearer ` prefix).
    ///
    /// The error text is surfaced to callers as
    /// [`EwsError::TokenSource`](crate::EwsError::TokenSource).
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

/// How the client authenticates against Exchange.
///
/// Credentials are only sent over HTTPS (or to loopback addresses, for
/// tests); see [`EwsClientBuilder::allow_insecure_http`](crate::EwsClientBuilder::allow_insecure_http).
#[derive(Clone)]
pub enum Auth {
    /// HTTP Basic, sent with every request. Exchange Online no longer
    /// accepts it; on-premises servers usually do when the EWS virtual
    /// directory has Basic authentication enabled.
    Basic {
        /// User name: `DOMAIN\user`, a UPN (`user@corp.example.com`) or
        /// the primary SMTP address, depending on the server.
        username: String,
        /// The password.
        password: String,
    },
    /// NTLMv2 (Windows Integrated Authentication), the default of
    /// on-premises EWS virtual directories.
    ///
    /// The handshake authenticates a connection, so the client keeps a
    /// small pool of dedicated HTTP/1.1 connections for NTLM. Channel
    /// binding tokens (Extended Protection for Authentication) are sent
    /// for HTTPS connections.
    Ntlm {
        /// User name: `DOMAIN\user`, a UPN (`user@corp.example.com`) or a
        /// bare account name combined with `domain`.
        username: String,
        /// The password.
        password: String,
        /// NetBIOS or DNS domain; taken from `username` when it has the
        /// `DOMAIN\user` form, and left empty for UPNs.
        domain: Option<String>,
    },
    /// OAuth 2 bearer tokens (Exchange Online, or on-premises servers with
    /// hybrid modern authentication).
    Bearer(Arc<dyn TokenSource>),
}

impl Auth {
    /// HTTP Basic credentials.
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Auth::Basic {
            username: username.into(),
            password: password.into(),
        }
    }

    /// NTLM credentials; the domain is taken from a `DOMAIN\user` user name.
    pub fn ntlm(username: impl Into<String>, password: impl Into<String>) -> Self {
        Auth::Ntlm {
            username: username.into(),
            password: password.into(),
            domain: None,
        }
    }

    /// NTLM credentials with an explicit domain.
    pub fn ntlm_with_domain(
        username: impl Into<String>,
        password: impl Into<String>,
        domain: impl Into<String>,
    ) -> Self {
        Auth::Ntlm {
            username: username.into(),
            password: password.into(),
            domain: Some(domain.into()),
        }
    }

    /// Bearer tokens obtained from `source`.
    pub fn bearer(source: impl TokenSource + 'static) -> Self {
        Auth::Bearer(Arc::new(source))
    }

    /// Short name of the scheme, for logs.
    pub fn scheme_name(&self) -> &'static str {
        match self {
            Auth::Basic { .. } => "basic",
            Auth::Ntlm { .. } => "ntlm",
            Auth::Bearer(_) => "bearer",
        }
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Auth::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Auth::Ntlm {
                username, domain, ..
            } => f
                .debug_struct("Ntlm")
                .field("username", username)
                .field("domain", domain)
                .field("password", &"<redacted>")
                .finish(),
            Auth::Bearer(_) => f.write_str("Bearer(<token source>)"),
        }
    }
}

/// `Authorization` header value for HTTP Basic.
pub(crate) fn basic_header(username: &str, password: &str) -> String {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {encoded}")
}

/// Splits NTLM credentials into user and domain: `DOMAIN\user` yields both
/// parts; anything else (including UPNs) keeps the user name whole with
/// the explicit domain or an empty one.
pub(crate) fn split_ntlm_user(username: &str, domain: Option<&str>) -> (String, String) {
    if let Some(domain) = domain {
        return (username.to_owned(), domain.to_owned());
    }
    match username.split_once('\\') {
        Some((domain, user)) if !user.is_empty() => (user.to_owned(), domain.to_owned()),
        _ => (username.to_owned(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_header_matches_rfc7617_example() {
        assert_eq!(
            basic_header("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }

    #[test]
    fn splits_ntlm_user_names() {
        assert_eq!(
            split_ntlm_user("CORP\\jane", None),
            ("jane".to_owned(), "CORP".to_owned())
        );
        assert_eq!(
            split_ntlm_user("jane@corp.example.com", None),
            ("jane@corp.example.com".to_owned(), String::new())
        );
        assert_eq!(
            split_ntlm_user("jane", Some("CORP")),
            ("jane".to_owned(), "CORP".to_owned())
        );
        assert_eq!(
            split_ntlm_user("CORP\\", None),
            ("CORP\\".to_owned(), String::new())
        );
    }

    #[test]
    fn debug_output_redacts_secrets() {
        for auth in [
            Auth::basic("jane", "hunter2"),
            Auth::ntlm_with_domain("jane", "hunter2", "CORP"),
            Auth::bearer(StaticToken::new("hunter2")),
        ] {
            let debug = format!("{auth:?}");
            assert!(!debug.contains("hunter2"), "{debug}");
        }
        assert_eq!(Auth::ntlm("CORP\\jane", "x").scheme_name(), "ntlm");
        assert!(format!("{:?}", StaticToken::new("hunter2")).contains("redacted"));
    }
}
