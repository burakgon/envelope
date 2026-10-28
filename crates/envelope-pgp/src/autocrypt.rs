//! Autocrypt Level 1 headers (<https://autocrypt.org/level1.html>):
//! `Autocrypt` on outgoing mail and `Autocrypt-Gossip` inside encrypted
//! mail.

use base64::Engine as _;

/// Key data larger than this is ignored.
const MAX_KEYDATA: usize = 64 * 1024;

/// A parsed `Autocrypt` or `Autocrypt-Gossip` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The address the key belongs to (as written).
    pub addr: String,
    /// `prefer-encrypt=mutual` (always false for gossip).
    pub prefer_encrypt: bool,
    /// Binary transferable public key.
    pub keydata: Vec<u8>,
}

/// Parses an `Autocrypt` header value. Invalid headers (missing attributes,
/// unknown critical attributes, undecodable key data) give `None`, as the
/// specification requires.
pub fn parse(value: &str) -> Option<Header> {
    parse_attrs(value, false)
}

/// Parses an `Autocrypt-Gossip` header value.
pub fn parse_gossip(value: &str) -> Option<Header> {
    parse_attrs(value, true)
}

fn parse_attrs(value: &str, gossip: bool) -> Option<Header> {
    let mut addr = None;
    let mut prefer_encrypt = false;
    let mut keydata = None;
    for attr in value.split(';') {
        let attr = attr.trim();
        if attr.is_empty() {
            continue;
        }
        let (name, val) = attr.split_once('=')?;
        let name = name.trim().to_ascii_lowercase();
        let val = val.trim().trim_matches('"');
        match name.as_str() {
            "addr" => addr = Some(val.to_string()),
            "prefer-encrypt" if !gossip => prefer_encrypt = val.eq_ignore_ascii_case("mutual"),
            "keydata" => {
                let compact: String = val.chars().filter(|c| !c.is_whitespace()).collect();
                keydata = base64::engine::general_purpose::STANDARD
                    .decode(&compact)
                    .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
                    .ok();
                // Keys in headers are small; anything huge is not one.
                keydata.as_ref().filter(|k| k.len() <= MAX_KEYDATA)?;
            }
            // Level 0 headers named their type; only OpenPGP ever existed.
            "type" if val == "1" => {}
            n if n.starts_with('_') => {}
            _ => return None,
        }
    }
    let addr = addr.filter(|a| a.contains('@'))?;
    let keydata = keydata.filter(|k| !k.is_empty())?;
    Some(Header {
        addr,
        prefer_encrypt,
        keydata,
    })
}

fn keydata_chunks(keydata: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(keydata);
    b64.as_bytes()
        .chunks(72)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The value of an `Autocrypt` header. The key data starts after a space and
/// is split into chunks separated by spaces, so the header folds into short
/// lines (whitespace inside `keydata` is ignored by readers).
pub fn format(addr: &str, prefer_encrypt: bool, keydata: &[u8]) -> String {
    let prefer = if prefer_encrypt {
        " prefer-encrypt=mutual;"
    } else {
        ""
    };
    format!("addr={addr};{prefer} keydata= {}", keydata_chunks(keydata))
}

/// The value of an `Autocrypt-Gossip` header.
pub fn format_gossip(addr: &str, keydata: &[u8]) -> String {
    format!("addr={addr}; keydata= {}", keydata_chunks(keydata))
}

/// Folds `Name: value` at spaces into lines of at most 78 characters.
pub fn fold(name: &str, value: &str) -> String {
    let mut out = format!("{name}:");
    let mut line_len = out.len();
    for word in value.split(' ').filter(|w| !w.is_empty()) {
        if line_len + 1 + word.len() > 78 && line_len > 1 {
            out.push_str("\r\n");
            line_len = 0;
        }
        out.push(' ');
        out.push_str(word);
        line_len += 1 + word.len();
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_level1_examples() {
        // From the Autocrypt Level 1 specification (key data shortened).
        let h = parse("addr=alice@autocrypt.example; prefer-encrypt=mutual; keydata=\r\n mDMEXEcE6RYJKwYBBAHaRw8BAQdArjWwk3FAqyiFbFBKT4TzXcVBqPTB3gmzlC/U\r\n b7O1u120JkFsaWNlIExvdmVsYWNlIDxhbGljZUBvcGVucGdwLmV4YW1wbGU+").unwrap();
        assert_eq!(h.addr, "alice@autocrypt.example");
        assert!(h.prefer_encrypt);
        assert_eq!(h.keydata[0], 0x98);
        let h = parse("addr=bob@example.org; keydata=mDMEXEcE6RYJKwYBBAHaRw8BAQdA").unwrap();
        assert!(!h.prefer_encrypt);
        // Optional attributes start with an underscore; others are critical.
        assert!(parse("addr=a@b.c; _foo=bar; keydata=mDMEXEcE").is_some());
        assert!(parse("addr=a@b.c; foo=bar; keydata=mDMEXEcE").is_none());
        assert!(parse("addr=a@b.c; type=1; keydata=mDMEXEcE").is_some());
        assert!(parse("addr=a@b.c").is_none());
        assert!(parse("keydata=mDMEXEcE").is_none());
        assert!(parse("addr=a@b.c; keydata=!!!").is_none());
        // Unknown prefer-encrypt values mean no preference.
        assert!(
            !parse("addr=a@b.c; prefer-encrypt=yes; keydata=mDMEXEcE")
                .unwrap()
                .prefer_encrypt
        );
    }

    #[test]
    fn formats_and_round_trips() {
        let key: Vec<u8> = (0..300u16).map(|i| (i % 251) as u8).collect();
        let value = format("me@example.com", true, &key);
        assert!(value.starts_with("addr=me@example.com; prefer-encrypt=mutual; keydata="));
        let back = parse(&value).unwrap();
        assert_eq!(back.keydata, key);
        assert!(back.prefer_encrypt);
        let folded = fold("Autocrypt", &value);
        assert!(folded.lines().all(|l| l.trim_end_matches('\r').len() <= 78));
        assert!(folded.ends_with("\r\n"));
        let unfolded = folded.trim_start_matches("Autocrypt:").replace("\r\n", "");
        assert_eq!(parse(&unfolded).unwrap().keydata, key);
        let gossip = parse_gossip(&format_gossip("you@example.com", &key)).unwrap();
        assert_eq!(gossip.addr, "you@example.com");
        assert!(!gossip.prefer_encrypt);
        // prefer-encrypt is not a gossip attribute.
        assert!(parse_gossip("addr=a@b.c; prefer-encrypt=mutual; keydata=mDMEXEcE").is_none());
    }
}
