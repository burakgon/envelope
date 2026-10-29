//! S/MIME in e-mail (RFC 8551): finding signed and encrypted parts in
//! messages, and signing or encrypting outgoing ones.

use base64::Engine;
use mail_parser::{MessageParser, MessagePart};
use rand::Rng;

use crate::cert::Certificate;
use crate::cms::{self, SignWith};
use crate::{Error, Result};

/// The kind of S/MIME content a message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `multipart/signed` with an `application/pkcs7-signature` part.
    Signed,
    /// `application/pkcs7-mime; smime-type=signed-data`: the content is
    /// inside the signature.
    Opaque,
    /// `application/pkcs7-mime; smime-type=enveloped-data` (or
    /// `authEnveloped-data`).
    Encrypted,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Signed => "smime-signed",
            Kind::Opaque => "smime-opaque",
            Kind::Encrypted => "smime-encrypted",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "smime-signed" => Kind::Signed,
            "smime-opaque" => Kind::Opaque,
            "smime-encrypted" => Kind::Encrypted,
            _ => return None,
        })
    }

    pub fn is_encrypted(self) -> bool {
        self == Kind::Encrypted
    }
}

fn subtype_of(part: &MessagePart<'_>, ctype: &str) -> Option<String> {
    let ct = part.content_type()?;
    ct.ctype()
        .eq_ignore_ascii_case(ctype)
        .then(|| ct.subtype().unwrap_or_default().to_ascii_lowercase())
}

fn file_name(part: &MessagePart<'_>) -> String {
    part.attachment_name()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A detached signature part (also `application/octet-stream` named
/// `smime.p7s`, as some gateways send it).
fn is_signature(part: &MessagePart<'_>) -> bool {
    match subtype_of(part, "application").as_deref() {
        Some("pkcs7-signature" | "x-pkcs7-signature") => true,
        Some("octet-stream") => file_name(part).ends_with(".p7s"),
        _ => false,
    }
}

fn is_signed(part: &MessagePart<'_>) -> bool {
    if subtype_of(part, "multipart").as_deref() != Some("signed") {
        return false;
    }
    let protocol = part
        .content_type()
        .and_then(|ct| ct.attribute("protocol"))
        .map(|p| p.trim().to_ascii_lowercase());
    let mut children = part.children();
    let (Some(_), Some(signature), None) = (children.next(), children.next(), children.next())
    else {
        return false;
    };
    matches!(
        protocol.as_deref(),
        Some("application/pkcs7-signature" | "application/x-pkcs7-signature")
    ) || is_signature(&signature)
}

/// The kind of an `application/pkcs7-mime` part: from its `smime-type`, or
/// from the CMS content when the sender left it out.
fn pkcs7_kind(part: &MessagePart<'_>) -> Option<Kind> {
    let pkcs7 = match subtype_of(part, "application").as_deref() {
        Some("pkcs7-mime" | "x-pkcs7-mime") => true,
        Some("octet-stream") => file_name(part).ends_with(".p7m"),
        _ => false,
    };
    if !pkcs7 {
        return None;
    }
    let declared = part
        .content_type()
        .and_then(|ct| ct.attribute("smime-type"))
        .map(|t| t.trim().to_ascii_lowercase());
    match declared.as_deref() {
        Some("enveloped-data" | "authenveloped-data") => Some(Kind::Encrypted),
        Some("signed-data") => Some(Kind::Opaque),
        Some("certs-only") => None,
        _ => {
            let kind = cms::content_type(&part.decoded())?;
            if cms::is_enveloped_data(kind) {
                Some(Kind::Encrypted)
            } else if cms::is_signed_data(kind) {
                Some(Kind::Opaque)
            } else {
                None
            }
        }
    }
}

/// Finds S/MIME content in a message (nested `message/rfc822` parts, such
/// as forwarded mail, do not count): the outermost protection.
pub fn detect(raw: &[u8]) -> Option<Kind> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    for part in root.parts() {
        if is_signed(&part) {
            return Some(Kind::Signed);
        }
        if let Some(kind) = pkcs7_kind(&part) {
            return Some(kind);
        }
    }
    None
}

/// Converts bare `\n` line ends to `\r\n` (signatures are made over
/// canonical text).
pub fn canonical_crlf(data: &[u8]) -> Vec<u8> {
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

/// The signed content and detached signature of a `multipart/signed`
/// message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedParts {
    /// The first part, headers included, with CRLF line ends.
    pub data: Vec<u8>,
    /// The decoded CMS signature.
    pub signature: Vec<u8>,
    /// The signed part is the whole message (not, say, a part followed by a
    /// mailing list footer).
    pub whole: bool,
}

/// Finds the first S/MIME `multipart/signed` part.
pub fn signed_parts(raw: &[u8]) -> Option<SignedParts> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    let root_id = root.root_part().id();
    let part = root.parts().find(is_signed)?;
    let mut children = part.children();
    let content = children.next()?;
    let signature = children.next()?;
    Some(SignedParts {
        data: canonical_crlf(content.raw()),
        signature: signature.decoded().into_owned(),
        whole: part.id() == root_id,
    })
}

/// The CMS data of the first `application/pkcs7-mime` part, and whether
/// that part is the whole message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub cms: Vec<u8>,
    pub kind: Kind,
    pub whole: bool,
}

