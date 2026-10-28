//! Heuristic cleanup of email bodies and character-budget allocation.

use super::format::render_email;
use super::{Budget, MailMessageCtx, UserCtx};

/// What to remove from a body before budgeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CleanOptions {
    /// Remove quoted earlier messages (reply headers and trailing `>` blocks).
    pub strip_quotes: bool,
    /// Remove signatures (`-- ` blocks, "Sent from my phone" lines).
    pub strip_signature: bool,
}

impl CleanOptions {
    /// Strip both quotes and signatures (thread tasks: the thread has the history).
    pub(crate) const THREAD: Self = Self {
        strip_quotes: true,
        strip_signature: true,
    };
}

/// How an over-long body is shortened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrimStyle {
    /// Keep the beginning and the end (requests and deadlines often close an email).
    HeadAndTail,
    /// Keep only the beginning.
    Head,
}

/// "On Tue, 27 Oct 2026 at 09:15, Alice <alice@example.com> wrote:" in many
/// languages (matched on the lowercased, trimmed line).
const REPLY_HEADER_ENDINGS: &[&str] = &[
    "wrote:",
    "writes:",
    "schrieb:",
    "a écrit :",
    "a écrit:",
    "escribió:",
    "ha scritto:",
    "scrisse:",
    "schreef:",
    "escreveu:",
    "napisał:",
    "napisał(a):",
    "yazdı:",
    "написал:",
    "написала:",
    "написал(а):",
    "skrev:",
    "kirjoitti:",
    "írta:",
    "napsal:",
    "napsal(a):",
    "έγραψε:",
    "写道：",
    "写道:",
    "寫道：",
    "작성:",
    "のメッセージ:",
];

/// Languages where the verb precedes the sender: "Am 27.10.2026 um 09:15
/// schrieb Anna <anna@example.com>:".
const REPLY_VERBS_BEFORE_NAME: &[&str] = &[
    " schrieb ",
    " schreef ",
    " skrev ",
    " kirjoitti ",
    " írta ",
    " wrote ",
];

/// Separator lines of Outlook-style quoting ("-----Original Message-----").
const ORIGINAL_MESSAGE_MARKERS: &[&str] = &[
    "original message",
    "ursprüngliche nachricht",
    "message d'origine",
    "mensaje original",
    "messaggio originale",
    "oorspronkelijk bericht",
    "orijinal ileti",
    "исходное сообщение",
    "oprindelig meddelelse",
    "ursprungligt meddelande",
    "alkuperäinen viesti",
];

/// Forwarded content is the message itself and must be kept.
const FORWARD_MARKERS: &[&str] = &[
    "forwarded message",
    "begin forwarded message",
    "weitergeleitete nachricht",
    "message transféré",
    "mensaje reenviado",
    "messaggio inoltrato",
    "doorgestuurd bericht",
    "iletilen ileti",
    "пересылаемое сообщение",
    "videresendt melding",
    "vidarebefordrat meddelande",
];

/// First line of an Outlook header block ("From: …").
const FROM_LABELS: &[&str] = &[
    "from:",
    "von:",
    "de:",
    "de :",
    "da:",
    "van:",
    "från:",
    "fra:",
    "kimden:",
    "od:",
    "от:",
    "feladó:",
    "lähettäjä:",
    "发件人:",
    "发件人：",
    "差出人:",
];

/// Following lines of an Outlook header block.
const HEADER_LABELS: &[&str] = &[
    "sent:",
    "date:",
    "to:",
    "subject:",
    "cc:",
    "gesendet:",
    "an:",
    "betreff:",
    "datum:",
    "envoyé :",
    "envoyé:",
    "à :",
    "à:",
    "objet :",
    "objet:",
    "enviado:",
    "para:",
    "asunto:",
    "inviato:",
    "a:",
    "oggetto:",
    "verzonden:",
    "aan:",
    "onderwerp:",
    "skickat:",
    "till:",
    "ämne:",
    "sendt:",
    "til:",
    "emne:",
    "gönderildi:",
    "tarih:",
    "kime:",
    "konu:",
    "bilgi:",
    "wysłane:",
    "do:",
    "temat:",
    "отправлено:",
    "кому:",
    "тема:",
    "дата:",
    "收件人:",
    "主题:",
    "日期:",
];

