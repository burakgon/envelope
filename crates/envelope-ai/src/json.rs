//! Lenient extraction of JSON from model answers.

use serde::de::DeserializeOwned;

use crate::error::AiError;

/// How many candidate spans are tried before giving up (bounds the work done on
/// pathological inputs).
const MAX_CANDIDATES: usize = 64;

/// Extracts and deserializes the JSON value a model was asked to produce.
///
/// Models often wrap JSON in Markdown code fences, add a sentence before or
/// after it, or leave trailing commas. This function tolerates all of that:
///
/// 1. the whole text, then the content of each code fence, then every balanced
///    `{…}` / `[…]` span (largest first) is tried;
/// 2. each candidate is parsed strictly, then again with trailing commas removed;
/// 3. the first candidate that deserializes into `T` wins.
///
/// # Errors
///
/// [`AiError::MalformedOutput`] when no candidate deserializes into `T`.
pub fn extract_json<T: DeserializeOwned>(text: &str) -> Result<T, AiError> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Err(AiError::malformed_output("the answer was empty"));
    }
    let mut last_error: Option<serde_json::Error> = None;
    for candidate in candidates(text).into_iter().take(MAX_CANDIDATES) {
        match parse_lenient::<T>(candidate) {
            Ok(value) => return Ok(value),
            Err(err) => last_error = Some(err),
        }
    }
    Err(AiError::malformed_output(match last_error {
        Some(err) => format!("no usable JSON found ({err})"),
        None => "no JSON found".to_owned(),
    }))
}

fn parse_lenient<T: DeserializeOwned>(candidate: &str) -> Result<T, serde_json::Error> {
    match serde_json::from_str::<T>(candidate) {
        Ok(value) => Ok(value),
        Err(err) => match remove_trailing_commas(candidate) {
            Some(repaired) => serde_json::from_str::<T>(&repaired).map_err(|_| err),
            None => Err(err),
        },
    }
}

/// Candidate substrings, most likely first.
fn candidates(text: &str) -> Vec<&str> {
    let mut out = vec![text];
    out.extend(fenced_blocks(text));
    let mut spans = balanced_spans(text);
    spans.sort_by_key(|(start, end)| std::cmp::Reverse(end - start));
    out.extend(spans.into_iter().map(|(start, end)| &text[start..end]));
    out.dedup();
    out
}

/// Contents of Markdown code fences (```json … ``` and plain ``` … ```).
fn fenced_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after_ticks = &rest[open + 3..];
        // Skip the info string ("json", "JSON5", …) up to the end of the line.
        let body_start = after_ticks.find('\n').map_or(0, |nl| nl + 1);
        let info = &after_ticks[..body_start];
        let body_start = if info.trim().chars().all(|c| c.is_ascii_alphanumeric()) {
            body_start
        } else {
            0
        };
        let body = &after_ticks[body_start..];
        match body.find("```") {
            Some(close) => {
                blocks.push(body[..close].trim());
                rest = &body[close + 3..];
            }
            None => {
                blocks.push(body.trim());
                break;
            }
        }
    }
    blocks
}

/// Byte ranges of balanced `{…}` / `[…]` spans (string-aware inside a span).
///
/// Every opener is tried as a start position, because quotes in surrounding
/// prose (`the "[" key`) make it impossible to know where JSON strings begin
/// before a span is chosen. The number of spans is bounded.
fn balanced_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    for (open, _) in bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'{' || **b == b'[')
    {
        if let Some(close) = matching_close(bytes, open) {
            spans.push((open, close + 1));
            if spans.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    spans
}

/// Index of the bracket closing the one at `open`, if the span is balanced.
fn matching_close(bytes: &[u8], open: usize) -> Option<usize> {
    let mut stack: Vec<u8> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                if stack.pop() != Some(b) {
                    return None;
                }
                if stack.is_empty() {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Removes commas that directly precede `}` or `]` (outside strings).
/// Returns `None` when there was nothing to remove.
fn remove_trailing_commas(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut in_string = false;
    let mut escaped = false;
    let mut segment_start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b',' => {
                let next = bytes[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
                if matches!(next, Some(b'}' | b']')) {
                    out.push_str(&text[segment_start..i]);
                    segment_start = i + 1;
                    changed = true;
                }
            }
            _ => {}
        }
    }
    if !changed {
        return None;
    }
    out.push_str(&text[segment_start..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_comma_repair_keeps_strings() {
        let repaired = remove_trailing_commas(r#"{"a": "x,}", "b": [1, 2,],}"#);
        assert_eq!(repaired.as_deref(), Some(r#"{"a": "x,}", "b": [1, 2]}"#));
        assert_eq!(remove_trailing_commas(r#"{"a": 1}"#), None);
    }

    #[test]
    fn spans_are_string_aware_and_survive_prose_quotes() {
        let text = r#"note "[" then {"k": "]}"} end"#;
        let spans: Vec<&str> = balanced_spans(text)
            .into_iter()
            .map(|(s, e)| &text[s..e])
            .collect();
        assert!(spans.contains(&r#"{"k": "]}"}"#), "{spans:?}");
        let value: serde_json::Value = extract_json(text).unwrap_or_default();
        assert_eq!(value["k"], "]}");
    }
}
