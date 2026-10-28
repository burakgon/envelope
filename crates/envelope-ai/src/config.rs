//! Provider configuration and the built-in preset catalog.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The wire protocol spoken by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    /// OpenAI Chat Completions (`/chat/completions`, `/models`). Covers OpenAI,
    /// OpenRouter, Groq, Mistral, DeepSeek, xAI, Together, Fireworks, Cerebras,
    /// Perplexity, LM Studio, Ollama (`/v1`), vLLM, LiteLLM and custom servers.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    /// Anthropic Messages API (`/messages`, `x-api-key`, `anthropic-version`).
    #[serde(rename = "anthropic")]
    Anthropic,
    /// Google Gemini API (`:streamGenerateContent?alt=sse`, `x-goog-api-key`).
    #[serde(rename = "gemini")]
    Gemini,
}

impl ProviderKind {
    /// Human-readable protocol name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
        }
    }
}

/// Everything needed to talk to one provider.
///
/// `Debug` output redacts the API key and extra header values.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Wire protocol.
    pub kind: ProviderKind,
    /// API root, e.g. `https://api.openai.com/v1`. A pasted endpoint URL such as
    /// `…/v1/chat/completions` is accepted and trimmed.
    pub base_url: String,
    /// API key; `None` (or empty) for local servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model id as returned by [`crate::AiClient::list_models`].
    #[serde(default)]
    pub model: String,
    /// Additional HTTP headers sent with every request (they override built-in ones).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
}

impl ProviderConfig {
    /// Creates a configuration.
    pub fn new(
        kind: ProviderKind,
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            extra_headers: BTreeMap::new(),
        }
    }

    /// Creates a configuration from a preset id (see [`presets`]).
    pub fn from_preset(preset_id: &str, api_key: Option<String>) -> Option<Self> {
        preset(preset_id).map(|p| p.config(api_key))
    }

    /// Returns the configuration with another model.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Returns the configuration with an additional HTTP header.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.insert(name.into(), value.into());
        self
    }

    /// The API key with surrounding whitespace removed, if one is set.
    pub(crate) fn key(&self) -> Option<&str> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
    }
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<&str> = self.extra_headers.keys().map(String::as_str).collect();
        f.debug_struct("ProviderConfig")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("api_key", &self.key().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("extra_headers", &headers)
            .finish()
    }
}

/// A ready-made provider entry for the settings UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Preset {
    /// Stable identifier, e.g. `"openrouter"`.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Wire protocol.
    pub kind: ProviderKind,
    /// Default API root (empty for "custom").
    pub base_url: &'static str,
    /// Whether the provider requires an API key.
    pub requires_key: bool,
    /// Where users create an API key.
    pub key_url: Option<&'static str>,
    /// A stable, generic model id to preselect. Only set where such an id exists;
    /// otherwise the user picks from [`crate::AiClient::list_models`].
    pub default_model: Option<&'static str>,
    /// Runs on the user's machine or network: mail never leaves it.
    pub local: bool,
}

impl Preset {
    /// Builds a [`ProviderConfig`] for this preset.
    pub fn config(&self, api_key: Option<String>) -> ProviderConfig {
        ProviderConfig::new(
            self.kind,
            self.base_url,
            api_key,
            self.default_model.unwrap_or_default(),
        )
    }
}

const fn cloud(
    id: &'static str,
    name: &'static str,
    kind: ProviderKind,
    base_url: &'static str,
    key_url: &'static str,
    default_model: Option<&'static str>,
) -> Preset {
    Preset {
        id,
        name,
        kind,
        base_url,
        requires_key: true,
        key_url: Some(key_url),
        default_model,
        local: false,
    }
}

const fn local(id: &'static str, name: &'static str, base_url: &'static str) -> Preset {
    Preset {
        id,
        name,
        kind: ProviderKind::OpenAiCompatible,
        base_url,
        requires_key: false,
        key_url: None,
        default_model: None,
        local: true,
    }
}

