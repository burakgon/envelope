//! `extract_json` against the ways models actually format JSON.

use envelope_ai::{AiErrorKind, extract_json};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, PartialEq)]
struct Reply {
    label: String,
    text: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Replies {
    replies: Vec<Reply>,
}

fn value(text: &str) -> Value {
    extract_json::<Value>(text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

#[test]
fn plain_json() {
    assert_eq!(value(r#"{"a": 1}"#), json!({"a": 1}));
    assert_eq!(value("  [1, 2, 3]\n"), json!([1, 2, 3]));
}

#[test]
fn code_fences_with_and_without_language() {
    assert_eq!(value("```json\n{\"a\": 1}\n```"), json!({"a": 1}));
    assert_eq!(value("```JSON\n[{\"a\": 1}]\n```\n"), json!([{"a": 1}]));
    assert_eq!(value("```\n{\"a\": 1}\n```"), json!({"a": 1}));
    assert_eq!(
        value("Here you go:\n```json\n{\"a\": 1}\n```\nLet me know!"),
        json!({"a": 1})
    );
    // An unterminated fence (answer cut off after the JSON).
    assert_eq!(value("```json\n{\"a\": 1}"), json!({"a": 1}));
}

#[test]
fn leading_and_trailing_prose() {
    assert_eq!(
        value("Sure! Here is the result: {\"events\": []} I hope this helps."),
        json!({"events": []})
    );
    assert_eq!(value("json\n{\"a\": true}"), json!({"a": true}));
}

#[test]
fn trailing_commas() {
    assert_eq!(
        value("{\"a\": [1, 2, 3,], \"b\": {\"c\": \"d\",},}"),
        json!({"a": [1, 2, 3], "b": {"c": "d"}})
    );
    assert_eq!(
        value("[\n  {\"x\": 1},\n  {\"x\": 2},\n]"),
        json!([{"x": 1}, {"x": 2}])
    );
}

#[test]
fn brackets_commas_and_quotes_inside_strings() {
    assert_eq!(
        value(r#"{"text": "a } b ] c, } \" ,]", "n": 1,}"#),
        json!({"text": "a } b ] c, } \" ,]", "n": 1})
    );
    assert_eq!(
        value(r#"Note: the "[" key. {"k": "]}"}"#),
        json!({"k": "]}"})
    );
}

#[test]
fn typed_extraction_skips_unrelated_json() {
    let text = "Citations [1] and {\"irrelevant\": true} before the answer:\n{\"replies\": [{\"label\": \"OK\", \"text\": \"Sounds good.\"},]}";
    let parsed: Replies = extract_json(text).expect("typed");
    assert_eq!(
        parsed.replies,
        vec![Reply {
            label: "OK".into(),
            text: "Sounds good.".into()
        }]
    );
}

#[test]
fn largest_candidate_wins_for_untyped_values() {
    assert_eq!(
        value("See [1]. Result: {\"items\": [1, 2], \"ok\": true}"),
        json!({"items": [1, 2], "ok": true})
    );
}

#[test]
fn unicode_and_bom() {
    assert_eq!(
        value("\u{feff}{\"başlık\": \"Görüşme — 会議 🗓\"}"),
        json!({"başlık": "Görüşme — 会議 🗓"})
    );
}

#[test]
fn failures_are_malformed_output() {
    for text in ["", "   ", "no json here", "{\"a\": ", "{\"a\": 1", "[1, 2"] {
        let err = extract_json::<Value>(text).expect_err(text);
        assert_eq!(err.kind(), AiErrorKind::MalformedOutput, "{text:?}");
    }
    let err = extract_json::<Replies>("{\"wrong\": 1}").expect_err("wrong shape");
    assert_eq!(err.kind(), AiErrorKind::MalformedOutput);
    assert!(
        err.to_string()
            .starts_with("The AI answer was not in the expected format")
    );
}
