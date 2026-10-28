//! MIME helpers: header and body extraction, part decoding, snippets.

use envelope_imap::BodyPart;
use mail_parser::{Message, MessageParser, PartKind};

use crate::db::mail::{AttachmentMeta, MessageMeta};
use crate::model::{Address, Flags, UnsubscribeInfo};

/// Header fields requested during header synchronization.
pub const SYNC_HEADER_FIELDS: &str = "DATE FROM SENDER REPLY-TO TO CC BCC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES \
     LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST LIST-ID AUTO-SUBMITTED CONTENT-TYPE";

fn addresses(list: Option<mail_parser::AddressList<'_>>) -> Vec<Address> {
    let Some(list) = list else {
        return Vec::new();
    };
    list.mailboxes()
        .filter_map(|m| {
            let email = m.address()?.trim().to_string();
            if email.is_empty() {
                return None;
            }
            Some(Address::new(m.name().map(|n| n.trim().to_string()), email))
        })
        .collect()
}

fn clean_id(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// Extracts list/search metadata from a parsed message (headers only is fine).
pub fn meta_from_message(msg: &Message<'_>) -> MessageMeta {
    let headers = msg.headers();
    let from = addresses(msg.from())
        .into_iter()
        .next()
        .or_else(|| addresses(msg.sender()).into_iter().next());
    let list_unsubscribe = headers
        .get("List-Unsubscribe")
        .map(|h| String::from_utf8_lossy(h.raw_value()).trim().to_string())
        .filter(|s| !s.is_empty());
    let unsubscribe_post = headers.get("List-Unsubscribe-Post").is_some_and(|h| {
        String::from_utf8_lossy(h.raw_value())
            .to_ascii_lowercase()
            .contains("one-click")
    });
    let list_id = headers
        .get("List-Id")
        .map(|h| String::from_utf8_lossy(h.raw_value()).trim().to_string())
        .filter(|s| !s.is_empty());
    let is_auto = headers.get("Auto-Submitted").is_some_and(|h| {
        let v = String::from_utf8_lossy(h.raw_value())
            .trim()
            .to_ascii_lowercase();
        !v.is_empty() && v != "no"
    });
    let date = msg.date().map(|d| d.to_timestamp() * 1000).unwrap_or(0);
    let content_type = msg.content_type();
    let calendar_method = content_type.as_ref().and_then(|ct| {
        (ct.ctype().eq_ignore_ascii_case("text")
            && ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("calendar")))
        .then(|| ct.attribute("method").map(|m| m.to_ascii_uppercase()))
        .flatten()
    });
    let likely_attachments = content_type.as_ref().is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case("multipart")
            && ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
    });
    MessageMeta {
        message_id: msg.message_id().map(clean_id).filter(|s| !s.is_empty()),
        in_reply_to: msg
            .in_reply_to()
            .and_then(|l| l.first().map(clean_id))
            .filter(|s| !s.is_empty()),
        references: msg
            .references()
            .map(|l| l.iter().map(clean_id).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
        subject: msg.subject().unwrap_or("").trim().to_string(),
        from,
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        bcc: addresses(msg.bcc()),
        reply_to: addresses(msg.reply_to()),
        date,
        list_unsubscribe,
        unsubscribe_post,
        list_id,
        is_auto,
        calendar_method,
        has_attachments: likely_attachments,
        ..Default::default()
    }
}

/// Parses a header block (as returned by `BODY[HEADER.FIELDS (…)]`).
pub fn meta_from_headers(raw: &[u8]) -> MessageMeta {
    let mut data = raw.to_vec();
    if !data.ends_with(b"\r\n\r\n") && !data.ends_with(b"\n\n") {
        data.extend_from_slice(b"\r\n\r\n");
    }
    match MessageParser::new().parse(&data) {
        Some(m) => meta_from_message(&m),
        None => MessageMeta::default(),
    }
}

