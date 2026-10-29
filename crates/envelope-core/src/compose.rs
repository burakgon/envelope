//! Building outgoing messages.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MbAddress;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;

use crate::error::{Error, Result};
use crate::model::Address;

/// A file to attach.
#[derive(Debug, Clone)]
pub struct FilePart {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
    /// Set for inline images referenced as `cid:` from the HTML body.
    pub content_id: Option<String>,
}

/// Everything needed to produce a message.
#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub from: Address,
    pub reply_to: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub html: String,
    pub text: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub attachments: Vec<FilePart>,
    pub read_receipt: bool,
    /// Override the generated Message-ID (without brackets).
    pub message_id: Option<String>,
    /// Extra raw headers (e.g. for iTIP replies).
    pub extra_headers: Vec<(String, String)>,
    /// A `text/calendar` alternative (`METHOD`, data) for invitations/replies.
    pub calendar: Option<(String, String)>,
    /// Render even without recipients (server-side drafts).
    pub allow_no_recipients: bool,
}

/// A rendered message ready to submit.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub mime: Vec<u8>,
    pub message_id: String,
    pub envelope_from: String,
    pub recipients: Vec<String>,
}

fn mb_list(list: &[Address]) -> MbAddress<'static> {
    MbAddress::new_list(
        list.iter()
            .map(|a| MbAddress::new_address(a.name.clone(), a.email.clone()))
            .collect(),
    )
}

/// Generates a new Message-ID for the given sender domain.
pub fn new_message_id(from_email: &str) -> String {
    let domain = from_email
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| !d.is_empty())
        .unwrap_or("envelope.local");
    format!("{}@{domain}", uuid::Uuid::new_v4().simple())
}

/// Wraps editor HTML into a complete document with neutral, client-friendly styles.
pub fn wrap_html(body: &str) -> String {
    if body
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("<!doctype")
        || body.contains("<html")
    {
        return body.to_string();
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"></head>\
         <body><div style=\"font-family: -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; font-size: 14px; line-height: 1.5;\">{body}</div></body></html>"
    )
}

/// Renders a MIME message.
pub fn render(out: &Outgoing) -> Result<Rendered> {
    if out.to.is_empty() && out.cc.is_empty() && out.bcc.is_empty() && !out.allow_no_recipients {
        return Err(Error::Invalid("Add at least one recipient".into()));
    }
    for a in out.to.iter().chain(&out.cc).chain(&out.bcc) {
        if !out.allow_no_recipients
            && (!a.email.contains('@') || a.email.contains(char::is_whitespace))
        {
            return Err(Error::Invalid(format!(
                "“{}” is not a valid e-mail address",
                a.email
            )));
        }
    }
    let message_id = out
        .message_id
        .clone()
        .unwrap_or_else(|| new_message_id(&out.from.email));
    let text = out
        .text
        .clone()
        .unwrap_or_else(|| crate::mime::html_to_text(&out.html));

    let mut b = MessageBuilder::new()
        .from(MbAddress::new_address(
            out.from.name.clone(),
            out.from.email.clone(),
        ))
        .subject(out.subject.clone())
        .message_id(MessageId::new(message_id.clone()))
        .date(chrono::Utc::now().timestamp())
        .header(
            "User-Agent",
            Raw::new(concat!("Envelope/", env!("CARGO_PKG_VERSION"))),
        );
    if !out.to.is_empty() {
        b = b.to(mb_list(&out.to));
    }
    if !out.cc.is_empty() {
        b = b.cc(mb_list(&out.cc));
    }
    // Bcc recipients are only part of the SMTP envelope, never the headers.
    if let Some(r) = &out.reply_to {
        b = b.reply_to(MbAddress::new_address(r.name.clone(), r.email.clone()));
    }
    if let Some(irt) = &out.in_reply_to {
        b = b.in_reply_to(MessageId::new(irt.clone()));
        let mut refs = out.references.clone();
        if !refs.contains(irt) {
            refs.push(irt.clone());
        }
        // Keep the header reasonably short: first + last 10 references.
        if refs.len() > 11 {
            let first = refs[0].clone();
            let tail = refs[refs.len() - 10..].to_vec();
            refs = std::iter::once(first).chain(tail).collect();
        }
        b = b.references(MessageId::new_list(refs.into_iter()));
    }
    if out.read_receipt {
        b = b.header(
            "Disposition-Notification-To",
            Raw::new(out.from.email.clone()),
        );
    }
    for (k, v) in &out.extra_headers {
        b = b.header(k.clone(), Raw::new(v.clone()));
    }

    match &out.calendar {
        Some((method, ics)) => {
            use mail_builder::mime::{BodyPart, MimePart};
            let calendar = MimePart::new(
                mail_builder::headers::content_type::ContentType::new("text/calendar")
                    .attribute("method", method.clone())
                    .attribute("charset", "utf-8"),
                BodyPart::Text(ics.clone().into()),
            );
            let alternative = MimePart::new(
                "multipart/alternative",
                vec![
                    MimePart::new("text/plain", BodyPart::Text(text.clone().into())),
                    MimePart::new("text/html", BodyPart::Text(wrap_html(&out.html).into())),
                    calendar,
                ],
            );
            b = b.body(alternative);
        }
        None => {
            use mail_builder::mime::{BodyPart, MimePart};
            let alternative = MimePart::new(
                "multipart/alternative",
                vec![
                    MimePart::new("text/plain", BodyPart::Text(text.clone().into())),
                    MimePart::new("text/html", BodyPart::Text(wrap_html(&out.html).into())),
                ],
            );
            let (pictures, files): (Vec<&FilePart>, Vec<&FilePart>) =
                out.attachments.iter().partition(|a| a.content_id.is_some());
            // Pictures in the text travel with it; files after it.
            let main = if pictures.is_empty() {
                alternative
            } else {
                let mut parts = vec![alternative];
                parts.extend(pictures.into_iter().map(|a| {
                    MimePart::new(a.mime_type.clone(), BodyPart::Binary(a.data.clone().into()))
                        .inline()
                        .cid(a.content_id.clone().unwrap_or_default())
                }));
                MimePart::new("multipart/related", parts)
            };
            if files.is_empty() {
                b = b.body(main);
            } else {
                let mut parts = vec![main];
                parts.extend(files.into_iter().map(file_part));
                b = b.body(MimePart::new("multipart/mixed", parts));
            }
        }
    }

    let mime = b.write_to_vec().map_err(Error::Io)?;
    let recipients = out
        .to
        .iter()
        .chain(&out.cc)
        .chain(&out.bcc)
        .map(|a| a.email.trim().to_string())
        .collect();
    Ok(Rendered {
        mime,
        message_id,
        envelope_from: out.from.email.clone(),
        recipients,
    })
}

