//! Name-based TZID resolution: IANA names (any case, vendor prefixes),
//! Windows zone ids, Outlook display-name TZIDs and UTC aliases.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::LazyLock;

use chrono::{NaiveDate, Offset, TimeZone};
use chrono_tz::{OffsetComponents, TZ_VARIANTS, Tz};

use super::windows::{LEGACY_DISPLAY_NAMES, LEGACY_WINDOWS_ZONES, WINDOWS_ZONES};

static IANA_LOWER: LazyLock<HashMap<String, Tz>> = LazyLock::new(|| {
    TZ_VARIANTS
        .iter()
        .map(|tz| (tz.name().to_ascii_lowercase(), *tz))
        .collect()
});

/// IANA zones keyed by their city component (`new york`, `buenos aires`).
static IANA_BY_CITY: LazyLock<HashMap<String, Vec<Tz>>> = LazyLock::new(|| {
    let mut map: HashMap<String, Vec<Tz>> = HashMap::new();
    for tz in TZ_VARIANTS.iter() {
        let name = tz.name();
        if !name.contains('/') || name.starts_with("Etc/") {
            continue;
        }
        if let Some(city) = name.rsplit('/').next() {
            map.entry(city.replace('_', " ").to_ascii_lowercase())
                .or_default()
                .push(*tz);
        }
    }
    map
});

static WINDOWS_LOWER: LazyLock<HashMap<String, Tz>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for zone in WINDOWS_ZONES {
        if let Ok(tz) = Tz::from_str(zone.iana) {
            map.insert(zone.id.to_ascii_lowercase(), tz);
        }
    }
    for (id, iana) in LEGACY_WINDOWS_ZONES {
        if let Ok(tz) = Tz::from_str(iana) {
            map.entry(id.to_ascii_lowercase()).or_insert(tz);
        }
    }
    map
});

/// A parsed display name: base offset and normalized city list.
struct DisplayEntry {
    offset: Option<i32>,
    cities: Vec<String>,
    key: String,
    tz: Tz,
}

static DISPLAY_NAMES: LazyLock<Vec<DisplayEntry>> = LazyLock::new(|| {
    let current = WINDOWS_ZONES.iter().map(|z| (z.display, z.iana));
    let legacy = LEGACY_DISPLAY_NAMES.iter().copied();
    current
        .chain(legacy)
        .filter_map(|(display, iana)| {
            let tz = Tz::from_str(iana).ok()?;
            let (offset, cities) = normalize_display(display);
            Some(DisplayEntry {
                key: display_key(offset, &cities),
                offset,
                cities,
                tz,
            })
        })
        .collect()
});

/// True for TZIDs that simply mean UTC (`UTC`, `Z`, `Etc/GMT`, …).
pub fn is_utc_alias(tzid: &str) -> bool {
    let t = tzid.trim().trim_matches('"').trim().to_ascii_lowercase();
    matches!(
        t.as_str(),
        "z" | "utc"
            | "uct"
            | "gmt"
            | "gmt0"
            | "gmt+0"
            | "gmt-0"
            | "utc+0"
            | "utc-0"
            | "utc+00:00"
            | "gmt+00:00"
            | "etc/utc"
            | "etc/uct"
            | "etc/gmt"
            | "etc/gmt0"
            | "etc/gmt+0"
            | "etc/gmt-0"
            | "etc/universal"
            | "etc/zulu"
            | "etc/greenwich"
            | "universal"
            | "zulu"
            | "greenwich"
            | "coordinated universal time"
            | "(utc) coordinated universal time"
            | "/utc"
    )
}

/// Resolves a TZID by name only, trying in order: exact IANA name,
/// case-insensitive IANA name, Windows zone id, vendor-prefixed IANA name
/// (`/mozilla.org/20050126_1/Europe/Berlin`, `/citadel.org/…`), Lotus Notes
/// style Windows names without the `Standard Time` suffix, and Outlook
/// display names (`(UTC+01:00) Amsterdam, Berlin, …`, also older `GMT`
/// spellings and localized city lists).
pub(crate) fn resolve_strong(tzid: &str) -> Option<Tz> {
    let t = tzid.trim().trim_matches('"').trim();
    if t.is_empty() {
        return None;
    }
    if is_utc_alias(t) {
        return Some(Tz::UTC);
    }
    if let Ok(tz) = Tz::from_str(t) {
        return Some(tz);
    }
    let lower = t.to_ascii_lowercase();
    if let Some(tz) = IANA_LOWER.get(&lower) {
        return Some(*tz);
    }
    if let Some(tz) = WINDOWS_LOWER.get(&lower) {
        return Some(*tz);
    }
    if lower.contains('/') {
        for (index, _) in lower.match_indices('/') {
            let suffix = &lower[index + 1..];
            if suffix.is_empty() {
                continue;
            }
            if let Some(tz) = IANA_LOWER.get(suffix) {
                return Some(*tz);
            }
        }
    }
    if let Some(tz) = WINDOWS_LOWER.get(&format!("{lower} standard time")) {
        return Some(*tz);
    }
    resolve_display(t)
}

