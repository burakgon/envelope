//! Tolerant content-line parser (RFC 5545 §3.1, RFC 6350 §3.3) producing a
//! [`Component`] tree.
//!
//! The parser never fails: malformed lines are either recovered (unfolded
//! text that lost its leading space, unquoted `mailto:` parameter values) or
//! skipped, missing `END` lines are closed implicitly and stray `END` lines
//! are ignored. Several top-level objects (multiple `VCALENDAR`s or `VCARD`s
//! in one stream) are returned in order.

use crate::charset;
use crate::component::{Component, Param, Property};

/// Parser switches. vCard 2.1 needs quoted-printable soft line breaks.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ParseOptions {
    /// Join lines ending in `=` for `ENCODING=QUOTED-PRINTABLE` properties.
    pub quoted_printable: bool,
}

/// Parses iCalendar or vCard text into its top-level components.
///
/// ```
/// let components = envelope_ical::parse_components(
///     "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
/// );
/// assert_eq!(components.len(), 1);
/// assert_eq!(components[0].components[0].name, "VEVENT");
/// ```
pub fn parse_components(input: &str) -> Vec<Component> {
    parse_with(input, ParseOptions::default())
}

/// Like [`parse_components`] but starts from raw bytes, which is the most
/// robust entry point for data received over the network: folding is undone
/// at the byte level (so producers that fold inside a multi-byte UTF-8
/// sequence are handled), and lines that are not valid UTF-8 are decoded as
/// Windows-1252.
pub fn parse_components_bytes(input: &[u8]) -> Vec<Component> {
    parse_with(&decode_bytes(input), ParseOptions::default())
}

/// Byte-level unfolding followed by per-line charset detection.
pub(crate) fn decode_bytes(input: &[u8]) -> String {
    let input = input.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(input);
    let mut unfolded = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let byte = input[i];
        let fold_len = match (byte, input.get(i + 1), input.get(i + 2)) {
            (b'\r', Some(b'\n'), Some(b' ' | b'\t')) => 3,
            (b'\n', Some(b' ' | b'\t'), _) => 2,
            _ => 0,
        };
        if fold_len > 0 {
            i += fold_len;
            continue;
        }
        unfolded.push(byte);
        i += 1;
    }
    match String::from_utf8(unfolded) {
        Ok(text) => text,
        Err(err) => {
            let bytes = err.into_bytes();
            let mut out = String::with_capacity(bytes.len());
            for (index, line) in bytes.split(|&b| b == b'\n').enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(&charset::decode_utf8_or_cp1252(line));
            }
            out
        }
    }
}

pub(crate) fn parse_with(input: &str, options: ParseOptions) -> Vec<Component> {
    let mut builder = TreeBuilder::default();
    for line in logical_lines(input, options) {
        builder.line(&line);
    }
    builder.finish()
}

/// Splits input into unfolded logical lines.
pub(crate) fn logical_lines(input: &str, options: ParseOptions) -> Vec<String> {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let physical: Vec<&str> = if !input.contains('\n') && input.contains('\r') {
        input.split('\r').collect()
    } else {
        input
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect()
    };

    let mut lines: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for raw in physical {
        if let Some(cur) = current.as_mut() {
            if options.quoted_printable && cur.ends_with('=') && is_quoted_printable(cur) {
                // Soft line break: drop the '=' and join verbatim.
                cur.pop();
                cur.push_str(raw);
                continue;
            }
            if let Some(rest) = raw.strip_prefix([' ', '\t']) {
                cur.push_str(rest);
                continue;
            }
        }
        if let Some(done) = current.take() {
            lines.push(done);
        }
        if raw.trim().is_empty() {
            continue;
        }
        current = Some(raw.to_owned());
    }
    if let Some(done) = current {
        lines.push(done);
    }
    lines
}

fn is_quoted_printable(line: &str) -> bool {
    let head = line.split(':').next().unwrap_or("");
    head.to_ascii_uppercase().contains("QUOTED-PRINTABLE")
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')
}

