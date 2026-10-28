//! Incremental SSE parsing at arbitrary chunk boundaries.

use envelope_ai::{SseEvent, SseParser};

fn parse_chunks(chunks: &[&[u8]]) -> Vec<SseEvent> {
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(parser.feed(chunk));
    }
    events.extend(parser.finish());
    events
}

/// Every way of splitting `input` in two, plus byte-by-byte feeding, must give
/// the same events as feeding it whole.
fn assert_split_invariant(input: &[u8]) -> Vec<SseEvent> {
    let whole = parse_chunks(&[input]);
    for split in 0..=input.len() {
        let (a, b) = input.split_at(split);
        assert_eq!(parse_chunks(&[a, b]), whole, "split at byte {split}");
    }
    let bytes: Vec<&[u8]> = input.chunks(1).collect();
    assert_eq!(parse_chunks(&bytes), whole, "byte-by-byte");
    for step in [2, 3, 5, 7, 13] {
        let pieces: Vec<&[u8]> = input.chunks(step).collect();
        assert_eq!(parse_chunks(&pieces), whole, "chunks of {step}");
    }
    whole
}

fn data(events: &[SseEvent]) -> Vec<&str> {
    events.iter().map(|e| e.data.as_str()).collect()
}

#[test]
fn lf_crlf_and_cr_line_endings_are_equivalent() {
    let lf = b"event: a\ndata: one\n\ndata: two\n\n";
    let crlf = b"event: a\r\ndata: one\r\n\r\ndata: two\r\n\r\n";
    let cr = b"event: a\rdata: one\r\rdata: two\r\r";
    let expected = assert_split_invariant(lf);
    assert_eq!(assert_split_invariant(crlf), expected);
    assert_eq!(assert_split_invariant(cr), expected);
    assert_eq!(data(&expected), vec!["one", "two"]);
    assert_eq!(expected[0].event, "a");
    assert_eq!(expected[1].event, "message");
}

#[test]
fn multi_line_data_fields_are_joined_with_newlines() {
    let events = assert_split_invariant(b"data: {\"a\":\ndata: 1}\ndata:\n\n");
    assert_eq!(data(&events), vec!["{\"a\":\n1}\n"]);
}

#[test]
fn comments_and_keep_alives_are_ignored() {
    let input = b": OPENROUTER PROCESSING\n\n:ping\r\n\r\ndata: x\n: inline comment\ndata: y\n\n";
    let mut parser = SseParser::new();
    let events: Vec<SseEvent> = parser.feed(input);
    assert_eq!(data(&events), vec!["x\ny"]);
    assert_eq!(parser.comment_count(), 3);
    assert_eq!(data(&assert_split_invariant(input)), vec!["x\ny"]);
}

#[test]
fn done_marker_and_empty_data() {
    let events = assert_split_invariant(b"data: [DONE]\n\ndata:\n\n");
    assert_eq!(events.len(), 2);
    assert!(events[0].is_done_marker());
    assert_eq!(events[1].data, "");
    assert!(!events[1].is_done_marker());
}

#[test]
fn leading_bom_is_stripped_once() {
    let mut input = "\u{feff}data: a\n\n".as_bytes().to_vec();
    input.extend_from_slice("data: \u{feff}b\n\n".as_bytes());
    let events = assert_split_invariant(&input);
    assert_eq!(data(&events), vec!["a", "\u{feff}b"]);
}

#[test]
fn unterminated_final_event_is_flushed_on_finish() {
    let events = assert_split_invariant(b"data: first\n\ndata: last");
    assert_eq!(data(&events), vec!["first", "last"]);
}

#[test]
fn fields_follow_the_specification() {
    let events = assert_split_invariant(
        b"id: 42\nretry: 3000\nevent: update\nunknown: ignored\ndata:no-space\ndata:  two-spaces\n\n",
    );
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.event, "update");
    assert_eq!(event.id.as_deref(), Some("42"));
    assert_eq!(event.retry, Some(3000));
    assert_eq!(event.data, "no-space\n two-spaces");
}

#[test]
fn invalid_utf8_is_replaced_not_fatal() {
    let events = assert_split_invariant(b"data: ok \xff\xfe end\n\n");
    assert_eq!(events[0].data, "ok \u{fffd}\u{fffd} end");
}

#[test]
fn multibyte_characters_split_anywhere() {
    let text = "data: Görüşürüz — 你好 — 🙂🙂\n\n";
    let events = assert_split_invariant(text.as_bytes());
    assert_eq!(data(&events), vec!["Görüşürüz — 你好 — 🙂🙂"]);
}

#[test]
fn buffered_length_reports_partial_event() {
    let mut parser = SseParser::new();
    assert!(parser.feed(b"data: 12345").is_empty());
    assert_eq!(parser.buffered_len(), "data: 12345".len());
    assert_eq!(parser.feed(b"\n").len(), 0);
    assert_eq!(parser.buffered_len(), "12345\n".len());
}
