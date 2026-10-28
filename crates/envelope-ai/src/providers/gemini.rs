//! Google Gemini API (`generativelanguage.googleapis.com/v1beta`).

use serde_json::{Map, Value, json};

use super::{
    BuiltRequest, Conversation, MAX_MODEL_PAGES, StreamShared, clamp_temperature, get_json,
    json_u32, looks_non_chat,
};
use crate::client::Inner;
use crate::error::AiError;
use crate::quirks::{Quirks, SentParams};
use crate::sse::SseEvent;
use crate::types::{ChatRequest, FinishReason, ModelInfo, Role};

/// Resource path of a model: `models/x` (the `models/` prefix is optional in
/// configs) or `tunedModels/x`.
pub(crate) fn model_path(model: &str) -> Result<String, AiError> {
    let path = if model.starts_with("models/") || model.starts_with("tunedModels/") {
        model.to_owned()
    } else {
        format!("models/{model}")
    };
    let valid = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '/'));
    if valid {
        Ok(path)
    } else {
        Err(AiError::InvalidConfig(format!(
            "\u{201c}{model}\u{201d} is not a valid Gemini model name"
        )))
    }
}

/// Builds a `POST {base}/models/{model}:streamGenerateContent?alt=sse` request.
pub(crate) fn build_chat(
    inner: &Inner,
    request: &ChatRequest,
    model: &str,
    quirks: &Quirks,
) -> Result<BuiltRequest, AiError> {
    let path = model_path(model)?;
    let conversation = Conversation::split(&request.messages, quirks.fold_system);
    conversation.require_turns()?;
    let mut sent = SentParams::default();
    let contents: Vec<Value> = conversation
        .turns
        .iter()
        .map(|(role, text)| {
            let role = if *role == Role::Assistant {
                "model"
            } else {
                "user"
            };
            json!({ "role": role, "parts": [{ "text": text }] })
        })
        .collect();
    let mut body = Map::new();
    body.insert("contents".into(), Value::Array(contents));
    if !conversation.system.is_empty() {
        body.insert(
            "systemInstruction".into(),
            json!({ "parts": [{ "text": conversation.system }] }),
        );
        sent.system = true;
    }

    let optional = !quirks.minimal;
    let mut generation = Map::new();
    if let Some(temperature) = request.temperature.and_then(|t| clamp_temperature(t, 2.0))
        && optional
        && !quirks.no_temperature
    {
        generation.insert("temperature".into(), Value::from(temperature));
        sent.temperature = true;
    }
    if let Some(max_tokens) = request.max_tokens {
        generation.insert("maxOutputTokens".into(), Value::from(max_tokens));
        sent.token_field = Some("maxOutputTokens");
    }
    if request.json && optional && !quirks.no_json_mode {
        generation.insert("responseMimeType".into(), Value::from("application/json"));
        sent.json_mode = true;
    }
    if let Some(effort) = request.effort
        && optional
        && !quirks.no_effort
    {
        generation.insert(
            "thinkingConfig".into(),
            json!({ "thinkingLevel": effort.as_str() }),
        );
        sent.effort = true;
    }
    if !generation.is_empty() {
        body.insert("generationConfig".into(), Value::Object(generation));
    }
    Ok(BuiltRequest {
        url: format!("{}/{path}:streamGenerateContent?alt=sse", inner.base_url),
        body: Value::Object(body),
        sent,
    })
}

/// Decoder for `data: {GenerateContentResponse}` events. The stream has no end
/// marker; it ends when the HTTP body ends.
#[derive(Debug, Default)]
pub(crate) struct GeminiStream;

impl GeminiStream {
    pub(crate) fn on_event(&mut self, shared: &mut StreamShared, event: &SseEvent) {
        if event.data.trim().is_empty() {
            return;
        }
        if let Some(value) = shared.parse_payload(&event.data) {
            self.on_json(shared, &value);
        }
    }

