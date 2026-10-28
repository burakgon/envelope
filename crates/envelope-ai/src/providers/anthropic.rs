//! Anthropic Messages API.

use serde_json::{Map, Value, json};

use super::{
    BuiltRequest, Conversation, MAX_MODEL_PAGES, StreamShared, clamp_temperature, get_json,
    json_u32,
};
use crate::client::Inner;
use crate::error::AiError;
use crate::quirks::{Quirks, SentParams};
use crate::sse::SseEvent;
use crate::types::{ChatRequest, FinishReason, ModelInfo, Role};

/// The `anthropic-version` header value. `2023-06-01` is the long-term stable
/// version that still accepts `temperature`.
pub(crate) const API_VERSION: &str = "2023-06-01";

/// `max_tokens` is mandatory for this API; used when the request has none.
/// Generous because thinking tokens of adaptive-thinking models count towards it.
pub(crate) const DEFAULT_MAX_TOKENS: u32 = 8192;

/// Builds a streaming `POST {base}/messages` request.
pub(crate) fn build_chat(
    inner: &Inner,
    request: &ChatRequest,
    model: &str,
    quirks: &Quirks,
) -> Result<BuiltRequest, AiError> {
    let conversation = Conversation::split(&request.messages, quirks.fold_system);
    conversation.require_turns()?;
    let mut max_tokens = request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).max(1);
    if let Some(cap) = quirks.max_tokens_cap {
        max_tokens = max_tokens.min(cap);
    }
    let mut sent = SentParams {
        token_field: Some("max_tokens"),
        ..SentParams::default()
    };
    let messages: Vec<Value> = conversation
        .turns
        .iter()
        .map(|(role, text)| {
            let role = if *role == Role::Assistant {
                "assistant"
            } else {
                "user"
            };
            json!({ "role": role, "content": text })
        })
        .collect();
    let mut body = Map::new();
    body.insert("model".into(), Value::from(model));
    body.insert("max_tokens".into(), Value::from(max_tokens));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), Value::Bool(true));

    let mut system = conversation.system;
    if request.json {
        // There is no schema-less JSON mode; reinforce the prompt instead.
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str("Respond with valid JSON only, without code fences or commentary.");
    }
    if !system.is_empty() {
        body.insert("system".into(), Value::from(system));
        sent.system = true;
    }
    let optional = !quirks.minimal;
    if let Some(temperature) = request.temperature.and_then(|t| clamp_temperature(t, 1.0))
        && optional
        && !quirks.no_temperature
    {
        body.insert("temperature".into(), Value::from(temperature));
        sent.temperature = true;
    }
    if let Some(effort) = request.effort
        && optional
        && !quirks.no_effort
    {
        body.insert("output_config".into(), json!({ "effort": effort.as_str() }));
        sent.effort = true;
    }
    Ok(BuiltRequest {
        url: format!("{}/messages", inner.base_url),
        body: Value::Object(body),
        sent,
    })
}

/// Decoder for named Messages API events (`message_start`, `content_block_delta`,
/// `message_delta`, `message_stop`, `ping`, `error`, …).
#[derive(Debug, Default)]
pub(crate) struct AnthropicStream;

impl AnthropicStream {
    pub(crate) fn on_event(&mut self, shared: &mut StreamShared, event: &SseEvent) {
        if event.data.trim().is_empty() {
            return;
        }
        let Some(value) = shared.parse_payload(&event.data) else {
            return;
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event.event.as_str());
        match kind {
            "message_start" => {
                if let Some(usage) = value.get("message").and_then(|m| m.get("usage")) {
                    read_usage(shared, usage);
                }
            }
            "content_block_start" => {
                let block = value.get("content_block");
                if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("text")
                    && let Some(text) = block.and_then(|b| b.get("text")).and_then(Value::as_str)
                {
                    shared.delta(text);
                }
            }
            "content_block_delta" => {
                let delta = value.get("delta");
                if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta")
                    && let Some(text) = delta.and_then(|d| d.get("text")).and_then(Value::as_str)
                {
                    shared.delta(text);
                }
            }
            "message_delta" => {
                if let Some(reason) = value
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    shared.set_finish(map_stop_reason(reason), reason);
                }
                if let Some(usage) = value.get("usage") {
                    read_usage(shared, usage);
                }
            }
            "message_stop" => shared.done(),
            "error" => shared.fail_with_payload(&value),
            // ping, content_block_stop and future event types carry nothing we need.
            _ => {}
        }
    }

    /// Handles a complete, non-streaming `message` object.
    pub(crate) fn on_message_json(&mut self, shared: &mut StreamShared, value: &Value) {
        if value.get("type").and_then(Value::as_str) == Some("error")
            || value.get("error").is_some()
        {
            shared.fail_with_payload(value);
            return;
        }
        if let Some(blocks) = value.get("content").and_then(Value::as_array) {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = block.get("text").and_then(Value::as_str)
                {
                    shared.delta(text);
                }
            }
        }
        if let Some(reason) = value.get("stop_reason").and_then(Value::as_str) {
            shared.set_finish(map_stop_reason(reason), reason);
        }
        if let Some(usage) = value.get("usage") {
            read_usage(shared, usage);
        }
        shared.done();
    }
}

/// Usage counts are cumulative. Input = uncached + cache writes + cache reads.
fn read_usage(shared: &mut StreamShared, usage: &Value) {
    let field = |key: &str| usage.get(key).and_then(json_u32);
    let input_parts = [
        field("input_tokens"),
        field("cache_creation_input_tokens"),
        field("cache_read_input_tokens"),
    ];
    if input_parts.iter().any(Option::is_some) {
        let total = input_parts
            .iter()
            .flatten()
            .fold(0_u32, |sum, n| sum.saturating_add(*n));
        shared.usage.input_tokens = Some(total);
    }
    if let Some(output) = field("output_tokens") {
        shared.usage.output_tokens = Some(output);
    }
}

fn map_stop_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        "" => FinishReason::Unknown,
        other => FinishReason::Other(other.to_lowercase()),
    }
}

/// Lists models via `GET {base}/models`, following `has_more`/`last_id` pagination.
pub(crate) async fn list_models(inner: &Inner) -> Result<Vec<ModelInfo>, AiError> {
    let url = format!("{}/models", inner.base_url);
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let mut query = vec![("limit", "1000".to_owned())];
        if let Some(cursor) = &after {
            query.push(("after_id", cursor.clone()));
        }
        let page = get_json(inner, &url, &query)
            .await
            .map_err(|failure| failure.into_error(inner))?;
        models.extend(parse_model_page(&page)?);
        let has_more = page
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let last_id = page
            .get("last_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (has_more, last_id) {
            (true, Some(cursor)) if after.as_deref() != Some(cursor.as_str()) => {
                after = Some(cursor)
            }
            _ => break,
        }
    }
    Ok(models)
}

pub(crate) fn parse_model_page(page: &Value) -> Result<Vec<ModelInfo>, AiError> {
    let items = page
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::invalid_response("unexpected model list format"))?;
    Ok(items
        .iter()
        .filter(|item| item.get("lifecycle").and_then(Value::as_str) != Some("retired"))
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }
            let name = item
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(id);
            Some(ModelInfo {
                name: name.to_owned(),
                context_length: item
                    .get("max_input_tokens")
                    .and_then(json_u32)
                    .filter(|n| *n > 0),
                ..ModelInfo::new(id)
            })
        })
        .collect())
}
