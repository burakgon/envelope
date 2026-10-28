//! Error type shared by the provider layer and the prompt output parsers.

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum number of characters of a provider-supplied message kept in an error.
const MAX_MESSAGE_CHARS: usize = 400;

/// Longest `retry_after` hint that is passed on; anything longer is clamped.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Everything that can go wrong while talking to an AI provider or parsing a
/// model's answer.
///
/// The `Display` output of every variant is a short sentence that can be shown
/// to users as-is. Provider-supplied details are kept in `message` fields for
/// logs and "details" disclosure UIs; they never contain the API key.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AiError {
    /// The provider needs an API key but none is configured.
    #[error("An API key is required for this AI provider.")]
    MissingApiKey,

    /// The provider settings are unusable (malformed base URL, invalid header, no model…).
    #[error("Invalid AI provider settings: {0}")]
    InvalidConfig(String),

    /// The request was rejected locally before anything was sent.
    #[error("Invalid AI request: {0}")]
    InvalidRequest(String),

    /// The provider rejected the credentials (HTTP 401 or an "invalid key" error).
    #[error("The AI provider rejected the API key: {message}")]
    Unauthorized {
        /// The provider's explanation.
        message: String,
    },

    /// The credentials are valid but may not use this resource or region (HTTP 403).
    #[error("The AI provider denied access: {message}")]
    PermissionDenied {
        /// The provider's explanation.
        message: String,
    },

    /// The account is out of credits, over its spending limit or out of quota.
    #[error("Your AI provider account has run out of credits or quota: {message}")]
    QuotaExceeded {
        /// The provider's explanation.
        message: String,
    },

    /// Too many requests. Retry after `retry_after` when the provider said how long to wait.
    #[error("{}", rate_limited_text(.retry_after))]
    RateLimited {
        /// How long the provider asked us to wait, if it said so.
        retry_after: Option<Duration>,
        /// The provider's explanation.
        message: String,
    },

    /// The configured model does not exist or is not available to this account.
    #[error("{}", model_not_found_text(.model))]
    ModelNotFound {
        /// The model id that was requested.
        model: String,
        /// The provider's explanation.
        message: String,
    },

    /// The prompt does not fit into the model's context window.
    #[error(
        "The text is too long for this model's context window. Try a shorter selection or a model with a larger context."
    )]
    ContextTooLong {
        /// The provider's explanation.
        message: String,
    },

    /// The provider is temporarily overloaded or down (HTTP 502/503/529).
    #[error("The AI provider is temporarily overloaded or unavailable. Please try again shortly.")]
    Overloaded {
        /// The provider's explanation.
        message: String,
    },

    /// The provider did not answer in time.
    #[error("The AI provider did not respond in time.")]
    Timeout,

    /// The provider could not be reached or the connection broke.
    #[error("Could not reach the AI provider: {message}")]
    Network {
        /// What failed, e.g. "connection refused".
        message: String,
    },

    /// The provider's safety system blocked the request or the answer.
    #[error("The AI provider blocked this request ({reason}).")]
    ContentBlocked {
        /// The provider's reason, e.g. "safety".
        reason: String,
    },

    /// Any other error reported by the provider.
    #[error("{}", provider_text(.status, .message))]
    Provider {
        /// HTTP status, if the error was not reported inside a stream.
        status: Option<u16>,
        /// The provider's explanation.
        message: String,
    },

    /// The provider answered with something that is not a valid response.
    #[error("The AI provider sent a response Envelope could not understand: {message}")]
    InvalidResponse {
        /// What was wrong with the response.
        message: String,
    },

    /// The provider has no model-list endpoint; the model id must be entered by hand.
    #[error("This provider does not publish a model list. Enter the model name manually.")]
    ModelListUnavailable,

    /// The model answered, but not in the format the task asked for.
    #[error("The AI answer was not in the expected format: {message}")]
    MalformedOutput {
        /// What was wrong with the answer.
        message: String,
    },
}

