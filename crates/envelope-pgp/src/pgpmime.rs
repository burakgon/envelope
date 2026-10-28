//! OpenPGP in e-mail: PGP/MIME (RFC 3156) and inline PGP, with protected
//! headers (the `protected-headers="v1"` scheme used by Thunderbird,
//! Enigmail, K-9 and Delta Chat, standardized as RFC 9788).

use mail_parser::{MessageParser, MessagePart};
use pgp::composed::SignedPublicKey;
use rand::Rng;

use crate::ops::{self, Signer};
use crate::{Error, Result};

/// The kind of OpenPGP content a message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `multipart/encrypted` (PGP/MIME).
    Encrypted,
    /// `multipart/signed` (PGP/MIME).
    Signed,
    /// A `-----BEGIN PGP MESSAGE-----` block in the text.
    InlineEncrypted,
    /// A `-----BEGIN PGP SIGNED MESSAGE-----` block in the text.
    InlineSigned,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Encrypted => "encrypted",
            Kind::Signed => "signed",
            Kind::InlineEncrypted => "inline-encrypted",
            Kind::InlineSigned => "inline-signed",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "encrypted" => Kind::Encrypted,
            "signed" => Kind::Signed,
            "inline-encrypted" => Kind::InlineEncrypted,
            "inline-signed" => Kind::InlineSigned,
            _ => return None,
        })
    }

    pub fn is_encrypted(self) -> bool {
        matches!(self, Kind::Encrypted | Kind::InlineEncrypted)
    }
}

pub const MESSAGE_BEGIN: &str = "-----BEGIN PGP MESSAGE-----";
pub const MESSAGE_END: &str = "-----END PGP MESSAGE-----";
pub const SIGNED_BEGIN: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
pub const SIGNATURE_END: &str = "-----END PGP SIGNATURE-----";

fn is_type(part: &MessagePart<'_>, ctype: &str, subtype: &str) -> bool {
    part.is_content_type(ctype, subtype)
}

fn protocol(part: &MessagePart<'_>) -> Option<String> {
    part.content_type().and_then(|ct| {
        ct.attribute("protocol")
            .map(|p| p.trim().to_ascii_lowercase())
    })
}

fn is_pgp_encrypted(part: &MessagePart<'_>) -> bool {
    is_type(part, "multipart", "encrypted")
        && (protocol(part).as_deref() == Some("application/pgp-encrypted")
            || part
                .children()
                .any(|c| is_type(&c, "application", "pgp-encrypted")))
}

fn is_pgp_signed(part: &MessagePart<'_>) -> bool {
    is_type(part, "multipart", "signed")
        && part.children().len() == 2
        && (protocol(part).as_deref() == Some("application/pgp-signature")
            || part
                .children()
                .any(|c| is_type(&c, "application", "pgp-signature")))
}

/// Finds OpenPGP content in a message (nested `message/rfc822` parts, such
/// as forwarded mail, do not count).
pub fn detect(raw: &[u8]) -> Option<Kind> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    let mut signed = false;
    for part in root.parts() {
        if is_pgp_encrypted(&part) {
            return Some(Kind::Encrypted);
        }
        if is_pgp_signed(&part) {
            signed = true;
        }
    }
    if signed {
        return Some(Kind::Signed);
    }
    let text = root
        .body_text(0)
        .map(|t| t.into_owned())
        .unwrap_or_default();
    if find_block(&text, MESSAGE_BEGIN, MESSAGE_END).is_some() {
        return Some(Kind::InlineEncrypted);
    }
    if find_block(&text, SIGNED_BEGIN, SIGNATURE_END).is_some() {
        return Some(Kind::InlineSigned);
    }
    None
}

/// Whether the `multipart/encrypted` part is the whole message (rather than
/// one part next to unencrypted ones).
pub fn encrypted_is_whole(raw: &[u8]) -> bool {
    let Some(msg) = MessageParser::new().parse(raw) else {
        return false;
    };
    let root = msg.root();
    is_pgp_encrypted(&root.root_part())
}

/// The OpenPGP message inside a `multipart/encrypted` message.
pub fn encrypted_payload(raw: &[u8]) -> Option<Vec<u8>> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    let part = root.parts().find(is_pgp_encrypted)?;
    let payload = part
        .children()
        .find(|c| !is_type(c, "application", "pgp-encrypted"))?
        .decoded()
        .into_owned();
    Some(payload)
}

