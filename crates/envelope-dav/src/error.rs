//! Error type shared by every operation of the crate.

use std::fmt;

use thiserror::Error;

use crate::discovery::ServiceType;

/// Convenience alias for results produced by this crate.
pub type Result<T, E = DavError> = std::result::Result<T, E>;

/// Everything that can go wrong while talking to a CalDAV / CardDAV server.
///
/// The variants are deliberately specific so callers can react without
/// inspecting status codes: [`DavError::InvalidSyncToken`] means "drop the
/// stored token and do a full resync", [`DavError::PreconditionFailed`] means
/// "the resource changed on the server, refetch before writing", and so on.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DavError {
    /// A URL could not be parsed or is not usable for DAV (unsupported scheme,
    /// missing host, ...).
    #[error("invalid URL `{url}`: {reason}")]
    InvalidUrl {
        /// The offending input.
        url: String,
        /// Why it was rejected.
        reason: String,
    },

    /// An argument supplied by the caller is not acceptable (for example a
    /// colour that is not a hex RGB value).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The server rejected the credentials (HTTP 401 after any automatic
    /// retry: Digest negotiation or a refreshed bearer token).
    #[error("authentication failed: the server rejected the credentials")]
    Unauthorized,

    /// The [`TokenSource`](crate::TokenSource) could not provide an access token.
    #[error("could not obtain an access token: {0}")]
    TokenSource(String),

    /// The server refused the operation (HTTP 403).
    #[error("access to {url} is forbidden{}", Condition(.condition))]
    Forbidden {
        /// The request URL.
        url: String,
        /// The WebDAV precondition reported in the `DAV:error` body, if any
        /// (for example `no-uid-conflict` or `valid-calendar-data`).
        condition: Option<String>,
    },

    /// The resource does not exist (HTTP 404 or 410).
    #[error("{url} was not found on the server")]
    NotFound {
        /// The request URL.
        url: String,
    },

    /// A conditional request failed (HTTP 412): the resource was modified on
    /// the server since the given ETag was obtained, or it already exists when
    /// creating with `If-None-Match: *`.
    #[error("precondition failed for {url}: the resource changed on the server or already exists")]
    PreconditionFailed {
        /// The request URL.
        url: String,
    },

    /// The request conflicts with the current state of the server (HTTP 409).
    #[error("conflict at {url}{}", Condition(.condition))]
    Conflict {
        /// The request URL.
        url: String,
        /// The WebDAV precondition reported in the `DAV:error` body, if any.
        condition: Option<String>,
    },

    /// The sync token sent with a `sync-collection` report is no longer
    /// accepted (RFC 6578 `DAV:valid-sync-token` precondition). The caller
    /// must forget the token and perform a full resync.
    #[error("the sync token is no longer valid; a full resync is required")]
    InvalidSyncToken,

    /// The server asked the client to slow down (HTTP 429, or 503 with
    /// `Retry-After`).
    #[error("rate limited by the server{}", RetryAfter(*.retry_after))]
    RateLimited {
        /// Seconds to wait before retrying, when the server said so.
        retry_after: Option<u64>,
    },

    /// Any other unexpected HTTP status.
    #[error("unexpected HTTP status {status} for {method} {url}")]
    Http {
        /// The HTTP status code.
        status: u16,
        /// The request method.
        method: String,
        /// The request URL.
        url: String,
        /// The beginning of the response body, for diagnostics.
        body: String,
    },

    /// A property could not be changed by `PROPPATCH`.
    #[error("the server refused to update property `{property}` (HTTP {status})")]
    PropertyUpdate {
        /// Local name of the rejected property.
        property: String,
        /// Status reported for it.
        status: u16,
    },

    /// Redirects did not converge.
    #[error("too many redirects while requesting {url}")]
    TooManyRedirects {
        /// The URL of the original request.
        url: String,
    },

    /// Service discovery found no CalDAV / CardDAV service.
    #[error("no {service} service found at {url}")]
    ServiceNotFound {
        /// The service that was looked for.
        service: ServiceType,
        /// Where discovery started.
        url: String,
    },

    /// The response body is not well-formed XML.
    #[error("malformed XML in server response: {0}")]
    Xml(String),

    /// The response is well-formed but does not make sense (for example a
    /// multistatus without hrefs).
    #[error("invalid server response: {0}")]
    InvalidResponse(String),

    /// The request timed out.
    #[error("request timed out")]
    Timeout(#[source] reqwest::Error),

    /// Connection, TLS or protocol level failure.
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),
}

