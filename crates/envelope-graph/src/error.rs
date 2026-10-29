//! Error types returned by the Graph client.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde_json::Value;

/// Error codes Microsoft Graph uses to signal that a delta (sync) token is no
/// longer valid and a full resynchronization is required.
const RESYNC_CODES: &[&str] = &[
    "syncStateNotFound",
    "syncStateInvalid",
    "resyncRequired",
    "ErrorInvalidSyncStateData",
    "ErrorSyncStateNotFound",
    "ErrorSyncFolderNotFound",
];

/// Details of an error response returned by Microsoft Graph.
///
/// Graph errors carry an HTTP status, a machine-readable `code`
/// (for example `ErrorItemNotFound` or `TooManyRequests`), a human-readable
/// message and diagnostic identifiers that Microsoft support asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status code of the response (`0` for a batched request that
    /// received no response).
    pub status: u16,
    /// Graph error code (`error.code`), or a generic code derived from the
    /// status when the body was not a Graph error document.
    pub code: String,
    /// Human-readable error message (`error.message`).
    pub message: String,
    /// Nested error code (`error.innerError.code`), when present.
    pub inner_code: Option<String>,
    /// The `request-id` of the failed request (from the response headers or
    /// `innerError`), useful when contacting Microsoft support.
    pub request_id: Option<String>,
    /// The `client-request-id` echoed back by Graph.
    pub client_request_id: Option<String>,
    /// Delay requested by the server through the `Retry-After` header.
    pub retry_after: Option<Duration>,
}