/// Mobile-client signatures ("Sent from my iPhone"), lowercased prefixes.
const DEVICE_SIGNATURES: &[&str] = &[
    "sent from my ",
    "sent from outlook",
    "get outlook for ",
    "sent from mail for windows",
    "sent from yahoo mail",
    "sent with proton mail",
    "sent from proton mail",
    "envoyé de mon ",
    "von meinem ",
    "enviado desde mi ",
    "enviado do meu ",
    "inviato da ",
    "verzonden vanaf mijn ",
    "iphone'umdan gönderildi",
    "ipad'imden gönderildi",
    "android'imden gönderildi",
    "отправлено с ",
    "wysłane z mojego ",
    "skickat från min ",
    "sendt fra min ",
];

/// A signature delimiter is only trusted near the end of a message.
const SIGNATURE_WINDOW: usize = 15;
/// Device signatures are only removed among the last lines.
const DEVICE_SIGNATURE_WINDOW: usize = 4;
/// Messages kept in a thread get at least this many characters each.
const MIN_CHARS_PER_MESSAGE: usize = 300;
/// Below this allocation a body keeps only its beginning.
const HEAD_ONLY_BELOW: usize = 600;

/// Cleans one body: normalizes line breaks, strips quotes and signatures,
/// trims trailing spaces and collapses runs of blank lines.
pub(crate) fn clean_body(text: &str, options: CleanOptions) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = normalized.lines().collect();
    if options.strip_quotes {
        lines = strip_quoted(lines);
    }
    if options.strip_signature {
        lines = strip_signature(lines);
    }
    collapse_blank_lines(&lines)
}

fn strip_quoted(lines: Vec<&str>) -> Vec<&str> {
    let mut cut = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let lower = line.trim().to_lowercase();
        if FORWARD_MARKERS.iter().any(|m| lower.contains(m)) {
            break;
        }
        if is_separator(&lower) || is_header_block(&lines, i) || is_underscore_rule(&lines, i) {
            cut = i;
            break;
        }
        if is_reply_header(&lower) {
            cut = wrapped_header_start(&lines, i);
            break;
        }
    }
    let kept = &lines[..cut];
    // Quoted lines followed by the author's own text are inline answers: keep
    // them. Quoted lines after the last own line are history: drop them.
    let last_own = kept
        .iter()
        .rposition(|l| !l.trim().is_empty() && !l.trim_start().starts_with('>'));
    let result: Vec<&str> = match last_own {
        Some(last) => kept[..=last].to_vec(),
        None => Vec::new(),
    };
    if result.iter().all(|l| l.trim().is_empty()) {
        lines
    } else {
        result
    }
}

fn is_reply_header(lower: &str) -> bool {
    if lower.chars().count() >= 300 || !lower.chars().any(|c| c.is_ascii_digit() || c == '@') {
        return false;
    }
    REPLY_HEADER_ENDINGS.iter().any(|e| lower.ends_with(e))
        || (lower.ends_with(':')
            && (lower.contains('@') || lower.contains('<'))
            && REPLY_VERBS_BEFORE_NAME
                .iter()
                .any(|verb| lower.contains(verb)))
}

/// Gmail wraps long headers: "On Tue, 27 Oct 2026 at 09:15, Alice Smith <" /
/// "alice@example.com> wrote:". When the detected line is only the tail of
/// such a header, the cut starts one line earlier.
fn wrapped_header_start(lines: &[&str], index: usize) -> usize {
    let words = lines[index].split_whitespace().count();
    if index == 0 || words >= 4 {
        return index;
    }
    let previous = lines[index - 1].trim();
    let joined = format!("{previous} {}", lines[index].trim()).to_lowercase();
    if !previous.is_empty()
        && previous.chars().any(|c| c.is_ascii_digit())
        && is_reply_header(&joined)
    {
        index - 1
    } else {
        index
    }
}