/// The signed content (canonical, with CRLF line ends) and the detached
/// signature of a `multipart/signed` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedParts {
    pub data: Vec<u8>,
    pub signature: Vec<u8>,
    /// Whether the signed part is the whole message body (rather than, say,
    /// a part followed by a mailing list footer).
    pub whole: bool,
}

/// Finds the first `multipart/signed` part.
pub fn signed_parts(raw: &[u8]) -> Option<SignedParts> {
    let msg = MessageParser::new().parse(raw)?;
    let root = msg.root();
    let root_id = root.root_part().id();
    let part = root.parts().find(is_pgp_signed)?;
    let mut children = part.children();
    let content = children.next()?;
    let signature = children.next()?;
    Some(SignedParts {
        data: canonical_crlf(content.raw()),
        signature: signature.decoded().into_owned(),
        whole: part.id() == root_id,
    })
}

/// Converts bare `\n` line ends to `\r\n`.
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

/// Byte range of the first armored block between `begin` and `end` that
/// starts a line (quoted blocks in replies do not count).
pub fn find_block(text: &str, begin: &str, end: &str) -> Option<std::ops::Range<usize>> {
    let mut from = 0;
    while let Some(i) = text[from..].find(begin) {
        let start = from + i;
        let at_line_start = start == 0 || text.as_bytes()[start - 1] == b'\n';
        if at_line_start {
            let tail = &text[start..];
            let stop = tail.find(end)?;
            return Some(start..start + stop + end.len());
        }
        from = start + begin.len();
    }
    None
}

/// Whether an entity (decrypted content) declares protected headers.
pub fn has_protected_headers(entity: &[u8]) -> bool {
    let Some(msg) = MessageParser::new().parse(entity) else {
        return false;
    };
    let root = msg.root();
    let declared = root
        .root_part()
        .content_type()
        .and_then(|ct| {
            ct.attribute("protected-headers")
                .map(|v| v.trim().to_string())
        })
        .is_some_and(|v| v.eq_ignore_ascii_case("v1"));
    declared || root.subject().is_some()
}

/// Removes "legacy display" parts from decrypted content: the
/// `text/rfc822-headers` (or `protected-headers` marked text) parts that
/// Thunderbird and Enigmail add so that clients without protected-header
/// support show the subject in the body. Clients that support protected
/// headers hide them.
pub fn strip_legacy_display(entity: &[u8]) -> Vec<u8> {
    let Some(msg) = MessageParser::new().parse(entity) else {
        return entity.to_vec();
    };
    let root = msg.root();
    let root_id = root.root_part().id();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for part in root.parts() {
        if part.id() == root_id || part.parent().is_none() {
            continue;
        }
        let Some(ct) = part.content_type() else {
            continue;
        };
        let legacy = (ct.ctype().eq_ignore_ascii_case("text")
            && ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("rfc822-headers")))
            || (ct.ctype().eq_ignore_ascii_case("text")
                && ct.attribute("protected-headers").is_some_and(|v| {
                    v.eq_ignore_ascii_case("v1") || v.eq_ignore_ascii_case("legacy-display")
                }));
        if !legacy {
            continue;
        }
        let start = part.offset_header() as usize;
        let end = part.offset_end() as usize;
        // Also drop the delimiter line in front of the part.
        let Some(delimiter) = entity[..start]
            .windows(4)
            .rposition(|w| w == b"\r\n--" || (w[1..] == *b"\n--" && w[0] != b'\r'))
        else {
            continue;
        };
        let from = if entity[delimiter] == b'\r' {
            delimiter
        } else {
            delimiter + 1
        };
        if from < end && end <= entity.len() {
            cuts.push((from, end));
        }
    }
    if cuts.is_empty() {
        return entity.to_vec();
    }
    cuts.sort_unstable();
    let mut out = Vec::with_capacity(entity.len());
    let mut pos = 0;
    for (from, end) in cuts {
        if from < pos {
            continue;
        }
        out.extend_from_slice(&entity[pos..from]);
        pos = end;
    }
    out.extend_from_slice(&entity[pos..]);
    out
}

// ---------------------------------------------------------------------------
// Building outgoing messages