impl DavError {
    /// HTTP status code associated with the error, when there is one.
    pub fn status(&self) -> Option<u16> {
        match self {
            DavError::Unauthorized => Some(401),
            DavError::Forbidden { .. } => Some(403),
            DavError::NotFound { .. } => Some(404),
            DavError::Conflict { .. } => Some(409),
            DavError::PreconditionFailed { .. } => Some(412),
            DavError::RateLimited { .. } => Some(429),
            DavError::Http { status, .. } | DavError::PropertyUpdate { status, .. } => {
                Some(*status)
            }
            DavError::Network(err) | DavError::Timeout(err) => err.status().map(|s| s.as_u16()),
            _ => None,
        }
    }

    /// True for failures that may succeed when retried later unchanged
    /// (network problems, timeouts, rate limiting and 5xx responses).
    pub fn is_transient(&self) -> bool {
        match self {
            DavError::Network(_) | DavError::Timeout(_) | DavError::RateLimited { .. } => true,
            DavError::Http { status, .. } => *status >= 500 && *status != 501 && *status != 507,
            _ => false,
        }
    }

    /// True when the error means the locally stored sync state is unusable and
    /// the collection has to be resynchronised from scratch.
    pub fn requires_full_resync(&self) -> bool {
        matches!(self, DavError::InvalidSyncToken)
    }

    pub(crate) fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            DavError::Timeout(err)
        } else {
            DavError::Network(err)
        }
    }

    pub(crate) fn invalid_url(url: impl Into<String>, reason: impl fmt::Display) -> Self {
        DavError::InvalidUrl {
            url: url.into(),
            reason: reason.to_string(),
        }
    }
}

/// Formats an optional precondition as ` (condition)`.
struct Condition<'a>(&'a Option<String>);

impl fmt::Display for Condition<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(condition) => write!(f, " ({condition})"),
            None => Ok(()),
        }
    }
}

/// Formats an optional retry delay as `; retry after N s`.
struct RetryAfter(Option<u64>);

impl fmt::Display for RetryAfter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(secs) => write!(f, "; retry after {secs} s"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_condition() {
        let err = DavError::Forbidden {
            url: "https://dav.example.com/cal/a.ics".into(),
            condition: Some("no-uid-conflict".into()),
        };
        assert_eq!(
            err.to_string(),
            "access to https://dav.example.com/cal/a.ics is forbidden (no-uid-conflict)"
        );
        let err = DavError::Conflict {
            url: "u".into(),
            condition: None,
        };
        assert_eq!(err.to_string(), "conflict at u");
    }

    #[test]
    fn classification_helpers() {
        assert!(DavError::InvalidSyncToken.requires_full_resync());
        assert!(!DavError::Unauthorized.requires_full_resync());
        assert!(
            DavError::RateLimited {
                retry_after: Some(3)
            }
            .is_transient()
        );
        assert_eq!(
            DavError::RateLimited {
                retry_after: Some(3)
            }
            .to_string(),
            "rate limited by the server; retry after 3 s"
        );
        let server_error = DavError::Http {
            status: 503,
            method: "PROPFIND".into(),
            url: "u".into(),
            body: String::new(),
        };
        assert!(server_error.is_transient());
        assert_eq!(server_error.status(), Some(503));
        assert!(!DavError::PreconditionFailed { url: "u".into() }.is_transient());
        assert_eq!(
            DavError::PreconditionFailed { url: "u".into() }.status(),
            Some(412)
        );
    }
}