fn is_separator(lower: &str) -> bool {
    let trimmed = lower.trim_matches(|c: char| c == '-' || c == '_' || c.is_whitespace());
    trimmed.len() < lower.len() && ORIGINAL_MESSAGE_MARKERS.contains(&trimmed)
}

fn starts_with_any(line: &str, labels: &[&str]) -> bool {
    let lower = line.trim().to_lowercase();
    labels.iter().any(|label| lower.starts_with(label))
}

/// "From: …" followed by at least two more header lines.
fn is_header_block(lines: &[&str], index: usize) -> bool {
    if !starts_with_any(lines[index], FROM_LABELS) {
        return false;
    }
    lines[index + 1..]
        .iter()
        .take(6)
        .filter(|l| starts_with_any(l, HEADER_LABELS))
        .count()
        >= 2
}

/// Outlook's "________________________________" rule right before a header block.
fn is_underscore_rule(lines: &[&str], index: usize) -> bool {
    let line = lines[index].trim();
    line.len() >= 10
        && line.chars().all(|c| c == '_')
        && (index + 1..lines.len().min(index + 3)).any(|i| is_header_block(lines, i))
}

fn strip_signature(mut lines: Vec<&str>) -> Vec<&str> {
    let window_start = lines.len().saturating_sub(SIGNATURE_WINDOW);
    // "-- " (RFC 3676) or a bare "--"; the first one near the end starts the signature.
    if let Some(pos) = (window_start..lines.len()).find(|&i| i > 0 && lines[i].trim_end() == "--")
        && lines[..pos].iter().any(|l| !l.trim().is_empty())
    {
        lines.truncate(pos);
    }
    let mut non_empty_seen = 0;
    let mut index = lines.len();
    while index > 0 && non_empty_seen < DEVICE_SIGNATURE_WINDOW {
        index -= 1;
        let lower = lines[index].trim().to_lowercase();
        if lower.is_empty() {
            continue;
        }
        non_empty_seen += 1;
        if DEVICE_SIGNATURES.iter().any(|sig| lower.starts_with(sig)) && lower.chars().count() < 80
        {
            lines.remove(index);
        }
    }
    lines
}

fn collapse_blank_lines(lines: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Shortens `text` to about `limit` characters at word boundaries, marking the
/// omission with "[…]".
pub(crate) fn truncate(text: &str, limit: usize, style: TrimStyle) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_owned();
    }
    let omitted_marker =
        |omitted: usize| format!("[\u{2026} {omitted} characters omitted \u{2026}]");
    if style == TrimStyle::Head || limit < HEAD_ONLY_BELOW {
        let head = head_chars(text, limit);
        let omitted = total - head.chars().count();
        return format!("{}\n{}", head.trim_end(), omitted_marker(omitted));
    }
    let head_len = limit * 3 / 4;
    let tail_len = limit - head_len;
    let head = head_chars(text, head_len);
    let tail = tail_chars(text, tail_len);
    let omitted = total.saturating_sub(head.chars().count() + tail.chars().count());
    format!(
        "{}\n{}\n{}",
        head.trim_end(),
        omitted_marker(omitted),
        tail.trim_start()
    )
}

/// The first `count` characters, cut back to a word boundary when one is near.
fn head_chars(text: &str, count: usize) -> &str {
    let end = text
        .char_indices()
        .nth(count)
        .map_or(text.len(), |(i, _)| i);
    let slice = &text[..end];
    match slice.rfind(char::is_whitespace) {
        Some(space) if slice[..space].chars().count() * 5 >= count * 4 => &slice[..space],
        _ => slice,
    }
}

/// The last `count` characters, moved forward to a word boundary when one is near.
fn tail_chars(text: &str, count: usize) -> &str {
    let total = text.chars().count();
    let start = text
        .char_indices()
        .nth(total.saturating_sub(count))
        .map_or(text.len(), |(i, _)| i);
    let slice = &text[start..];
    match slice.find(char::is_whitespace) {
        Some(space) if slice[space..].chars().count() * 5 >= count * 4 => &slice[space..],
        _ => slice,
    }
}