/// A coarse, serializable classification of [`AiError`], handy for UIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiErrorKind {
    /// See [`AiError::MissingApiKey`].
    MissingApiKey,
    /// See [`AiError::InvalidConfig`].
    InvalidConfig,
    /// See [`AiError::InvalidRequest`].
    InvalidRequest,
    /// See [`AiError::Unauthorized`].
    Unauthorized,
    /// See [`AiError::PermissionDenied`].
    PermissionDenied,
    /// See [`AiError::QuotaExceeded`].
    QuotaExceeded,
    /// See [`AiError::RateLimited`].
    RateLimited,
    /// See [`AiError::ModelNotFound`].
    ModelNotFound,
    /// See [`AiError::ContextTooLong`].
    ContextTooLong,
    /// See [`AiError::Overloaded`].
    Overloaded,
    /// See [`AiError::Timeout`].
    Timeout,
    /// See [`AiError::Network`].
    Network,
    /// See [`AiError::ContentBlocked`].
    ContentBlocked,
    /// See [`AiError::Provider`].
    Provider,
    /// See [`AiError::InvalidResponse`].
    InvalidResponse,
    /// See [`AiError::ModelListUnavailable`].
    ModelListUnavailable,
    /// See [`AiError::MalformedOutput`].
    MalformedOutput,
}

impl AiError {
    /// The coarse kind of this error.
    pub fn kind(&self) -> AiErrorKind {
        match self {
            Self::MissingApiKey => AiErrorKind::MissingApiKey,
            Self::InvalidConfig(_) => AiErrorKind::InvalidConfig,
            Self::InvalidRequest(_) => AiErrorKind::InvalidRequest,
            Self::Unauthorized { .. } => AiErrorKind::Unauthorized,
            Self::PermissionDenied { .. } => AiErrorKind::PermissionDenied,
            Self::QuotaExceeded { .. } => AiErrorKind::QuotaExceeded,
            Self::RateLimited { .. } => AiErrorKind::RateLimited,
            Self::ModelNotFound { .. } => AiErrorKind::ModelNotFound,
            Self::ContextTooLong { .. } => AiErrorKind::ContextTooLong,
            Self::Overloaded { .. } => AiErrorKind::Overloaded,
            Self::Timeout => AiErrorKind::Timeout,
            Self::Network { .. } => AiErrorKind::Network,
            Self::ContentBlocked { .. } => AiErrorKind::ContentBlocked,
            Self::Provider { .. } => AiErrorKind::Provider,
            Self::InvalidResponse { .. } => AiErrorKind::InvalidResponse,
            Self::ModelListUnavailable => AiErrorKind::ModelListUnavailable,
            Self::MalformedOutput { .. } => AiErrorKind::MalformedOutput,
        }
    }

    /// Whether repeating the same request later may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. }
            | Self::Overloaded { .. }
            | Self::Timeout
            | Self::Network { .. } => true,
            Self::Provider { status, .. } => status.is_none_or(|s| s >= 500),
            _ => false,
        }
    }

    /// How long the provider asked to wait before retrying, if it said so.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    pub(crate) fn invalid_response(message: impl Into<String>) -> Self {
        Self::InvalidResponse {
            message: message.into(),
        }
    }

    pub(crate) fn malformed_output(message: impl Into<String>) -> Self {
        Self::MalformedOutput {
            message: message.into(),
        }
    }

    /// Converts a transport error, naming `host` in connection failures.
    pub(crate) fn from_transport(err: reqwest::Error, host: &str) -> Self {
        if err.is_timeout() {
            return Self::Timeout;
        }
        let cause = root_cause(&err);
        if err.is_connect() {
            Self::Network {
                message: format!("could not connect to {host} ({cause})"),
            }
        } else if err.is_body() || err.is_decode() {
            Self::Network {
                message: format!("the connection to {host} was interrupted ({cause})"),
            }
        } else if err.is_builder() {
            Self::InvalidConfig(cause)
        } else {
            Self::Network {
                message: format!("request to {host} failed ({cause})"),
            }
        }
    }
}

fn rate_limited_text(retry_after: &Option<Duration>) -> String {
    let Some(wait) = retry_after else {
        return "The AI provider's rate limit was reached. Please try again shortly.".to_owned();
    };
    let secs = wait
        .as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() > 0));
    if secs <= 1 {
        "The AI provider's rate limit was reached. Please try again in a moment.".to_owned()
    } else if secs < 120 {
        format!("The AI provider's rate limit was reached. Please try again in {secs} seconds.")
    } else {
        let minutes = secs.div_ceil(60);
        format!("The AI provider's rate limit was reached. Please try again in {minutes} minutes.")
    }
}

fn model_not_found_text(model: &str) -> String {
    if model.trim().is_empty() {
        "The selected model is not available from this provider.".to_owned()
    } else {
        format!("The model \u{201c}{model}\u{201d} is not available from this provider.")
    }
}

