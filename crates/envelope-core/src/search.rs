//! Search query language → SQL over the FTS5 index.
//!
//! Supported syntax (Gmail-like):
//! `words`, `"exact phrase"`, `-exclude`, `a OR b`, `from:`, `to:`, `cc:`,
//! `subject:`, `filename:`, `has:attachment`, `has:invite`, `is:unread`,
//! `is:read`, `is:starred`, `is:draft`, `is:replied`, `is:vip`, `in:<folder>`,
//! `label:<name>`, `before:YYYY-MM-DD`, `after:YYYY-MM-DD`, `older_than:7d`,
//! `newer_than:2w`, `larger:5M`, `smaller:100K`.

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use rusqlite::types::Value;

/// A parsed token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Term {
        text: String,
        quoted: bool,
        negated: bool,
    },
    Op {
        key: String,
        value: String,
        negated: bool,
    },
    Or,
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let negated = chars[i] == '-' && i + 1 < chars.len() && !chars[i + 1].is_whitespace();
        if negated {
            i += 1;
        }
        // Read a word that may contain key:"quoted value".
        let mut word = String::new();
        let mut quoted = false;
        while i < chars.len() && !chars[i].is_whitespace() {
            if chars[i] == '"' {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    word.push(chars[i]);
                    i += 1;
                }
                i += 1;
            } else {
                word.push(chars[i]);
                i += 1;
            }
        }
        if word.is_empty() {
            continue;
        }
        if !quoted && !negated && word == "OR" {
            tokens.push(Token::Or);
            continue;
        }
        if let Some((key, value)) = word.split_once(':')
            && !key.is_empty()
            && !value.is_empty()
            && key.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
            && KNOWN_KEYS.contains(&key.to_ascii_lowercase().as_str())
        {
            tokens.push(Token::Op {
                key: key.to_ascii_lowercase(),
                value: value.to_string(),
                negated,
            });
            continue;
        }
        tokens.push(Token::Term {
            text: word,
            quoted,
            negated,
        });
    }
    tokens
}

const KNOWN_KEYS: &[&str] = &[
    "from",
    "to",
    "cc",
    "bcc",
    "subject",
    "filename",
    "has",
    "is",
    "in",
    "label",
    "before",
    "after",
    "older_than",
    "newer_than",
    "larger",
    "smaller",
    "older",
    "newer",
];

/// FTS5-safe phrase for a piece of text (with optional prefix match).
fn fts_phrase(text: &str, prefix: bool) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    if words.is_empty() {
        return None;
    }
    let phrase = format!("\"{}\"", words.join(" ").replace('"', "\"\""));
    Some(if prefix { format!("{phrase}*") } else { phrase })
}

fn parse_date(s: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(s, "%Y/%m/%d"))
        .or_else(|_| NaiveDate::parse_from_str(s, "%d.%m.%Y"))
        .ok()?;
    let dt = d.and_hms_opt(0, 0, 0)?;
    Some(
        chrono::Local
            .from_local_datetime(&dt)
            .single()?
            .timestamp_millis(),
    )
}

fn parse_relative(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    let d = match unit {
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        "m" => Duration::days(n * 30),
        "y" => Duration::days(n * 365),
        "h" => Duration::hours(n),
        _ => return None,
    };
    Some((Utc::now() - d).timestamp_millis())
}

