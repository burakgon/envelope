//! Provider-specific wire formats.
//!
//! Each submodule knows how to build a streaming chat request, decode the
//! provider's stream events and list its models. Shared helpers live here.

pub(crate) mod anthropic;
pub(crate) mod gemini;
pub(crate) mod openai;

use serde_json::Value;

use crate::client::Inner;
use crate::error::{AiError, ErrorDetails};
use crate::quirks::SentParams;
use crate::types::{ChatEvent, ChatMessage, FinishReason, Role, Usage};

/// Error bodies are truncated to this many bytes.
pub(crate) const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// Model lists larger than this are rejected (OpenRouter's is ~1 MB).
const MODEL_LIST_LIMIT: usize = 32 * 1024 * 1024;

/// Upper bound on pages fetched from paginated model lists.
pub(crate) const MAX_MODEL_PAGES: usize = 20;

/// A fully built HTTP request body for a chat call.
#[derive(Debug)]
pub(crate) struct BuiltRequest {
    pub url: String,
    pub body: Value,
    pub sent: SentParams,
}

/// The conversation split the way Anthropic and Gemini need it: one system text
/// and alternating turns (consecutive messages of the same role merged).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Conversation {
    pub system: String,
    pub turns: Vec<(Role, String)>,
}

impl Conversation {
    /// Splits `messages`; with `fold_system`, the system text is prepended to the
    /// first user turn instead of being returned separately.
    pub(crate) fn split(messages: &[ChatMessage], fold_system: bool) -> Self {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut turns: Vec<(Role, String)> = Vec::new();
        for message in messages {
            if message.role == Role::System {
                if !message.content.trim().is_empty() {
                    system_parts.push(message.content.as_str());
                }
                continue;
            }
            match turns.last_mut() {
                Some((role, text)) if *role == message.role => {
                    text.push_str("\n\n");
                    text.push_str(&message.content);
                }
                _ => turns.push((message.role, message.content.clone())),
            }
        }
        let mut system = system_parts.join("\n\n");
        if fold_system && !system.is_empty() {
            match turns.iter_mut().find(|(role, _)| *role == Role::User) {
                Some((_, text)) => *text = format!("{system}\n\n{text}"),
                None => turns.insert(0, (Role::User, system.clone())),
            }
            system.clear();
        }
        Self { system, turns }
    }

    /// Fails when there is nothing for the model to answer.
    pub(crate) fn require_turns(&self) -> Result<(), AiError> {
        if self.turns.iter().any(|(role, _)| *role == Role::User) {
            Ok(())
        } else {
            Err(AiError::InvalidRequest(
                "the request contains no user message".to_owned(),
            ))
        }
    }
}

/// OpenAI-style messages with system messages merged into the first user turn.
pub(crate) fn fold_system_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let conversation = Conversation::split(messages, true);
    conversation
        .turns
        .into_iter()
        .map(|(role, content)| ChatMessage { role, content })
        .collect()
}

/// A temperature value clamped to `[0, max]` and rounded to two decimals (so
/// `0.2f32` is sent as `0.2`, not `0.20000000298`), or `None` if it is not a number.
pub(crate) fn clamp_temperature(value: f32, max: f32) -> Option<f64> {
    value
        .is_finite()
        .then(|| (f64::from(value.clamp(0.0, max)) * 100.0).round() / 100.0)
}

/// Reads a JSON number (or numeric string) as `u32`, saturating.
pub(crate) fn json_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(n) => n
            .as_u64()
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .or_else(|| {
                n.as_f64()
                    .filter(|f| *f >= 0.0)
                    .map(|f| f.min(f64::from(u32::MAX)) as u32)
            }),
        Value::String(s) => s
            .trim()
            .parse::<u64>()
            .ok()
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        _ => None,
    }
}

/// Reads a non-negative price (number or numeric string). Negative values mean
/// "variable" (e.g. routers) and yield `None`.
pub(crate) fn json_price(value: &Value) -> Option<f64> {
    let price = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (price.is_finite() && price >= 0.0).then_some(price)
}

/// Rounds away floating-point noise from unit conversions (3.0000000000000004 → 3).
pub(crate) fn round_price(price: f64) -> f64 {
    (price * 1_000_000.0).round() / 1_000_000.0
}

/// Model ids that are clearly not chat models (embeddings, speech, images…).
pub(crate) fn looks_non_chat(id: &str) -> bool {
    let id = id.to_lowercase();
    [
        "embed",
        "whisper",
        "tts",
        "dall-e",
        "moderation",
        "transcribe",
        "rerank",
        "davinci",
        "babbage",
        "realtime",
        "sora",
        "imagen",
        "image",
        "audio",
        "speech",
        "computer-use",
    ]
    .iter()
    .any(|needle| id.contains(needle))
}

/// State shared by all provider stream decoders.
#[derive(Debug, Default)]
pub(crate) struct StreamShared {
    pub out: Vec<Result<ChatEvent, AiError>>,
    pub model: String,
    pub text_emitted: bool,
    pub finish: Option<FinishReason>,
    pub finish_raw: Option<String>,
    pub usage: Usage,
    pub finished: bool,
}

impl StreamShared {
    pub(crate) fn delta(&mut self, text: &str) {
        if !text.is_empty() && !self.finished {
            self.text_emitted = true;
            self.out.push(Ok(ChatEvent::Delta(text.to_owned())));
        }
    }

    pub(crate) fn set_finish(&mut self, reason: FinishReason, raw: &str) {
        self.finish = Some(reason);
        self.finish_raw = Some(raw.to_owned());
    }

