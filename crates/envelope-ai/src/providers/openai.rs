//! OpenAI Chat Completions protocol, spoken by OpenAI and many compatible servers.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::{
    BuiltRequest, StreamShared, clamp_temperature, fold_system_messages, get_json, json_price,
    json_u32, looks_non_chat, round_price,
};
use crate::client::{Flavor, Inner};
use crate::error::AiError;
use crate::quirks::{Quirks, SentParams, TokenField};
use crate::sse::SseEvent;
use crate::types::{ChatRequest, FinishReason, ModelInfo, Role, Usage};

/// Builds a streaming `POST {base}/chat/completions` request.
pub(crate) fn build_chat(
    inner: &Inner,
    request: &ChatRequest,
    model: &str,
    quirks: &Quirks,
) -> Result<BuiltRequest, AiError> {
    let messages = if quirks.fold_system {
        fold_system_messages(&request.messages)
    } else {
        request.messages.clone()
    };
    if messages.is_empty() {
        return Err(AiError::InvalidRequest(
            "the request contains no messages".to_owned(),
        ));
    }
    let mut sent = SentParams {
        system: messages.iter().any(|m| m.role == Role::System),
        ..SentParams::default()
    };
    let mut body = Map::new();
    body.insert("model".into(), Value::from(model));
    body.insert(
        "messages".into(),
        serde_json::to_value(&messages).map_err(|e| AiError::InvalidRequest(e.to_string()))?,
    );
    body.insert("stream".into(), Value::Bool(true));

    let optional = !quirks.minimal;
    if optional && !quirks.no_stream_options {
        body.insert("stream_options".into(), json!({ "include_usage": true }));
        sent.stream_options = true;
    }
    if let Some(temperature) = request.temperature.and_then(|t| clamp_temperature(t, 2.0))
        && optional
        && !quirks.no_temperature
    {
        body.insert("temperature".into(), Value::from(temperature));
        sent.temperature = true;
    }
    if let Some(max_tokens) = request.max_tokens {
        let field = match quirks.token_field {
            TokenField::MaxTokens => "max_tokens",
            TokenField::MaxCompletionTokens => "max_completion_tokens",
            TokenField::Auto if inner.flavor == Flavor::OpenAi => "max_completion_tokens",
            TokenField::Auto => "max_tokens",
        };
        body.insert(field.into(), Value::from(max_tokens));
        sent.token_field = Some(field);
    }
    if request.json && optional && !quirks.no_json_mode {
        body.insert("response_format".into(), json!({ "type": "json_object" }));
        sent.json_mode = true;
    }
    if let Some(effort) = request.effort
        && optional
        && !quirks.no_effort
    {
        if inner.flavor == Flavor::OpenRouter {
            body.insert("reasoning".into(), json!({ "effort": effort.as_str() }));
        } else {
            body.insert("reasoning_effort".into(), Value::from(effort.as_str()));
        }
        sent.effort = true;
    }
    Ok(BuiltRequest {
        url: format!("{}/chat/completions", inner.base_url),
        body: Value::Object(body),
        sent,
    })
}

/// Decoder for `data: {chat.completion.chunk}` streams ending in `data: [DONE]`.
#[derive(Debug, Default)]
pub(crate) struct OpenAiStream;

impl OpenAiStream {
    pub(crate) fn on_event(&mut self, shared: &mut StreamShared, event: &SseEvent) {
        if event.is_done_marker() {
            shared.done();
            return;
        }
        if event.data.trim().is_empty() {
            return;
        }
        let Some(value) = shared.parse_payload(&event.data) else {
            return;
        };
        if event.event == "error" {
            shared.fail_with_payload(&value);
            return;
        }
        self.on_json(shared, &value);
    }

    /// Handles one chunk object, or a complete non-streaming completion.
    pub(crate) fn on_json(&mut self, shared: &mut StreamShared, value: &Value) {
        if value.get("error").is_some_and(|e| !e.is_null()) {
            shared.fail_with_payload(value);
            return;
        }
        if let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            for container in ["delta", "message"] {
                if let Some(content) = choice.get(container).and_then(|c| c.get("content")) {
                    shared.delta(&content_text(content));
                }
            }
            if let Some(text) = choice.get("text").and_then(Value::as_str) {
                shared.delta(text);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                shared.set_finish(map_finish(reason), reason);
            }
        }
        let usage = value
            .get("usage")
            .filter(|u| u.is_object())
            .or_else(|| value.get("x_groq").and_then(|x| x.get("usage")));
        if let Some(usage) = usage {
            merge_usage(&mut shared.usage, usage);
        }
    }
}

/// `content` is usually a string; some servers send an array of typed parts
/// (with "thinking" parts that must not be shown).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| {
                part.get("type")
                    .and_then(Value::as_str)
                    .is_none_or(|kind| kind == "text")
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

fn map_finish(reason: &str) -> FinishReason {
    match reason {
        "stop" | "end_turn" | "stop_sequence" | "eos" => FinishReason::Stop,
        "length" | "max_tokens" | "model_length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        "tool_calls" | "function_call" | "tool_use" => FinishReason::ToolCalls,
        "" => FinishReason::Unknown,
        other => FinishReason::Other(other.to_lowercase()),
    }
}