fn provider_text(status: &Option<u16>, message: &str) -> String {
    match status {
        Some(code) => format!("The AI provider returned an error (HTTP {code}): {message}"),
        None => format!("The AI provider returned an error: {message}"),
    }
}

/// The innermost cause of an error chain, which is usually the most informative part
/// ("Connection refused (os error 111)", "invalid peer certificate…").
fn root_cause(err: &(dyn std::error::Error + 'static)) -> String {
    let mut current = err;
    while let Some(next) = current.source() {
        current = next;
    }
    let text = current.to_string();
    if text.is_empty() {
        err.to_string()
    } else {
        text
    }
}

/// Structured information extracted from an error response or in-stream error event.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ErrorDetails {
    /// HTTP status (or the HTTP-like status embedded in a stream error).
    pub status: Option<u16>,
    /// Human-readable message, cleaned and truncated.
    pub message: String,
    /// Machine-readable codes (`type`, `code`, `status`, `reason`), lowercased.
    pub codes: Vec<String>,
    /// Retry hint from headers or the body.
    pub retry_after: Option<Duration>,
}

impl ErrorDetails {
    /// Builds details from a non-success HTTP response.
    pub(crate) fn from_http(status: u16, headers: &HeaderMap, body: &str) -> Self {
        let mut details = match serde_json::from_str::<Value>(body.trim()) {
            Ok(value) => Self::from_payload(&value),
            Err(_) => Self {
                message: plain_text_message(body),
                ..Self::default()
            },
        };
        details.status = Some(status);
        if details.message.is_empty() {
            details.message = canonical_reason(status);
        }
        if let Some(wait) = retry_after_from_headers(headers) {
            details.retry_after = Some(wait);
        }
        details
    }

    /// Builds details from a JSON error payload (an HTTP error body or an in-stream
    /// error event). Understands the OpenAI, OpenRouter, Anthropic, Gemini, Ollama,
    /// vLLM and FastAPI/pydantic shapes.
    pub(crate) fn from_payload(value: &Value) -> Self {
        let value = match value {
            Value::Array(items) => items.first().unwrap_or(&Value::Null),
            other => other,
        };
        let mut details = Self::default();
        match value.get("error") {
            Some(error @ Value::Object(_)) => details.read_error_object(error),
            Some(Value::String(text)) => details.message = text.clone(),
            _ => details.read_top_level(value),
        }
        details.message = clean_message(&details.message);
        details
    }

    fn read_error_object(&mut self, error: &Value) {
        self.message = first_string(error, &["message", "msg", "detail"]).unwrap_or_default();
        for key in ["type", "code", "status"] {
            if let Some(code) = error.get(key).and_then(Value::as_str) {
                self.codes.push(code.to_lowercase());
            }
        }
        if let Some(status) = error.get("code").and_then(Value::as_u64) {
            self.status = u16::try_from(status).ok();
        }
        if let Some(items) = error.get("details").and_then(Value::as_array) {
            for item in items {
                if let Some(reason) = item.get("reason").and_then(Value::as_str) {
                    self.codes.push(reason.to_lowercase());
                }
                if let Some(delay) = item.get("retryDelay").and_then(Value::as_str) {
                    self.retry_after = parse_duration_text(delay);
                }
            }
        }
        if let Some(metadata) = error.get("metadata") {
            self.read_openrouter_metadata(metadata);
        }
    }

