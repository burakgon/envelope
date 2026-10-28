//! Serializer: components → folded CRLF text (RFC 5545 §3.1).

use crate::component::{Component, Param, Property};

/// Maximum octets per physical line, excluding the CRLF.
const MAX_LINE_OCTETS: usize = 75;

/// Serializes a component and everything below it into `out`.
pub(crate) fn write_component(component: &Component, out: &mut String) {
    fold_into(&format!("BEGIN:{}", component.name), out);
    for property in &component.properties {
        write_property(property, out);
    }
    for child in &component.components {
        write_component(child, out);
    }
    fold_into(&format!("END:{}", component.name), out);
}

/// Serializes several top-level components back to back.
pub fn write_components(components: &[Component]) -> String {
    let mut out = String::new();
    for component in components {
        write_component(component, &mut out);
    }
    out
}

/// Serializes one property as a folded content line.
pub(crate) fn write_property(property: &Property, out: &mut String) {
    let mut line = String::with_capacity(property.name.len() + property.value.len() + 16);
    if let Some(group) = property.group.as_deref().filter(|g| !g.is_empty()) {
        line.push_str(group);
        line.push('.');
    }
    line.push_str(&property.name);
    for param in &property.params {
        write_param(param, &mut line);
    }
    line.push(':');
    push_sanitized_value(&property.value, &mut line);
    fold_into(&line, out);
}

fn write_param(param: &Param, line: &mut String) {
    line.push(';');
    line.push_str(&param.name);
    if param.values.is_empty() {
        return;
    }
    line.push('=');
    for (index, value) in param.values.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        let encoded = encode_param_value(value);
        if needs_quotes(&encoded) {
            line.push('"');
            line.push_str(&encoded);
            line.push('"');
        } else {
            line.push_str(&encoded);
        }
    }
}

/// RFC 6868 caret encoding: `^` → `^^`, newline → `^n`, `"` → `^'`.
fn encode_param_value(value: &str) -> String {
    if !value.contains(['^', '\n', '\r', '"']) {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len() + 4);
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '^' => out.push_str("^^"),
            '"' => out.push_str("^'"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("^n");
            }
            '\n' => out.push_str("^n"),
            other => out.push(other),
        }
    }
    out
}

fn needs_quotes(value: &str) -> bool {
    value.contains([':', ';', ',']) || value.starts_with(' ') || value.ends_with(' ')
}

/// Raw values must never contain line breaks; turn stray ones into `\n`.
fn push_sanitized_value(value: &str, line: &mut String) {
    if !value.contains(['\r', '\n']) {
        line.push_str(value);
        return;
    }
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line.push_str("\\n");
            }
            '\n' => line.push_str("\\n"),
            other => line.push(other),
        }
    }
}

/// Appends `line` folded at 75 octets (never inside a UTF-8 sequence), CRLF terminated.
pub(crate) fn fold_into(line: &str, out: &mut String) {
    let mut start = 0;
    let mut limit = MAX_LINE_OCTETS;
    while line.len() - start > limit {
        let mut end = start + limit;
        while end > start && !line.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            // Unreachable for valid UTF-8 (a char is at most 4 octets), but
            // never loop forever.
            break;
        }
        out.push_str(&line[start..end]);
        out.push_str("\r\n ");
        start = end;
        // Continuation lines start with a space, leaving 74 octets of content.
        limit = MAX_LINE_OCTETS - 1;
    }
    out.push_str(&line[start..]);
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical_lines(text: &str) -> Vec<&str> {
        text.split("\r\n").filter(|l| !l.is_empty()).collect()
    }

    #[test]
    fn folds_at_75_octets_with_crlf() {
        let long = "A".repeat(200);
        let mut out = String::new();
        fold_into(&format!("DESCRIPTION:{long}"), &mut out);
        assert!(out.ends_with("\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'));
        for line in physical_lines(&out) {
            assert!(line.len() <= 75, "line too long: {}", line.len());
        }
        let unfolded = out.replace("\r\n ", "");
        assert_eq!(unfolded, format!("DESCRIPTION:{long}\r\n"));
    }

    #[test]
    fn folding_never_splits_utf8() {
        // Mix of 2, 3 and 4 octet characters so every boundary position occurs.
        let text: String = "ğüşıöç€😀日本語".repeat(20);
        let mut out = String::new();
        fold_into(&format!("SUMMARY:{text}"), &mut out);
        for line in physical_lines(&out) {
            assert!(line.len() <= 75);
            // Each physical line is valid UTF-8 by construction (it is a &str),
            // so check the content survives unfolding unchanged.
        }
        assert_eq!(out.replace("\r\n ", ""), format!("SUMMARY:{text}\r\n"));
    }

    #[test]
    fn quotes_and_caret_encodes_params() {
        let p = Property::new("ATTENDEE", "mailto:a@example.com")
            .with_param("CN", "Doe, John")
            .with_param("X-NOTE", "say \"hi\"\nnow ^");
        let line = p.to_content_line();
        assert_eq!(
            line,
            "ATTENDEE;CN=\"Doe, John\";X-NOTE=say ^'hi^'^nnow ^^:mailto:a@example.com\r\n"
        );
    }

    #[test]
    fn bare_params_and_groups() {
        let mut p = Property::new("TEL", "+1 555");
        p.group = Some("item1".into());
        p.params.push(Param::with_values("WORK", vec![]));
        assert_eq!(p.to_content_line(), "item1.TEL;WORK:+1 555\r\n");
    }

    #[test]
    fn stray_newlines_in_values_are_escaped() {
        let p = Property::new("SUMMARY", "a\r\nb\nc");
        assert_eq!(p.to_content_line(), "SUMMARY:a\\nb\\nc\r\n");
    }
}