    /// Handles one `GenerateContentResponse` (or an array of them).
    pub(crate) fn on_json(&mut self, shared: &mut StreamShared, value: &Value) {
        if let Value::Array(chunks) = value {
            for chunk in chunks {
                self.on_json(shared, chunk);
            }
            return;
        }
        if value.get("error").is_some_and(|e| !e.is_null()) {
            shared.fail_with_payload(value);
            return;
        }
        let candidate = value
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first());
        if candidate.is_none()
            && let Some(reason) = value
                .get("promptFeedback")
                .and_then(|f| f.get("blockReason"))
                .and_then(Value::as_str)
        {
            shared.fail(AiError::ContentBlocked {
                reason: reason.to_lowercase().replace('_', " "),
            });
            return;
        }
        if let Some(candidate) = candidate {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(Value::as_array)
            {
                let text: String = parts
                    .iter()
                    .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect();
                shared.delta(&text);
            }
            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                shared.set_finish(map_finish(reason), reason);
            }
        }
        if let Some(usage) = value.get("usageMetadata") {
            let field = |key: &str| usage.get(key).and_then(json_u32);
            if let Some(input) = field("promptTokenCount") {
                shared.usage.input_tokens = Some(input);
            }
            let output = [field("candidatesTokenCount"), field("thoughtsTokenCount")];
            if output.iter().any(Option::is_some) {
                shared.usage.output_tokens = Some(
                    output
                        .iter()
                        .flatten()
                        .fold(0_u32, |sum, n| sum.saturating_add(*n)),
                );
            }
        }
    }
}

fn map_finish(reason: &str) -> FinishReason {
    match reason {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" | "CONTINUATION" => FinishReason::Length,
        "SAFETY"
        | "RECITATION"
        | "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT" => FinishReason::ContentFilter,
        "" | "FINISH_REASON_UNSPECIFIED" => FinishReason::Unknown,
        other => FinishReason::Other(other.to_lowercase()),
    }
}

/// Lists models that support `generateContent`, following `nextPageToken`.
pub(crate) async fn list_models(inner: &Inner) -> Result<Vec<ModelInfo>, AiError> {
    let url = format!("{}/models", inner.base_url);
    let mut models = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let mut query = vec![("pageSize", "1000".to_owned())];
        if let Some(token) = &token {
            query.push(("pageToken", token.clone()));
        }
        let page = get_json(inner, &url, &query)
            .await
            .map_err(|failure| failure.into_error(inner))?;
        models.extend(parse_model_page(&page)?);
        match page
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            Some(next) if token.as_deref() != Some(next) => token = Some(next.to_owned()),
            _ => break,
        }
    }
    Ok(models)
}

pub(crate) fn parse_model_page(page: &Value) -> Result<Vec<ModelInfo>, AiError> {
    let Some(items) = page.get("models") else {
        // An empty page may omit the field entirely.
        return if page.is_object() {
            Ok(Vec::new())
        } else {
            Err(AiError::invalid_response("unexpected model list format"))
        };
    };
    let items = items
        .as_array()
        .ok_or_else(|| AiError::invalid_response("unexpected model list format"))?;
    Ok(items
        .iter()
        .filter(|item| {
            item.get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_none_or(|methods| {
                    methods
                        .iter()
                        .any(|m| m.as_str() == Some("generateContent"))
                })
        })
        .filter_map(|item| {
            let name = item.get("name").and_then(Value::as_str)?.trim();
            let id = name.strip_prefix("models/").unwrap_or(name);
            if id.is_empty() || looks_non_chat(id) {
                return None;
            }
            let display = item
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(id);
            Some(ModelInfo {
                name: display.to_owned(),
                context_length: item
                    .get("inputTokenLimit")
                    .and_then(json_u32)
                    .filter(|n| *n > 0),
                ..ModelInfo::new(id)
            })
        })
        .collect())
}
