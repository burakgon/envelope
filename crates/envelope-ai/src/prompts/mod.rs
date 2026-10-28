//! Prompt library for mail tasks.
//!
//! Every builder is a pure function returning the [`ChatMessage`]s to send;
//! tasks with structured output come with a parser for the model's answer.
//! [`Task::request`] turns the messages into a [`ChatRequest`] with settings
//! tuned for the task (temperature, JSON mode, reasoning effort).
//!
//! Design rules shared by all prompts:
//!
//! * **Email content is untrusted.** Bodies, headers and pasted text are wrapped
//!   in XML-style delimiters (`<email>`, `<text>`, `<draft>` …); look-alike tags
//!   inside the content are neutralized and invisible "smuggling" characters are
//!   removed. The system prompt tells the model to treat delimited content as
//!   data and never follow instructions found in it, and the task is restated
//!   after the data.
//! * **Bounded size.** Quoted replies and signatures are stripped heuristically,
//!   then bodies are trimmed to a character [`Budget`], favoring the newest
//!   message of a thread.
//! * **Grounding.** Prompts give models what they are bad at computing — weekday
//!   names, the user's time zone, ready-made date ranges — and mark the user's own
//!   messages with "(you)".

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{ChatMessage, ChatRequest, ReasoningEffort};

mod ask;
mod autocomplete;
mod clean;
mod compose;
mod events;
mod format;
mod reply;
mod rewrite;
mod search;
mod summarize;
mod triage;

pub use ask::{Citation, ask_inbox, cited_sources};
pub use autocomplete::{autocomplete, clean_autocomplete};
pub use compose::{ComposedEmail, compose, parse_compose};
pub use events::{EventTime, ExtractedEvent, extract_events, parse_events};
pub use reply::{QuickReply, draft_reply, parse_quick_replies, quick_replies};
pub use rewrite::{RewriteStyle, rewrite, translate};
pub use search::{nl_to_search, parse_search_query};
pub use summarize::summarize_thread;
pub use triage::{TriageCategory, TriageResult, parse_triage, triage};

/// One email given to a prompt as context.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MailMessageCtx {
    /// Stable id of the message in Envelope; parsers map model output back to it.
    pub id: String,
    /// Sender, e.g. `Alice Smith <alice@example.com>`.
    pub from: String,
    /// Primary recipients, each like `Name <address>`.
    pub to: Vec<String>,
    /// Carbon-copy recipients.
    pub cc: Vec<String>,
    /// When the message was sent, RFC 3339 (with the sender's UTC offset if known).
    pub date: String,
    /// Subject line.
    pub subject: String,
    /// Plain-text body (HTML already converted to text).
    pub body_text: String,
}

/// The person the assistant works for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UserCtx {
    /// Display name, used for sign-offs, e.g. `Burak Yılmaz`.
    pub name: String,
    /// Email address, used to recognize the user's own messages.
    pub email: String,
    /// IANA time zone, e.g. `Europe/Istanbul`.
    pub timezone: String,
    /// Current time, RFC 3339 with the user's UTC offset.
    pub now: String,
    /// Preferred language for generated summaries and labels (e.g. `Turkish` or
    /// `tr-TR`). `None` follows the language of the mail. Replies always follow
    /// the thread's language.
    pub language: Option<String>,
    /// Free-form writing-style instructions from the user's settings, applied
    /// to replies, new emails and rewrites.
    pub writing_style: Option<String>,
}

/// Character budget for email text included in a prompt.
///
/// Only email/body text counts; instructions and headers come on top. Roughly
/// 3–4 characters make a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum number of characters of email text.
    pub max_chars: usize,
}

impl Budget {
    /// Default budget: about 6–8k tokens of email text.
    pub const DEFAULT_MAX_CHARS: usize = 24_000;
    /// Smallest budget accepted; smaller values are raised to this.
    pub const MIN_MAX_CHARS: usize = 1_000;
    /// Largest budget [`Budget::for_context_window`] returns: bigger prompts are
    /// slow and expensive and rarely improve mail tasks.
    pub const MAX_AUTO_CHARS: usize = 120_000;

    /// A budget of `max_chars` characters (at least [`Budget::MIN_MAX_CHARS`]).
    pub fn new(max_chars: usize) -> Self {
        Self {
            max_chars: max_chars.max(Self::MIN_MAX_CHARS),
        }
    }

