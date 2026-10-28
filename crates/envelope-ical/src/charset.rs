//! Minimal legacy charset decoding for old calendar files and vCard 2.1
//! `CHARSET=` parameters. Only the single-byte charsets commonly found in
//! address books and calendars are supported; everything else is decoded as
//! UTF-8 with a Windows-1252 fallback.

/// Windows-1252 code points for bytes 0x80..=0x9F (0 = undefined → U+FFFD).
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    Utf8,
    Latin1,
    Windows1252,
    Latin5,
    Windows1254,
    Latin9,
}

fn charset_from_name(name: &str) -> Option<Charset> {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match normalized.as_str() {
        "utf8" | "usascii" | "ascii" => Charset::Utf8,
        "iso88591" | "latin1" | "l1" => Charset::Latin1,
        "windows1252" | "cp1252" | "xansi" | "ansi" => Charset::Windows1252,
        "iso88599" | "latin5" | "l5" => Charset::Latin5,
        "windows1254" | "cp1254" => Charset::Windows1254,
        "iso885915" | "latin9" => Charset::Latin9,
        _ => return None,
    })
}

/// Decodes `bytes` according to the charset `name` (case and punctuation
/// insensitive). Unknown charsets are treated like UTF-8 with a Windows-1252
/// fallback, which is what most producers actually meant.
pub(crate) fn decode(bytes: &[u8], name: Option<&str>) -> String {
    match name.and_then(charset_from_name).unwrap_or(Charset::Utf8) {
        Charset::Utf8 => decode_utf8_or_cp1252(bytes),
        charset => bytes.iter().map(|&b| single_byte(b, charset)).collect(),
    }
}

/// UTF-8 if the bytes are valid UTF-8, Windows-1252 otherwise.
pub(crate) fn decode_utf8_or_cp1252(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes
            .iter()
            .map(|&b| single_byte(b, Charset::Windows1252))
            .collect(),
    }
}

fn single_byte(byte: u8, charset: Charset) -> char {
    let turkish = |b: u8| -> Option<char> {
        Some(match b {
            0xD0 => 'Ğ',
            0xDD => 'İ',
            0xDE => 'Ş',
            0xF0 => 'ğ',
            0xFD => 'ı',
            0xFE => 'ş',
            _ => return None,
        })
    };
    let cp1252 = |b: u8| -> char {
        if (0x80..=0x9F).contains(&b) {
            let code = CP1252_HIGH[usize::from(b - 0x80)];
            if code == 0 {
                char::REPLACEMENT_CHARACTER
            } else {
                char::from_u32(u32::from(code)).unwrap_or(char::REPLACEMENT_CHARACTER)
            }
        } else {
            char::from(b)
        }
    };
    match charset {
        Charset::Utf8 | Charset::Latin1 => char::from(byte),
        Charset::Windows1252 => cp1252(byte),
        Charset::Latin5 => turkish(byte).unwrap_or(char::from(byte)),
        Charset::Windows1254 => turkish(byte).unwrap_or_else(|| match byte {
            // Windows-1254 differs from 1252 in a few positions of the 0x80 block too.
            0x8E | 0x9E => char::REPLACEMENT_CHARACTER,
            _ => cp1252(byte),
        }),
        Charset::Latin9 => match byte {
            0xA4 => '€',
            0xA6 => 'Š',
            0xA8 => 'š',
            0xB4 => 'Ž',
            0xB8 => 'ž',
            0xBC => 'Œ',
            0xBD => 'œ',
            0xBE => 'Ÿ',
            other => char::from(other),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_known_charsets() {
        assert_eq!(decode(b"M\xfcller", Some("ISO-8859-1")), "Müller");
        assert_eq!(decode(b"\x80 5", Some("windows-1252")), "€ 5");
        assert_eq!(decode(b"\xdeeker \xfd\xf0", Some("ISO-8859-9")), "Şeker ığ");
        assert_eq!(decode(b"\xa4", Some("latin9")), "€");
        assert_eq!(decode("Ölçü".as_bytes(), Some("UTF-8")), "Ölçü");
    }

    #[test]
    fn invalid_utf8_falls_back_to_cp1252() {
        assert_eq!(decode(b"caf\xe9", None), "café");
        assert_eq!(decode(b"caf\xe9", Some("x-unknown")), "café");
    }
}