/// How to protect an outgoing message.
#[derive(Debug, Clone, Default)]
pub struct Protect<'a> {
    /// Encrypt to these certificates (include the sender's own).
    pub encrypt_to: Vec<&'a SignedPublicKey>,
    /// Also encrypt to these, without naming them in the message (Bcc).
    pub hidden: Vec<&'a SignedPublicKey>,
    /// Sign with this key (inside the encryption, or as `multipart/signed`).
    pub signer: Option<Signer<'a>>,
    /// When encrypting: copy the headers into the encrypted part.
    pub protect_headers: bool,
    /// When encrypting with protected headers: replace the visible subject
    /// with `...`.
    pub hide_subject: bool,
    /// Extra header fields for the protected part (e.g. `Autocrypt-Gossip`),
    /// already formatted with their line ends.
    pub inner_headers: Vec<String>,
}

impl Protect<'_> {
    fn encrypts(&self) -> bool {
        !self.encrypt_to.is_empty() || !self.hidden.is_empty()
    }
}

/// The subject shown outside an encrypted message with protected headers.
pub const HIDDEN_SUBJECT: &str = "...";

const CONTENT_FIELDS: [&str; 7] = [
    "content-type",
    "content-transfer-encoding",
    "content-disposition",
    "content-id",
    "content-description",
    "content-language",
    "content-location",
];

const PROTECTED_FIELDS: [&str; 10] = [
    "date",
    "from",
    "sender",
    "to",
    "cc",
    "reply-to",
    "subject",
    "message-id",
    "in-reply-to",
    "references",
];

/// A header field as written: its name and the complete lines (folded
/// continuation lines included, final line end excluded).
struct Field<'a> {
    name: String,
    raw: &'a [u8],
}

fn split_message(message: &[u8]) -> Result<(&[u8], &[u8])> {
    let end = message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::Invalid("message without a body".into()))?;
    Ok((&message[..end + 2], &message[end + 4..]))
}

fn fields(header: &[u8]) -> Vec<Field<'_>> {
    let mut out: Vec<Field<'_>> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < header.len() {
        // End of a line.
        if header[i] == b'\n' {
            let next = header.get(i + 1).copied();
            let continues = matches!(next, Some(b' ') | Some(b'\t'));
            if !continues {
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
        }
        i += 1;
    }
    out
}

