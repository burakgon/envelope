//! Provider stream decoding, with fixtures split at every byte boundary.

mod common;

use common::fixtures::*;
use envelope_ai::{
    AiError, AiErrorKind, ChatEvent, FinishReason, ProviderKind, StreamDecoder, Usage,
};

type Item = Result<ChatEvent, AiError>;

fn decode(kind: ProviderKind, chunks: &[&[u8]]) -> Vec<Item> {
    let mut decoder = StreamDecoder::new(kind);
    let mut items = Vec::new();
    for chunk in chunks {
        items.extend(decoder.feed(chunk));
    }
    items.extend(decoder.finish());
    assert!(decoder.is_finished());
    items
}

/// Decodes `fixture` whole, split at every byte, in three pieces at many
/// positions, and byte by byte; all results must be identical.
fn decode_everywhere(kind: ProviderKind, fixture: &str) -> Vec<Item> {
    let bytes = fixture.as_bytes();
    let whole = decode(kind, &[bytes]);
    for split in 0..=bytes.len() {
        let (a, b) = bytes.split_at(split);
        assert_eq!(decode(kind, &[a, b]), whole, "split at byte {split}");
    }
    for first in (0..bytes.len()).step_by(11) {
        for second in (first..bytes.len()).step_by(17) {
            let pieces = [&bytes[..first], &bytes[first..second], &bytes[second..]];
            assert_eq!(
                decode(kind, &pieces),
                whole,
                "split at {first} and {second}"
            );
        }
    }
    let single: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(decode(kind, &single), whole, "byte by byte");
    whole
}

fn text(items: &[Item]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            Ok(ChatEvent::Delta(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn last(items: &[Item]) -> &Item {
    items.last().expect("at least one item")
}

fn done(finish_reason: FinishReason, input: u32, output: u32) -> Item {
    Ok(ChatEvent::Done {
        finish_reason,
        usage: Some(Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
        }),
    })
}

#[test]
fn openai_stream() {
    let items = decode_everywhere(ProviderKind::OpenAiCompatible, OPENAI_STREAM);
    assert_eq!(text(&items), OPENAI_TEXT);
    assert_eq!(last(&items), &done(FinishReason::Stop, 12, 5));
    let deltas = items
        .iter()
        .filter(|i| matches!(i, Ok(ChatEvent::Delta(_))))
        .count();
    assert_eq!(deltas, 3, "empty deltas are not emitted");
}

#[test]
fn openrouter_stream_with_comments_reasoning_and_usage_chunk() {
    let items = decode_everywhere(ProviderKind::OpenAiCompatible, OPENROUTER_STREAM);
    assert_eq!(text(&items), "Hello");
    assert_eq!(last(&items), &done(FinishReason::Stop, 10, 2));
    let terminal = items
        .iter()
        .filter(|i| !matches!(i, Ok(ChatEvent::Delta(_))))
        .count();
    assert_eq!(
        terminal, 1,
        "the repeated finish reason yields a single Done"
    );
}

#[test]
fn openrouter_mid_stream_error() {
    let items = decode_everywhere(ProviderKind::OpenAiCompatible, OPENROUTER_ERROR_STREAM);
    assert_eq!(items[0], Ok(ChatEvent::Delta("Partial".into())));
    assert_eq!(
        last(&items),
        &Err(AiError::Provider {
            status: None,
            message: "Provider disconnected unexpectedly".into()
        })
    );
    assert_eq!(items.len(), 2);
}

#[test]
fn openrouter_numeric_error_code_is_classified() {
    let stream = "data: {\"error\":{\"code\":502,\"message\":\"Upstream down\"},\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n";
    let items = decode_everywhere(ProviderKind::OpenAiCompatible, stream);
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], Err(e) if e.kind() == AiErrorKind::Overloaded));
}

#[test]
fn anthropic_stream_skips_thinking_and_unknown_events() {
    let items = decode_everywhere(ProviderKind::Anthropic, ANTHROPIC_STREAM);
    assert_eq!(text(&items), ANTHROPIC_TEXT);
    assert_eq!(last(&items), &done(FinishReason::Stop, 30, 15));
}

#[test]
fn anthropic_error_event() {
    let items = decode_everywhere(ProviderKind::Anthropic, ANTHROPIC_ERROR_STREAM);
    assert_eq!(items[0], Ok(ChatEvent::Delta("Partial".into())));
    assert_eq!(
        last(&items),
        &Err(AiError::Overloaded {
            message: "Overloaded".into()
        })
    );
}

#[test]
fn gemini_stream_with_crlf_and_thoughts() {
    let items = decode_everywhere(ProviderKind::Gemini, GEMINI_STREAM);
    assert_eq!(text(&items), GEMINI_TEXT);
    assert_eq!(last(&items), &done(FinishReason::Stop, 9, 10));
}

#[test]
fn gemini_blocked_prompt_and_safety_stop() {
    let items = decode_everywhere(ProviderKind::Gemini, GEMINI_BLOCKED_STREAM);
    assert_eq!(
        items,
        vec![Err(AiError::ContentBlocked {
            reason: "prohibited content".into()
        })]
    );
    let items = decode_everywhere(ProviderKind::Gemini, GEMINI_SAFETY_STREAM);
    assert_eq!(
        items,
        vec![Err(AiError::ContentBlocked {
            reason: "safety".into()
        })]
    );
}

#[test]
fn gemini_error_payload_in_stream() {
    let stream = "data: {\"error\": {\"code\": 503, \"message\": \"The model is overloaded.\", \"status\": \"UNAVAILABLE\"}}\r\n\r\n";
    let items = decode_everywhere(ProviderKind::Gemini, stream);
    assert!(matches!(&items[..], [Err(e)] if e.kind() == AiErrorKind::Overloaded));
}

#[test]
fn stream_without_done_marker_ends_at_eof() {
    let stream = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"length\"}]}\n\n";
    let items = decode_everywhere(ProviderKind::OpenAiCompatible, stream);
    assert_eq!(
        items,
        vec![
            Ok(ChatEvent::Delta("Hi".into())),
            Ok(ChatEvent::Done {
                finish_reason: FinishReason::Length,
                usage: None
            })
        ]
    );
}

#[test]
fn empty_and_malformed_streams_are_errors() {
    for kind in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ] {
        let items = decode(kind, &[b""]);
        assert!(
            matches!(&items[..], [Err(e)] if e.kind() == AiErrorKind::InvalidResponse),
            "{kind:?}: {items:?}"
        );
        let items = decode_everywhere(kind, ": only a comment\n\n");
        assert!(matches!(&items[..], [Err(e)] if e.kind() == AiErrorKind::InvalidResponse));
    }
    let items = decode_everywhere(ProviderKind::OpenAiCompatible, "data: {not json}\n\n");
    assert!(matches!(&items[..], [Err(e)] if e.kind() == AiErrorKind::InvalidResponse));
}

#[test]
fn nothing_is_emitted_after_the_terminal_item() {
    let mut decoder = StreamDecoder::new(ProviderKind::OpenAiCompatible);
    let mut items = decoder.feed(OPENAI_STREAM.as_bytes());
    assert!(decoder.is_finished());
    items.extend(decoder.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n"));
    items.extend(decoder.finish());
    assert_eq!(text(&items), OPENAI_TEXT);
    assert_eq!(
        items
            .iter()
            .filter(|i| matches!(i, Ok(ChatEvent::Done { .. })))
            .count(),
        1
    );
}
