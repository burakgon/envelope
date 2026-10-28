//! Error types.

use std::fmt;

use serde::Deserialize;

/// Convenience alias for results returned by this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while talking to Google.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The stored sync token is no longer valid.
    ///
    /// Returned for HTTP `410 Gone` on Calendar list requests and for the
    /// People API's `EXPIRED_SYNC_TOKEN` error (People sync tokens expire
    /// seven days after the full sync). The caller must wipe the locally
    /// synced collection and start over with a full sync without a token.
    #[error("the sync token is no longer valid; a full sync is required")]
    SyncTokenExpired,

    /// The [`TokenSource`](crate::TokenSource) could not provide an access token.
    #[error("could not obtain a Google access token: {0}")]
    Auth(String),

    /// Google answered with an error status (after retries, where applicable).
    #[error(transparent)]
    Api(ApiError),

    /// Network, TLS or timeout failure.
    #[error("HTTP request to Google failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A response body did not have the expected JSON shape.
    #[error("could not decode the {context} response: {source}")]
    Decode {
        /// The API method whose response failed to decode (e.g. `events.list`).
        context: &'static str,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },

    /// A response was valid JSON but could not be interpreted (for example an
    /// event without a start time returned by a write).
    #[error("unexpected response from Google: {0}")]
    InvalidResponse(String),

    /// The input cannot be represented in Google's data model or the client
    /// was configured incorrectly.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl Error {
    /// Classifies an API error; sync-token expiry gets its own variant.
    ///
    /// `sync_request` is true for list requests that carry a sync token; only
    /// for those does a bare `410 Gone` mean the token expired (elsewhere a
    /// `410` means the resource itself was deleted).
    pub(crate) fn from_api(error: ApiError, sync_request: bool) -> Self {
        let expired = error.has_reason("EXPIRED_SYNC_TOKEN")
            || error.has_reason("fullSyncRequired")
            || error.has_reason("updatedMinTooLongAgo")
            || (sync_request && error.status == 410 && !error.has_reason("deleted"));
        if expired {
            Error::SyncTokenExpired
        } else {
            Error::Api(error)
        }
    }

    /// The HTTP status code if this is an [`Error::Api`].
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api(api) => Some(api.status),
            _ => None,
        }
    }

    /// The API error details if this is an [`Error::Api`].
    pub fn api_error(&self) -> Option<&ApiError> {
        match self {
            Error::Api(api) => Some(api),
            _ => None,
        }
    }

    /// True if Google still rejected the credentials after a token refresh
    /// (the account needs to be re-authorised), or no token could be obtained.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Error::Auth(_)) || self.status() == Some(401)
    }

    /// True for `404 Not Found`.
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }

    /// True for `410 Gone` outside of sync requests (the resource was deleted).
    pub fn is_gone(&self) -> bool {
        self.status() == Some(410)
    }

    /// True for `412 Precondition Failed` (an `If-Match` etag did not match:
    /// re-fetch the resource and re-apply the change).
    pub fn is_precondition_failed(&self) -> bool {
        self.status() == Some(412)
    }

    /// True if the request was rejected by rate limiting even after retries.
    pub fn is_rate_limited(&self) -> bool {
        self.api_error().is_some_and(ApiError::is_rate_limited)
    }
}

/// An error response from a Google API.
///
/// Google reports errors as
/// `{"error": {"code", "message", "status", "errors": [{"reason", …}], "details": […]}}`;
/// the machine-readable reasons from both the legacy `errors[]` array and
/// `google.rpc.ErrorInfo` details are collected in [`ApiError::reasons`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status code.
    pub status: u16,
    /// Machine-readable reasons such as `rateLimitExceeded`, `notFound`,
    /// `conditionNotMet` or `EXPIRED_SYNC_TOKEN`, in the order Google sent them.
    pub reasons: Vec<String>,
    /// Canonical status name such as `PERMISSION_DENIED`, when present.
    pub status_text: Option<String>,
    /// Human-readable message.
    pub message: String,
}

