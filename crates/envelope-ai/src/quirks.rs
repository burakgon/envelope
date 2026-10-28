//! Adaptive handling of optional request parameters.
//!
//! OpenAI-compatible servers differ in which optional parameters they accept
//! (`max_tokens` vs `max_completion_tokens`, `temperature` on reasoning models,
//! `stream_options`, `response_format`, reasoning effort, system messages…).
//! Rather than hardcoding per-model tables, the client sends the best request,
//! and when the server answers 400/422 naming a parameter it does not accept,
//! the request is retried without it. Adaptations that led to a successful
//! response are remembered per endpoint and model for the life of the process.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use crate::config::ProviderKind;
use crate::error::ErrorDetails;

/// How many times one request may be adapted and retried.
pub(crate) const MAX_ADAPTATIONS: usize = 4;

/// Upper bound on remembered endpoint/model combinations.
const CACHE_CAPACITY: usize = 256;

/// Fallback `max_tokens` for Anthropic models with a lower output limit.
pub(crate) const ANTHROPIC_SAFE_MAX_TOKENS: u32 = 4096;

/// Which token-limit parameter an OpenAI-compatible server expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TokenField {
    /// Decide from the endpoint (official OpenAI → `max_completion_tokens`).
    #[default]
    Auto,
    /// Classic `max_tokens`.
    MaxTokens,
    /// Newer `max_completion_tokens`.
    MaxCompletionTokens,
}

/// Request adaptations for one endpoint + model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Quirks {
    pub token_field: TokenField,
    pub no_temperature: bool,
    pub no_json_mode: bool,
    pub no_stream_options: bool,
    pub no_effort: bool,
    /// Merge system messages into the first user message.
    pub fold_system: bool,
    /// Cap for `max_tokens` (Anthropic models with smaller output limits).
    pub max_tokens_cap: Option<u32>,
    /// Last resort: send no optional parameters at all.
    pub minimal: bool,
}

/// Which optional parameters a built request actually contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SentParams {
    pub temperature: bool,
    pub token_field: Option<&'static str>,
    pub json_mode: bool,
    pub stream_options: bool,
    pub effort: bool,
    pub system: bool,
}

impl SentParams {
    fn any_strippable(&self) -> bool {
        self.temperature || self.json_mode || self.stream_options || self.effort
    }
}

static CACHE: LazyLock<Mutex<HashMap<String, Quirks>>> = LazyLock::new(Default::default);

/// Cache key for an endpoint + model combination.
pub(crate) fn cache_key(kind: ProviderKind, base_url: &str, model: &str) -> String {
    format!("{kind:?}|{base_url}|{model}")
}

/// Quirks learned earlier for this endpoint + model (defaults if none).
pub(crate) fn load(key: &str) -> Quirks {
    let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.get(key).copied().unwrap_or_default()
}

/// Remembers quirks that led to a successful request.
pub(crate) fn store(key: &str, quirks: Quirks) {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= CACHE_CAPACITY && !cache.contains_key(key) {
        cache.clear();
    }
    cache.insert(key.to_owned(), quirks);
}