/// Parses one unfolded content line. Returns `None` for lines that are not
/// syntactically a property.
pub(crate) fn parse_content_line(line: &str) -> Option<Property> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b';' && bytes[i] != b':' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let full_name = line[..i].trim();
    if full_name.is_empty() || !full_name.bytes().all(is_name_byte) {
        return None;
    }
    let (group, name) = match full_name.rfind('.') {
        Some(dot) => (
            Some(full_name[..dot].to_owned()).filter(|g| !g.is_empty()),
            &full_name[dot + 1..],
        ),
        None => (None, full_name),
    };
    if name.is_empty() {
        return None;
    }

    let mut params = Vec::new();
    while i < bytes.len() && bytes[i] == b';' {
        i += 1;
        let start = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b';' | b':') {
            i += 1;
        }
        let param_name = line[start..i].trim().to_ascii_uppercase();
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'=' {
            i += 1;
            let mut values = Vec::new();
            loop {
                let (value, next) = parse_param_value(line, i);
                values.push(value);
                i = next;
                if i < bytes.len() && bytes[i] == b',' {
                    i += 1;
                    continue;
                }
                break;
            }
            if !param_name.is_empty() {
                params.push(Param {
                    name: param_name,
                    values,
                });
            }
        } else if !param_name.is_empty() {
            params.push(Param {
                name: param_name,
                values: Vec::new(),
            });
        }
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    Some(Property {
        group,
        name: name.to_ascii_uppercase(),
        params,
        value: line[i + 1..].to_owned(),
    })
}

/// Parses one parameter value starting at byte `start`; returns the decoded
/// value and the index of the delimiter that ended it.
fn parse_param_value(line: &str, start: usize) -> (String, usize) {
    let bytes = line.as_bytes();
    let mut i = start;
    if i < bytes.len() && bytes[i] == b'"' {
        if let Some(rel) = line[i + 1..].find('"') {
            let value = &line[i + 1..i + 1 + rel];
            i += rel + 2;
            // Skip garbage between a closing quote and the next delimiter.
            while i < bytes.len() && !matches!(bytes[i], b',' | b';' | b':') {
                i += 1;
            }
            return (decode_caret(value), i);
        }
        // Unterminated quote: treat the rest as an unquoted value.
        i += 1;
    }
    let value_start = i;
    while i < bytes.len() && !matches!(bytes[i], b',' | b';' | b':') {
        i += 1;
    }
    // Real-world producers sometimes leave `mailto:` URIs unquoted
    // (`SENT-BY=mailto:a@b.c`). Keep the scheme and address together.
    if i < bytes.len()
        && bytes[i] == b':'
        && line[value_start..i].eq_ignore_ascii_case("mailto")
        && line[i + 1..].contains(':')
    {
        i += 1;
        while i < bytes.len() && !matches!(bytes[i], b',' | b';' | b':') {
            i += 1;
        }
    }
    (decode_caret(&line[value_start..i]), i)
}

/// RFC 6868 caret decoding: `^n` → newline, `^^` → `^`, `^'` → `"`.
fn decode_caret(value: &str) -> String {
    if !value.contains('^') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '^' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('n' | 'N') => {
                chars.next();
                out.push('\n');
            }
            Some('^') => {
                chars.next();
                out.push('^');
            }
            Some('\'') => {
                chars.next();
                out.push('"');
            }
            _ => out.push('^'),
        }
    }
    out
}

/// Components that can only live inside one of the listed parents. Used to
/// recover from missing `END` lines.
fn allowed_parents(name: &str) -> &'static [&'static str] {
    match name {
        "VEVENT" | "VTODO" | "VJOURNAL" | "VFREEBUSY" | "VTIMEZONE" | "VAVAILABILITY" => {
            &["VCALENDAR"]
        }
        "STANDARD" | "DAYLIGHT" => &["VTIMEZONE"],
        "VALARM" => &["VEVENT", "VTODO"],
        "AVAILABLE" => &["VAVAILABILITY"],
        _ => &[],
    }
}

#[derive(Default)]
struct TreeBuilder {
    stack: Vec<Component>,
    roots: Vec<Component>,
}