fn parse_size(s: &str) -> Option<i64> {
    let s = s.to_ascii_uppercase();
    let (num, mult) = if let Some(n) = s.strip_suffix("MB").or_else(|| s.strip_suffix('M')) {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("KB").or_else(|| s.strip_suffix('K')) {
        (n, 1024)
    } else if let Some(n) = s.strip_suffix("GB").or_else(|| s.strip_suffix('G')) {
        (n, 1024 * 1024 * 1024)
    } else {
        (s.as_str(), 1)
    };
    let n: f64 = num.trim().parse().ok()?;
    Some((n * mult as f64) as i64)
}

/// The SQL condition (over `messages m`) for a query, or `None` if empty.
pub fn compile(query: &str) -> Option<(String, Vec<Value>)> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return None;
    }
    let mut conds: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let mut fts_must: Vec<String> = Vec::new();
    let mut fts_not: Vec<String> = Vec::new();
    let last_term_index = tokens.iter().rposition(|t| {
        matches!(
            t,
            Token::Term {
                quoted: false,
                negated: false,
                ..
            }
        )
    });

    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Or => {}
            Token::Term {
                text,
                quoted,
                negated,
            } => {
                // Typing-friendly: the last bare word matches as a prefix.
                let prefix = !quoted && Some(i) == last_term_index;
                let Some(mut expr) = fts_phrase(text, prefix || !quoted) else {
                    i += 1;
                    continue;
                };
                // Collapse "a OR b OR c".
                while i + 2 < tokens.len() && tokens[i + 1] == Token::Or {
                    if let Token::Term {
                        text: t2,
                        quoted: q2,
                        ..
                    } = &tokens[i + 2]
                        && let Some(e2) = fts_phrase(t2, !q2)
                    {
                        expr = format!("{expr} OR {e2}");
                    }
                    i += 2;
                }
                if *negated {
                    fts_not.push(expr);
                } else if expr.contains(" OR ") {
                    fts_must.push(format!("({expr})"));
                } else {
                    fts_must.push(expr);
                }
            }
            Token::Op {
                key,
                value,
                negated,
            } => {
                let column_term =
                    |col: &str| fts_phrase(value, true).map(|p| format!("{col} : {p}"));
                let (cond, fts): (Option<String>, Option<String>) = match key.as_str() {
                    "from" => (None, column_term("sender")),
                    "to" | "cc" | "bcc" => (None, column_term("recipients")),
                    "subject" => (None, column_term("subject")),
                    "filename" => (None, column_term("attachments")),
                    "has" => match value.to_ascii_lowercase().as_str() {
                        "attachment" | "attachments" | "file" => (Some("m.has_attachments = 1".into()), None),
                        "invite" | "event" | "calendar" => (Some("m.calendar_method IS NOT NULL".into()), None),
                        "unsubscribe" | "list" => (Some("m.list_unsubscribe IS NOT NULL".into()), None),
                        _ => (None, None),
                    },
                    "is" => match value.to_ascii_lowercase().as_str() {
                        "unread" => (Some("(m.flags & 1) = 0".into()), None),
                        "vip" | "vips" => (Some(crate::vip::FROM_VIP.into()), None),
                        "read" => (Some("(m.flags & 1) = 1".into()), None),
                        "starred" | "flagged" | "important" => (Some("(m.flags & 2) = 2".into()), None),
                        "draft" => (Some("(m.flags & 8) = 8".into()), None),
                        "replied" | "answered" => (Some("(m.flags & 4) = 4".into()), None),
                        "snoozed" => (Some("m.thread_id IN (SELECT id FROM threads WHERE snoozed_until IS NOT NULL)".into()), None),
                        _ => (None, None),
                    },
                    "in" | "label" => {
                        let v = value.to_lowercase();
                        if v == "anywhere" || v == "all" {
                            (None, None)
                        } else {
                            values.push(Value::Text(v.clone()));
                            values.push(Value::Text(v.clone()));
                            values.push(Value::Text(v));
                            (
                                Some(
                                    "m.id IN (SELECT mf.message_id FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                                      WHERE f.role = ? OR lower(f.name) = ? OR lower(f.path) = ?)"
                                        .into(),
                                ),
                                None,
                            )
                        }
                    }
                    "before" | "older" => match parse_date(value) {
                        Some(ts) => {
                            values.push(Value::Integer(ts));
                            (Some("m.date < ?".into()), None)
                        }
                        None => (None, None),
                    },
                    "after" | "newer" => match parse_date(value) {
                        Some(ts) => {
                            values.push(Value::Integer(ts));
                            (Some("m.date >= ?".into()), None)
                        }
                        None => (None, None),
                    },
                    "older_than" => match parse_relative(value) {
                        Some(ts) => {
                            values.push(Value::Integer(ts));
                            (Some("m.date < ?".into()), None)
                        }
                        None => (None, None),
                    },
                    "newer_than" => match parse_relative(value) {
                        Some(ts) => {
                            values.push(Value::Integer(ts));
                            (Some("m.date >= ?".into()), None)
                        }
                        None => (None, None),
                    },
                    "larger" => match parse_size(value) {
                        Some(n) => {
                            values.push(Value::Integer(n));
                            (Some("m.size > ?".into()), None)
                        }
                        None => (None, None),
                    },
                    "smaller" => match parse_size(value) {
                        Some(n) => {
                            values.push(Value::Integer(n));
                            (Some("m.size < ?".into()), None)
                        }
                        None => (None, None),
                    },
                    _ => (None, None),
                };
                if let Some(c) = cond {
                    conds.push(if *negated { format!("NOT ({c})") } else { c });
                }
                if let Some(f) = fts {
                    if *negated {
                        fts_not.push(f);
                    } else {
                        fts_must.push(f);
                    }
                }
            }
        }
        i += 1;
    }

    // FTS conditions go first in the parameter list; build final SQL in order.
    let mut sql_parts: Vec<String> = Vec::new();
    let mut ordered: Vec<Value> = Vec::new();
    if !fts_must.is_empty() {
        sql_parts.push("m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)".into());
        ordered.push(Value::Text(fts_must.join(" AND ")));
    }
    for f in fts_not {
        sql_parts
            .push("m.id NOT IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)".into());
        ordered.push(Value::Text(f));
    }
    sql_parts.extend(conds);
    ordered.extend(values);
    if sql_parts.is_empty() {
        return None;
    }
    Some((sql_parts.join(" AND "), ordered))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_prefix_last() {
        let (sql, v) = compile("roadmap rev").unwrap();
        assert!(sql.contains("MATCH"));
        assert_eq!(v[0], Value::Text("\"roadmap\"* AND \"rev\"*".into()));
    }

    #[test]
    fn operators() {
        let (sql, v) =
            compile("from:maya@northwind.design has:attachment is:unread -newsletter").unwrap();
        assert!(sql.contains("has_attachments = 1"));
        assert!(sql.contains("(m.flags & 1) = 0"));
        assert!(sql.contains("NOT IN"));
        assert_eq!(
            v[0],
            Value::Text("sender : \"maya northwind design\"*".into())
        );
        assert_eq!(v[1], Value::Text("\"newsletter\"*".into()));
    }

    #[test]
    fn quoted_and_or() {
        let (_, v) = compile("\"q4 roadmap\" berlin OR istanbul").unwrap();
        assert_eq!(
            v[0],
            Value::Text("\"q4 roadmap\" AND (\"berlin\"* OR \"istanbul\"*)".into())
        );
    }

    #[test]
    fn dates_and_sizes() {
        let (sql, v) = compile("after:2026-01-01 larger:2M in:inbox").unwrap();
        assert!(sql.contains("m.date >= ?"));
        assert!(sql.contains("m.size > ?"));
        assert!(v.contains(&Value::Integer(2 * 1024 * 1024)));
    }

    #[test]
    fn vips() {
        let (sql, v) = compile("is:vip budget").unwrap();
        assert!(sql.contains(crate::vip::FROM_VIP));
        assert_eq!(v, [Value::Text("\"budget\"*".into())]);
    }

    #[test]
    fn unknown_key_is_text() {
        let (_, v) = compile("http://example.com").unwrap();
        assert!(matches!(&v[0], Value::Text(t) if t.contains("example")));
    }
}
