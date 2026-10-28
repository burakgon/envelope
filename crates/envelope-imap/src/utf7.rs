//! Modified UTF-7 mailbox name encoding (RFC 3501 §5.1.3).

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

fn engine() -> GeneralPurpose {
    // Same as the IMAP alphabet: standard base64 with ',' instead of '/'.
    let alphabet =
        Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,")
            .expect("valid alphabet");
    GeneralPurpose::new(
        &alphabet,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
}

/// Encodes a UTF-8 mailbox name into modified UTF-7.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&engine().encode(bytes));
        out.push('-');
        pending.clear();
    };
    for ch in name.chars() {
        if (' '..='~').contains(&ch) {
            flush(&mut pending, &mut out);
            if ch == '&' {
                out.push_str("&-");
            } else {
                out.push(ch);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(ch.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Decodes a modified UTF-7 mailbox name. Returns `None` if the input is not valid.
pub fn decode(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];
        let end = rest.find('-')?;
        let chunk = &rest[..end];
        rest = &rest[end + 1..];
        if chunk.is_empty() {
            out.push('&');
            continue;
        }
        let bytes = engine().decode(chunk).ok()?;
        if bytes.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        out.push_str(&String::from_utf16(&units).ok()?);
    }
    out.push_str(rest);
    Some(out)
}

/// Decodes modified UTF-7, falling back to the raw input when it is not valid
/// (servers with `UTF8=ACCEPT` send plain UTF-8 names).
pub fn decode_lossy(name: &str) -> String {
    if name.contains('&') {
        decode(name).unwrap_or_else(|| name.to_string())
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for s in [
            "INBOX",
            "Gönderilmiş Öğeler",
            "Entwürfe",
            "台北/日本語",
            "Tom & Jerry",
            "~peter/mail/&-",
            "😀 emoji",
        ] {
            assert_eq!(decode(&encode(s)).as_deref(), Some(s), "{s}");
        }
    }

    #[test]
    fn rfc_example() {
        assert_eq!(
            decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-").as_deref(),
            Some("~peter/mail/台北/日本語")
        );
        assert_eq!(
            encode("~peter/mail/台北/日本語"),
            "~peter/mail/&U,BTFw-/&ZeVnLIqe-"
        );
    }

    #[test]
    fn lossy_keeps_utf8() {
        assert_eq!(decode_lossy("Gönderilenler"), "Gönderilenler");
        assert_eq!(decode_lossy("A&B"), "A&B");
    }
}
