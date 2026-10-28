//! Typed value parsing and formatting: DATE, DATE-TIME, DURATION, PERIOD,
//! UTC-OFFSET (RFC 5545 §3.3) and the [`EventTime`] ↔ property mapping.
//!
//! All parsers are lenient: they accept the extended ISO 8601 forms
//! (`2024-01-31T09:00:00`), missing seconds, fractional seconds, lowercase
//! designators and surrounding whitespace, and return `None` instead of
//! failing on garbage.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Utc};

use crate::component::Property;
use crate::model::EventTime;

/// Parses a DATE value (`20240131` or `2024-01-31`).
pub fn parse_date(value: &str) -> Option<NaiveDate> {
    let digits: String = value.trim().chars().filter(|c| *c != '-').collect();
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year = digits[0..4].parse().ok()?;
    let month = digits[4..6].parse().ok()?;
    let day = digits[6..8].parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
}

/// Parses a DATE-TIME value. Returns the wall-clock time and whether it was
/// marked as UTC (trailing `Z`).
pub fn parse_date_time(value: &str) -> Option<(NaiveDateTime, bool)> {
    let value = value.trim();
    let (body, utc) = match value.strip_suffix(['Z', 'z']) {
        Some(body) => (body, true),
        None => (value, false),
    };
    let (date_part, time_part) = body.split_once(['T', 't'])?;
    let date = parse_date(date_part)?;
    let time_digits: String = time_part
        .split(['.', ','])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| *c != ':')
        .collect();
    if !time_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hour, minute, second) = match time_digits.len() {
        6 => (
            time_digits[0..2].parse().ok()?,
            time_digits[2..4].parse().ok()?,
            time_digits[4..6].parse::<u32>().ok()?,
        ),
        4 => (
            time_digits[0..2].parse().ok()?,
            time_digits[2..4].parse().ok()?,
            0,
        ),
        2 => (time_digits[0..2].parse().ok()?, 0, 0),
        _ => return None,
    };
    // Leap seconds (":60") are clamped; 24:00:00 means the next midnight.
    let second = second.min(59);
    let date_time = if hour == 24 && minute == 0 && second == 0 {
        date.succ_opt()?.and_time(NaiveTime::MIN)
    } else {
        date.and_time(NaiveTime::from_hms_opt(hour, minute, second)?)
    };
    Some((date_time, utc))
}

/// Formats a DATE value (`YYYYMMDD`).
pub fn format_date(date: NaiveDate) -> String {
    date.format("%Y%m%d").to_string()
}

/// Formats a local DATE-TIME value (`YYYYMMDDTHHMMSS`).
pub fn format_date_time(local: NaiveDateTime) -> String {
    local.format("%Y%m%dT%H%M%S").to_string()
}

