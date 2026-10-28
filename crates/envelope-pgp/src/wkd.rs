//! Web Key Directory (draft-koch-openpgp-webkey-service): where a domain
//! publishes the keys of its addresses.

use sha1::{Digest, Sha1};

const ZBASE32: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// z-base-32 encoding (as used by WKD).
pub fn zbase32(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 8 / 5 + 1);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ZBASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ZBASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The hashed local part: z-base-32 of the SHA-1 of the local part with
/// ASCII letters lower-cased.
pub fn local_hash(local: &str) -> String {
    let lowered = local.to_ascii_lowercase();
    zbase32(&Sha1::digest(lowered.as_bytes()))
}

/// The two places to look for the key of an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Urls {
    /// `https://openpgpkey.<domain>/.well-known/openpgpkey/<domain>/hu/…`
    pub advanced: String,
    /// `https://<domain>/.well-known/openpgpkey/hu/…`
    pub direct: String,
}

/// WKD addresses for an e-mail address; `None` when it is not one.
pub fn urls(email: &str) -> Option<Urls> {
    let (local, domain) = email.trim().rsplit_once('@')?;
    let domain = domain.trim().to_ascii_lowercase();
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
    {
        return None;
    }
    let hash = local_hash(local);
    let l = percent_encode(local);
    Some(Urls {
        advanced: format!(
            "https://openpgpkey.{domain}/.well-known/openpgpkey/{domain}/hu/{hash}?l={l}"
        ),
        direct: format!("https://{domain}/.well-known/openpgpkey/hu/{hash}?l={l}"),
    })
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The keys.openpgp.org address for looking up a key by e-mail.
pub fn keys_openpgp_org(email: &str) -> String {
    format!(
        "https://keys.openpgp.org/vks/v1/by-email/{}",
        percent_encode(email.trim())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_the_specification() {
        // The example from draft-koch-openpgp-webkey-service, section 3.1.
        assert_eq!(local_hash("Joe.Doe"), "iy9q119eutrkn8s1mk4r39qejnbu3n5q");
        let u = urls("Joe.Doe@Example.ORG").unwrap();
        assert_eq!(
            u.advanced,
            "https://openpgpkey.example.org/.well-known/openpgpkey/example.org/hu/iy9q119eutrkn8s1mk4r39qejnbu3n5q?l=Joe.Doe"
        );
        assert_eq!(
            u.direct,
            "https://example.org/.well-known/openpgpkey/hu/iy9q119eutrkn8s1mk4r39qejnbu3n5q?l=Joe.Doe"
        );
    }

    #[test]
    fn zbase32_vectors() {
        assert_eq!(zbase32(b""), "");
        // A 160-bit digest always encodes to 32 characters.
        assert_eq!(local_hash("x").len(), 32);
        // Known short vectors (bits padded with zeros).
        assert_eq!(zbase32(&[0x00]), "yy");
        assert_eq!(zbase32(&[0xff]), "9h");
        assert_eq!(zbase32(&[0xf0, 0xbf, 0xc7]), "6n9hq");
    }

    #[test]
    fn rejects_non_addresses() {
        assert!(urls("nobody").is_none());
        assert!(urls("a@localhost").is_none());
        assert!(urls("a@exa mple.org").is_none());
        assert_eq!(
            urls("a+tag@example.org").unwrap().direct,
            format!(
                "https://example.org/.well-known/openpgpkey/hu/{}?l=a%2Btag",
                local_hash("a+tag")
            )
        );
        assert_eq!(
            keys_openpgp_org("x+y@example.org"),
            "https://keys.openpgp.org/vks/v1/by-email/x%2By%40example.org"
        );
    }
}
