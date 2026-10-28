//! TEXT value escaping (RFC 5545 §3.3.11, RFC 6350 §3.4) and list splitting.

/// Escapes a string for use as an iCalendar / vCard TEXT value.
///
/// Backslash, semicolon, comma and line breaks are escaped; carriage returns
/// are dropped so that `\r\n` becomes a single `\n`.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

/// Reverses [`escape_text`].
///
/// Accepts `\n` and `\N` for line breaks and the escapes `\\`, `\,`, `\;`.
/// The non-standard `\:` and `\"` seen in the wild are unescaped as well; any
/// other backslash sequence is kept literally.
pub fn unescape_text(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(e @ ('\\' | ',' | ';' | ':' | '"')) => out.push(e),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a raw (still escaped) value on every `sep` that is not escaped
/// with a backslash. The returned pieces are still escaped.
pub fn split_unescaped(raw: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == sep {
            parts.push(&raw[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&raw[start..]);
    parts
}

/// Splits a raw TEXT list (e.g. `CATEGORIES`) on unescaped commas and
/// unescapes every element. Empty elements are dropped and the rest trimmed.
pub fn text_list(raw: &str) -> Vec<String> {
    split_unescaped(raw, ',')
        .into_iter()
        .map(|part| unescape_text(part).trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_round_trip() {
        let original = "Line one\nsemi; colon, comma \\ backslash";
        let escaped = escape_text(original);
        assert_eq!(escaped, "Line one\\nsemi\\; colon\\, comma \\\\ backslash");
        assert_eq!(unescape_text(&escaped), original);
    }

    #[test]
    fn escape_drops_carriage_returns() {
        assert_eq!(escape_text("a\r\nb"), "a\\nb");
    }

    #[test]
    fn unescape_variants() {
        assert_eq!(unescape_text("a\\Nb"), "a\nb");
        assert_eq!(unescape_text("x\\:y"), "x:y");
        assert_eq!(unescape_text("C:\\temp"), "C:\\temp");
        assert_eq!(unescape_text("trailing\\"), "trailing\\");
        assert_eq!(unescape_text("plain"), "plain");
    }

    #[test]
    fn split_respects_escapes() {
        assert_eq!(split_unescaped("a\\,b,c", ','), vec!["a\\,b", "c"]);
        assert_eq!(split_unescaped("a\\\\,b", ','), vec!["a\\\\", "b"]);
        assert_eq!(split_unescaped("", ','), vec![""]);
        assert_eq!(
            split_unescaped("Doe;John;;;", ';'),
            vec!["Doe", "John", "", "", ""]
        );
    }

    #[test]
    fn text_list_trims_and_drops_empty() {
        assert_eq!(
            text_list("Work, Meeting\\, important,,"),
            vec!["Work".to_string(), "Meeting, important".to_string()]
        );
    }
}