impl ApiError {
    /// Builds an error from a non-success HTTP response.
    pub(crate) fn from_response(status: u16, headers: &HeaderMap, body: &[u8]) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let mut error = Self::from_body(status, serde_json::from_slice::<Value>(body).ok(), body);
        if let Some(request_id) = header("request-id") {
            error.request_id = Some(request_id);
        }
        if let Some(client_request_id) = header("client-request-id") {
            error.client_request_id = Some(client_request_id);
        }
        error.retry_after = header("retry-after").and_then(|v| parse_retry_after(&v, Utc::now()));
        error
    }

    /// Builds an error from one response of a JSON batch.
    pub(crate) fn from_batch(
        status: u16,
        headers: &BTreeMap<String, String>,
        body: Option<&Value>,
    ) -> Self {
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let mut error = Self::from_body(status, body.cloned(), &[]);
        if let Some(request_id) = header("request-id") {
            error.request_id = Some(request_id);
        }
        if let Some(client_request_id) = header("client-request-id") {
            error.client_request_id = Some(client_request_id);
        }
        error.retry_after = header("retry-after").and_then(|v| parse_retry_after(&v, Utc::now()));
        error
    }

    fn from_body(status: u16, json: Option<Value>, raw: &[u8]) -> Self {
        let parsed = json
            .and_then(|v| serde_json::from_value::<ErrorEnvelope>(v).ok())
            .and_then(|e| e.error);
        match parsed {
            Some(body) => {
                let inner = body.inner_error.unwrap_or_default();
                ApiError {
                    status,
                    code: body
                        .code
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| generic_code(status).to_owned()),
                    message: body.message.unwrap_or_default(),
                    inner_code: inner.code,
                    request_id: inner.request_id,
                    client_request_id: inner.client_request_id,
                    retry_after: None,
                }
            }
            None => {
                let text = String::from_utf8_lossy(raw);
                let text = text.trim();
                let message = if text.is_empty() {
                    generic_code(status).to_owned()
                } else {
                    text.chars().take(512).collect()
                };
                ApiError {
                    status,
                    code: generic_code(status).to_owned(),
                    message,
                    inner_code: None,
                    request_id: None,
                    client_request_id: None,
                    retry_after: None,
                }
            }
        }
    }

    /// True when the error means a delta link is stale and the collection must
    /// be synchronized again from scratch.
    pub fn is_resync_required(&self) -> bool {
        self.status == 410
            || RESYNC_CODES.iter().any(|c| {
                self.code.eq_ignore_ascii_case(c)
                    || self
                        .inner_code
                        .as_deref()
                        .is_some_and(|inner| inner.eq_ignore_ascii_case(c))
            })
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Microsoft Graph returned {} {}: {}",
            self.status, self.code, self.message
        )?;
        if let Some(request_id) = &self.request_id {
            write!(f, " (request-id {request_id})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// Errors produced by [`GraphClient`](crate::GraphClient) and the mapping helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    /// The [`TokenSource`](crate::TokenSource) could not provide an access token.
    #[error("could not obtain a Microsoft Graph access token: {0}")]
    Auth(String),
    /// Microsoft Graph answered with an error status.
    #[error(transparent)]
    Api(Box<ApiError>),
    /// The HTTP exchange failed (DNS, connection, TLS, timeout, …).
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// A response could not be decoded into the expected shape.
    #[error("unexpected response from Microsoft Graph: {0}")]
    Decode(String),
    /// The caller supplied an argument the client cannot use.
    #[error("invalid argument: {0}")]
    InvalidInput(String),
    /// A recurrence rule could not be converted between RRULE and Graph form.
    #[error(transparent)]
    Recurrence(#[from] RecurrenceError),
}

impl GraphError {
    /// The Graph error details, if this is an API error.
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            GraphError::Api(e) => Some(e),
            _ => None,
        }
    }

    /// HTTP status of an API error.
    pub fn status(&self) -> Option<u16> {
        self.api().map(|e| e.status)
    }

    /// Graph error code of an API error.
    pub fn code(&self) -> Option<&str> {
        self.api().map(|e| e.code.as_str())
    }

    /// `Retry-After` delay requested by the server, if any.
    pub fn retry_after(&self) -> Option<Duration> {
        self.api().and_then(|e| e.retry_after)
    }

    /// True for `404 Not Found` (including `ErrorItemNotFound`).
    pub fn is_not_found(&self) -> bool {
        self.api().is_some_and(|e| {
            e.status == 404
                || e.code.eq_ignore_ascii_case("ErrorItemNotFound")
                || e.code.eq_ignore_ascii_case("itemNotFound")
        })
    }

    /// True for `429 Too Many Requests`.
    pub fn is_throttled(&self) -> bool {
        self.status() == Some(429)
    }

    /// True for `401 Unauthorized` (the token was rejected even after a refresh).
    pub fn is_unauthorized(&self) -> bool {
        self.status() == Some(401)
    }

    /// True for `403 Forbidden`: the grant lacks the permission the request
    /// needs (To Do answers `ErrorAccessDenied` to a grant without
    /// `Tasks.ReadWrite`), or the item may not be changed.
    pub fn is_forbidden(&self) -> bool {
        self.status() == Some(403)
    }

    /// True for `412 Precondition Failed`: an `If-Match` etag no longer
    /// matches, so the item changed on the server.
    pub fn is_precondition_failed(&self) -> bool {
        self.status() == Some(412)
    }

    /// True when a stored delta link is no longer valid (`410 Gone`,
    /// `syncStateNotFound`, …). The caller should discard the delta link,
    /// start a new round without a cursor and reconcile local state.
    pub fn is_resync_required(&self) -> bool {
        self.api().is_some_and(ApiError::is_resync_required)
    }

    /// True for failures that may succeed when retried later (throttling,
    /// server errors, transport failures and batched requests without a
    /// response).
    pub fn is_transient(&self) -> bool {
        match self {
            GraphError::Api(e) => e.status == 0 || e.status == 429 || e.status >= 500,
            GraphError::Transport(_) => true,
            _ => false,
        }
    }

    pub(crate) fn decode(context: &str, err: impl fmt::Display) -> Self {
        GraphError::Decode(format!("{context}: {err}"))
    }
}

