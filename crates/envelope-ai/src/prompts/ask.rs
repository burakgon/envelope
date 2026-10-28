//! Answering questions about the user's mail, with citations.

use serde::{Deserialize, Serialize};

use super::clean::{CleanOptions, render_independent};
use super::format::{
    describe_datetime, first_name, sanitize, security_rules, timezone_label, user_label,
};
use super::{Budget, MailMessageCtx, UserCtx};
use crate::types::ChatMessage;

/// Each source email gets at most this many characters.
const MAX_CHARS_PER_SOURCE: usize = 6_000;
/// Ranges like `[2-5]` are expanded only when this short.
const MAX_CITATION_RANGE: usize = 20;

/// A citation `[n]` in an answer, mapped to the cited message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Citation {
    /// The number used in the answer (1-based position in `sources`).
    pub index: usize,
    /// [`MailMessageCtx::id`] of the cited message.
    pub message_id: String,
}

/// Builds a prompt that answers `question` from the retrieved `sources` only,
/// as Markdown citing sources as `[n]` (n = 1-based position in `sources`), and
/// says clearly when the answer is not in the provided mail.
///
/// Use with [`super::Task::AskInbox`]; map citations with [`cited_sources`].
pub fn ask_inbox(
    user: &UserCtx,
    question: &str,
    sources: &[MailMessageCtx],
    budget: Budget,
) -> Vec<ChatMessage> {
    let who = user_label(user);
    let first = first_name(user);
    let security = security_rules(user);
    let tz = timezone_label(user);
    let now = if user.now.trim().is_empty() {
        "unknown".to_owned()
    } else {
        describe_datetime(&user.now)
    };
    let system = format!(
        "You are the email assistant built into Envelope, a desktop email client. You answer questions from {who} about their email, using only the emails provided as sources.

{security}

How to answer:
- The sources are <email> elements numbered by their id attribute. They come from a search, so some may be irrelevant: ignore those.
- Base every statement on the sources and cite them with the source number in square brackets right after the statement, e.g. \"The invoice is due on 12 May [2].\" Use [1][3] for several sources. Cite only numbers that exist.
- If the sources do not contain the answer, say so plainly in the first sentence, e.g. \"I couldn't find this in your emails.\", and then mention the closest related information you did find, with citations, if any. Never guess, and never use outside knowledge for facts about {first}'s mail.
- Lead with the direct answer, then add only the supporting details that matter, as short paragraphs or bullets. Use Markdown sparingly (bold, bullets); no headings, no code fences.
- Say who wrote something and when if that helps, e.g. \"Alice wrote on 3 March that \u{2026}\".
- Now is {now}; {first}'s time zone is {tz}. Use this for questions such as \"last week\" or \"when is my next \u{2026}\".
- Answer in the language of the question."
    );
    let sources_text = if sources.is_empty() {
        "Sources: none (the search found no matching emails).".to_owned()
    } else {
        let rendered = render_independent(
            user,
            sources,
            budget,
            MAX_CHARS_PER_SOURCE,
            CleanOptions::THREAD,
        );
        format!(
            "Sources ({} {}):\n\n{rendered}",
            sources.len(),
            if sources.len() == 1 {
                "email"
            } else {
                "emails"
            }
        )
    };
    let question = sanitize(question);
    let user_message = format!(
        "{sources_text}\n\n<question>\n{}\n</question>\n\nAnswer the question using only the sources above, citing them as [n].",
        question.trim()
    );
    vec![ChatMessage::system(system), ChatMessage::user(user_message)]
}

/// Finds the citations in an [`ask_inbox`] answer and maps them to message ids,
/// in order of first appearance and without duplicates. Accepts `[2]`,
/// `[1][3]`, `[1, 3]` and short ranges like `[2-4]`; numbers outside
/// `1..=sources.len()` and non-numeric brackets (Markdown links) are ignored.
pub fn cited_sources(answer: &str, sources: &[MailMessageCtx]) -> Vec<Citation> {
    let mut citations: Vec<Citation> = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        for index in parse_citation_group(&after[..close]) {
            let Some(source) = index.checked_sub(1).and_then(|i| sources.get(i)) else {
                continue;
            };
            if !citations.iter().any(|c| c.index == index) {
                citations.push(Citation {
                    index,
                    message_id: source.id.clone(),
                });
            }
        }
        rest = &after[close + 1..];
    }
    citations
}

/// "1, 3" → [1, 3]; "2-4" → [2, 3, 4]; anything else → [].
fn parse_citation_group(group: &str) -> Vec<usize> {
    if group.len() > 40 {
        return Vec::new();
    }
    let mut numbers = Vec::new();
    for part in group.split([',', ';']) {
        let part = part.trim();
        let range = part.split_once(['-', '\u{2013}']);
        match range {
            Some((from, to)) => {
                let (Ok(from), Ok(to)) = (from.trim().parse::<usize>(), to.trim().parse::<usize>())
                else {
                    return Vec::new();
                };
                if from > to || to - from > MAX_CITATION_RANGE {
                    return Vec::new();
                }
                numbers.extend(from..=to);
            }
            None => match part.parse::<usize>() {
                Ok(number) => numbers.push(number),
                Err(_) => return Vec::new(),
            },
        }
    }
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_groups() {
        assert_eq!(parse_citation_group("2"), vec![2]);
        assert_eq!(parse_citation_group("1, 3"), vec![1, 3]);
        assert_eq!(parse_citation_group("2-4"), vec![2, 3, 4]);
        assert!(parse_citation_group("link text").is_empty());
        assert!(parse_citation_group("1-1000").is_empty());
        assert!(parse_citation_group("").is_empty());
    }
}
