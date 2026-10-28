//! Turning Graph `iCalUId` values into series-wide iCalendar UIDs.
//!
//! Graph exposes Exchange's *global object id* (`PidLidGlobalObjectId`,
//! [MS-OXOCAL] 2.2.1.27) hex-encoded as `iCalUId`. Its layout is:
//!
//! | bytes  | field                                                 |
//! |--------|-------------------------------------------------------|
//! | 0–15   | class id `04 00 00 00 82 00 E0 00 74 C5 B7 10 1A 82 E0 08` |
//! | 16–19  | instance date (year hi, year lo, month, day); zero unless the item is an occurrence |
//! | 20–27  | creation time (FILETIME)                              |
//! | 28–35  | reserved, zero                                        |
//! | 36–39  | size of the data (little endian)                      |
//! | 40–    | data                                                  |
//!
//! Because the instance date is part of the id, every occurrence of a series
//! has a different `iCalUId`. The series-wide value is the *clean* global
//! object id with the date bytes zeroed. When the meeting came from another
//! calendar system the data holds `vCal-Uid`, `01 00 00 00` and the original
//! UTF-8 UID, which Exchange exports as the iCalendar `UID`
//! ([MS-OXCICAL] 2.1.3.1.1.20.26). [`series_uid`] applies the same rules.

/// Hex prefix (class id) of Exchange global object ids.
const GOID_CLASS_ID: &str = "040000008200E00074C5B7101A82E008";

/// Marker that precedes a third-party UID inside the global object id data.
const VCAL_UID_MARKER: &[u8] = b"vCal-Uid\x01\x00\x00\x00";

/// Returns the series-wide iCalendar UID for an Exchange `iCalUId`.
///
/// * third-party UIDs embedded in the id are returned as-is (so they match
///   the `UID` of the original invitation);
/// * otherwise the instance date is zeroed, so occurrences and exceptions
///   share their series master's UID;
/// * values that are not Exchange global object ids are returned unchanged.
pub fn series_uid(ical_uid: &str) -> String {
    let trimmed = ical_uid.trim();
    let Some(bytes) = decode_goid(trimmed) else {
        return trimmed.to_owned();
    };
    if let Some(uid) = embedded_uid(&bytes) {
        return uid;
    }
    let mut clean = String::with_capacity(trimmed.len());
    clean.push_str(&trimmed[..32]);
    clean.push_str("00000000");
    clean.push_str(&trimmed[40..]);
    clean
}

/// True when the value looks like an Exchange global object id.
pub fn is_global_object_id(value: &str) -> bool {
    decode_goid(value.trim()).is_some()
}

fn decode_goid(hex: &str) -> Option<Vec<u8>> {
    if hex.len() < 80
        || !hex.len().is_multiple_of(2)
        || !hex.is_char_boundary(32)
        || !hex[..32].eq_ignore_ascii_case(GOID_CLASS_ID)
    {
        return None;
    }
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        })
        .collect()
}

fn embedded_uid(bytes: &[u8]) -> Option<String> {
    let size_bytes: [u8; 4] = bytes.get(36..40)?.try_into().ok()?;
    let size = usize::try_from(u32::from_le_bytes(size_bytes)).ok()?;
    let data = bytes.get(40..40usize.checked_add(size)?)?;
    let uid = data.strip_prefix(VCAL_UID_MARKER)?;
    let uid = match uid.iter().position(|b| *b == 0) {
        Some(nul) => &uid[..nul],
        None => uid,
    };
    let uid = std::str::from_utf8(uid).ok()?.trim();
    (!uid.is_empty()).then(|| uid.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A series master id (instance date zero).
    const MASTER: &str = "040000008200E00074C5B7101A82E00800000000D3D70B8A1A5AD301000000000000000010000000E5DA5F0F18E5AB4AA6E6E6B79C1C2A4E";
    /// The same series, occurrence of 2017-05-14 (07E1 05 0E).
    const OCCURRENCE: &str = "040000008200E00074C5B7101A82E00807E1050ED3D70B8A1A5AD301000000000000000010000000E5DA5F0F18E5AB4AA6E6E6B79C1C2A4E";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    #[test]
    fn occurrences_share_the_master_uid() {
        assert!(is_global_object_id(OCCURRENCE));
        assert_eq!(series_uid(OCCURRENCE), MASTER);
        assert_eq!(series_uid(MASTER), MASTER);
    }

    #[test]
    fn extracts_third_party_uids() {
        let uid = "7kukuqrfedlm2f9t0vr42q2qc8@google.com";
        let mut data = VCAL_UID_MARKER.to_vec();
        data.extend_from_slice(uid.as_bytes());
        data.push(0);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[
            0x04, 0x00, 0x00, 0x00, 0x82, 0x00, 0xE0, 0x00, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82,
            0xE0, 0x08,
        ]);
        bytes.extend_from_slice(&[0x07, 0xE8, 0x03, 0x04]); // an occurrence on 2024-03-04
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
        bytes.extend_from_slice(&data);
        assert_eq!(series_uid(&hex(&bytes)), uid);
    }

    #[test]
    fn leaves_other_values_alone() {
        assert_eq!(series_uid("abc@example.com"), "abc@example.com");
        assert_eq!(
            series_uid("040000008200E00074C5B7101A82E008"),
            "040000008200E00074C5B7101A82E008"
        );
        let not_hex = format!("{}{}", GOID_CLASS_ID, "Z".repeat(60));
        assert_eq!(series_uid(&not_hex), not_hex);
    }
}