    fn read_openrouter_metadata(&mut self, metadata: &Value) {
        if let Some(kind) = metadata.get("error_type").and_then(Value::as_str) {
            self.codes.push(kind.to_lowercase());
        }
        if let Some(reasons) = metadata.get("reasons").and_then(Value::as_array) {
            self.codes.push("moderation".to_owned());
            let reasons: Vec<&str> = reasons.iter().filter_map(Value::as_str).collect();
            if !reasons.is_empty() {
                self.message = format!("{} ({})", self.message, reasons.join(", "));
            }
        }
        let raw = match metadata.get("raw") {
            Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
                Ok(inner) => Self::from_payload(&inner).message,
                Err(_) => raw.clone(),
            },
            Some(inner @ Value::Object(_)) => Self::from_payload(inner).message,
            _ => String::new(),
        };
        let raw = clean_message(&raw);
        if !raw.is_empty() && !self.message.contains(&raw) {
            self.message = if self.message.is_empty() {
                raw
            } else {
                format!("{} ({raw})", self.message)
            };
        }
    }

    fn read_top_level(&mut self, value: &Value) {
        match value.get("message") {
            Some(Value::String(text)) => self.message = text.clone(),
            Some(nested @ Value::Object(_)) => {
                self.message = first_string(nested, &["message", "detail"]).unwrap_or_default();
            }
            _ => {}
        }
        match value.get("detail") {
            Some(Value::String(text)) if self.message.is_empty() => self.message = text.clone(),
            Some(Value::Array(items)) => {
                let parts: Vec<String> =
                    items.iter().filter_map(describe_validation_item).collect();
                if !parts.is_empty() {
                    let joined = parts.join("; ");
                    self.message = if self.message.is_empty() {
                        joined
                    } else {
                        format!("{} ({joined})", self.message)
                    };
                }
            }
            _ => {}
        }
        for key in ["type", "code"] {
            if let Some(code) = value.get(key).and_then(Value::as_str) {
                self.codes.push(code.to_lowercase());
            }
        }
    }

    /// The message plus all codes, lowercased; used for heuristics.
    pub(crate) fn haystack(&self) -> String {
        let mut text = self.message.to_lowercase();
        for code in &self.codes {
            text.push(' ');
            text.push_str(code);
        }
        text
    }
}

/// `{"loc": ["body", "reasoning_effort"], "msg": "Extra inputs are not permitted"}`
/// becomes `body.reasoning_effort: Extra inputs are not permitted`.
fn describe_validation_item(item: &Value) -> Option<String> {
    let msg = item.get("msg").and_then(Value::as_str)?;
    let loc = item
        .get("loc")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|part| match part {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(".")
        })
        .unwrap_or_default();
    Some(if loc.is_empty() {
        msg.to_owned()
    } else {
        format!("{loc}: {msg}")
    })
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

/// Collapses whitespace and truncates provider messages so errors stay readable.
pub(crate) fn clean_message(message: &str) -> String {
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_MESSAGE_CHARS {
        return collapsed;
    }
    let mut truncated: String = collapsed.chars().take(MAX_MESSAGE_CHARS).collect();
    truncated.push('\u{2026}');
    truncated
}

/// Plain-text error bodies are used as-is; HTML error pages are not useful to users.
fn plain_text_message(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.starts_with('<') {
        String::new()
    } else {
        clean_message(trimmed)
    }
}

fn canonical_reason(status: u16) -> String {
    reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
        .unwrap_or("unknown error")
        .to_owned()
}

/// Parses `retry-after-ms` / `retry-after` (seconds or HTTP date).
pub(crate) fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(millis) = header("retry-after-ms").and_then(|v| v.parse::<f64>().ok())
        && let Some(wait) = seconds_to_duration(millis / 1000.0)
    {
        return Some(wait);
    }
    let value = header("retry-after")?;
    if let Some(wait) = value.parse::<f64>().ok().and_then(seconds_to_duration) {
        return Some(wait);
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let delta = date.with_timezone(&chrono::Utc) - chrono::Utc::now();
    Some(
        delta
            .to_std()
            .unwrap_or(Duration::ZERO)
            .min(MAX_RETRY_AFTER),
    )
}

/// Parses protobuf-style durations such as `"17s"` or `"0.5s"`.
fn parse_duration_text(text: &str) -> Option<Duration> {
    let number = text.trim().strip_suffix('s').unwrap_or(text.trim());
    number.parse::<f64>().ok().and_then(seconds_to_duration)
}

fn seconds_to_duration(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds)
        .ok()
        .map(|wait| wait.min(MAX_RETRY_AFTER))
}

/// What kind of request produced an error; affects how 404s are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Chat,
    Models,
}