static PRESETS: [Preset; 17] = [
    cloud(
        "openai",
        "OpenAI",
        ProviderKind::OpenAiCompatible,
        "https://api.openai.com/v1",
        "https://platform.openai.com/api-keys",
        None,
    ),
    cloud(
        "anthropic",
        "Anthropic",
        ProviderKind::Anthropic,
        "https://api.anthropic.com/v1",
        "https://console.anthropic.com/settings/keys",
        None,
    ),
    cloud(
        "gemini",
        "Google Gemini",
        ProviderKind::Gemini,
        "https://generativelanguage.googleapis.com/v1beta",
        "https://aistudio.google.com/app/apikey",
        None,
    ),
    cloud(
        "openrouter",
        "OpenRouter",
        ProviderKind::OpenAiCompatible,
        "https://openrouter.ai/api/v1",
        "https://openrouter.ai/settings/keys",
        Some("openrouter/auto"),
    ),
    cloud(
        "groq",
        "Groq",
        ProviderKind::OpenAiCompatible,
        "https://api.groq.com/openai/v1",
        "https://console.groq.com/keys",
        None,
    ),
    cloud(
        "mistral",
        "Mistral AI",
        ProviderKind::OpenAiCompatible,
        "https://api.mistral.ai/v1",
        "https://console.mistral.ai/api-keys",
        None,
    ),
    cloud(
        "deepseek",
        "DeepSeek",
        ProviderKind::OpenAiCompatible,
        "https://api.deepseek.com/v1",
        "https://platform.deepseek.com/api_keys",
        None,
    ),
    cloud(
        "xai",
        "xAI",
        ProviderKind::OpenAiCompatible,
        "https://api.x.ai/v1",
        "https://console.x.ai",
        None,
    ),
    cloud(
        "together",
        "Together AI",
        ProviderKind::OpenAiCompatible,
        "https://api.together.xyz/v1",
        "https://api.together.ai/settings/api-keys",
        None,
    ),
    cloud(
        "fireworks",
        "Fireworks AI",
        ProviderKind::OpenAiCompatible,
        "https://api.fireworks.ai/inference/v1",
        "https://app.fireworks.ai/account/api-keys",
        None,
    ),
    cloud(
        "cerebras",
        "Cerebras",
        ProviderKind::OpenAiCompatible,
        "https://api.cerebras.ai/v1",
        "https://cloud.cerebras.ai",
        None,
    ),
    // Perplexity has no model-list endpoint for chat completions, and "sonar" is
    // its stable, generic model id.
    cloud(
        "perplexity",
        "Perplexity",
        ProviderKind::OpenAiCompatible,
        "https://api.perplexity.ai",
        "https://www.perplexity.ai/account/api/keys",
        Some("sonar"),
    ),
    local("ollama", "Ollama", "http://localhost:11434/v1"),
    local("lmstudio", "LM Studio", "http://localhost:1234/v1"),
    local("vllm", "vLLM", "http://localhost:8000/v1"),
    local("litellm", "LiteLLM proxy", "http://localhost:4000/v1"),
    Preset {
        id: "custom",
        name: "Custom (OpenAI-compatible)",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "",
        requires_key: false,
        key_url: None,
        default_model: None,
        local: false,
    },
];

/// The built-in provider catalog, in display order.
pub fn presets() -> &'static [Preset] {
    &PRESETS
}

/// Looks up a preset by id.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_are_unique_and_sane() {
        let mut ids: Vec<&str> = presets().iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets().len());
        for p in presets() {
            if p.id != "custom" {
                assert!(p.base_url.starts_with("http"), "{}", p.id);
            }
            assert_eq!(p.requires_key, p.key_url.is_some(), "{}", p.id);
            assert!(!(p.local && p.requires_key), "{}", p.id);
        }
    }

    #[test]
    fn default_models_only_where_generic() {
        let with_default: Vec<(&str, &str)> = presets()
            .iter()
            .filter_map(|p| p.default_model.map(|m| (p.id, m)))
            .collect();
        assert_eq!(
            with_default,
            vec![("openrouter", "openrouter/auto"), ("perplexity", "sonar")]
        );
        assert!(
            presets()
                .iter()
                .filter(|p| p.local)
                .all(|p| p.default_model.is_none())
        );
    }

    #[test]
    fn debug_redacts_secrets() {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            "https://example.com/v1",
            Some("sk-secret-value".into()),
            "test-model",
        )
        .with_header("X-Proxy-Token", "proxy-secret");
        let printed = format!("{config:?}");
        assert!(!printed.contains("sk-secret-value"));
        assert!(!printed.contains("proxy-secret"));
        assert!(printed.contains("X-Proxy-Token"));
        assert!(printed.contains("<redacted>"));
    }

    #[test]
    fn config_roundtrips_through_json() {
        let config = ProviderConfig::from_preset("openrouter", Some("k".into()));
        let config = config.map(|c| c.with_header("X-Test", "1"));
        let json = serde_json::to_string(&config).unwrap_or_default();
        assert!(json.contains("\"kind\":\"openai_compatible\""));
        let back: Option<ProviderConfig> = serde_json::from_str(&json).ok().flatten();
        assert_eq!(back, config);
    }
}