/// Converts IMAP system flags + keywords into our flag bits and extra keywords.
pub fn flags_from_imap(flags: &[String]) -> (i64, Vec<String>) {
    let mut bits = 0;
    let mut keywords = Vec::new();
    for f in flags {
        match f.to_ascii_lowercase().as_str() {
            "\\seen" => bits |= Flags::SEEN,
            "\\flagged" => bits |= Flags::FLAGGED,
            "\\answered" => bits |= Flags::ANSWERED,
            "\\draft" => bits |= Flags::DRAFT,
            "$forwarded" | "forwarded" => bits |= Flags::FORWARDED,
            "\\recent" | "\\deleted" | "$hasattachment" | "$hasnoattachment" => {}
            _ => keywords.push(f.clone()),
        }
    }
    (bits, keywords)
}

/// Content extracted from a full message.
#[derive(Debug, Clone, Default)]
pub struct ParsedBody {
    pub html: Option<String>,
    pub text: Option<String>,
    pub snippet: String,
    pub attachments: Vec<AttachmentMeta>,
    /// `(METHOD, iCalendar text)` of an attached invitation.
    pub calendar: Option<(String, String)>,
}

/// Parses a complete RFC 5322 message.
pub fn parse_full(raw: &[u8]) -> Option<(MessageMeta, ParsedBody)> {
    let msg = MessageParser::new().parse(raw)?;
    let mut meta = meta_from_message(&msg);
    let html = msg
        .body_html(0)
        .map(|h| h.into_owned())
        .filter(|h| !h.trim().is_empty());
    let mut text = msg
        .body_text(0)
        .map(|t| t.into_owned())
        .filter(|t| !t.trim().is_empty());
    // mail-parser synthesizes text from HTML when there is no text part, which is
    // exactly what we want for search and snippets.
    if text.is_none()
        && let Some(h) = &html
    {
        text = Some(html_to_text(h));
    }
    let mut attachments = Vec::new();
    let mut calendar = None;
    for (index, part) in msg.parts().enumerate() {
        let ct = part.content_type();
        let mime = ct
            .as_ref()
            .map(|c| {
                format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream"))
                    .to_ascii_lowercase()
            })
            .unwrap_or_else(|| "text/plain".into());
        if mime == "text/calendar" || mime == "application/ics" {
            let method = ct
                .as_ref()
                .and_then(|c| c.attribute("method"))
                .map(|m| m.to_ascii_uppercase());
            if let Some(body) = part.text() {
                let method = method
                    .or_else(|| ics_method(&body))
                    .unwrap_or_else(|| "PUBLISH".into());
                calendar = Some((method, body.into_owned()));
            }
            if !part.is_attachment() {
                attachments.push(AttachmentMeta {
                    part_id: format!("mp:{index}"),
                    filename: "invite.ics".into(),
                    mime_type: mime.clone(),
                    size: part.decoded_len() as i64,
                    content_id: None,
                    inline: true,
                    encoding: None,
                });
                continue;
            }
        }
        let is_body_part = part.in_html_body() || part.in_text_body();
        if part.is_multipart() || (is_body_part && !part.is_attachment()) {
            continue;
        }
        if matches!(part.kind(), PartKind::Text | PartKind::Html)
            && !part.is_attachment()
            && part.attachment_name().is_none()
        {
            continue;
        }
        let content_id = part.content_id().map(clean_id);
        let inline = part.is_inline() && content_id.is_some();
        let filename = part
            .attachment_name()
            .map(str::to_string)
            .unwrap_or_else(|| default_filename(&mime, index));
        attachments.push(AttachmentMeta {
            part_id: format!("mp:{index}"),
            filename,
            mime_type: mime,
            size: part.decoded_len() as i64,
            content_id,
            inline,
            encoding: None,
        });
    }
    if let Some((method, _)) = &calendar {
        meta.calendar_method = Some(method.clone());
    }
    meta.has_attachments = attachments.iter().any(|a| !a.inline);
    let snippet = make_snippet(text.as_deref().unwrap_or(""));
    Some((
        meta,
        ParsedBody {
            html,
            text,
            snippet,
            attachments,
            calendar,
        },
    ))
}

/// Extracts the decoded content of the part addressed by an `mp:<index>` id
/// from a full message.
pub fn extract_part(raw: &[u8], part_id: &str) -> Option<Vec<u8>> {
    let index: usize = part_id.strip_prefix("mp:")?.parse().ok()?;
    let msg = MessageParser::new().parse(raw)?;
    let part = msg.parts().nth(index)?;
    Some(part.decoded().into_owned())
}