impl TreeBuilder {
    fn line(&mut self, line: &str) {
        let Some(property) = parse_content_line(line) else {
            self.continuation(line);
            return;
        };
        match property.name.as_str() {
            "BEGIN" => {
                let name = property.value.trim().to_ascii_uppercase();
                if name.is_empty() {
                    return;
                }
                self.prepare_begin(&name);
                self.stack.push(Component::new(name));
            }
            "END" => {
                let name = property.value.trim().to_ascii_uppercase();
                if let Some(position) = self.stack.iter().rposition(|c| c.name == name) {
                    while self.stack.len() > position {
                        self.close_top();
                    }
                }
            }
            _ => {
                if let Some(top) = self.stack.last_mut() {
                    top.properties.push(property);
                }
            }
        }
    }

    /// Closes components implicitly when a `BEGIN` cannot nest where we are.
    fn prepare_begin(&mut self, name: &str) {
        if name == "VCALENDAR" || name == "VCARD" {
            while !self.stack.is_empty() {
                self.close_top();
            }
            return;
        }
        let parents = allowed_parents(name);
        if parents.is_empty() {
            return;
        }
        if let Some(position) = self
            .stack
            .iter()
            .rposition(|c| parents.contains(&c.name.as_str()))
        {
            while self.stack.len() > position + 1 {
                self.close_top();
            }
        }
    }

    fn close_top(&mut self) {
        if let Some(done) = self.stack.pop() {
            match self.stack.last_mut() {
                Some(parent) => parent.components.push(done),
                None => self.roots.push(done),
            }
        }
    }

    /// A line that is not a property: most likely text that lost its folding
    /// whitespace. Append it to the previous property.
    fn continuation(&mut self, line: &str) {
        let Some(previous) = self
            .stack
            .last_mut()
            .and_then(|top| top.properties.last_mut())
        else {
            return;
        };
        let binary = previous
            .param_values("ENCODING")
            .iter()
            .any(|e| e.eq_ignore_ascii_case("b") || e.eq_ignore_ascii_case("base64"));
        if binary {
            previous.value.push_str(line.trim());
        } else {
            previous.value.push_str("\\n");
            previous.value.push_str(line);
        }
    }