/// An attached file. A message (forwarded as an attachment) goes as it is, as
/// RFC 2046 wants of `message/rfc822`, unless its lines are too long for that.
fn file_part(a: &FilePart) -> mail_builder::mime::MimePart<'static> {
    use mail_builder::mime::{BodyPart, MimePart};
    if a.mime_type.eq_ignore_ascii_case("message/rfc822") {
        let data = crlf(&a.data);
        if data.split(|b| *b == b'\n').all(|line| line.len() <= 999) {
            let encoding = if data.is_ascii() { "7bit" } else { "8bit" };
            return MimePart::new("message/rfc822", BodyPart::Binary(data.into()))
                .attachment(a.filename.clone())
                .transfer_encoding(encoding);
        }
    }
    MimePart::new(a.mime_type.clone(), BodyPart::Binary(a.data.clone().into()))
        .attachment(a.filename.clone())
}

/// Line ends as mail wants them.
fn crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 40);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// Pictures written into the text as `data:` URLs (pasted or inserted) become
/// inline parts it refers to by `cid:`, which every mail program shows.
pub fn inline_data_images(html: &str) -> (String, Vec<FilePart>) {
    use base64::Engine as _;
    static DATA_IMAGE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)(<img\b[^>]*?\bsrc\s*=\s*)(["'])data:(image/[a-z0-9.+-]+);base64,([a-z0-9+/=\s]+)["']"#,
        )
        .expect("valid regex")
    });
    let mut parts = Vec::new();
    let out = DATA_IMAGE.replace_all(html, |c: &regex::Captures<'_>| {
        let mime = c[3].to_ascii_lowercase();
        let cleaned: String = c[4].chars().filter(|ch| !ch.is_whitespace()).collect();
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()) else {
            return c[0].to_string();
        };
        let n = parts.len() + 1;
        let cid = format!("image{n}.{}@envelope", uuid::Uuid::new_v4().simple());
        let ext = match mime.as_str() {
            "image/jpeg" => "jpg",
            "image/svg+xml" => "svg",
            other => other.trim_start_matches("image/"),
        };
        parts.push(FilePart {
            filename: format!("image{n}.{ext}"),
            mime_type: mime,
            data,
            content_id: Some(cid.clone()),
        });
        format!("{}{}cid:{cid}{}", &c[1], &c[2], &c[2])
    });
    (out.into_owned(), parts)
}