impl ApiError {
    /// The first machine-readable reason, if any.
    pub fn reason(&self) -> Option<&str> {
        self.reasons.first().map(String::as_str)
    }

    /// True if Google listed `reason` among the error reasons.
    pub fn has_reason(&self, reason: &str) -> bool {
        self.reasons.iter().any(|r| r == reason)
    }

    /// True for rate-limit errors (`429`, or `403` with a rate-limit reason).
    pub fn is_rate_limited(&self) -> bool {
        self.status == 429
            || self.has_reason("rateLimitExceeded")
            || self.has_reason("userRateLimitExceeded")
            || self.has_reason("RATE_LIMIT_EXCEEDED")
    }

    /// True if the request may succeed when retried after a backoff:
    /// rate limiting and transient server errors.
    pub fn is_retryable(&self) -> bool {
        self.is_rate_limited() || matches!(self.status, 500 | 502 | 503 | 504)
    }

    /// Parses an error response body. Never fails: bodies that are not
    /// Google's JSON error format (e.g. an HTML page from a proxy) are kept as
    /// a truncated message.
    pub(crate) fn from_response(status: u16, body: &[u8]) -> Self {
        let mut error = ApiError {
            status,
            reasons: Vec::new(),
            status_text: None,
            message: String::new(),
        };
        match serde_json::from_slice::<ErrorEnvelope>(body) {
            Ok(ErrorEnvelope {
                error: Some(ErrorPayload::Detailed(payload)),
                ..
            }) => {
                error.message = payload.message.unwrap_or_default();
                error.status_text = payload.status;
                for item in payload.errors {
                    if let Some(reason) = item.reason {
                        push_unique(&mut error.reasons, reason);
                    }
                    if error.message.is_empty() {
                        error.message = item.message.unwrap_or_default();
                    }
                }
                for detail in payload.details {
                    let is_error_info = detail
                        .get("@type")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|t| t.ends_with("google.rpc.ErrorInfo"));
                    if let Some(reason) = detail.get("reason").and_then(serde_json::Value::as_str)
                        && is_error_info
                    {
                        push_unique(&mut error.reasons, reason.to_owned());
                    }
                }
            }
            // OAuth-style errors: {"error": "invalid_grant", "error_description": "…"}.
            Ok(ErrorEnvelope {
                error: Some(ErrorPayload::Code(code)),
                error_description,
            }) => {
                error.message = error_description.unwrap_or_else(|| code.clone());
                error.reasons.push(code);
            }
            _ => {
                let text = String::from_utf8_lossy(body);
                error.message = truncate(text.trim(), 300);
            }
        }
        if error.message.is_empty() {
            error.message = reqwest::StatusCode::from_u16(status)
                .ok()
                .and_then(|s| s.canonical_reason())
                .unwrap_or("unknown error")
                .to_owned();
        }
        error
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Google API error {}", self.status)?;
        if let Some(reason) = self.reason() {
            write!(f, " ({reason})")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ApiError {}

/// Fails if a list response hands back the page token that was just
/// requested, which would otherwise make paging loop forever.
pub(crate) fn ensure_page_progress(
    requested: Option<&str>,
    next: Option<&str>,
    context: &str,
) -> Result<()> {
    if next.is_some() && next == requested {
        return Err(Error::InvalidResponse(format!(
            "{context} returned the page token it was asked for; paging cannot advance"
        )));
    }
    Ok(())
}

fn push_unique(reasons: &mut Vec<String>, reason: String) {
    if !reason.is_empty() && !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}…", &text[..idx]),
        None => text.to_owned(),
    }
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorPayload>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorPayload {
    Detailed(DetailedError),
    Code(String),
}

#[derive(Deserialize)]
struct DetailedError {
    message: Option<String>,
    status: Option<String>,
    #[serde(default)]
    errors: Vec<ErrorItem>,
    #[serde(default)]
    details: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ErrorItem {
    reason: Option<String>,
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calendar_rate_limit_error() {
        let body = br#"{"error":{"errors":[{"domain":"usageLimits","reason":"rateLimitExceeded","message":"Rate Limit Exceeded"}],"code":403,"message":"Rate Limit Exceeded"}}"#;
        let err = ApiError::from_response(403, body);
        assert_eq!(err.reason(), Some("rateLimitExceeded"));
        assert_eq!(err.message, "Rate Limit Exceeded");
        assert!(err.is_rate_limited());
        assert!(err.is_retryable());
        assert_eq!(
            err.to_string(),
            "Google API error 403 (rateLimitExceeded): Rate Limit Exceeded"
        );
    }

    #[test]
    fn forbidden_without_rate_reason_is_not_retryable() {
        let body = br#"{"error":{"errors":[{"domain":"calendar","reason":"forbiddenForNonOrganizer","message":"Shared properties can only be changed by the organizer of the event."}],"code":403,"message":"Shared properties can only be changed by the organizer of the event."}}"#;
        let err = ApiError::from_response(403, body);
        assert!(!err.is_retryable());
        assert_eq!(err.reason(), Some("forbiddenForNonOrganizer"));
    }

    #[test]
    fn parses_error_info_details() {
        let body = br#"{"error":{"code":400,"message":"Sync token is expired. Clear local cache and retry call without the sync token.","status":"FAILED_PRECONDITION","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"EXPIRED_SYNC_TOKEN","domain":"people.googleapis.com"}]}}"#;
        let err = ApiError::from_response(400, body);
        assert_eq!(err.status_text.as_deref(), Some("FAILED_PRECONDITION"));
        assert!(err.has_reason("EXPIRED_SYNC_TOKEN"));
        assert!(matches!(
            Error::from_api(err, false),
            Error::SyncTokenExpired
        ));
    }

    #[test]
    fn gone_is_sync_expiry_only_for_sync_requests() {
        let full_sync = br#"{"error":{"errors":[{"domain":"calendar","reason":"fullSyncRequired","message":"Sync token is no longer valid, a full sync is required.","locationType":"parameter","location":"syncToken"}],"code":410,"message":"Sync token is no longer valid, a full sync is required."}}"#;
        let deleted = br#"{"error":{"errors":[{"domain":"global","reason":"deleted","message":"Resource has been deleted"}],"code":410,"message":"Resource has been deleted"}}"#;
        assert!(matches!(
            Error::from_api(ApiError::from_response(410, full_sync), false),
            Error::SyncTokenExpired
        ));
        assert!(matches!(
            Error::from_api(ApiError::from_response(410, b""), true),
            Error::SyncTokenExpired
        ));
        let gone = Error::from_api(ApiError::from_response(410, deleted), true);
        assert!(gone.is_gone());
        assert!(!Error::from_api(ApiError::from_response(410, b""), false).is_rate_limited());
    }

    #[test]
    fn non_json_bodies_are_kept_as_message() {
        let err = ApiError::from_response(502, b"<html>Bad Gateway</html>");
        assert_eq!(err.message, "<html>Bad Gateway</html>");
        assert!(err.is_retryable());
        let empty = ApiError::from_response(503, b"");
        assert_eq!(empty.message, "Service Unavailable");
    }

    #[test]
    fn paging_must_advance() {
        assert!(ensure_page_progress(None, Some("p2"), "events.list").is_ok());
        assert!(ensure_page_progress(Some("p2"), Some("p3"), "events.list").is_ok());
        assert!(ensure_page_progress(Some("p3"), None, "events.list").is_ok());
        assert!(matches!(
            ensure_page_progress(Some("p2"), Some("p2"), "events.list"),
            Err(Error::InvalidResponse(_))
        ));
    }

    #[test]
    fn oauth_style_errors_are_understood() {
        let err = ApiError::from_response(
            400,
            br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        );
        assert_eq!(err.reason(), Some("invalid_grant"));
        assert_eq!(err.message, "Token has been expired or revoked.");
    }
}
