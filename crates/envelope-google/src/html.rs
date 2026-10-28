//! Minimal HTML → plain-text conversion.
//!
//! Google Calendar stores descriptions edited in its web UI as HTML, and the
//! People API may return biographies as HTML. The canonical model wants a
//! plain-text description next to the optional HTML one, so this module
//! renders markup roughly the way a browser would lay it out as text.

/// Heuristically detects HTML markup.
pub(crate) fn looks_like_html(text: &str) -> bool {
    if !text.contains('<') && !text.contains('&') {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

const MARKERS: &[&str] = &[
    "<br",
    "<p>",
    "<p ",
    "</p>",
    "<a ",
    "</a>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<u>",
    "</u>",
    "<ul",
    "</ul>",
    "<ol",
    "</ol>",
    "<li",
    "</li>",
    "<div",
    "</div>",
    "<span",
    "</span>",
    "<strong",
    "<em>",
    "</em>",
    "<h1",
    "<h2",
    "<h3",
    "<h4",
    "<table",
    "<tr",
    "<td",
    "<html",
    "<body",
    "<font",
    "<img",
    "<hr",
    "<blockquote",
    "<pre",
    "&nbsp;",
    "&amp;",
    "&lt;",
    "&gt;",
    "&quot;",
    "&#",
];

/// Renders HTML as plain text: tags are dropped, block elements and `<br>`
/// become line breaks, list items get bullets, whitespace is collapsed,
/// entities are decoded and link targets are appended when the link text
/// does not already show them.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut lists: Vec<Option<u32>> = Vec::new();
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut rest = html;
    while let Some(first) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if first == '<'
            && let Some((tag, consumed)) = parse_tag(rest)
        {
            rest = &rest[consumed..];
            match (tag.name.as_str(), tag.closing) {
                ("script" | "style", false) => rest = skip_raw_text(rest, &tag.name),
                ("br", _) => {
                    trim_trailing_spaces(&mut out);
                    out.push('\n');
                }
                ("ul", false) => {
                    line_break(&mut out);
                    lists.push(None);
                }
                ("ol", false) => {
                    line_break(&mut out);
                    lists.push(Some(1));
                }
                ("ul" | "ol", true) => {
                    lists.pop();
                    line_break(&mut out);
                }
                ("li", false) => {
                    line_break(&mut out);
                    match lists.last_mut() {
                        Some(Some(next)) => {
                            out.push_str(&format!("{next}. "));
                            *next = next.saturating_add(1);
                        }
                        _ => out.push_str("• "),
                    }
                }
                ("p", true) => {
                    line_break(&mut out);
                    out.push('\n');
                }
                (
                    "p" | "li" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "table"
                    | "blockquote" | "pre" | "section" | "article" | "header" | "footer" | "hr",
                    _,
                ) => line_break(&mut out),
                ("td" | "th", false) => push_space(&mut out),
                ("img", _) => {
                    if let Some(alt) = attribute(tag.attributes, "alt").filter(|a| !a.is_empty()) {
                        push_space(&mut out);
                        out.push_str(&alt);
                    }
                }
                ("a", false) => anchors.push(Anchor {
                    href: attribute(tag.attributes, "href"),
                    start: out.len(),
                }),
                ("a", true) => {
                    if let Some(anchor) = anchors.pop() {
                        append_link_target(&mut out, anchor);
                    }
                }
                _ => {}
            }
            continue;
        }
        if first == '&'
            && let Some((decoded, consumed)) = decode_entity(rest)
        {
            out.push(decoded);
            rest = &rest[consumed..];
            continue;
        }
        if first.is_whitespace() {
            push_space(&mut out);
        } else {
            out.push(first);
        }
        rest = &rest[first.len_utf8()..];
    }
    tidy(&out)
}

struct Tag<'a> {
    name: String,
    closing: bool,
    attributes: &'a str,
}

struct Anchor {
    href: Option<String>,
    start: usize,
}

/// Parses a tag at the start of `input` (which begins with `<`). Returns the
/// tag and the number of bytes consumed, or `None` if this is a literal `<`.
fn parse_tag(input: &str) -> Option<(Tag<'_>, usize)> {
    let after = input.get(1..)?;
    let next = after.chars().next()?;
    if !(next.is_ascii_alphabetic() || next == '/' || next == '!') {
        return None;
    }
    let mut quote: Option<char> = None;
    let mut end = None;
    for (idx, ch) in after.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '>' => {
                end = Some(idx);
                break;
            }
            None => {}
        }
    }
    let end = end?;
    let content = &after[..end];
    let closing = content.starts_with('/');
    let body = content.trim_start_matches('/');
    let name_len = body
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    let tag = Tag {
        name: body[..name_len].to_ascii_lowercase(),
        closing,
        attributes: &body[name_len..],
    };
    Some((tag, end + 2))
}

/// Skips the content of a raw-text element such as `<script>`.
fn skip_raw_text<'a>(rest: &'a str, name: &str) -> &'a str {
    let closing = format!("</{name}");
    let lower = rest.to_ascii_lowercase();
    match lower.find(&closing) {
        Some(start) => {
            let after = &rest[start..];
            after.find('>').map_or("", |end| &after[end + 1..])
        }
        None => "",
    }
}

