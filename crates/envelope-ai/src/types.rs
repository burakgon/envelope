//! Provider-independent request/response types.

use serde::{Deserialize, Serialize};

/// Who authored a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Instructions that frame the whole conversation.
    System,
    /// The human (or the application acting for them).
    User,
    /// The model.
    Assistant,
}

/// One message of a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Author of the message.
    pub role: Role,
    /// Plain-text content.
    pub content: String,
}

impl ChatMessage {
    /// Creates a message with the given role.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    /// Creates a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    /// Creates a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    /// Creates an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
}

/// How much hidden reasoning a reasoning-capable model should do.
///
/// This is a latency hint: `Low` makes reasoning models answer much faster. It is
/// mapped to each provider's native parameter and silently dropped for models
/// that do not support it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Minimal thinking: fastest answers.
    Low,
    /// Balanced.
    Medium,
    /// Thorough: slowest answers.
    High,
}

impl ReasoningEffort {
    /// The lowercase wire name used by every provider ("low", "medium", "high").
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A provider-independent chat completion request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    /// The conversation, typically a system message followed by user/assistant turns.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature. Clamped to the provider's range; dropped for models
    /// that only support their default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Upper bound on generated tokens. Providers that require a value (Anthropic)
    /// get a sensible default when this is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Ask for a JSON answer (`response_format: json_object` / `responseMimeType`)
    /// where the provider supports it. The prompt must still ask for JSON.
    #[serde(default)]
    pub json: bool,
    /// Overrides the configured model for this request only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning-effort hint for reasoning models (see [`ReasoningEffort`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
}

impl ChatRequest {
    /// Creates a request with default settings.
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            ..Self::default()
        }
    }

    /// Sets the sampling temperature.
    #[must_use]
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the maximum number of generated tokens.
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Enables or disables the JSON-output hint.
    #[must_use]
    pub fn json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    /// Uses a different model than the configured one for this request.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the reasoning-effort hint.
    #[must_use]
    pub fn effort(mut self, effort: ReasoningEffort) -> Self {
        self.effort = Some(effort);
        self
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural end of the answer (or a stop sequence).
    Stop,
    /// The token limit was reached; the answer is truncated.
    Length,
    /// A safety filter stopped generation.
    ContentFilter,
    /// The model wants to call a tool.
    ToolCalls,
    /// Another provider-specific reason (lowercased).
    Other(String),
    /// The provider did not say.
    Unknown,
}

/// Token accounting for one request, as reported by the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens, including cached ones.
    pub input_tokens: Option<u32>,
    /// Generated tokens, including hidden reasoning tokens where reported.
    pub output_tokens: Option<u32>,
}

impl Usage {
    /// Whether the provider reported anything at all.
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none() && self.output_tokens.is_none()
    }
}

/// One event of a streamed chat completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatEvent {
    /// The next piece of the answer text.
    Delta(String),
    /// The answer is complete. Always the last event of a successful stream.
    Done {
        /// Why generation stopped.
        finish_reason: FinishReason,
        /// Token usage, if the provider reported it.
        usage: Option<Usage>,
    },
}

/// A model offered by a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// The id to put into [`crate::ProviderConfig::model`].
    pub id: String,
    /// Human-readable name (falls back to the id).
    pub name: String,
    /// Context window in tokens, if known.
    pub context_length: Option<u32>,
    /// USD per million input tokens, if known.
    pub input_price_per_mtok: Option<f64>,
    /// USD per million output tokens, if known.
    pub output_price_per_mtok: Option<f64>,
}

impl ModelInfo {
    /// A model with only an id (the name defaults to the id).
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            context_length: None,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
        }
    }
}