/// Formats a UTC DATE-TIME value (`YYYYMMDDTHHMMSSZ`).
pub fn format_utc(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Parses a DURATION (`-PT15M`, `P1D`, `P2W`, `+PT1H30M`, …) into a signed
/// [`TimeDelta`]. Days and weeks are treated as 24 h / 7 × 24 h; callers add
/// the result to wall-clock times so nominal days stay nominal.
pub fn parse_duration(value: &str) -> Option<TimeDelta> {
    let value = value.trim();
    let (negative, rest) = match value.as_bytes().first()? {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let mut seconds: i64 = 0;
    let mut number = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in rest.chars() {
        match c {
            '0'..='9' => number.push(c),
            'T' | 't' => in_time = true,
            _ => {
                if number.is_empty() {
                    return None;
                }
                let n: i64 = number.parse().ok()?;
                number.clear();
                let unit: i64 = match (c.to_ascii_uppercase(), in_time) {
                    ('W', _) => 7 * 86_400,
                    ('D', _) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    // Tolerate a missing 'T' for hours ("P1H").
                    ('H', false) => 3_600,
                    _ => return None,
                };
                seconds = seconds.checked_add(n.checked_mul(unit)?)?;
                any = true;
            }
        }
    }
    if !number.is_empty() || !any {
        return None;
    }
    let seconds = if negative { -seconds } else { seconds };
    TimeDelta::try_seconds(seconds)
}

/// Formats a signed duration in the most compact RFC 5545 form.
pub fn format_duration(duration: TimeDelta) -> String {
    let total = duration.num_seconds();
    let sign = if total < 0 { "-" } else { "" };
    let mut rest = total.unsigned_abs();
    if rest == 0 {
        return "PT0S".to_owned();
    }
    if rest.is_multiple_of(7 * 86_400) {
        return format!("{sign}P{}W", rest / (7 * 86_400));
    }
    let mut out = format!("{sign}P");
    let days = rest / 86_400;
    rest %= 86_400;
    if days > 0 {
        out.push_str(&format!("{days}D"));
    }
    if rest > 0 {
        out.push('T');
        let hours = rest / 3_600;
        let minutes = (rest % 3_600) / 60;
        let secs = rest % 60;
        if hours > 0 {
            out.push_str(&format!("{hours}H"));
        }
        if minutes > 0 {
            out.push_str(&format!("{minutes}M"));
        }
        if secs > 0 {
            out.push_str(&format!("{secs}S"));
        }
    }
    out
}

/// Parses a UTC-OFFSET (`+0100`, `-0530`, `+013045`, tolerant of `+01:00`
/// and `+01`) into seconds east of UTC.
pub fn parse_utc_offset(value: &str) -> Option<i32> {
    let value = value.trim();
    let (sign, rest) = match value.as_bytes().first()? {
        b'-' => (-1, &value[1..]),
        b'+' => (1, &value[1..]),
        _ => (1, value),
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes, seconds): (i32, i32, i32) = match digits.len() {
        2 => (digits.parse().ok()?, 0, 0),
        4 => (digits[0..2].parse().ok()?, digits[2..4].parse().ok()?, 0),
        6 => (
            digits[0..2].parse().ok()?,
            digits[2..4].parse().ok()?,
            digits[4..6].parse().ok()?,
        ),
        _ => return None,
    };
    if hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60 + seconds))
}

/// Formats seconds east of UTC as a UTC-OFFSET (`+0100`, `-033000` keeps seconds only when needed).
pub fn format_utc_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    let (h, m, s) = (abs / 3_600, (abs % 3_600) / 60, abs % 60);
    if s == 0 {
        format!("{sign}{h:02}{m:02}")
    } else {
        format!("{sign}{h:02}{m:02}{s:02}")
    }
}

/// Parses a single date or date-time value in the context of a property's
/// `VALUE` and `TZID` parameters. `tzid` is kept verbatim; resolving it is
/// the job of the [`crate::tz`] module.
pub fn parse_event_time(
    value: &str,
    value_type: Option<&str>,
    tzid: Option<&str>,
) -> Option<EventTime> {
    let value = value.trim();
    let wants_date = value_type.is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    if wants_date || !value.contains(['T', 't']) {
        if let Some(date) = parse_date(value) {
            return Some(EventTime::Date { date });
        }
        if wants_date {
            // VALUE=DATE but a date-time was written: keep the date part.
            return parse_date_time(value).map(|(dt, _)| EventTime::Date { date: dt.date() });
        }
        return None;
    }
    let (local, utc) = parse_date_time(value)?;
    if utc {
        return Some(EventTime::Utc {
            at: local.and_utc(),
        });
    }
    match tzid.map(str::trim).filter(|t| !t.is_empty()) {
        Some(tzid) => Some(EventTime::Zoned {
            local,
            tzid: tzid.to_owned(),
        }),
        None => Some(EventTime::Floating { local }),
    }
}