/// Extracts an attribute value (entity-decoded) from a tag's attribute text.
fn attribute(attributes: &str, name: &str) -> Option<String> {
    let lower = attributes.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(found) = lower[search_from..].find(name) {
        let start = search_from + found;
        search_from = start + name.len();
        let preceded_ok = lower[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == '/');
        let rest = attributes[start + name.len()..].trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        if !preceded_ok {
            continue;
        }
        let value = value.trim_start();
        let raw = match value.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let inner = &value[1..];
                inner.find(q).map_or(inner, |end| &inner[..end])
            }
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or_default(),
        };
        return Some(decode_entities(raw));
    }
    None
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if ch == '&'
            && let Some((decoded, consumed)) = decode_entity(rest)
        {
            out.push(decoded);
            rest = &rest[consumed..];
            continue;
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Decodes the character reference at the start of `input` (which begins
/// with `&`). Returns the character and the number of bytes consumed.
fn decode_entity(input: &str) -> Option<(char, usize)> {
    let (end, _) = input.char_indices().take(12).find(|&(_, c)| c == ';')?;
    let name = input.get(1..end)?;
    let ch = if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse::<u32>().ok()?,
        };
        char::from_u32(code)?
    } else {
        match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            "nbsp" => ' ',
            "ndash" => '–',
            "mdash" => '—',
            "hellip" => '…',
            "lsquo" => '‘',
            "rsquo" => '’',
            "ldquo" => '“',
            "rdquo" => '”',
            "laquo" => '«',
            "raquo" => '»',
            "bull" => '•',
            "middot" => '·',
            "copy" => '©',
            "reg" => '®',
            "trade" => '™',
            "euro" => '€',
            "times" => '×',
            _ => return None,
        }
    };
    Some((ch, end + 1))
}

/// Appends ` <target>` after a link whose text does not show the target.
fn append_link_target(out: &mut String, anchor: Anchor) {
    let Some(href) = anchor.href.map(|h| unwrap_redirect(h.trim())) else {
        return;
    };
    let lower = href.to_ascii_lowercase();
    if href.is_empty() || href.starts_with('#') || lower.starts_with("javascript:") {
        return;
    }
    let target = href.strip_prefix("mailto:").unwrap_or(&href).to_owned();
    let text = out.get(anchor.start..).unwrap_or_default().trim();
    if text.is_empty() {
        out.push_str(&target);
    } else if !looks_like_address(text) && text != target {
        out.push_str(&format!(" <{target}>"));
    }
}

/// Google wraps links in descriptions as `https://www.google.com/url?q=<target>&…`.
fn unwrap_redirect(href: &str) -> String {
    if let Some(query) = href
        .strip_prefix("https://www.google.com/url?")
        .or_else(|| href.strip_prefix("http://www.google.com/url?"))
        && let Some((_, target)) = url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "q" || key == "url")
    {
        return target.into_owned();
    }
    href.to_owned()
}

fn looks_like_address(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("www.")
        || (!text.contains(char::is_whitespace) && text.contains('@'))
}

fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with([' ', '\n']) {
        out.push(' ');
    }
}

fn trim_trailing_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

fn line_break(out: &mut String) {
    trim_trailing_spaces(out);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Trims every line and collapses runs of blank lines into one.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.split('\n') {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank_run = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_html() {
        assert!(looks_like_html("Agenda<br>1. Intro"));
        assert!(looks_like_html("<a href=\"https://x.test\">x</a>"));
        assert!(looks_like_html("Tom &amp; Jerry"));
        assert!(!looks_like_html("Plain text\nwith lines & ampersand < 3"));
    }

    #[test]
    fn converts_google_editor_markup() {
        let html = "Agenda:<br><ul><li>Intro</li><li><b>Demo</b></li></ul>Join at <a href=\"https://www.google.com/url?q=https://example.com/join&amp;sa=D\">our page</a>.<br><br>Thanks &amp; see you";
        assert_eq!(
            html_to_text(html),
            "Agenda:\n• Intro\n• Demo\nJoin at our page <https://example.com/join>.\n\nThanks & see you"
        );
    }

    #[test]
    fn links_showing_their_target_are_not_repeated() {
        let html = "<a href=\"https://meet.google.com/abc-defg-hij\">https://meet.google.com/abc-defg-hij</a> or <a href=\"mailto:bob@example.com\">bob@example.com</a>";
        assert_eq!(
            html_to_text(html),
            "https://meet.google.com/abc-defg-hij or bob@example.com"
        );
    }

    #[test]
    fn paragraphs_ordered_lists_and_entities() {
        let html = "<p>First&nbsp;para</p>\n<p>Second</p><ol>\n<li>one</li>\n<li>two</li></ol><p>&#169; &#x263A; &lt;tag&gt;</p>";
        assert_eq!(
            html_to_text(html),
            "First para\n\nSecond\n\n1. one\n2. two\n© ☺ <tag>"
        );
    }

    #[test]
    fn ignores_scripts_comments_and_literal_angles() {
        let html = "<style>p{color:red}</style>a < b <!-- hidden --><span>c</span><script>alert(1)</script>";
        assert_eq!(html_to_text(html), "a < b c");
    }

    #[test]
    fn unterminated_markup_does_not_panic() {
        assert_eq!(html_to_text("text <a href=\"x"), "text <a href=\"x");
        assert_eq!(html_to_text("&#xZZ; &unknown; &"), "&#xZZ; &unknown; &");
        assert_eq!(html_to_text("<!-- open"), "");
    }
}
