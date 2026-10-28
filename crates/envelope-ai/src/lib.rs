//! Provider-agnostic LLM client and mail-assistant prompt library for Envelope.
//!
//! The crate has two halves:
//!
//! * **Provider layer** — [`AiClient`] talks to any OpenAI-compatible endpoint
//!   (OpenAI, OpenRouter, Groq, Mistral, DeepSeek, xAI, Together, Fireworks,
//!   Cerebras, Perplexity, LM Studio, Ollama, vLLM, LiteLLM, custom URLs), the
//!   Anthropic Messages API and the Google Gemini API. Responses are streamed as
//!   [`ChatEvent`]s; dropping the stream cancels the HTTP request. Model lists
//!   are always fetched from the provider.
//! * **Prompt library** — [`prompts`] builds carefully engineered,
//!   injection-hardened prompts for mail tasks (summaries, replies, triage,
//!   event extraction, search, …) and parses their structured answers.
//!
//! Privacy: the crate never logs prompt contents, model output or API keys.
//!
//! ```no_run
//! use envelope_ai::{AiClient, ChatEvent, ChatMessage, ChatRequest, ProviderConfig};
//! use futures::StreamExt;
//!
//! # async fn demo() -> Result<(), envelope_ai::AiError> {
//! let Some(config) = ProviderConfig::from_preset("openrouter", Some("my-key".into())) else {
//!     return Ok(());
//! };
//! let client = AiClient::new(config)?;
//! let request = ChatRequest::new(vec![ChatMessage::user("Hello!")]);
//! let mut stream = client.chat_stream(request);
//! while let Some(event) = stream.next().await {
//!     match event? {
//!         ChatEvent::Delta(text) => print!("{text}"),
//!         ChatEvent::Done { .. } => break,
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod client;
mod config;
mod decode;
mod error;
mod json;
pub mod prompts;
mod providers;
mod quirks;
mod sse;
mod types;

pub use client::{AiClient, ClientOptions, ConnectionReport};
pub use config::{Preset, ProviderConfig, ProviderKind, preset, presets};
pub use decode::StreamDecoder;
pub use error::{AiError, AiErrorKind};
pub use json::extract_json;
pub use sse::{SseEvent, SseParser};
pub use types::{
    ChatEvent, ChatMessage, ChatRequest, FinishReason, ModelInfo, ReasoningEffort, Role, Usage,
};