/// Reads the single [`EventTime`] carried by a property such as `DTSTART`.
pub fn property_time(property: &Property) -> Option<EventTime> {
    let first = property.value.split(',').next().unwrap_or("");
    // PERIOD values are tolerated: only the start matters.
    let first = first.split('/').next().unwrap_or("");
    parse_event_time(first, property.param("VALUE"), property.param("TZID"))
}

/// Reads every [`EventTime`] of a multi-valued property (`RDATE`, `EXDATE`).
/// PERIOD values (`start/end` or `start/duration`) contribute their start.
pub fn property_times(property: &Property) -> Vec<EventTime> {
    let value_type = property
        .param("VALUE")
        .filter(|v| !v.eq_ignore_ascii_case("PERIOD"));
    property
        .value
        .split(',')
        .filter_map(|item| {
            let start = item.split('/').next().unwrap_or("");
            parse_event_time(start, value_type, property.param("TZID"))
        })
        .collect()
}

/// Builds a property (e.g. `DTSTART`) for an [`EventTime`].
pub fn time_property(name: &str, time: &EventTime) -> Property {
    match time {
        EventTime::Date { date } => {
            Property::new(name, format_date(*date)).with_param("VALUE", "DATE")
        }
        EventTime::Zoned { local, tzid } => {
            if crate::tz::is_utc_alias(tzid) {
                Property::new(name, format_utc(local.and_utc()))
            } else {
                Property::new(name, format_date_time(*local)).with_param("TZID", tzid.clone())
            }
        }
        EventTime::Utc { at } => Property::new(name, format_utc(*at)),
        EventTime::Floating { local } => Property::new(name, format_date_time(*local)),
    }
}