/// Weighted water-filling: messages shorter than their fair share keep their
/// full length and the leftover is shared among the longer ones.
pub(crate) fn allocate(lengths: &[usize], weights: &[usize], budget: usize) -> Vec<usize> {
    let mut allocation = vec![0; lengths.len()];
    let mut open: Vec<usize> = (0..lengths.len()).collect();
    let mut left = budget;
    while !open.is_empty() {
        let total_weight: usize = open.iter().map(|&i| weights[i].max(1)).sum();
        let share = |i: usize| left.saturating_mul(weights[i].max(1)) / total_weight;
        let (fits, rest): (Vec<usize>, Vec<usize>) =
            open.iter().partition(|&&i| lengths[i] <= share(i));
        if fits.is_empty() {
            for &i in &rest {
                allocation[i] = share(i);
            }
            break;
        }
        for i in fits {
            allocation[i] = lengths[i];
            left = left.saturating_sub(lengths[i]);
        }
        open = rest;
    }
    allocation
}

/// A thread rendered for a prompt.
pub(crate) struct RenderedThread {
    /// The `<email>` blocks, oldest first.
    pub text: String,
    /// Number of emails included.
    pub included: usize,
    /// Number of older emails left out to respect the budget.
    pub omitted: usize,
}

/// Renders a thread: cleans bodies, drops the oldest middle messages when the
/// budget cannot give each one a useful share (the first message, which usually
/// states the topic, is kept), and trims bodies favoring the newest message.
pub(crate) fn render_thread(
    user: &UserCtx,
    thread: &[MailMessageCtx],
    budget: Budget,
    options: CleanOptions,
) -> RenderedThread {
    let budget = budget.max_chars.max(Budget::MIN_MAX_CHARS);
    let capacity = (budget / MIN_CHARS_PER_MESSAGE).max(1);
    let indices: Vec<usize> = if thread.len() <= capacity {
        (0..thread.len()).collect()
    } else if capacity == 1 {
        vec![thread.len() - 1]
    } else {
        std::iter::once(0)
            .chain(thread.len() - (capacity - 1)..thread.len())
            .collect()
    };
    let bodies: Vec<String> = indices
        .iter()
        .map(|&i| clean_body(&thread[i].body_text, options))
        .collect();
    let lengths: Vec<usize> = bodies.iter().map(|b| b.chars().count()).collect();
    let newest = indices.len().saturating_sub(1);
    let weights: Vec<usize> = (0..indices.len())
        .map(|i| if i == newest { 2 } else { 1 })
        .collect();
    let allocation = allocate(&lengths, &weights, budget);
    let blocks: Vec<String> = indices
        .iter()
        .zip(bodies.iter().zip(allocation))
        .map(|(&i, (body, limit))| {
            let body = truncate(body, limit, TrimStyle::HeadAndTail);
            render_email(i + 1, &thread[i], &body, user)
        })
        .collect();
    RenderedThread {
        text: blocks.join("\n\n"),
        included: indices.len(),
        omitted: thread.len() - indices.len(),
    }
}