/// Maps provider error details onto an [`AiError`].
pub(crate) fn classify(details: ErrorDetails, model: &str, endpoint: Endpoint) -> AiError {
    let ErrorDetails {
        status,
        message,
        codes,
        retry_after,
    } = details;
    let lower = message.to_lowercase();
    let has_code = |needles: &[&str]| codes.iter().any(|c| needles.iter().any(|n| c.contains(n)));
    let has_text = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    let or_default = |fallback: &str| {
        if message.is_empty() {
            fallback.to_owned()
        } else {
            message.clone()
        }
    };

    if status == Some(401)
        || has_code(&[
            "authentication",
            "invalid_api_key",
            "api_key_invalid",
            "unauthenticated",
            "unauthorized",
        ])
        || has_text(&[
            "api key not valid",
            "invalid api key",
            "incorrect api key",
            "invalid x-api-key",
        ])
    {
        return AiError::Unauthorized {
            message: or_default("invalid or missing API key"),
        };
    }
    if status == Some(402)
        || has_code(&["insufficient_quota", "billing", "payment_required"])
        || has_text(&[
            "credit balance is too low",
            "insufficient credits",
            "insufficient balance",
            "exceeded your current quota",
        ])
    {
        return AiError::QuotaExceeded {
            message: or_default("insufficient credits"),
        };
    }
    if has_code(&["moderation", "content_filter", "content_policy"])
        || (status == Some(403) && has_text(&["flagged", "moderation"]))
    {
        return AiError::ContentBlocked {
            reason: or_default("content policy"),
        };
    }
    if status == Some(429) || has_code(&["rate_limit", "resource_exhausted", "too_many_requests"]) {
        return AiError::RateLimited {
            retry_after,
            message: or_default("too many requests"),
        };
    }
    if status == Some(403) || has_code(&["permission"]) {
        return AiError::PermissionDenied {
            message: or_default("forbidden"),
        };
    }
    if status == Some(413)
        || has_code(&[
            "context_length_exceeded",
            "request_too_large",
            "string_above_max_length",
        ])
        || is_context_overflow(&lower)
    {
        return AiError::ContextTooLong {
            message: or_default("request too large"),
        };
    }
    if endpoint == Endpoint::Chat
        && (has_code(&["model_not_found"])
            || is_missing_model(&lower)
            || (status == Some(404) && lower.contains("model")))
    {
        return AiError::ModelNotFound {
            model: model.to_owned(),
            message: or_default("model not found"),
        };
    }
    if matches!(status, Some(502 | 503 | 529)) || has_code(&["overloaded", "unavailable"]) {
        return AiError::Overloaded {
            message: or_default("service unavailable"),
        };
    }
    if matches!(status, Some(408 | 504)) || has_code(&["timeout", "deadline_exceeded"]) {
        return AiError::Timeout;
    }
    if status == Some(404) {
        return AiError::Provider {
            status,
            message: format!(
                "{} \u{2014} check the provider's base URL",
                or_default("not found")
            ),
        };
    }
    AiError::Provider {
        status,
        message: or_default("unknown error"),
    }
}