impl From<ApiError> for GraphError {
    fn from(value: ApiError) -> Self {
        GraphError::Api(Box::new(value))
    }
}

/// Failure to convert between an iCalendar `RRULE` and a Graph
/// `patternedRecurrence`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecurrenceError {
    /// The rule is syntactically invalid.
    #[error("invalid recurrence rule: {0}")]
    Invalid(String),
    /// The rule is valid but cannot be expressed in the target format.
    #[error("recurrence rule cannot be represented: {0}")]
    Unsupported(String),
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default, alias = "innererror")]
    inner_error: Option<InnerError>,
}

#[derive(Default, Deserialize)]
struct InnerError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default, rename = "request-id")]
    request_id: Option<String>,
    #[serde(default, rename = "client-request-id")]
    client_request_id: Option<String>,
}

fn generic_code(status: u16) -> &'static str {
    match status {
        400 => "badRequest",
        401 => "unauthenticated",
        403 => "accessDenied",
        404 => "itemNotFound",
        409 => "conflict",
        410 => "gone",
        412 => "preconditionFailed",
        413 => "requestEntityTooLarge",
        429 => "tooManyRequests",
        500 => "internalServerError",
        502 => "badGateway",
        503 => "serviceUnavailable",
        504 => "gatewayTimeout",
        _ => "httpError",
    }
}

/// Parses a `Retry-After` header: either delay-seconds or an HTTP date.
pub(crate) fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return Duration::try_from_secs_f64(seconds).ok();
    }
    let at = DateTime::parse_from_rfc2822(value).ok()?;
    let delta = at.with_timezone(&Utc) - now;
    Some(delta.to_std().unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use reqwest::header::HeaderValue;

    #[test]
    fn parses_graph_error_document() {
        let body = br#"{
          "error": {
            "code": "ErrorItemNotFound",
            "message": "The specified object was not found in the store.",
            "innerError": {
              "date": "2024-05-01T10:00:00",
              "request-id": "1d3e2a5c-0000-4000-8000-000000000001",
              "client-request-id": "1d3e2a5c-0000-4000-8000-000000000002"
            }
          }
        }"#;
        let err = ApiError::from_response(404, &HeaderMap::new(), body);
        assert_eq!(err.code, "ErrorItemNotFound");
        assert_eq!(
            err.request_id.as_deref(),
            Some("1d3e2a5c-0000-4000-8000-000000000001")
        );
        assert!(GraphError::from(err).is_not_found());
    }

    #[test]
    fn headers_override_inner_error_ids_and_parse_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("request-id", HeaderValue::from_static("from-header"));
        headers.insert("retry-after", HeaderValue::from_static("7"));
        let err = ApiError::from_response(429, &headers, b"not json");
        assert_eq!(err.code, "tooManyRequests");
        assert_eq!(err.message, "not json");
        assert_eq!(err.request_id.as_deref(), Some("from-header"));
        assert_eq!(err.retry_after, Some(Duration::from_secs(7)));
    }

    #[test]
    fn retry_after_http_date() {
        let now = Utc.with_ymd_and_hms(2015, 10, 21, 7, 27, 30).single();
        let now = now.unwrap_or_default();
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after("garbage", now), None);
        assert_eq!(
            parse_retry_after("1.5", now),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_retry_after("-3", now), None);
        assert_eq!(parse_retry_after("1e300", now), None);
    }

    #[test]
    fn detects_resync_errors() {
        let gone = ApiError::from_response(410, &HeaderMap::new(), b"");
        assert!(gone.is_resync_required());
        let body = br#"{"error":{"code":"SyncStateNotFound","message":"x"}}"#;
        let err = ApiError::from_response(400, &HeaderMap::new(), body);
        assert!(err.is_resync_required());
        let other = ApiError::from_response(400, &HeaderMap::new(), b"{}");
        assert!(!other.is_resync_required());
    }
}