    fn finish(mut self) -> Vec<Component> {
        while !self.stack.is_empty() {
            self.close_top();
        }
        self.roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_crlf_lf_and_tabs() {
        let text = "BEGIN:VEVENT\r\nSUMMARY:Hello\r\n  World\r\nDESCRIPTION:a\n\tb\nEND:VEVENT\n";
        let c = parse_components(text);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].value("SUMMARY"), Some("Hello World"));
        assert_eq!(c[0].value("DESCRIPTION"), Some("ab"));
    }

    #[test]
    fn strips_bom_and_handles_lowercase() {
        let text = "\u{FEFF}begin:vcalendar\r\nversion:2.0\r\nbegin:vevent\r\nuid:x\r\nend:vevent\r\nend:vcalendar\r\n";
        let c = parse_components(text);
        assert_eq!(c[0].name, "VCALENDAR");
        assert_eq!(c[0].value("VERSION"), Some("2.0"));
        assert_eq!(c[0].components[0].value("UID"), Some("x"));
    }

    #[test]
    fn parses_quoted_and_multi_valued_params() {
        let p = parse_content_line(
            "ATTENDEE;CN=\"Doe; John: Jr.\";MEMBER=\"mailto:a@x.com\",\"mailto:b@x.com\";ROLE=REQ-PARTICIPANT:mailto:john@x.com",
        )
        .expect("valid line");
        assert_eq!(p.param("CN"), Some("Doe; John: Jr."));
        assert_eq!(
            p.param_values("MEMBER"),
            vec!["mailto:a@x.com", "mailto:b@x.com"]
        );
        assert_eq!(p.param("ROLE"), Some("REQ-PARTICIPANT"));
        assert_eq!(p.value, "mailto:john@x.com");
    }

    #[test]
    fn unquoted_mailto_param_is_recovered() {
        let p = parse_content_line("ORGANIZER;SENT-BY=mailto:boss@x.com:mailto:me@x.com")
            .expect("valid line");
        assert_eq!(p.param("SENT-BY"), Some("mailto:boss@x.com"));
        assert_eq!(p.value, "mailto:me@x.com");
    }

    #[test]
    fn caret_decoding() {
        let p = parse_content_line(
            "X-APPLE-STRUCTURED-LOCATION;X-ADDRESS=\"1 Main St^nTown\";X-TITLE=^'Home^':geo:1,2",
        )
        .expect("valid line");
        assert_eq!(p.param("X-ADDRESS"), Some("1 Main St\nTown"));
        assert_eq!(p.param("X-TITLE"), Some("\"Home\""));
    }

    #[test]
    fn groups_and_bare_params() {
        let p = parse_content_line("item1.TEL;WORK;VOICE:+1").expect("valid line");
        assert_eq!(p.group.as_deref(), Some("item1"));
        assert_eq!(p.name, "TEL");
        assert_eq!(p.params.len(), 2);
        assert!(p.params.iter().all(|param| param.values.is_empty()));
    }

    #[test]
    fn multiple_calendars_and_missing_end() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:1\nBEGIN:VALARM\nACTION:DISPLAY\nBEGIN:VEVENT\nUID:2\nEND:VCALENDAR\nBEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:3\n";
        let c = parse_components(text);
        assert_eq!(c.len(), 2);
        let first: Vec<_> = c[0].components.iter().map(|e| e.value("UID")).collect();
        assert_eq!(first, vec![Some("1"), Some("2")]);
        assert_eq!(c[0].components[0].components[0].name, "VALARM");
        assert_eq!(c[1].components[0].value("UID"), Some("3"));
    }

    #[test]
    fn stray_end_is_ignored_and_properties_outside_are_dropped() {
        let text =
            "FOO:bar\nEND:VEVENT\nBEGIN:VCALENDAR\nEND:VTODO\nPRODID:x\nEND:VCALENDAR\nJUNK\n";
        let c = parse_components(text);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].value("PRODID"), Some("x"));
    }

    #[test]
    fn broken_folding_becomes_continuation() {
        let text = "BEGIN:VEVENT\nDESCRIPTION:Join here\nJoin Zoom Meeting https://zoom.us/j/1\nEND:VEVENT\n";
        let c = parse_components(text);
        assert_eq!(
            c[0].text("DESCRIPTION").as_deref(),
            Some("Join here\nJoin Zoom Meeting https://zoom.us/j/1")
        );
    }

    #[test]
    fn quoted_printable_soft_breaks() {
        let text = "BEGIN:VCARD\nNOTE;ENCODING=QUOTED-PRINTABLE:first=\nsecond\nEND:VCARD\n";
        let c = parse_with(
            text,
            ParseOptions {
                quoted_printable: true,
            },
        );
        assert_eq!(c[0].value("NOTE"), Some("firstsecond"));
    }

    #[test]
    fn byte_level_unfolding_repairs_split_utf8() {
        // "Ş" is 0xC5 0x9E; fold in the middle of it.
        let bytes = b"BEGIN:VEVENT\r\nSUMMARY:\xC5\r\n \x9Eeker\r\nEND:VEVENT\r\n";
        let c = parse_components_bytes(bytes);
        assert_eq!(c[0].value("SUMMARY"), Some("Şeker"));
    }

    #[test]
    fn latin1_lines_are_decoded() {
        let bytes = b"BEGIN:VEVENT\nSUMMARY:M\xfcnchen\nLOCATION:\xc3\x96rnek\nEND:VEVENT\n";
        let c = parse_components_bytes(bytes);
        assert_eq!(c[0].value("SUMMARY"), Some("München"));
        assert_eq!(c[0].value("LOCATION"), Some("Örnek"));
    }

    #[test]
    fn classic_mac_line_endings() {
        let c = parse_components("BEGIN:VEVENT\rUID:1\rEND:VEVENT\r");
        assert_eq!(c[0].value("UID"), Some("1"));
    }

    #[test]
    fn garbage_never_panics() {
        let inputs = [
            "",
            ":",
            ";",
            "BEGIN:",
            "END:",
            "\"",
            ";;;:::",
            "A;B=\"unterminated:val",
            "BEGIN:VEVENT\nX;=:\n",
            "\u{FEFF}",
            " leading continuation",
            "BEGIN:VCALENDAR\n END:VCALENDAR",
        ];
        for input in inputs {
            let _ = parse_components(input);
            let _ = parse_components_bytes(input.as_bytes());
        }
    }
}