    pub(crate) fn fail(&mut self, err: AiError) {
        if !self.finished {
            self.out.push(Err(err));
            self.finished = true;
        }
    }

    pub(crate) fn fail_with_payload(&mut self, payload: &Value) {
        let details = ErrorDetails::from_payload(payload);
        let err = crate::error::classify(details, &self.model, crate::error::Endpoint::Chat);
        self.fail(err);
    }

    /// Emits the terminal `Done` event (or `ContentBlocked` if a safety filter
    /// stopped the answer before any text was produced).
    pub(crate) fn done(&mut self) {
        if self.finished {
            return;
        }
        let reason = self.finish.take().unwrap_or(FinishReason::Unknown);
        if reason == FinishReason::ContentFilter && !self.text_emitted {
            let raw = self
                .finish_raw
                .take()
                .unwrap_or_else(|| "content filter".to_owned());
            self.fail(AiError::ContentBlocked {
                reason: raw.to_lowercase().replace('_', " "),
            });
            return;
        }
        let usage = (!self.usage.is_empty()).then_some(self.usage);
        self.out.push(Ok(ChatEvent::Done {
            finish_reason: reason,
            usage,
        }));
        self.finished = true;
    }

    /// Handles a clean end of the HTTP body without an explicit end marker.
    pub(crate) fn eof(&mut self) {
        if self.finished {
            return;
        }
        if self.finish.is_some() || self.text_emitted {
            self.done();
        } else {
            self.fail(AiError::invalid_response(
                "the provider closed the stream without sending an answer",
            ));
        }
    }

    /// Parses an event payload; a malformed payload ends the stream with an error.
    pub(crate) fn parse_payload(&mut self, data: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(data) {
            Ok(value) => Some(value),
            Err(err) => {
                self.fail(AiError::invalid_response(format!(
                    "malformed stream event ({err})"
                )));
                None
            }
        }
    }
}

/// Why a GET request failed: an HTTP error status (kept raw so callers can try
/// fallbacks on 404) or anything else.
#[derive(Debug)]
pub(crate) enum GetFailure {
    Status(ErrorDetails),
    Other(AiError),
}

impl GetFailure {
    pub(crate) fn into_error(self, inner: &Inner) -> AiError {
        match self {
            Self::Status(details) => {
                crate::error::classify(details, &inner.config.model, crate::error::Endpoint::Models)
            }
            Self::Other(err) => err,
        }
    }

    pub(crate) fn status(&self) -> Option<u16> {
        match self {
            Self::Status(details) => details.status,
            Self::Other(_) => None,
        }
    }
}

/// GETs a JSON document with the provider's headers.
pub(crate) async fn get_json(
    inner: &Inner,
    url: &str,
    query: &[(&str, String)],
) -> Result<Value, GetFailure> {
    let response = inner
        .http
        .get(url)
        .headers(inner.headers.clone())
        .query(query)
        .timeout(inner.options.request_timeout)
        .send()
        .await
        .map_err(|err| GetFailure::Other(AiError::from_transport(err, &inner.host)))?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let (body, _) = read_limited(response, ERROR_BODY_LIMIT).await;
        let body = String::from_utf8_lossy(&body);
        return Err(GetFailure::Status(ErrorDetails::from_http(
            status.as_u16(),
            &headers,
            &body,
        )));
    }
    let (body, complete) = read_limited(response, MODEL_LIST_LIMIT).await;
    if !complete {
        return Err(GetFailure::Other(AiError::invalid_response(
            "the response was too large or was interrupted",
        )));
    }
    serde_json::from_slice(&body).map_err(|err| {
        GetFailure::Other(AiError::invalid_response(format!(
            "the response is not valid JSON ({err})"
        )))
    })
}

/// Reads at most `limit` bytes of a body. The flag is `false` when the body was
/// truncated or the connection failed midway.
pub(crate) async fn read_limited(mut response: reqwest::Response, limit: usize) -> (Vec<u8>, bool) {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = limit.saturating_sub(body.len());
                if chunk.len() > room {
                    body.extend_from_slice(&chunk[..room]);
                    return (body, false);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return (body, true),
            Err(_) => return (body, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_merges_and_folds() {
        let messages = vec![
            ChatMessage::system("S1"),
            ChatMessage::user("U1"),
            ChatMessage::user("U2"),
            ChatMessage::system("S2"),
            ChatMessage::assistant("A1"),
        ];
        let split = Conversation::split(&messages, false);
        assert_eq!(split.system, "S1\n\nS2");
        assert_eq!(
            split.turns,
            vec![
                (Role::User, "U1\n\nU2".to_owned()),
                (Role::Assistant, "A1".to_owned())
            ]
        );
        let folded = Conversation::split(&messages, true);
        assert!(folded.system.is_empty());
        assert_eq!(folded.turns[0].1, "S1\n\nS2\n\nU1\n\nU2");
    }

    #[test]
    fn prices_and_numbers() {
        assert_eq!(json_price(&Value::from("-1")), None);
        assert_eq!(
            json_price(&Value::from("0.000003")).map(|p| round_price(p * 1e6)),
            Some(3.0)
        );
        assert_eq!(json_u32(&Value::from(5_000_000_000_u64)), Some(u32::MAX));
        assert_eq!(json_u32(&Value::from("128000")), Some(128_000));
    }

    #[test]
    fn non_chat_heuristics() {
        assert!(looks_non_chat("text-embedding-3-small"));
        assert!(looks_non_chat("whisper-large-v3"));
        assert!(!looks_non_chat("test-model"));
        assert!(!looks_non_chat("llama-3.2-11b-vision"));
    }
}
