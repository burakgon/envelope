//! Incremental Server-Sent Events parser.
//!
//! Implements the WHATWG "event stream interpretation" algorithm on raw bytes,
//! so it can be fed network chunks split at arbitrary positions — including in
//! the middle of a CRLF pair or a multi-byte UTF-8 character.

/// One dispatched server-sent event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` field; `"message"` when the event did not name a type.
    pub event: String,
    /// All `data:` lines of the event joined with `\n`.
    pub data: String,
    /// The `id:` field of this event, if present.
    pub id: Option<String>,
    /// The `retry:` field of this event, if present and valid.
    pub retry: Option<u64>,
}

impl SseEvent {
    /// Whether this is the OpenAI-style `data: [DONE]` end-of-stream marker.
    pub fn is_done_marker(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// Incremental SSE parser.
///
/// Handles LF, CRLF and lone CR line endings, multi-line `data:` fields,
/// comments (`: keep-alive`, `: OPENROUTER PROCESSING`), a leading UTF-8 BOM, and
/// events split across arbitrary chunk boundaries. Invalid UTF-8 is replaced
/// with U+FFFD rather than aborting the stream.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes of the current, not yet terminated line.
    line: Vec<u8>,
    /// The previous chunk ended with `\r`; a leading `\n` in the next chunk
    /// belongs to the same line terminator.
    pending_cr: bool,
    /// Whether the first line (which may start with a BOM) was processed.
    started: bool,
    event_type: String,
    data: String,
    id: Option<String>,
    retry: Option<u64>,
    comments: u64,
}

impl SseParser {
    /// Creates an empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next chunk of bytes and returns every event it completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mut rest = chunk;
        if self.pending_cr && !rest.is_empty() {
            self.pending_cr = false;
            if rest[0] == b'\n' {
                rest = &rest[1..];
            }
        }
        while let Some(pos) = rest.iter().position(|&b| b == b'\n' || b == b'\r') {
            self.line.extend_from_slice(&rest[..pos]);
            self.process_line(&mut events);
            let terminator = rest[pos];
            rest = &rest[pos + 1..];
            if terminator == b'\r' {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    Some(_) => {}
                    None => self.pending_cr = true,
                }
            }
        }
        self.line.extend_from_slice(rest);
        events
    }

    /// Signals the end of the stream and returns the last event if the stream
    /// did not end with a blank line.
    ///
    /// The SSE specification discards such an unterminated event; this parser
    /// dispatches it instead, because some servers omit the final blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.process_line(&mut events);
        }
        self.pending_cr = false;
        self.dispatch(&mut events);
        events.pop()
    }

    /// Number of bytes buffered for the event currently being received. Lets
    /// callers bound memory use against misbehaving servers.
    pub fn buffered_len(&self) -> usize {
        self.line.len() + self.data.len()
    }

    /// Number of comment lines seen so far (keep-alives, "processing" notices).
    pub fn comment_count(&self) -> u64 {
        self.comments
    }

    fn process_line(&mut self, events: &mut Vec<SseEvent>) {
        let raw = std::mem::take(&mut self.line);
        let mut text = String::from_utf8_lossy(&raw);
        if !self.started {
            self.started = true;
            if let Some(stripped) = text.strip_prefix('\u{feff}') {
                text = stripped.to_owned().into();
            }
        }
        if text.is_empty() {
            self.dispatch(events);
            return;
        }
        if text.starts_with(':') {
            self.comments += 1;
            return;
        }
        let (field, value) = match text.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (text.as_ref(), ""),
        };
        match field {
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            "event" => self.event_type = value.to_owned(),
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry = value.parse().ok();
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        let event_type = std::mem::take(&mut self.event_type);
        let id = self.id.take();
        let retry = self.retry.take();
        if self.data.is_empty() {
            return;
        }
        let mut data = std::mem::take(&mut self.data);
        if data.ends_with('\n') {
            data.pop();
        }
        events.push(SseEvent {
            event: if event_type.is_empty() {
                "message".to_owned()
            } else {
                event_type
            },
            data,
            id,
            retry,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(parser.feed(chunk));
        }
        out.extend(parser.finish());
        out
    }

    #[test]
    fn basic_event() {
        let events = parse_all(&[b"event: ping\ndata: {\"a\":1}\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
        assert_eq!(events[0].data, "{\"a\":1}");
    }

    #[test]
    fn data_without_space_and_field_without_colon() {
        let events = parse_all(&[b"data:x\ndata\n\n"]);
        assert_eq!(events[0].data, "x\n");
    }

    #[test]
    fn empty_data_buffer_does_not_dispatch() {
        let events = parse_all(&[b"event: nothing\n\nid: 5\n\n"]);
        assert!(events.is_empty());
    }

    #[test]
    fn lone_cr_split_before_lf() {
        let events = parse_all(&[b"data: a\r", b"\ndata: b\r", b"\r", b"\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "a\nb");
    }

    #[test]
    fn utf8_split_inside_character() {
        let bytes = "data: ğüşİ 漢字 🚀\n\n".as_bytes();
        for split in 0..bytes.len() {
            let events = parse_all(&[&bytes[..split], &bytes[split..]]);
            assert_eq!(events.len(), 1, "split at {split}");
            assert_eq!(events[0].data, "ğüşİ 漢字 🚀");
        }
    }

    #[test]
    fn retry_and_id_fields() {
        let events = parse_all(&[b"id: 7\nretry: 1500\nretry: x\ndata: y\n\n"]);
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].retry, Some(1500));
    }
}
