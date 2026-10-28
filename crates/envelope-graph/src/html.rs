//! Small text helpers for Graph bodies: HTML to plain text and meeting-link
//! detection.

use url::Url;

/// Elements whose content is not rendered.
const SKIPPED: &[&str] = &["head", "style", "script", "title", "template", "noscript"];

/// Elements that start a new line.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "thead",
    "tfoot",
    "tr",
    "ul",
];

/// Hosts of common online meeting services.
const MEETING_HOSTS: &[&str] = &[
    "teams.microsoft.com",
    "teams.live.com",
    "zoom.us",
    "zoomgov.com",
    "meet.google.com",
    "webex.com",
    "gotomeeting.com",
    "meet.goto.com",
    "whereby.com",
    "bluejeans.com",
    "chime.aws",
    "join.skype.com",
    "meet.lync.com",
    "meet.jit.si",
];

/// Converts an HTML body (as Outlook produces it) to readable plain text:
/// drops `<head>`, styles, scripts and comments, turns block elements and
/// `<br>` into line breaks, prefixes list items with `• `, decodes entities
/// and collapses whitespace.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_until: Option<String> = None;
    let mut pre_depth = 0usize;
    let mut pending_space = false;

    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip_until.is_none() {
                push_text(&mut out, rest, pre_depth > 0, &mut pending_space);
            }
            break;
        };
        if lt > 0 && skip_until.is_none() {
            push_text(&mut out, &rest[..lt], pre_depth > 0, &mut pending_space);
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some((tag, after)) = split_tag(rest) else {
            // A lone `<`: treat it as text.
            if skip_until.is_none() {
                push_text(&mut out, "<", pre_depth > 0, &mut pending_space);
            }
            rest = &rest[1..];
            continue;
        };
        rest = after;
        let (closing, name) = tag_name(tag);
        if let Some(skipped) = &skip_until {
            if closing && name == *skipped {
                skip_until = None;
            }
            continue;
        }
        if name.is_empty() {
            continue;
        }
        if !closing && SKIPPED.contains(&name.as_str()) && !tag.ends_with("/>") {
            skip_until = Some(name);
            continue;
        }
        match name.as_str() {
            "br" => {
                out.push('\n');
                pending_space = false;
            }
            "pre" => {
                pre_depth = if closing {
                    pre_depth.saturating_sub(1)
                } else {
                    pre_depth + 1
                };
                newline(&mut out, &mut pending_space);
            }
            "li" if !closing => {
                newline(&mut out, &mut pending_space);
                out.push_str("• ");
            }
            "td" | "th" if !closing => pending_space = true,
            "hr" => {
                newline(&mut out, &mut pending_space);
                out.push_str("---");
                newline(&mut out, &mut pending_space);
            }
            other if BLOCKS.contains(&other) => newline(&mut out, &mut pending_space),
            _ => {}
        }
    }
    tidy(&out)
}

/// Splits `<…>` off the front of `input`, honoring quoted attribute values.
fn split_tag(input: &str) -> Option<(&str, &str)> {
    let starts_tag = input[1..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
    if !starts_tag {
        return None;
    }
    let mut quote: Option<char> = None;
    for (i, ch) in input.char_indices().skip(1) {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(ch),
            (None, '>') => return Some((&input[..=i], &input[i + 1..])),
            (None, '<') => return None,
            _ => {}
        }
    }
    None
}

/// Lower-cased element name of a tag and whether it is a closing tag.
fn tag_name(tag: &str) -> (bool, String) {
    let inner = tag.trim_start_matches('<');
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let name: String = inner
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    (closing, name)
}

fn newline(out: &mut String, pending_space: &mut bool) {
    *pending_space = false;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn push_text(out: &mut String, text: &str, preformatted: bool, pending_space: &mut bool) {
    let decoded = decode_entities(text);
    if preformatted {
        out.push_str(&decoded);
        return;
    }
    for ch in decoded.chars() {
        if ch.is_whitespace() && ch != '\u{a0}' {
            *pending_space = true;
            continue;
        }
        if *pending_space && !out.is_empty() && !out.ends_with(['\n', ' ']) {
            out.push(' ');
        }
        *pending_space = false;
        out.push(if ch == '\u{a0}' { ' ' } else { ch });
    }
}

/// Trims trailing whitespace and allows at most one empty line in a row.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// Decodes HTML character references (named ones in common use and all
/// numeric ones).
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(i, _)| i);
        let decoded = end.and_then(|end| decode_entity(&rest[1..end]).map(|c| (c, end)));
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse::<u32>().ok()?,
        };
        return char::from_u32(code).filter(|c| *c != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "shy" => '\u{ad}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        _ => return None,
    })
}

/// Finds the first link to a known online meeting service (Teams, Zoom,
/// Google Meet, Webex, …) in text or HTML.
pub fn find_meeting_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find("https://") {
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(candidate.len());
        let raw = candidate[..end].trim_end_matches(['.', ',', ';', ')', ']']);
        let link = decode_entities(raw);
        if let Ok(url) = Url::parse(&link) {
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            if MEETING_HOSTS
                .iter()
                .any(|h| host == *h || host.ends_with(&format!(".{h}")))
            {
                return Some(link);
            }
        }
        rest = &candidate[end.max(1)..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_outlook_html() {
        let html = "<html><head>\r\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">\
            <style type=\"text/css\" style=\"display:none;\"><!-- P {margin-top:0;margin-bottom:0;} --></style>\
            </head><body dir=\"ltr\"><div>Hello&nbsp;team,</div><div><br></div>\
            <p>Agenda:</p><ul><li>Q3 &amp; Q4 numbers</li><li>Hiring</li></ul>\
            <!-- a comment --><p>See   you<br>Alex</p></body></html>";
        assert_eq!(
            html_to_text(html),
            "Hello team,\n\nAgenda:\n• Q3 & Q4 numbers\n• Hiring\nSee you\nAlex"
        );
    }

    #[test]
    fn handles_quotes_tables_and_stray_brackets() {
        let html = "<a href=\"x>y\">link</a> 1 < 2 <table><tr><td>a</td><td>b</td></tr></table>";
        assert_eq!(html_to_text(html), "link 1 < 2\na b");
        assert_eq!(html_to_text("<pre>  keep\n  this</pre>"), "  keep\n  this");
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            decode_entities("&lt;b&gt; &#65;&#x42; &unknown; & done"),
            "<b> AB &unknown; & done"
        );
        assert_eq!(decode_entities("&#0;"), "&#0;");
    }

    #[test]
    fn finds_meeting_links() {
        let body = "<a href=\"https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d&amp;x=1\">Join</a>";
        assert_eq!(
            find_meeting_url(body).as_deref(),
            Some(
                "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d&x=1"
            )
        );
        assert_eq!(
            find_meeting_url("Zoom: https://us02web.zoom.us/j/123?pwd=abc.").as_deref(),
            Some("https://us02web.zoom.us/j/123?pwd=abc")
        );
        assert_eq!(find_meeting_url("see https://example.com/zoom.us"), None);
        assert_eq!(find_meeting_url("no links"), None);
    }
}