/// Last-resort resolution of TZIDs that only carry an offset
/// (`(UTC+05:00) Unknown City`, `GMT+03:00`, `UTC-5`) to a fixed `Etc/GMT±N`
/// zone. Only whole-hour offsets can be represented.
pub(crate) fn resolve_offset_only(tzid: &str) -> Option<Tz> {
    let t = tzid.trim().trim_matches('"').trim();
    let lower = strip_marks(&t.to_lowercase());
    let offset = if lower.starts_with('(') {
        normalize_display(t).0
    } else {
        let rest = lower
            .strip_prefix("utc")
            .or_else(|| lower.strip_prefix("gmt"))?;
        parse_offset(rest.trim())
    }?;
    fixed_offset_zone(offset)
}

/// The `Etc/GMT±N` zone for a whole-hour offset (note the inverted sign).
pub(crate) fn fixed_offset_zone(offset_seconds: i32) -> Option<Tz> {
    if offset_seconds % 3_600 != 0 {
        return None;
    }
    let hours = offset_seconds / 3_600;
    let name = match hours {
        0 => "Etc/UTC".to_owned(),
        h if h > 0 => format!("Etc/GMT-{h}"),
        h => format!("Etc/GMT+{}", -h),
    };
    Tz::from_str(&name).ok()
}

/// Lower-cases and removes combining marks, so Turkish `İstanbul` matches
/// `istanbul` without a Unicode normalization library.
fn strip_marks(s: &str) -> String {
    s.chars()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect()
}

/// Parses `+01:00`, `+01.00`, `+0100`, `-5`, `` (empty → 0).
fn parse_offset(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let rest = rest.trim();
    let (hours, minutes) = if let Some((h, m)) = rest.split_once([':', '.']) {
        (h.trim().parse::<i32>().ok()?, m.trim().parse::<i32>().ok()?)
    } else if rest.len() == 4 && rest.bytes().all(|b| b.is_ascii_digit()) {
        (rest[..2].parse().ok()?, rest[2..].parse().ok()?)
    } else {
        (rest.parse::<i32>().ok()?, 0)
    };
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// Splits a display name into its offset and normalized city tokens.
fn normalize_display(display: &str) -> (Option<i32>, Vec<String>) {
    let lower = strip_marks(&display.trim().to_lowercase());
    let (offset, rest) = match (lower.strip_prefix('('), lower.find(')')) {
        (Some(_), Some(close)) => {
            let inner = lower[1..close].trim();
            let offset = inner
                .strip_prefix("utc")
                .or_else(|| inner.strip_prefix("gmt"))
                .and_then(parse_offset);
            (offset, &lower[close + 1..])
        }
        _ => (None, lower.as_str()),
    };
    let cities = rest
        .replace(['/', ';'], ",")
        .split(',')
        .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|c| !c.is_empty())
        .collect();
    (offset, cities)
}

fn display_key(offset: Option<i32>, cities: &[String]) -> String {
    format!("{}|{}", offset.unwrap_or(i32::MIN), cities.join(","))
}

/// Standard (non-DST) offset of a zone, in seconds.
fn base_offset(tz: Tz) -> Option<i32> {
    let reference = NaiveDate::from_ymd_opt(2025, 1, 15)?.and_hms_opt(12, 0, 0)?;
    let offset = tz.offset_from_utc_datetime(&reference);
    let base = offset.base_utc_offset().num_seconds();
    i32::try_from(base)
        .ok()
        .or_else(|| Some(offset.fix().local_minus_utc()))
}

fn resolve_display(tzid: &str) -> Option<Tz> {
    let (offset, cities) = normalize_display(tzid);
    if cities.is_empty() {
        return None;
    }
    let key = display_key(offset, &cities);
    if let Some(entry) = DISPLAY_NAMES.iter().find(|e| e.key == key) {
        return Some(entry.tz);
    }
    // Without an explicit offset, also accept the bare city part
    // ("Eastern Time (US & Canada)").
    if offset.is_none()
        && let Some(entry) = DISPLAY_NAMES.iter().find(|e| e.cities == cities)
    {
        return Some(entry.tz);
    }
    // Only treat strings that look like display names (offset prefix or a
    // city list) as such; anything else is not a display name.
    if offset.is_none() && cities.len() < 2 {
        return resolve_city(&cities, None);
    }
    // City-token match against known display names (handles localized and
    // re-worded names): prefer entries whose offset agrees.
    let mut best: Option<(usize, Tz)> = None;
    for entry in DISPLAY_NAMES.iter() {
        let shared = entry.cities.iter().filter(|c| cities.contains(c)).count();
        if shared == 0 {
            continue;
        }
        let score = shared * 2 + usize::from(offset.is_some() && entry.offset == offset) * 100;
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, entry.tz));
        }
    }
    if let Some((score, tz)) = best
        && (offset.is_none() || score >= 100)
    {
        return Some(tz);
    }
    resolve_city(&cities, offset).or(best.map(|(_, tz)| tz))
}