/// Adjusts `quirks` after a 400/422 rejection. Returns `true` when something
/// changed and the request should be retried.
pub(crate) fn adapt(
    quirks: &mut Quirks,
    kind: ProviderKind,
    details: &ErrorDetails,
    sent: &SentParams,
) -> bool {
    if !matches!(details.status, Some(400 | 422)) {
        return false;
    }
    let text = details.haystack();
    let mentions = |needle: &str| text.contains(needle);
    let rejected = || {
        [
            "unsupported",
            "not supported",
            "does not support",
            "unrecognized",
            "unknown",
            "extra",
            "not permitted",
            "not allowed",
            "invalid",
            "not enabled",
            "only the default",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    };
    let before = *quirks;

    match (kind, sent.token_field) {
        (ProviderKind::OpenAiCompatible, Some("max_tokens"))
            if mentions("max_tokens") && (mentions("max_completion_tokens") || rejected()) =>
        {
            quirks.token_field = TokenField::MaxCompletionTokens;
        }
        (ProviderKind::OpenAiCompatible, Some("max_completion_tokens"))
            if mentions("max_completion_tokens") && rejected() =>
        {
            quirks.token_field = TokenField::MaxTokens;
        }
        (ProviderKind::Anthropic, Some("max_tokens"))
            if mentions("max_tokens") && quirks.max_tokens_cap.is_none() =>
        {
            quirks.max_tokens_cap = Some(ANTHROPIC_SAFE_MAX_TOKENS);
        }
        _ => {}
    }
    if sent.temperature && mentions("temperature") {
        quirks.no_temperature = true;
    }
    if sent.stream_options && mentions("stream_options") {
        quirks.no_stream_options = true;
    }
    if sent.json_mode
        && [
            "response_format",
            "json mode",
            "json_object",
            "responsemimetype",
            "response_mime_type",
            "mime type",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    {
        quirks.no_json_mode = true;
    }
    if sent.effort
        && ["reasoning", "effort", "thinking", "output_config"]
            .iter()
            .any(|needle| text.contains(needle))
    {
        quirks.no_effort = true;
    }
    if sent.system
        && (mentions("system") || mentions("developer instruction"))
        && (mentions("role") || rejected() || mentions("only user and assistant"))
    {
        quirks.fold_system = true;
    }
    if *quirks == before && !quirks.minimal && sent.any_strippable() {
        quirks.minimal = true;
    }
    *quirks != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejection(message: &str) -> ErrorDetails {
        ErrorDetails {
            status: Some(400),
            message: message.to_owned(),
            ..ErrorDetails::default()
        }
    }

    #[test]
    fn switches_token_field() {
        let mut q = Quirks::default();
        let sent = SentParams {
            token_field: Some("max_tokens"),
            ..SentParams::default()
        };
        let err = rejection(
            "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
        );
        assert!(adapt(&mut q, ProviderKind::OpenAiCompatible, &err, &sent));
        assert_eq!(q.token_field, TokenField::MaxCompletionTokens);
    }

    #[test]
    fn drops_temperature_and_effort() {
        let mut q = Quirks::default();
        let sent = SentParams {
            temperature: true,
            effort: true,
            ..SentParams::default()
        };
        let err = rejection(
            "Unsupported value: 'temperature' does not support 0.2 with this model. Only the default (1) value is supported.",
        );
        assert!(adapt(&mut q, ProviderKind::OpenAiCompatible, &err, &sent));
        assert!(q.no_temperature);
        assert!(!q.no_effort);
        let err = rejection("body.reasoning_effort: Extra inputs are not permitted");
        assert!(adapt(&mut q, ProviderKind::OpenAiCompatible, &err, &sent));
        assert!(q.no_effort);
    }

    #[test]
    fn folds_system_for_gemma_style_errors() {
        let mut q = Quirks::default();
        let sent = SentParams {
            system: true,
            ..SentParams::default()
        };
        let err = rejection("Developer instruction is not enabled for models/test-model");
        assert!(adapt(&mut q, ProviderKind::Gemini, &err, &sent));
        assert!(q.fold_system);
    }

    #[test]
    fn unknown_rejection_falls_back_to_minimal_once() {
        let mut q = Quirks::default();
        let sent = SentParams {
            json_mode: true,
            ..SentParams::default()
        };
        let err = rejection("Bad request");
        assert!(adapt(&mut q, ProviderKind::OpenAiCompatible, &err, &sent));
        assert!(q.minimal);
        assert!(!adapt(&mut q, ProviderKind::OpenAiCompatible, &err, &sent));
    }

    #[test]
    fn non_400_errors_never_adapt() {
        let mut q = Quirks::default();
        let sent = SentParams {
            temperature: true,
            ..SentParams::default()
        };
        let mut err = rejection("temperature");
        err.status = Some(500);
        assert!(!adapt(&mut q, ProviderKind::OpenAiCompatible, &err, &sent));
    }
}