/// "Re: " / "Fwd: " subject prefixes without stacking them.
pub fn prefixed_subject(prefix: &str, subject: &str) -> String {
    let lower = subject.trim().to_ascii_lowercase();
    let p = prefix.to_ascii_lowercase();
    if lower.starts_with(&p) {
        subject.trim().to_string()
    } else {
        format!("{prefix}{}", subject.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_reply_with_attachment() {
        let out = Outgoing {
            from: Address::new(Some("Alex Morgan".into()), "alex@envelope.test"),
            to: vec![Address::new(Some("Elif Yılmaz".into()), "elif@example.com")],
            bcc: vec![Address::new(None, "hidden@example.com")],
            subject: "Re: Hafta sonu planı?".into(),
            html: "<p>Harika, görüşürüz! ☕</p>".into(),
            in_reply_to: Some("abc@x".into()),
            references: vec!["root@x".into()],
            attachments: vec![FilePart {
                filename: "plan.txt".into(),
                mime_type: "text/plain".into(),
                data: b"hello".to_vec(),
                content_id: None,
            }],
            ..Default::default()
        };
        let r = render(&out).unwrap();
        let s = String::from_utf8_lossy(&r.mime);
        assert!(s.contains("In-Reply-To: <abc@x>"));
        assert!(s.contains("References: <root@x>"));
        assert!(!s.contains("hidden@example.com"));
        assert_eq!(r.recipients.len(), 2);
        let (meta, body) = crate::mime::parse_full(&r.mime).unwrap();
        assert_eq!(meta.subject, "Re: Hafta sonu planı?");
        assert_eq!(meta.to[0].name.as_deref(), Some("Elif Yılmaz"));
        assert_eq!(body.attachments.len(), 1);
        assert!(body.html.unwrap().contains("görüşürüz"));
    }

    #[test]
    fn pictures_in_the_text_become_inline_parts() {
        // A 1×1 PNG, as the editor writes a pasted or inserted picture.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let html = format!(
            "<p>Look:</p><img alt=\"x\" src=\"data:image/png;base64,{png}\"><img src='data:image/png;base64,!!'>"
        );
        let (out, parts) = inline_data_images(&html);
        assert_eq!(parts.len(), 1, "the broken one stays as it was");
        let cid = parts[0].content_id.clone().unwrap();
        assert!(out.contains(&format!("src=\"cid:{cid}\"")), "{out}");
        assert!(out.contains("data:image/png;base64,!!"));
        assert_eq!(&parts[0].data[1..4], b"PNG");
        assert_eq!(parts[0].filename, "image1.png");

        let r = render(&Outgoing {
            from: Address::new(None, "alex@envelope.test"),
            to: vec![Address::new(None, "elif@example.com")],
            subject: "Picture".into(),
            html: out,
            attachments: parts,
            ..Default::default()
        })
        .unwrap();
        let s = String::from_utf8_lossy(&r.mime);
        assert!(s.contains("multipart/related"), "{s}");
        assert!(s.contains(&format!("Content-ID: <{cid}>")), "{s}");
    }

    #[test]
    fn forwards_a_message_as_an_attachment() {
        let original = render(&Outgoing {
            from: Address::new(Some("Elif".into()), "elif@example.com"),
            to: vec![Address::new(None, "alex@envelope.test")],
            subject: "Plan".into(),
            html: "<p>Cumartesi 11</p>".into(),
            ..Default::default()
        })
        .unwrap();
        let r = render(&Outgoing {
            from: Address::new(None, "alex@envelope.test"),
            to: vec![Address::new(None, "noah@example.com")],
            subject: "Fwd: Plan".into(),
            html: "<p>See below.</p>".into(),
            attachments: vec![FilePart {
                filename: "Plan.eml".into(),
                mime_type: "message/rfc822".into(),
                data: original.mime.clone(),
                content_id: None,
            }],
            ..Default::default()
        })
        .unwrap();
        let s = String::from_utf8_lossy(&r.mime);
        // Not base64: RFC 2046 wants a message part as it is.
        assert!(s.contains("Content-Type: message/rfc822"), "{s}");
        assert!(s.contains("Content-Transfer-Encoding: 7bit"), "{s}");
        assert!(s.contains("Subject: Plan"), "{s}");
        let (_, body) = crate::mime::parse_full(&r.mime).unwrap();
        assert!(body.html.unwrap().contains("See below."));
    }

    #[test]
    fn subject_prefix() {
        assert_eq!(prefixed_subject("Re: ", "Hello"), "Re: Hello");
        assert_eq!(prefixed_subject("Re: ", "RE: Hello"), "RE: Hello");
    }
}