fn new_boundary() -> String {
    let n: u128 = rand::thread_rng().r#gen();
    format!("------------envelope{n:032x}")
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Signs and/or encrypts a complete RFC 5322 message (as rendered by
/// mail-builder, with CRLF line ends): the message content becomes the
/// protected part, the envelope headers stay outside.
pub fn protect(message: &[u8], opts: &Protect<'_>) -> Result<Vec<u8>> {
    let encrypting = opts.encrypts();
    if !encrypting && opts.signer.is_none() {
        return Ok(message.to_vec());
    }
    let (header, body) = split_message(message)?;
    let all = fields(header);
    let protect_headers = encrypting && opts.protect_headers;

    // The protected part: content headers (+ copies of the envelope headers).
    let mut inner: Vec<u8> = Vec::with_capacity(message.len() + 256);
    if protect_headers {
        for f in all
            .iter()
            .filter(|f| PROTECTED_FIELDS.contains(&f.name.as_str()))
        {
            inner.extend_from_slice(f.raw);
            inner.extend_from_slice(b"\r\n");
        }
    }
    for h in &opts.inner_headers {
        inner.extend_from_slice(h.as_bytes());
    }
    let mut has_type = false;
    for f in all
        .iter()
        .filter(|f| CONTENT_FIELDS.contains(&f.name.as_str()))
    {
        inner.extend_from_slice(f.raw);
        if f.name == "content-type" {
            has_type = true;
            if protect_headers {
                inner.extend_from_slice(b"; protected-headers=\"v1\"");
            }
        }
        inner.extend_from_slice(b"\r\n");
    }
    if !has_type {
        inner.extend_from_slice(b"Content-Type: text/plain; charset=\"utf-8\"");
        if protect_headers {
            inner.extend_from_slice(b"; protected-headers=\"v1\"");
        }
        inner.extend_from_slice(b"\r\n");
    }
    inner.extend_from_slice(b"\r\n");
    inner.extend_from_slice(body);

    // The visible envelope.
    let mut out: Vec<u8> = Vec::with_capacity(inner.len() * 2);
    for f in all
        .iter()
        .filter(|f| !CONTENT_FIELDS.contains(&f.name.as_str()) && f.name != "mime-version")
    {
        if f.name == "subject" && protect_headers && opts.hide_subject {
            out.extend_from_slice(format!("Subject: {HIDDEN_SUBJECT}\r\n").as_bytes());
            continue;
        }
        out.extend_from_slice(f.raw);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"MIME-Version: 1.0\r\n");
    let boundary = new_boundary();
    if encrypting {
        let armored = ops::encrypt(&inner, &opts.encrypt_to, &opts.hidden, opts.signer.as_ref())?;
        out.extend_from_slice(
            format!(
                "Content-Type: multipart/encrypted;\r\n protocol=\"application/pgp-encrypted\";\r\n boundary=\"{boundary}\"\r\n\r\n\
                 This is an OpenPGP/MIME encrypted message (RFC 4880 and 3156)\r\n\
                 --{boundary}\r\n\
                 Content-Type: application/pgp-encrypted\r\n\
                 Content-Description: PGP/MIME version identification\r\n\r\n\
                 Version: 1\r\n\r\n\
                 --{boundary}\r\n\
                 Content-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\
                 Content-Description: OpenPGP encrypted message\r\n\
                 Content-Disposition: inline; filename=\"encrypted.asc\"\r\n\r\n\
                 {}\r\n\
                 --{boundary}--\r\n",
                crlf(armored.trim_end())
            )
            .as_bytes(),
        );
    } else if let Some(signer) = &opts.signer {
        let (signature, hash) = ops::sign_detached(&inner, signer)?;
        out.extend_from_slice(
            format!(
                "Content-Type: multipart/signed; micalg={};\r\n protocol=\"application/pgp-signature\";\r\n boundary=\"{boundary}\"\r\n\r\n\
                 This is an OpenPGP/MIME signed message (RFC 4880 and 3156)\r\n\
                 --{boundary}\r\n",
                ops::micalg(hash)
            )
            .as_bytes(),
        );
        out.extend_from_slice(&inner);
        out.extend_from_slice(
            format!(
                "\r\n--{boundary}\r\n\
                 Content-Type: application/pgp-signature; name=\"signature.asc\"\r\n\
                 Content-Description: OpenPGP digital signature\r\n\
                 Content-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n\
                 {}\r\n\
                 --{boundary}--\r\n",
                crlf(signature.trim_end())
            )
            .as_bytes(),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::{self, generate};
    use crate::ops::{Issuer, SigStatus, decrypt, verify_detached};

    const PLAIN: &[u8] = b"From: Alex Morgan <alex@envelope.test>\r\n\
To: Elif <elif@example.com>\r\n\
Subject: =?utf-8?q?Gizli_plan_=E2=98=95?=\r\n\
Message-ID: <abc@envelope.test>\r\n\
Date: Tue, 28 Oct 2026 10:00:00 +0000\r\n\
Autocrypt: addr=alex@envelope.test; keydata=AAAA\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=\"utf-8\"\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Merhaba, g=C3=B6r=C3=BC=C5=9F=C3=BCr=C3=BCz.\r\n";

    fn lookup_in(certs: Vec<SignedPublicKey>) -> impl Fn(&Issuer) -> Vec<SignedPublicKey> {
        move |issuer: &Issuer| {
            certs
                .iter()
                .filter(|c| {
                    cert::key_ids(c)
                        .iter()
                        .any(|id| issuer.key_ids.contains(id))
                })
                .cloned()
                .collect()
        }
    }

    #[test]
    fn signs_as_multipart_signed() {
        let alex = generate("Alex", "alex@envelope.test", None).unwrap();
        let out = protect(
            PLAIN,
            &Protect {
                signer: Some(Signer {
                    key: &alex,
                    passphrase: None,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Content-Type: multipart/signed; micalg=pgp-sha256;"));
        assert!(text.contains("Subject: =?utf-8?q?Gizli_plan_=E2=98=95?="));
        assert!(text.contains("Autocrypt: addr=alex@envelope.test"));
        assert_eq!(detect(&out), Some(Kind::Signed));
        let parts = signed_parts(&out).unwrap();
        assert!(parts.whole);
        assert!(parts.data.starts_with(b"Content-Type: text/plain"));
        assert!(parts.data.ends_with(b"=C3=BCz.\r\n"));
        let lookup = lookup_in(vec![alex.to_public_key()]);
        let results = verify_detached(&parts.data, &parts.signature, &lookup).unwrap();
        assert_eq!(results[0].status, SigStatus::Good);
        // Still verifies after a trip through a mail store with bare LF.
        let lf = String::from_utf8(out.clone())
            .unwrap()
            .replace("\r\n", "\n");
        let parts = signed_parts(lf.as_bytes()).unwrap();
        assert_eq!(
            verify_detached(&parts.data, &parts.signature, &lookup).unwrap()[0].status,
            SigStatus::Good
        );
        // The body still reads as a normal message.
        let msg = MessageParser::new().parse(&out[..]).unwrap();
        assert_eq!(msg.body_text(0).unwrap().trim(), "Merhaba, görüşürüz.");
    }

    #[test]
    fn encrypts_with_protected_headers() {
        let alex = generate("Alex", "alex@envelope.test", None).unwrap();
        let elif = generate("Elif", "elif@example.com", None).unwrap();
        let out = protect(
            PLAIN,
            &Protect {
                encrypt_to: vec![&elif.to_public_key(), &alex.to_public_key()],
                signer: Some(Signer {
                    key: &alex,
                    passphrase: None,
                }),
                protect_headers: true,
                hide_subject: true,
                inner_headers: vec![
                    "Autocrypt-Gossip: addr=elif@example.com; keydata=AAAA\r\n".into(),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("Subject: ...\r\n"));
        assert!(!text.contains("Gizli"));
        assert!(!text.contains("Merhaba"));
        assert!(text.contains("To: Elif <elif@example.com>"));
        assert!(text.contains("protocol=\"application/pgp-encrypted\""));
        assert_eq!(detect(&out), Some(Kind::Encrypted));
        assert!(encrypted_is_whole(&out));
        let payload = encrypted_payload(&out).unwrap();
        assert!(payload.starts_with(b"-----BEGIN PGP MESSAGE-----"));
        let lookup = lookup_in(vec![alex.to_public_key()]);
        let plain = decrypt(&payload, &[(&elif, None)], &lookup).unwrap();
        assert_eq!(plain.signatures[0].status, SigStatus::Good);
        assert!(has_protected_headers(&plain.data));
        let inner = MessageParser::new().parse(&plain.data[..]).unwrap();
        assert_eq!(inner.subject(), Some("Gizli plan ☕"));
        assert_eq!(inner.body_text(0).unwrap().trim(), "Merhaba, görüşürüz.");
        assert!(inner.root().headers().get("Autocrypt-Gossip").is_some());
        // The sender keeps a readable copy.
        assert!(decrypt(&payload, &[(&alex, None)], &lookup).is_ok());
    }

    #[test]
    fn unprotected_messages_pass_through() {
        assert_eq!(protect(PLAIN, &Protect::default()).unwrap(), PLAIN);
    }

    #[test]
    fn finds_inline_blocks() {
        let text = "Hi\n\n-----BEGIN PGP MESSAGE-----\n\nhQEM\n-----END PGP MESSAGE-----\nbye";
        let r = find_block(text, MESSAGE_BEGIN, MESSAGE_END).unwrap();
        assert!(text[r.clone()].starts_with(MESSAGE_BEGIN));
        assert!(text[r].ends_with(MESSAGE_END));
        // Quoted in a reply: not a block of this message.
        assert!(
            find_block(
                "> -----BEGIN PGP MESSAGE-----\n> x\n> -----END PGP MESSAGE-----",
                MESSAGE_BEGIN,
                MESSAGE_END
            )
            .is_none()
        );
        let raw = format!(
            "From: a@b.c\r\nSubject: x\r\n\r\n{}",
            text.replace('\n', "\r\n")
        );
        assert_eq!(detect(raw.as_bytes()), Some(Kind::InlineEncrypted));
        assert_eq!(detect(b"From: a@b.c\r\n\r\nplain"), None);
    }

    #[test]
    fn canonical_line_ends() {
        assert_eq!(canonical_crlf(b"a\nb\r\nc\n"), b"a\r\nb\r\nc\r\n");
    }
}