/// Finds an IANA zone named after one of the cities, preferring one whose
/// standard offset equals `offset`.
fn resolve_city(cities: &[String], offset: Option<i32>) -> Option<Tz> {
    let mut fallback = None;
    for city in cities {
        let Some(candidates) = IANA_BY_CITY.get(city.as_str()) else {
            continue;
        };
        for tz in candidates {
            if offset.is_none() || base_offset(*tz) == offset {
                return Some(*tz);
            }
            fallback.get_or_insert(*tz);
        }
    }
    fallback
}

/// Maps a Windows zone id (`Pacific Standard Time`) to its primary IANA zone.
pub fn windows_to_iana(windows_id: &str) -> Option<Tz> {
    WINDOWS_LOWER
        .get(&windows_id.trim().to_ascii_lowercase())
        .copied()
}

/// Maps an IANA zone to the Windows zone id whose primary zone it is
/// (`Europe/Berlin` → `W. Europe Standard Time`). Common aliases of the
/// canonical names are understood too.
pub fn iana_to_windows(tz: Tz) -> Option<&'static str> {
    let name = match tz.name() {
        "Asia/Calcutta" => "Asia/Kolkata",
        "Asia/Katmandu" => "Asia/Kathmandu",
        "Asia/Rangoon" => "Asia/Yangon",
        "Europe/Kiev" => "Europe/Kyiv",
        "America/Godthab" => "America/Nuuk",
        "America/Buenos_Aires" => "America/Argentina/Buenos_Aires",
        "America/Indianapolis" => "America/Indiana/Indianapolis",
        "Asia/Istanbul" | "Turkey" => "Europe/Istanbul",
        "UTC" | "Etc/UCT" | "Etc/Universal" | "Etc/Zulu" | "Universal" | "Zulu" | "GMT"
        | "Etc/GMT" => "Etc/UTC",
        "US/Eastern" => "America/New_York",
        "US/Central" => "America/Chicago",
        "US/Mountain" => "America/Denver",
        "US/Pacific" => "America/Los_Angeles",
        other => other,
    };
    WINDOWS_ZONES.iter().find(|z| z.iana == name).map(|z| z.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_entry_is_a_known_zone() {
        for zone in WINDOWS_ZONES {
            assert!(
                Tz::from_str(zone.iana).is_ok(),
                "{} -> {}",
                zone.id,
                zone.iana
            );
            assert_eq!(windows_to_iana(zone.id).map(|t| t.name()), Some(zone.iana));
            let expected = if is_utc_alias(zone.display) {
                "UTC"
            } else {
                zone.iana
            };
            assert_eq!(
                resolve_strong(zone.display).map(|t| t.name()),
                Some(expected),
                "{}",
                zone.display
            );
        }
        for (id, iana) in LEGACY_WINDOWS_ZONES {
            assert!(Tz::from_str(iana).is_ok(), "{id} -> {iana}");
        }
        for (display, iana) in LEGACY_DISPLAY_NAMES {
            assert_eq!(
                resolve_strong(display).map(|t| t.name()),
                Some(*iana),
                "{display}"
            );
        }
        assert_eq!(WINDOWS_ZONES.len(), 142);
    }

    #[test]
    fn iana_names_in_any_case_and_with_prefixes() {
        assert_eq!(resolve_strong("Europe/Berlin"), Some(Tz::Europe__Berlin));
        assert_eq!(resolve_strong("europe/berlin"), Some(Tz::Europe__Berlin));
        assert_eq!(
            resolve_strong(" \"America/New_York\" "),
            Some(Tz::America__New_York)
        );
        assert_eq!(
            resolve_strong("/mozilla.org/20050126_1/Europe/Berlin"),
            Some(Tz::Europe__Berlin)
        );
        assert_eq!(
            resolve_strong("/citadel.org/20190914_1/America/Argentina/Buenos_Aires"),
            Some(Tz::America__Argentina__Buenos_Aires)
        );
        assert_eq!(
            resolve_strong("/softwarestudio.org/Olson_20011030_5/America/New_York"),
            Some(Tz::America__New_York)
        );
        assert_eq!(resolve_strong("Asia/Calcutta"), Some(Tz::Asia__Calcutta));
        assert_eq!(resolve_strong(""), None);
        assert_eq!(resolve_strong("Not/AZone"), None);
    }

    #[test]
    fn windows_names() {
        assert_eq!(
            resolve_strong("W. Europe Standard Time"),
            Some(Tz::Europe__Berlin)
        );
        assert_eq!(
            resolve_strong("Turkey Standard Time"),
            Some(Tz::Europe__Istanbul)
        );
        assert_eq!(
            resolve_strong("Pacific Standard Time"),
            Some(Tz::America__Los_Angeles)
        );
        assert_eq!(
            resolve_strong("pacific standard time"),
            Some(Tz::America__Los_Angeles)
        );
        assert_eq!(
            resolve_strong("India Standard Time"),
            Some(Tz::Asia__Kolkata)
        );
        assert_eq!(
            resolve_strong("Mid-Atlantic Standard Time").map(|t| t.name()),
            Some("Etc/GMT+2")
        );
        // Lotus Notes style without the suffix.
        assert_eq!(resolve_strong("W. Europe"), Some(Tz::Europe__Berlin));
        assert_eq!(resolve_strong("Eastern"), Some(Tz::America__New_York));
        assert_eq!(
            iana_to_windows(Tz::Europe__Berlin),
            Some("W. Europe Standard Time")
        );
        assert_eq!(
            iana_to_windows(Tz::Asia__Calcutta),
            Some("India Standard Time")
        );
        assert_eq!(
            iana_to_windows(Tz::Europe__Istanbul),
            Some("Turkey Standard Time")
        );
        assert_eq!(iana_to_windows(Tz::UTC), Some("UTC"));
        assert_eq!(iana_to_windows(Tz::Europe__Vaduz), None);
    }

    #[test]
    fn outlook_display_names() {
        assert_eq!(
            resolve_strong("(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna"),
            Some(Tz::Europe__Berlin)
        );
        assert_eq!(
            resolve_strong("(GMT+01.00) Amsterdam / Berlin / Bern / Rome / Stockholm / Vienna"),
            Some(Tz::Europe__Berlin)
        );
        assert_eq!(
            resolve_strong("(UTC+03:00) Istanbul"),
            Some(Tz::Europe__Istanbul)
        );
        assert_eq!(
            resolve_strong("(UTC+03:00) İstanbul"),
            Some(Tz::Europe__Istanbul)
        );
        assert_eq!(
            resolve_strong("(UTC-05:00) Eastern Time (US & Canada)"),
            Some(Tz::America__New_York)
        );
        assert_eq!(
            resolve_strong("Eastern Time (US & Canada)"),
            Some(Tz::America__New_York)
        );
        // German Outlook.
        assert_eq!(
            resolve_strong("(UTC+01:00) Amsterdam, Berlin, Bern, Rom, Stockholm, Wien"),
            Some(Tz::Europe__Berlin)
        );
        // Unknown wording but a known IANA city.
        assert_eq!(resolve_strong("(UTC+01:00) Oslo"), Some(Tz::Europe__Oslo));
        // Older London display name.
        assert_eq!(
            resolve_strong("(GMT) Dublin, Edinburgh, Lisbon, London"),
            Some(Tz::Europe__London)
        );
    }

    #[test]
    fn offset_only_fallback() {
        assert_eq!(resolve_strong("(UTC+05:00) Nowhere"), None);
        assert_eq!(
            resolve_offset_only("(UTC+05:00) Nowhere").map(|t| t.name()),
            Some("Etc/GMT-5")
        );
        assert_eq!(
            resolve_offset_only("GMT-03:00").map(|t| t.name()),
            Some("Etc/GMT+3")
        );
        assert_eq!(
            resolve_offset_only("UTC+3").map(|t| t.name()),
            Some("Etc/GMT-3")
        );
        assert_eq!(resolve_offset_only("(UTC+05:30) Nowhere"), None);
        assert_eq!(resolve_offset_only("Customized Time Zone"), None);
    }

    #[test]
    fn utc_aliases() {
        for alias in [
            "UTC",
            "Z",
            "Etc/UTC",
            "GMT",
            "utc",
            "Coordinated Universal Time",
        ] {
            assert!(is_utc_alias(alias), "{alias}");
            assert_eq!(resolve_strong(alias), Some(Tz::UTC));
        }
        assert!(!is_utc_alias("GMT Standard Time"));
        assert_eq!(
            resolve_strong("GMT Standard Time"),
            Some(Tz::Europe__London)
        );
    }
}