fn ics_method(ics: &str) -> Option<String> {
    ics.lines()
        .find_map(|l| l.trim().strip_prefix("METHOD:"))
        .map(|m| m.trim().to_ascii_uppercase())
}

fn default_filename(mime: &str, index: usize) -> String {
    let ext = mime_guess::get_mime_extensions_str(mime)
        .and_then(|e| e.first())
        .copied()
        .unwrap_or("bin");
    format!("attachment-{index}.{ext}")
}

/// Decodes a single body part fetched by part number.
pub fn decode_text_part(raw: &[u8], part: &BodyPart) -> String {
    let charset = part.param("charset").unwrap_or("utf-8");
    let mut doc = format!(
        "Content-Type: {}; charset=\"{}\"\r\nContent-Transfer-Encoding: {}\r\n\r\n",
        part.mime_type,
        charset.replace('"', ""),
        if part.encoding.is_empty() {
            "7bit"
        } else {
            &part.encoding
        }
    )
    .into_bytes();
    doc.extend_from_slice(raw);
    let Some(msg) = MessageParser::new().parse(&doc) else {
        return String::from_utf8_lossy(raw).into_owned();
    };
    if part.mime_type == "text/html" {
        msg.body_html(0).map(|s| s.into_owned()).unwrap_or_default()
    } else {
        msg.root_part()
            .text()
            .map(|s| s.into_owned())
            .unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned())
    }
}

/// Decodes a binary part (attachment) according to its transfer encoding.
pub fn decode_binary(raw: &[u8], encoding: &str) -> Vec<u8> {
    let mut doc = format!(
        "Content-Type: application/octet-stream\r\nContent-Transfer-Encoding: {}\r\n\r\n",
        if encoding.is_empty() {
            "7bit"
        } else {
            encoding
        }
    )
    .into_bytes();
    doc.extend_from_slice(raw);
    match MessageParser::new().parse(&doc) {
        Some(msg) => msg.root_part().decoded().into_owned(),
        None => raw.to_vec(),
    }
}

/// Decodes an RFC 2047 encoded-word string (e.g. a filename parameter).
pub fn decode_words(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_string();
    }
    let doc = format!("Subject: {value}\r\n\r\n");
    MessageParser::new()
        .parse(doc.as_bytes())
        .and_then(|m| m.subject().map(str::to_string))
        .unwrap_or_else(|| value.to_string())
}

/// Reads a MIME parameter honoring RFC 2231 (`name*`, `name*0*`, …) and RFC 2047.
pub fn param_value(params: &[(String, String)], name: &str) -> Option<String> {
    if let Some((_, v)) = params.iter().find(|(k, _)| k == name) {
        return Some(decode_words(v));
    }
    let star = format!("{name}*");
    if let Some((_, v)) = params.iter().find(|(k, _)| *k == star) {
        return Some(decode_2231(v, true));
    }
    // Continuations: name*0, name*0*, name*1*, …
    let mut pieces: Vec<(u32, String, bool)> = params
        .iter()
        .filter_map(|(k, v)| {
            let rest = k.strip_prefix(&star)?;
            let (num, encoded) = match rest.strip_suffix('*') {
                Some(n) => (n, true),
                None => (rest, false),
            };
            Some((num.parse().ok()?, v.clone(), encoded))
        })
        .collect();
    if pieces.is_empty() {
        return None;
    }
    pieces.sort_by_key(|p| p.0);
    let first_encoded = pieces[0].2;
    let joined: String = pieces.iter().map(|p| p.1.as_str()).collect();
    Some(if first_encoded {
        decode_2231(&joined, true)
    } else {
        joined
    })
}

fn decode_2231(value: &str, has_charset: bool) -> String {
    let (charset, encoded) = if has_charset {
        let mut parts = value.splitn(3, '\'');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(cs), Some(_lang), Some(rest)) => (cs.to_ascii_lowercase(), rest),
            _ => ("utf-8".to_string(), value),
        }
    } else {
        ("utf-8".to_string(), value)
    };
    let bytes = percent_encoding::percent_decode_str(encoded).collect::<Vec<u8>>();
    if charset.is_empty() || charset == "utf-8" || charset == "us-ascii" {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    // Let mail-parser handle other charsets via an encoded word.
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    decode_words(&format!("=?{charset}?B?{b64}?="))
}

