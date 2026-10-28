//! Sans-IO decoding of provider streams into [`ChatEvent`]s.

use serde_json::Value;

use crate::config::ProviderKind;
use crate::error::AiError;
use crate::providers::StreamShared;
use crate::providers::anthropic::AnthropicStream;
use crate::providers::gemini::GeminiStream;
use crate::providers::openai::OpenAiStream;
use crate::sse::{SseEvent, SseParser};
use crate::types::ChatEvent;

/// Largest single event the decoder buffers before giving up on the stream.
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
enum ProviderStream {
    OpenAi(OpenAiStream),
    Anthropic(AnthropicStream),
    Gemini(GeminiStream),
}

/// Turns the raw bytes of a provider's streaming response into chat events.
///
/// The decoder does no I/O: feed it body chunks exactly as they arrive (they may
/// be split anywhere, even inside a UTF-8 character) and forward the returned
/// items. A stream always ends with exactly one terminal item — either
/// [`ChatEvent::Done`] or an error — after which further input is ignored.
///
/// In-stream errors (Anthropic `error` events, OpenRouter error chunks, Gemini
/// error payloads) are surfaced as classified [`AiError`]s.
#[derive(Debug)]
pub struct StreamDecoder {
    parser: SseParser,
    provider: ProviderStream,
    shared: StreamShared,
}

impl StreamDecoder {
    /// Creates a decoder for the given wire protocol.
    pub fn new(kind: ProviderKind) -> Self {
        Self::with_model(kind, "")
    }

    /// Creates a decoder that names `model` in model-related errors.
    pub(crate) fn with_model(kind: ProviderKind, model: &str) -> Self {
        let provider = match kind {
            ProviderKind::OpenAiCompatible => ProviderStream::OpenAi(OpenAiStream),
            ProviderKind::Anthropic => ProviderStream::Anthropic(AnthropicStream),
            ProviderKind::Gemini => ProviderStream::Gemini(GeminiStream),
        };
        Self {
            parser: SseParser::new(),
            provider,
            shared: StreamShared {
                model: model.to_owned(),
                ..StreamShared::default()
            },
        }
    }

    /// Feeds the next chunk of the response body.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Result<ChatEvent, AiError>> {
        if self.shared.finished {
            return Vec::new();
        }
        for event in self.parser.feed(chunk) {
            self.dispatch(&event);
            if self.shared.finished {
                break;
            }
        }
        if !self.shared.finished && self.parser.buffered_len() > MAX_EVENT_BYTES {
            self.shared.fail(AiError::invalid_response(
                "a stream event exceeded the size limit",
            ));
        }
        std::mem::take(&mut self.shared.out)
    }

    /// Signals the end of the response body. Emits the terminal item if the
    /// provider did not send an explicit end marker.
    pub fn finish(&mut self) -> Vec<Result<ChatEvent, AiError>> {
        if !self.shared.finished {
            if let Some(event) = self.parser.finish() {
                self.dispatch(&event);
            }
            self.shared.eof();
        }
        std::mem::take(&mut self.shared.out)
    }

    /// Whether the terminal item has been produced.
    pub fn is_finished(&self) -> bool {
        self.shared.finished
    }

    /// Decodes a complete (non-streaming) JSON response, for servers that ignore
    /// the streaming flag.
    pub(crate) fn decode_json(&mut self, value: &Value) -> Vec<Result<ChatEvent, AiError>> {
        match &mut self.provider {
            ProviderStream::OpenAi(stream) => stream.on_json(&mut self.shared, value),
            ProviderStream::Anthropic(stream) => stream.on_message_json(&mut self.shared, value),
            ProviderStream::Gemini(stream) => stream.on_json(&mut self.shared, value),
        }
        self.shared.eof();
        std::mem::take(&mut self.shared.out)
    }

    fn dispatch(&mut self, event: &SseEvent) {
        match &mut self.provider {
            ProviderStream::OpenAi(stream) => stream.on_event(&mut self.shared, event),
            ProviderStream::Anthropic(stream) => stream.on_event(&mut self.shared, event),
            ProviderStream::Gemini(stream) => stream.on_event(&mut self.shared, event),
        }
    }
}