fn merge_usage(usage: &mut Usage, value: &Value) {
    let input = value
        .get("prompt_tokens")
        .or_else(|| value.get("input_tokens"))
        .and_then(json_u32);
    let output = value
        .get("completion_tokens")
        .or_else(|| value.get("output_tokens"))
        .and_then(json_u32);
    if input.is_some() {
        usage.input_tokens = input;
    }
    if output.is_some() {
        usage.output_tokens = output;
    }
}

/// Lists models via `GET {base}/models`, falling back to Ollama's native
/// `GET /api/tags` when the OpenAI-style endpoint does not exist.
pub(crate) async fn list_models(inner: &Inner) -> Result<Vec<ModelInfo>, AiError> {
    let url = format!("{}/models", inner.base_url);
    match get_json(inner, &url, &[]).await {
        Ok(value) => parse_model_list(&value),
        Err(failure) if matches!(failure.status(), Some(404 | 405)) => {
            let root = inner
                .base_url
                .strip_suffix("/v1")
                .unwrap_or(&inner.base_url);
            match get_json(inner, &format!("{root}/api/tags"), &[]).await {
                Ok(value) => parse_ollama_tags(&value),
                Err(_) => Err(AiError::ModelListUnavailable),
            }
        }
        Err(failure) => Err(failure.into_error(inner)),
    }
}

/// Parses `{"data": [...]}`, `{"models": [...]}` or a bare array (Together).
pub(crate) fn parse_model_list(value: &Value) -> Result<Vec<ModelInfo>, AiError> {
    let items = match value {
        Value::Array(items) => items,
        Value::Object(_) => value
            .get("data")
            .or_else(|| value.get("models"))
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::invalid_response("unexpected model list format"))?,
        _ => return Err(AiError::invalid_response("unexpected model list format")),
    };
    let mut seen = HashSet::new();
    Ok(items
        .iter()
        .filter_map(parse_model)
        .filter(|model| seen.insert(model.id.clone()))
        .collect())
}

fn parse_model(item: &Value) -> Option<ModelInfo> {
    let id = item
        .get("id")
        .or_else(|| item.get("name"))
        .and_then(Value::as_str)?
        .trim();
    if id.is_empty() || looks_non_chat(id) || !is_chat_capable(item) {
        return None;
    }
    let name = ["name", "display_name"]
        .iter()
        .filter_map(|key| item.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or(id);
    let context_length = [
        item.get("context_length"),
        item.get("context_window"),
        item.get("max_context_length"),
        item.get("max_model_len"),
        item.get("loaded_context_length"),
        item.get("max_input_tokens"),
        item.get("top_provider")
            .and_then(|p| p.get("context_length")),
    ]
    .into_iter()
    .flatten()
    .find_map(json_u32)
    .filter(|n| *n > 0);
    let (input_price, output_price) = item.get("pricing").map_or((None, None), parse_pricing);
    Some(ModelInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        context_length,
        input_price_per_mtok: input_price,
        output_price_per_mtok: output_price,
    })
}

/// Uses whatever capability metadata the server provides.
fn is_chat_capable(item: &Value) -> bool {
    if item.get("active") == Some(&Value::Bool(false)) {
        return false;
    }
    if item.get("supports_chat") == Some(&Value::Bool(false)) {
        return false;
    }
    if item
        .get("capabilities")
        .and_then(|caps| caps.get("completion_chat"))
        == Some(&Value::Bool(false))
    {
        return false;
    }
    if let Some(kind) = item.get("type").and_then(Value::as_str) {
        let kind = kind.to_lowercase();
        if [
            "embedding",
            "embeddings",
            "image",
            "audio",
            "rerank",
            "moderation",
            "transcribe",
            "tts",
            "stt",
            "video",
        ]
        .contains(&kind.as_str())
        {
            return false;
        }
    }
    if let Some(outputs) = item
        .get("architecture")
        .and_then(|arch| arch.get("output_modalities"))
        .and_then(Value::as_array)
    {
        return outputs.iter().any(|m| m.as_str() == Some("text"));
    }
    true
}

/// OpenRouter: `prompt`/`completion` in USD per token (strings; "-1" = variable).
/// Together: `input`/`output` in USD per million tokens.
fn parse_pricing(pricing: &Value) -> (Option<f64>, Option<f64>) {
    if pricing.get("prompt").is_some() || pricing.get("completion").is_some() {
        let per_mtok = |key: &str| {
            pricing
                .get(key)
                .and_then(json_price)
                .map(|p| round_price(p * 1_000_000.0))
        };
        (per_mtok("prompt"), per_mtok("completion"))
    } else {
        let price = |key: &str| pricing.get(key).and_then(json_price).map(round_price);
        (price("input"), price("output"))
    }
}

/// Parses Ollama's `GET /api/tags` response.
pub(crate) fn parse_ollama_tags(value: &Value) -> Result<Vec<ModelInfo>, AiError> {
    let items = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::invalid_response("unexpected model list format"))?;
    let mut seen = HashSet::new();
    Ok(items
        .iter()
        .filter_map(|item| {
            let id = item
                .get("model")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)?
                .trim();
            if id.is_empty() || looks_non_chat(id) {
                return None;
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(id);
            Some(ModelInfo {
                name: name.trim().to_owned(),
                ..ModelInfo::new(id)
            })
        })
        .filter(|model| seen.insert(model.id.clone()))
        .collect())
}