pub fn payload(raw: &[u8]) -> Option<Payload> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    let root_id = root.root_part().id();
    let part = root.parts().find(|p| pkcs7_kind(p).is_some())?;
    Some(Payload {
        cms: part.decoded().into_owned(),
        kind: pkcs7_kind(&part)?,
        whole: part.id() == root_id,
    })
}

/// Whether a part type is S/MIME's own (signatures and containers), which
/// is never shown as an attachment.
pub fn is_smime_type(mime_type: &str) -> bool {
    matches!(
        mime_type.to_ascii_lowercase().as_str(),
        "application/pkcs7-signature"
            | "application/x-pkcs7-signature"
            | "application/pkcs7-mime"
            | "application/x-pkcs7-mime"
    )
}

// ---------------------------------------------------------------------------
// Outgoing messages

/// How to protect an outgoing message.
#[derive(Default)]
pub struct Protect<'a> {
    /// Sign (as `multipart/signed`, inside the encryption when encrypting).
    pub sign: Option<SignWith<'a>>,
    /// Encrypt to these certificates (include the sender's own).
    pub encrypt_to: Vec<&'a Certificate>,
}

const CONTENT_FIELDS: [&str; 7] = [
    "content-type",
    "content-transfer-encoding",
    "content-disposition",
    "content-id",
    "content-description",
    "content-language",
    "content-location",
];

/// A header field as written: its name and its lines (folded continuation
/// lines included, the final line end excluded).
struct Field<'a> {
    name: String,
    raw: &'a [u8],
}

fn fields(header: &[u8]) -> Vec<Field<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..header.len() {
        if header[i] != b'\n' || matches!(header.get(i + 1), Some(b' ' | b'\t')) {
            continue;
        }
        let mut line = &header[start..i];
        if line.ends_with(b"\r") {
            line = &line[..line.len() - 1];
        }
        if let Some(colon) = line.iter().position(|&b| b == b':') {
            out.push(Field {
                name: String::from_utf8_lossy(&line[..colon])
                    .trim()
                    .to_ascii_lowercase(),
                raw: line,
            });
        }
        start = i + 1;
    }
    out
}

fn boundary() -> String {
    let n: u128 = rand::thread_rng().r#gen();
    format!("------------envelope{n:032x}")
}

fn base64_lines(data: &[u8]) -> String {
    let text = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(text.len() + text.len() / 38);
    for line in text.as_bytes().chunks(76) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push_str("\r\n");
    }
    out
}

/// Signs and/or encrypts a complete RFC 5322 message (CRLF line ends): its
/// content becomes the protected entity, the other header fields stay
/// outside. S/MIME does not protect the subject.
pub fn protect(message: &[u8], opts: &Protect<'_>) -> Result<Vec<u8>> {
    if opts.sign.is_none() && opts.encrypt_to.is_empty() {
        return Ok(message.to_vec());
    }
    let end = message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::Invalid("message without a body".into()))?;
    let (header, body) = (&message[..end + 2], &message[end + 4..]);
    let all = fields(header);

    // The entity to protect: the content fields and the body.
    let mut entity: Vec<u8> = Vec::with_capacity(message.len());
    let mut has_type = false;
    for f in all
        .iter()
        .filter(|f| CONTENT_FIELDS.contains(&f.name.as_str()))
    {
        has_type |= f.name == "content-type";
        entity.extend_from_slice(f.raw);
        entity.extend_from_slice(b"\r\n");
    }
    if !has_type {
        entity.extend_from_slice(b"Content-Type: text/plain; charset=\"utf-8\"\r\n");
    }
    entity.extend_from_slice(b"\r\n");
    entity.extend_from_slice(body);
    let mut entity = canonical_crlf(&entity);

    if let Some(with) = &opts.sign {
        let (signature, micalg) = cms::sign_detached(&entity, with)?;
        let b = boundary();
        let mut signed = format!(
            "Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\";\r\n micalg={micalg}; boundary=\"{b}\"\r\n\r\n\
             This is a cryptographically signed message in MIME format.\r\n\r\n--{b}\r\n"
        )
        .into_bytes();
        signed.extend_from_slice(&entity);
        signed.extend_from_slice(
            format!(
                "\r\n--{b}\r\n\
                 Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\
                 Content-Disposition: attachment; filename=\"smime.p7s\"\r\n\
                 Content-Description: S/MIME Cryptographic Signature\r\n\r\n\
                 {}\r\n--{b}--\r\n",
                base64_lines(&signature)
            )
            .as_bytes(),
        );
        entity = signed;
    }
    if !opts.encrypt_to.is_empty() {
        let cms = cms::encrypt(&entity, &opts.encrypt_to)?;
        entity = format!(
            "Content-Type: application/pkcs7-mime; smime-type=enveloped-data;\r\n name=\"smime.p7m\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"smime.p7m\"\r\n\
             Content-Description: S/MIME Encrypted Message\r\n\r\n{}",
            base64_lines(&cms)
        )
        .into_bytes();
    }

    let mut out: Vec<u8> = Vec::with_capacity(entity.len() + header.len());
    for f in all
        .iter()
        .filter(|f| !CONTENT_FIELDS.contains(&f.name.as_str()) && f.name != "mime-version")
    {
        out.extend_from_slice(f.raw);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    out.extend_from_slice(&entity);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_line_ends() {
        assert_eq!(canonical_crlf(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn plain_messages_pass_through() {
        let plain = b"From: a@b.c\r\nSubject: x\r\n\r\nhi\r\n";
        assert_eq!(protect(plain, &Protect::default()).unwrap(), plain);
        assert_eq!(detect(plain), None);
    }
}