/// Renders independent emails (search results, triage batches): every email is
/// kept and gets an equal share, capped at `per_email_cap` characters.
pub(crate) fn render_independent(
    user: &UserCtx,
    messages: &[MailMessageCtx],
    budget: Budget,
    per_email_cap: usize,
    options: CleanOptions,
) -> String {
    let count = messages.len().max(1);
    let share =
        (budget.max_chars.max(Budget::MIN_MAX_CHARS) / count).clamp(200, per_email_cap.max(200));
    messages
        .iter()
        .enumerate()
        .map(|(i, message)| {
            let body = clean_body(&message.body_text, options);
            let body = truncate(&body, share, TrimStyle::Head);
            render_email(i + 1, message, &body, user)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: CleanOptions = CleanOptions::THREAD;

    #[test]
    fn strips_gmail_reply_header_and_quotes() {
        let body = "Sounds good, see you then.\n\nOn Tue, 27 Oct 2026 at 09:15, Alice <alice@example.com> wrote:\n> Shall we meet at 3?\n> Alice";
        assert_eq!(clean_body(body, THREAD), "Sounds good, see you then.");
    }

    #[test]
    fn strips_wrapped_reply_header() {
        let body = "Yes.\n\nOn Tue, 27 Oct 2026 at 09:15, Alice Smith <\nalice@example.com> wrote:\n> Question?";
        assert_eq!(clean_body(body, THREAD), "Yes.");
    }

    #[test]
    fn strips_localized_headers() {
        let turkish = "Tamam, teşekkürler.\n\n27 Eki 2026 Sal, 09:15 tarihinde Ali Veli <ali@example.com> şunu yazdı:\n> Merhaba";
        assert_eq!(clean_body(turkish, THREAD), "Tamam, teşekkürler.");
        let german = "Danke!\n\nAm 27.10.2026 um 09:15 schrieb Anna <anna@example.com>:\n> Hallo";
        assert_eq!(clean_body(german, THREAD), "Danke!");
    }

    #[test]
    fn strips_outlook_blocks() {
        let body = "Approved.\n\n________________________________\nFrom: Bob <bob@example.com>\nSent: Monday, October 26, 2026 4:00 PM\nTo: Ada\nSubject: Budget\n\nPlease approve.";
        assert_eq!(clean_body(body, THREAD), "Approved.");
        let body =
            "Noted.\n-----Original Message-----\nFrom: Bob\nSent: today\nTo: Ada\nSubject: x";
        assert_eq!(clean_body(body, THREAD), "Noted.");
        let turkish = "Olur.\n\nKimden: Ali <ali@example.com>\nGönderildi: 27 Ekim 2026 Salı 09:15\nKime: Ada\nKonu: Bütçe";
        assert_eq!(clean_body(turkish, THREAD), "Olur.");
    }

    #[test]
    fn keeps_forwarded_content_and_inline_answers() {
        let forwarded = "FYI\n\n---------- Forwarded message ---------\nFrom: Bob <bob@example.com>\nDate: Mon, 26 Oct 2026\nSubject: Launch\nTo: Ada\n\nWe launch on Friday.";
        assert!(clean_body(forwarded, THREAD).contains("We launch on Friday."));
        let inline = "> Can you send the report?\nSure, attached.\n> And the invoice?\nTomorrow.\n> Thanks\n> Bob";
        assert_eq!(
            clean_body(inline, THREAD),
            "> Can you send the report?\nSure, attached.\n> And the invoice?\nTomorrow."
        );
    }

    #[test]
    fn strips_signatures() {
        let body = "See attached.\n\n-- \nAda Lovelace\nAnalytical Engines Ltd\n+44 1234";
        assert_eq!(clean_body(body, THREAD), "See attached.");
        let body = "On my way.\n\nSent from my iPhone";
        assert_eq!(clean_body(body, THREAD), "On my way.");
        let body = "Tamam.\n\niPhone'umdan gönderildi";
        assert_eq!(clean_body(body, THREAD), "Tamam.");
    }

    #[test]
    fn fully_quoted_body_is_kept() {
        let body = "> only quoted\n> text";
        assert_eq!(clean_body(body, THREAD), "> only quoted\n> text");
    }

    #[test]
    fn allocation_is_fair_and_bounded() {
        assert_eq!(
            allocate(&[100, 5000, 5000], &[1, 1, 2], 4100),
            vec![100, 1333, 2666]
        );
        assert_eq!(allocate(&[10, 20], &[1, 1], 1000), vec![10, 20]);
        let total: usize = allocate(&[900, 900, 900, 900], &[1, 1, 1, 2], 1000)
            .iter()
            .sum();
        assert!(total <= 1000);
    }

    #[test]
    fn truncation_marks_omissions() {
        let text = "word ".repeat(400);
        let out = truncate(&text, 1000, TrimStyle::HeadAndTail);
        assert!(out.contains("characters omitted"));
        assert!(out.chars().count() < 1100);
        let out = truncate(&text, 100, TrimStyle::Head);
        assert!(out.starts_with("word word"));
        assert!(out.ends_with("characters omitted \u{2026}]"));
        assert_eq!(truncate("short", 100, TrimStyle::Head), "short");
    }
}