/// What to download to render a message, decided from its BODYSTRUCTURE.
#[derive(Debug, Clone, Default)]
pub struct BodyPlan<'a> {
    pub html: Option<&'a BodyPart>,
    pub text: Option<&'a BodyPart>,
    pub calendar: Option<&'a BodyPart>,
    pub attachments: Vec<AttachmentMeta>,
}

/// Picks the parts to display and lists attachments.
pub fn body_plan(root: &BodyPart) -> BodyPlan<'_> {
    let mut plan = BodyPlan::default();
    walk_plan(root, &mut plan, false);
    plan
}

fn part_filename(p: &BodyPart) -> Option<String> {
    param_value(&p.disposition_params, "filename").or_else(|| param_value(&p.params, "name"))
}

fn walk_plan<'a>(p: &'a BodyPart, plan: &mut BodyPlan<'a>, in_attachment: bool) {
    let is_attachment = p.disposition.as_deref() == Some("attachment");
    if p.is_multipart() {
        for c in &p.children {
            walk_plan(c, plan, in_attachment);
        }
        return;
    }
    if p.mime_type == "message/rfc822" {
        // Forwarded message: list as attachment, do not render inline.
        plan.attachments.push(AttachmentMeta {
            part_id: p.part_id.clone(),
            filename: part_filename(p).unwrap_or_else(|| "forwarded-message.eml".into()),
            mime_type: p.mime_type.clone(),
            size: p.size as i64,
            content_id: None,
            inline: false,
            encoding: Some(p.encoding.clone()),
        });
        return;
    }
    let filename = part_filename(p);
    match p.mime_type.as_str() {
        "text/html" if !is_attachment && !in_attachment && plan.html.is_none() => {
            plan.html = Some(p);
            return;
        }
        "text/plain" if !is_attachment && filename.is_none() && plan.text.is_none() => {
            plan.text = Some(p);
            return;
        }
        "text/calendar" | "application/ics" => {
            if plan.calendar.is_none() {
                plan.calendar = Some(p);
            }
            if !is_attachment && filename.is_none() {
                // Keep invitations reachable (hidden from the attachment list).
                plan.attachments.push(AttachmentMeta {
                    part_id: p.part_id.clone(),
                    filename: "invite.ics".into(),
                    mime_type: "text/calendar".into(),
                    size: p.size as i64,
                    content_id: None,
                    inline: true,
                    encoding: Some(p.encoding.clone()),
                });
                return;
            }
        }
        _ => {}
    }
    let content_id = p.content_id.as_deref().map(clean_id);
    let inline = p.disposition.as_deref() != Some("attachment")
        && content_id.is_some()
        && p.mime_type.starts_with("image/");
    plan.attachments.push(AttachmentMeta {
        part_id: p.part_id.clone(),
        filename: filename
            .unwrap_or_else(|| default_filename(&p.mime_type, plan.attachments.len())),
        mime_type: p.mime_type.clone(),
        size: estimated_decoded_size(p),
        content_id,
        inline,
        encoding: Some(p.encoding.clone()),
    });
}

fn estimated_decoded_size(p: &BodyPart) -> i64 {
    if p.encoding == "base64" {
        (p.size as i64) * 3 / 4
    } else {
        p.size as i64
    }
}

/// Converts HTML to readable plain text.
pub fn html_to_text(html: &str) -> String {
    mail_parser::html_to_text(html)
}

/// Builds a one-line preview: skips quoted text and signatures.
pub fn make_snippet(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t == "--"
            || t == "-- "
            || t.starts_with("-----Original Message")
            || t.starts_with("________________")
        {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if (lower.starts_with("on ") && lower.ends_with("wrote:"))
            || lower.ends_with("yazdı:")
            || lower.ends_with("schrieb:")
        {
            break;
        }
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
        if out.len() > 400 {
            break;
        }
    }
    let collapsed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut snippet: String = collapsed.chars().take(200).collect();
    if collapsed.chars().count() > 200 {
        snippet.push('…');
    }
    snippet
}