/// Formats an [`EventTime`] value without parameters (as used inside RRULE or error messages).
pub fn format_event_time(time: &EventTime) -> String {
    match time {
        EventTime::Date { date } => format_date(*date),
        EventTime::Zoned { local, .. } | EventTime::Floating { local } => format_date_time(*local),
        EventTime::Utc { at } => format_utc(*at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").expect("valid test datetime")
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("20240229"), NaiveDate::from_ymd_opt(2024, 2, 29));
        assert_eq!(
            parse_date("2024-02-29"),
            NaiveDate::from_ymd_opt(2024, 2, 29)
        );
        assert_eq!(parse_date("20230229"), None);
        assert_eq!(parse_date("2024"), None);
        assert_eq!(parse_date("abcdefgh"), None);
    }

    #[test]
    fn date_times() {
        assert_eq!(
            parse_date_time("20240131T090000Z"),
            Some((dt("2024-01-31 09:00:00"), true))
        );
        assert_eq!(
            parse_date_time("20240131T0900"),
            Some((dt("2024-01-31 09:00:00"), false))
        );
        assert_eq!(
            parse_date_time("2024-01-31T09:00:00.123z"),
            Some((dt("2024-01-31 09:00:00"), true))
        );
        assert_eq!(
            parse_date_time("20240131T235960"),
            Some((dt("2024-01-31 23:59:59"), false))
        );
        assert_eq!(
            parse_date_time("20240131T240000"),
            Some((dt("2024-02-01 00:00:00"), false))
        );
        assert_eq!(parse_date_time("20240131"), None);
        assert_eq!(parse_date_time("20240131T25"), None);
        assert_eq!(parse_date_time("T"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("-PT15M"), TimeDelta::try_minutes(-15));
        assert_eq!(parse_duration("P1D"), TimeDelta::try_days(1));
        assert_eq!(parse_duration("P2W"), TimeDelta::try_days(14));
        assert_eq!(parse_duration("+PT1H30M"), TimeDelta::try_minutes(90));
        assert_eq!(
            parse_duration("P1DT2H3M4S"),
            TimeDelta::try_seconds(86_400 + 7_384)
        );
        assert_eq!(parse_duration("PT0S"), TimeDelta::try_seconds(0));
        assert_eq!(parse_duration("p1w2d"), TimeDelta::try_days(9));
        assert_eq!(parse_duration("P"), None);
        assert_eq!(parse_duration("PT"), None);
        assert_eq!(parse_duration("15M"), None);
        assert_eq!(parse_duration("P99999999999999999999D"), None);
        assert_eq!(parse_duration("P9999999999999W"), None);
    }

    #[test]
    fn duration_formatting_round_trips() {
        for text in [
            "PT0S",
            "-PT15M",
            "P1D",
            "P2W",
            "P1DT2H3M4S",
            "-P1W",
            "PT1H30M",
        ] {
            let parsed = parse_duration(text).expect("valid duration");
            assert_eq!(
                parse_duration(&format_duration(parsed)),
                Some(parsed),
                "{text}"
            );
        }
        assert_eq!(format_duration(TimeDelta::minutes(-15)), "-PT15M");
        assert_eq!(format_duration(TimeDelta::days(14)), "P2W");
    }

    #[test]
    fn utc_offsets() {
        assert_eq!(parse_utc_offset("+0100"), Some(3_600));
        assert_eq!(parse_utc_offset("-0530"), Some(-19_800));
        assert_eq!(parse_utc_offset("+01:00"), Some(3_600));
        assert_eq!(parse_utc_offset("+005328"), Some(3_208));
        assert_eq!(parse_utc_offset("+2500"), None);
        assert_eq!(format_utc_offset(-19_800), "-0530");
        assert_eq!(format_utc_offset(3_208), "+005328");
        assert_eq!(format_utc_offset(0), "+0000");
    }

    #[test]
    fn event_times() {
        assert_eq!(
            parse_event_time("20240101", None, None),
            Some(EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 1, 1).expect("date")
            })
        );
        assert_eq!(
            parse_event_time("20240101T100000", Some("DATE"), None),
            Some(EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 1, 1).expect("date")
            })
        );
        assert_eq!(
            parse_event_time("20240101T100000", None, Some("Europe/Berlin")),
            Some(EventTime::Zoned {
                local: dt("2024-01-01 10:00:00"),
                tzid: "Europe/Berlin".into()
            })
        );
        assert_eq!(
            parse_event_time("20240101T100000Z", None, Some("Europe/Berlin")),
            Some(EventTime::Utc {
                at: dt("2024-01-01 10:00:00").and_utc()
            })
        );
        assert_eq!(
            parse_event_time("20240101T100000", None, None),
            Some(EventTime::Floating {
                local: dt("2024-01-01 10:00:00")
            })
        );
    }

    #[test]
    fn multi_valued_and_period_properties() {
        let p = Property::new(
            "RDATE",
            "19970101T180000Z/19970102T070000Z,19970109T180000Z/PT5H30M",
        )
        .with_param("VALUE", "PERIOD");
        let times = property_times(&p);
        assert_eq!(times.len(), 2);
        assert_eq!(
            times[1],
            EventTime::Utc {
                at: dt("1997-01-09 18:00:00").and_utc()
            }
        );

        let p = Property::new("EXDATE", "20240101T090000,20240108T090000")
            .with_param("TZID", "Europe/Istanbul");
        assert_eq!(property_times(&p).len(), 2);
    }

    #[test]
    fn time_property_round_trip() {
        let times = [
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 3, 1).expect("date"),
            },
            EventTime::Zoned {
                local: dt("2024-03-01 09:30:00"),
                tzid: "America/New_York".into(),
            },
            EventTime::Utc {
                at: dt("2024-03-01 09:30:00").and_utc(),
            },
            EventTime::Floating {
                local: dt("2024-03-01 09:30:00"),
            },
        ];
        for time in times {
            let p = time_property("DTSTART", &time);
            assert_eq!(property_time(&p), Some(time));
        }
        let utc_zoned = EventTime::Zoned {
            local: dt("2024-03-01 09:30:00"),
            tzid: "UTC".into(),
        };
        assert_eq!(
            time_property("DTSTART", &utc_zoned).value,
            "20240301T093000Z"
        );
    }
}