fn is_context_overflow(lower: &str) -> bool {
    [
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "prompt is too long",
        "prompt too long",
        "too many tokens",
        "input is too long",
        "input token count",
        "exceeds the maximum number of tokens",
        "reduce the length of the messages",
        "too large for model",
        "exceeds the context",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_missing_model(lower: &str) -> bool {
    if lower.contains("no endpoints found") {
        return true;
    }
    lower.contains("model")
        && [
            "not found",
            "does not exist",
            "not a valid model",
            "invalid model",
            "unknown model",
            "no such model",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn details(status: u16, body: &str) -> ErrorDetails {
        ErrorDetails::from_http(status, &HeaderMap::new(), body)
    }

    #[test]
    fn openai_style_401() {
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-***.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#;
        let err = classify(details(401, body), "test-model", Endpoint::Chat);
        assert_eq!(
            err,
            AiError::Unauthorized {
                message: "Incorrect API key provided: sk-***.".into()
            }
        );
    }

    #[test]
    fn gemini_invalid_key_is_400_but_unauthorized() {
        let body = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"}]}}"#;
        let err = classify(details(400, body), "m", Endpoint::Chat);
        assert_eq!(err.kind(), AiErrorKind::Unauthorized);
    }

    #[test]
    fn gemini_retry_info_in_body() {
        let body = r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"17s"}]}}"#;
        let err = classify(details(429, body), "m", Endpoint::Chat);
        assert_eq!(err.retry_after(), Some(Duration::from_secs(17)));
    }

    #[test]
    fn retry_after_header_variants() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_secs(7))
        );
        headers.insert("retry-after-ms", HeaderValue::from_static("1500"));
        assert_eq!(
            retry_after_from_headers(&headers),
            Some(Duration::from_millis(1500))
        );
        let mut dated = HeaderMap::new();
        dated.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after_from_headers(&dated), Some(Duration::ZERO));
        let mut bogus = HeaderMap::new();
        bogus.insert("retry-after", HeaderValue::from_static("-5"));
        assert_eq!(retry_after_from_headers(&bogus), None);
    }

    #[test]
    fn context_overflow_messages() {
        for body in [
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, you requested 9000 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#,
            r#"{"error":{"code":400,"message":"The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).","status":"INVALID_ARGUMENT"}}"#,
        ] {
            let err = classify(details(400, body), "m", Endpoint::Chat);
            assert_eq!(err.kind(), AiErrorKind::ContextTooLong, "{body}");
        }
    }

    #[test]
    fn model_not_found_variants() {
        let cases = [
            (
                404,
                r#"{"error":{"message":"The model `x` does not exist or you do not have access to it.","type":"invalid_request_error","code":"model_not_found"}}"#,
            ),
            (
                404,
                r#"{"type":"error","error":{"type":"not_found_error","message":"model: test-model"}}"#,
            ),
            (
                400,
                r#"{"error":{"message":"test-model is not a valid model ID","code":400}}"#,
            ),
            (
                404,
                r#"{"error":"model \"test-model\" not found, try pulling it first"}"#,
            ),
        ];
        for (status, body) in cases {
            let err = classify(details(status, body), "test-model", Endpoint::Chat);
            assert_eq!(err.kind(), AiErrorKind::ModelNotFound, "{body}");
        }
        let wrong_url = classify(details(404, "Not Found"), "test-model", Endpoint::Chat);
        assert_eq!(wrong_url.kind(), AiErrorKind::Provider);
        assert!(wrong_url.to_string().contains("base URL"));
    }

    #[test]
    fn anthropic_stream_error_types() {
        let overloaded = ErrorDetails::from_payload(&serde_json::json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"}
        }));
        assert_eq!(
            classify(overloaded, "m", Endpoint::Chat).kind(),
            AiErrorKind::Overloaded
        );
        let billing = ErrorDetails::from_payload(&serde_json::json!({
            "type": "error",
            "error": {"type": "billing_error", "message": "Payment required"}
        }));
        assert_eq!(
            classify(billing, "m", Endpoint::Chat).kind(),
            AiErrorKind::QuotaExceeded
        );
    }

    #[test]
    fn openrouter_raw_metadata_and_moderation() {
        let body = r#"{"error":{"code":400,"message":"Provider returned error","metadata":{"provider_name":"X","raw":"{\"error\":{\"message\":\"This model's maximum context length is 4096 tokens.\"}}"}}}"#;
        let err = classify(details(400, body), "m", Endpoint::Chat);
        assert_eq!(err.kind(), AiErrorKind::ContextTooLong);

        let body = r#"{"error":{"code":403,"message":"Input was flagged","metadata":{"reasons":["violence"],"flagged_input":"...","provider_name":"X","model_slug":"m"}}}"#;
        let err = classify(details(403, body), "m", Endpoint::Chat);
        assert_eq!(err.kind(), AiErrorKind::ContentBlocked);
    }

    #[test]
    fn pydantic_validation_errors_keep_field_paths() {
        let body = r#"{"detail":[{"loc":["body","reasoning_effort"],"msg":"Extra inputs are not permitted","type":"extra_forbidden"}]}"#;
        let d = details(422, body);
        assert_eq!(
            d.message,
            "body.reasoning_effort: Extra inputs are not permitted"
        );
    }

    #[test]
    fn html_bodies_fall_back_to_reason_phrase() {
        let d = details(502, "<html><body>Bad gateway</body></html>");
        assert_eq!(d.message, "Bad Gateway");
        assert_eq!(
            classify(d, "m", Endpoint::Chat).kind(),
            AiErrorKind::Overloaded
        );
    }

    #[test]
    fn display_texts_are_user_presentable() {
        let err = AiError::RateLimited {
            retry_after: Some(Duration::from_millis(7200)),
            message: String::new(),
        };
        assert_eq!(
            err.to_string(),
            "The AI provider's rate limit was reached. Please try again in 8 seconds."
        );
        assert!(err.is_retryable());
        let err = AiError::ModelNotFound {
            model: "test-model".into(),
            message: String::new(),
        };
        assert_eq!(
            err.to_string(),
            "The model \u{201c}test-model\u{201d} is not available from this provider."
        );
        assert!(!err.is_retryable());
    }

    #[test]
    fn long_messages_are_truncated() {
        let long = "x ".repeat(1000);
        let cleaned = clean_message(&long);
        assert!(cleaned.chars().count() <= MAX_MESSAGE_CHARS + 1);
        assert!(cleaned.ends_with('\u{2026}'));
    }
}