/// Parses a `List-Unsubscribe` header.
pub fn parse_unsubscribe(header: &str, one_click: bool) -> UnsubscribeInfo {
    let mut url = None;
    let mut mailto = None;
    for item in header.split(',') {
        let item = item
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim();
        let lower = item.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            if url.is_none() {
                url = Some(item.to_string());
            }
        } else if lower.starts_with("mailto:") && mailto.is_none() {
            mailto = Some(item.to_string());
        }
    }
    UnsubscribeInfo {
        one_click: one_click && url.as_deref().is_some_and(|u| u.starts_with("https://")),
        url,
        mailto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_to_meta() {
        let raw = b"From: =?UTF-8?Q?Elif_Y=C4=B1lmaz?= <Elif@Example.com>\r\nTo: a@b.test, \"Bob, Jr.\" <bob@c.test>\r\nSubject: =?UTF-8?B?SGFmdGEgc29udSBwbGFuxLE/?=\r\nMessage-ID: <abc@x>\r\nIn-Reply-To: <parent@x>\r\nReferences: <root@x> <parent@x>\r\nDate: Tue, 28 Oct 2026 10:00:00 +0300\r\nList-Unsubscribe: <https://x.test/u>, <mailto:u@x.test>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\r\n";
        let m = meta_from_headers(raw);
        assert_eq!(
            m.from.as_ref().unwrap().name.as_deref(),
            Some("Elif Yılmaz")
        );
        assert_eq!(m.subject, "Hafta sonu planı?");
        assert_eq!(m.to.len(), 2);
        assert_eq!(m.to[1].name.as_deref(), Some("Bob, Jr."));
        assert_eq!(m.message_id.as_deref(), Some("abc@x"));
        assert_eq!(m.in_reply_to.as_deref(), Some("parent@x"));
        assert_eq!(m.references, vec!["root@x", "parent@x"]);
        assert!(m.date > 0);
        assert!(m.unsubscribe_post);
        let u = parse_unsubscribe(m.list_unsubscribe.as_deref().unwrap(), true);
        assert_eq!(u.url.as_deref(), Some("https://x.test/u"));
        assert_eq!(u.mailto.as_deref(), Some("mailto:u@x.test"));
        assert!(u.one_click);
    }

    #[test]
    fn snippet_skips_quotes() {
        let s = make_snippet("Sounds good!\n\nSee you there.\n\nOn Mon, Alex wrote:\n> old text\n");
        assert_eq!(s, "Sounds good! See you there.");
    }

    #[test]
    fn rfc2231_params() {
        let params = vec![
            ("filename*0*".to_string(), "utf-8''%E2%98%95%20".to_string()),
            ("filename*1*".to_string(), "tables.gif".to_string()),
        ];
        assert_eq!(
            param_value(&params, "filename").as_deref(),
            Some("☕ tables.gif")
        );
        let params = vec![("name".to_string(), "=?UTF-8?B?w5Zub20ucGRm?=".to_string())];
        assert_eq!(param_value(&params, "name").as_deref(), Some("Önom.pdf"));
    }

    #[test]
    fn full_parse_with_attachment() {
        let raw = b"From: a@b.test\r\nSubject: hi\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\nHello there\r\n--x\r\nContent-Type: application/pdf; name=doc.pdf\r\nContent-Disposition: attachment; filename=doc.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--x--\r\n";
        let (meta, body) = parse_full(raw).unwrap();
        assert!(meta.has_attachments);
        assert_eq!(body.snippet, "Hello there");
        assert_eq!(body.attachments.len(), 1);
        assert_eq!(body.attachments[0].filename, "doc.pdf");
        assert_eq!(
            extract_part(raw, &body.attachments[0].part_id).unwrap(),
            b"%PDF-"
        );
    }

    #[test]
    fn decode_qp_part() {
        let part = BodyPart {
            part_id: "1".into(),
            mime_type: "text/plain".into(),
            params: vec![("charset".into(), "iso-8859-9".into())],
            encoding: "quoted-printable".into(),
            ..Default::default()
        };
        assert_eq!(decode_text_part(b"G=FCnayd=FDn", &part), "Günaydın");
    }
}