    /// A budget that fits a model's context window, leaving room for the
    /// instructions and the answer. Useful with [`crate::ModelInfo::context_length`],
    /// e.g. for small local models.
    pub fn for_context_window(context_tokens: u32) -> Self {
        let tokens = u64::from(context_tokens);
        let reserved = 2_000 + (tokens / 4).min(4_096);
        let usable = tokens.saturating_sub(reserved);
        let chars = usize::try_from(usable.saturating_mul(3)).unwrap_or(usize::MAX);
        Self::new(chars.min(Self::MAX_AUTO_CHARS))
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_chars: Self::DEFAULT_MAX_CHARS,
        }
    }
}

/// The mail tasks of this library, with recommended request settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    /// [`summarize_thread`]
    SummarizeThread,
    /// [`quick_replies`]
    QuickReplies,
    /// [`draft_reply`]
    DraftReply,
    /// [`compose`]
    Compose,
    /// [`rewrite`] with any style except [`RewriteStyle::FixGrammar`]
    Rewrite,
    /// [`rewrite`] with [`RewriteStyle::FixGrammar`]
    Proofread,
    /// [`translate`]
    Translate,
    /// [`extract_events`]
    ExtractEvents,
    /// [`ask_inbox`]
    AskInbox,
    /// [`nl_to_search`]
    NlToSearch,
    /// [`triage`]
    Triage,
    /// [`autocomplete`]
    Autocomplete,
}

impl Task {
    /// Recommended sampling temperature.
    pub fn temperature(self) -> f32 {
        match self {
            Self::QuickReplies => 0.7,
            Self::Compose => 0.6,
            Self::DraftReply => 0.5,
            Self::Rewrite => 0.4,
            Self::SummarizeThread | Self::AskInbox | Self::Autocomplete => 0.2,
            Self::Translate => 0.1,
            Self::Proofread | Self::ExtractEvents | Self::NlToSearch | Self::Triage => 0.0,
        }
    }

    /// Recommended output cap. Only autocomplete is capped: caps can starve
    /// reasoning models, and the other prompts already ask for concise output.
    pub fn max_tokens(self) -> Option<u32> {
        match self {
            Self::Autocomplete => Some(48),
            _ => None,
        }
    }

    /// Whether the task expects JSON output (enables the provider's JSON mode).
    pub fn json(self) -> bool {
        matches!(
            self,
            Self::QuickReplies | Self::Compose | Self::ExtractEvents | Self::Triage
        )
    }

    /// Recommended reasoning effort: low for interactive tasks, provider default
    /// for tasks that benefit from careful reasoning.
    pub fn effort(self) -> Option<ReasoningEffort> {
        match self {
            Self::ExtractEvents | Self::AskInbox => None,
            _ => Some(ReasoningEffort::Low),
        }
    }

    /// Wraps prompt messages into a request with the recommended settings.
    pub fn request(self, messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            messages,
            temperature: Some(self.temperature()),
            max_tokens: self.max_tokens(),
            json: self.json(),
            model: None,
            effort: self.effort(),
        }
    }
}

/// Finds the list in a JSON answer: a bare array, one of `keys`, or the first
/// array-valued field of an object.
fn json_list<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    match value {
        Value::Array(items) => Some(items),
        Value::Object(map) => keys
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_array))
            .or_else(|| map.values().find_map(Value::as_array)),
        _ => None,
    }
}

/// First non-empty string among `keys` of a JSON object, trimmed.
fn json_str(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_for_context_window() {
        assert_eq!(Budget::for_context_window(4_096).max_chars, 3_216);
        assert_eq!(
            Budget::for_context_window(1_000).max_chars,
            Budget::MIN_MAX_CHARS
        );
        assert_eq!(
            Budget::for_context_window(1_000_000).max_chars,
            Budget::MAX_AUTO_CHARS
        );
    }

    #[test]
    fn task_requests_carry_settings() {
        let request = Task::Triage.request(vec![ChatMessage::user("x")]);
        assert!(request.json);
        assert_eq!(request.temperature, Some(0.0));
        assert_eq!(request.effort, Some(ReasoningEffort::Low));
        let request = Task::Autocomplete.request(Vec::new());
        assert_eq!(request.max_tokens, Some(48));
        assert!(!request.json);
    }

    #[test]
    fn json_list_shapes() {
        let bare = serde_json::json!([1, 2]);
        assert_eq!(json_list(&bare, &[]).map(Vec::len), Some(2));
        let wrapped = serde_json::json!({"note": "x", "items": [1]});
        assert_eq!(json_list(&wrapped, &["replies"]).map(Vec::len), Some(1));
        assert!(json_list(&serde_json::json!("x"), &[]).is_none());
    }
}
