//! Conversions between Google's `EventDateTime` and [`EventTime`].

use chrono::offset::LocalResult;
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use envelope_ical::EventTime;
use serde_json::{Value, json};

use super::wire;
use crate::error::{Error, Result};

/// Parses a Google `{date | dateTime, timeZone}` value.
///
/// * `date` → [`EventTime::Date`].
/// * `dateTime` with a (known) time zone → [`EventTime::Zoned`] holding the
///   wall-clock time in that zone (Google renders the instant in the
///   calendar's offset, which may differ from the event's zone).
/// * `dateTime` without a zone → [`EventTime::Utc`]: the event follows the
///   calendar's zone, so the absolute instant is what matters.
///
/// `fallback_tz` is used when the value has no `timeZone` of its own (for
/// `originalStartTime`, the event's start zone).
pub(crate) fn parse(
    value: &wire::DateTimeValue,
    fallback_tz: Option<&str>,
) -> Result<EventTime, String> {
    if let Some(date) = value.date.as_deref().filter(|d| !d.is_empty()) {
        return NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map(|date| EventTime::Date { date })
            .map_err(|e| format!("invalid date {date:?}: {e}"));
    }
    let Some(raw) = value.date_time.as_deref().filter(|d| !d.is_empty()) else {
        return Err("time has neither `date` nor `dateTime`".to_owned());
    };
    let tz_name = value
        .time_zone
        .as_deref()
        .filter(|t| !t.is_empty())
        .or(fallback_tz);
    match DateTime::parse_from_rfc3339(raw) {
        Ok(instant) => {
            let zoned = tz_name.and_then(|name| Some((name, name.parse::<Tz>().ok()?)));
            Ok(match zoned {
                Some((name, tz)) => EventTime::Zoned {
                    local: instant.with_timezone(&tz).naive_local(),
                    tzid: name.to_owned(),
                },
                None => EventTime::Utc {
                    at: instant.with_timezone(&Utc),
                },
            })
        }
        Err(err) => {
            // Google accepts offset-less input when a zone is given; accept
            // the same leniently on output.
            let local = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                .map_err(|_| format!("invalid dateTime {raw:?}: {err}"))?;
            Ok(match tz_name {
                Some(name) => EventTime::Zoned {
                    local,
                    tzid: name.to_owned(),
                },
                None => EventTime::Floating { local },
            })
        }
    }
}

/// Parses an RFC 3339 timestamp such as `created` or `updated`.
pub(crate) fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Formats an instant as RFC 3339 in UTC (`2024-01-01T09:00:00Z`).
pub(crate) fn rfc3339(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Renders an [`EventTime`] as Google's `EventDateTime` JSON.
///
/// The fields not used are sent as explicit `null`s so that a patch turning
/// an all-day event into a timed one (or back) clears the old field.
/// `recurring` adds `timeZone: "UTC"` to UTC times, because Google requires
/// a zone on recurring events. Floating times are anchored in `floating_tz`.
pub(crate) fn to_json(
    time: &EventTime,
    recurring: bool,
    floating_tz: Option<&str>,
) -> Result<Value> {
    match time {
        EventTime::Date { date } => Ok(json!({
            "date": date.format("%Y-%m-%d").to_string(),
            "dateTime": null,
            "timeZone": null,
        })),
        EventTime::Utc { at } => Ok(json!({
            "date": null,
            "dateTime": rfc3339(at),
            "timeZone": if recurring { Value::from("UTC") } else { Value::Null },
        })),
        EventTime::Zoned { local, tzid } => zoned_json(local, tzid),
        EventTime::Floating { local } => match floating_tz {
            Some(tzid) => zoned_json(local, tzid),
            None => Err(Error::InvalidInput(
                "Google Calendar has no floating times; set ClientConfig::floating_time_zone \
                 or use a zoned time"
                    .to_owned(),
            )),
        },
    }
}

fn zoned_json(local: &NaiveDateTime, tzid: &str) -> Result<Value> {
    let tz = tzid.parse::<Tz>().map_err(|_| {
        Error::InvalidInput(format!(
            "time zone {tzid:?} is not an IANA time zone, which Google Calendar requires"
        ))
    })?;
    let date_time = match tz.from_local_datetime(local) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => {
            at.to_rfc3339_opts(SecondsFormat::Secs, true)
        }
        // A wall-clock time skipped by a DST transition: let Google resolve
        // it against the zone, which it allows when `timeZone` is present.
        LocalResult::None => local.format("%Y-%m-%dT%H:%M:%S").to_string(),
    };
    Ok(json!({ "date": null, "dateTime": date_time, "timeZone": tzid }))
}

/// The absolute instant of a time (dates and floating times are taken as UTC).
pub(crate) fn to_utc(time: &EventTime) -> Option<DateTime<Utc>> {
    match time {
        EventTime::Utc { at } => Some(*at),
        EventTime::Zoned { local, tzid } => {
            let tz = tzid.parse::<Tz>().ok()?;
            tz.from_local_datetime(local)
                .earliest()
                .map(|at| at.with_timezone(&Utc))
        }
        EventTime::Date { date } => date.and_hms_opt(0, 0, 0).map(|t| t.and_utc()),
        EventTime::Floating { local } => Some(local.and_utc()),
    }
}

/// Formats a time for query parameters such as `originalStart`.
pub(crate) fn to_query_value(time: &EventTime) -> Option<String> {
    match time {
        EventTime::Date { date } => Some(date.format("%Y-%m-%d").to_string()),
        other => to_utc(other).map(|at| rfc3339(&at)),
    }
}

/// The default end of an event without one: a day after an all-day start,
/// otherwise the start itself.
pub(crate) fn default_end(start: &EventTime) -> EventTime {
    match start {
        EventTime::Date { date } => EventTime::Date {
            date: date.succ_opt().unwrap_or(*date),
        },
        other => other.clone(),
    }
}

/// Converts `time` to the form of `template` (the series' start) where that
/// is lossless: UTC or differently zoned instants become wall-clock times in
/// the template's zone, zoned times become UTC for a UTC template. Dates
/// and floating times are left alone.
pub(crate) fn conform(time: EventTime, template: &EventTime) -> EventTime {
    match (template, &time) {
        (
            EventTime::Zoned { tzid: target, .. },
            EventTime::Utc { .. } | EventTime::Zoned { .. },
        ) => {
            if matches!(&time, EventTime::Zoned { tzid, .. } if tzid == target) {
                return time;
            }
            let (Ok(tz), Some(at)) = (target.parse::<Tz>(), to_utc(&time)) else {
                return time;
            };
            EventTime::Zoned {
                local: at.with_timezone(&tz).naive_local(),
                tzid: target.clone(),
            }
        }
        (EventTime::Utc { .. }, EventTime::Zoned { .. }) => match to_utc(&time) {
            Some(at) => EventTime::Utc { at },
            None => time,
        },
        _ => time,
    }
}

/// True if two occurrence identifiers denote the same occurrence.
pub(crate) fn same_occurrence(a: &EventTime, b: &EventTime) -> bool {
    match (a, b) {
        (EventTime::Date { date: x }, EventTime::Date { date: y }) => x == y,
        (EventTime::Date { .. }, _) | (_, EventTime::Date { .. }) => false,
        _ => match (to_utc(a), to_utc(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(date: Option<&str>, date_time: Option<&str>, tz: Option<&str>) -> wire::DateTimeValue {
        wire::DateTimeValue {
            date: date.map(str::to_owned),
            date_time: date_time.map(str::to_owned),
            time_zone: tz.map(str::to_owned),
        }
    }

    fn local(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").expect("valid")
    }

    #[test]
    fn parses_dates_zoned_and_utc_values() {
        assert_eq!(
            parse(&value(Some("2024-05-01"), None, None), None),
            Ok(EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 5, 1).expect("date")
            })
        );
        // Instant rendered in the calendar's offset (+03:00), event in New York.
        assert_eq!(
            parse(
                &value(
                    None,
                    Some("2024-01-15T17:00:00+03:00"),
                    Some("America/New_York")
                ),
                None
            ),
            Ok(EventTime::Zoned {
                local: local("2024-01-15T09:00:00"),
                tzid: "America/New_York".to_owned()
            })
        );
        assert_eq!(
            parse(&value(None, Some("2024-01-15T09:00:00+01:00"), None), None),
            Ok(EventTime::Utc {
                at: local("2024-01-15T08:00:00").and_utc()
            })
        );
        assert_eq!(
            parse(
                &value(None, Some("2024-01-15T09:00:00Z"), None),
                Some("Europe/Istanbul")
            ),
            Ok(EventTime::Zoned {
                local: local("2024-01-15T12:00:00"),
                tzid: "Europe/Istanbul".to_owned()
            })
        );
        assert!(parse(&value(None, None, None), None).is_err());
        assert!(parse(&value(None, Some("yesterday"), None), None).is_err());
    }

    #[test]
    fn unknown_zone_keeps_the_instant() {
        assert_eq!(
            parse(
                &value(None, Some("2024-01-15T09:00:00Z"), Some("Mars/Olympus")),
                None
            ),
            Ok(EventTime::Utc {
                at: local("2024-01-15T09:00:00").and_utc()
            })
        );
    }

    #[test]
    fn renders_google_date_times() {
        let zoned = EventTime::Zoned {
            local: local("2024-07-01T09:30:00"),
            tzid: "Europe/Berlin".to_owned(),
        };
        assert_eq!(
            to_json(&zoned, true, None).expect("json"),
            json!({"date": null, "dateTime": "2024-07-01T09:30:00+02:00", "timeZone": "Europe/Berlin"})
        );
        let utc = EventTime::Utc {
            at: local("2024-07-01T07:30:00").and_utc(),
        };
        assert_eq!(
            to_json(&utc, false, None).expect("json"),
            json!({"date": null, "dateTime": "2024-07-01T07:30:00Z", "timeZone": null})
        );
        assert_eq!(
            to_json(&utc, true, None).expect("json")["timeZone"],
            json!("UTC")
        );
        let floating = EventTime::Floating {
            local: local("2024-07-01T09:30:00"),
        };
        assert!(to_json(&floating, false, None).is_err());
        assert_eq!(
            to_json(&floating, false, Some("Europe/Berlin")).expect("json"),
            to_json(&zoned, false, None).expect("json")
        );
        let windows_zone = EventTime::Zoned {
            local: local("2024-07-01T09:30:00"),
            tzid: "W. Europe Standard Time".to_owned(),
        };
        assert!(matches!(
            to_json(&windows_zone, false, None),
            Err(Error::InvalidInput(_))
        ));
        // 02:30 does not exist in Berlin on 2024-03-31.
        let gap = EventTime::Zoned {
            local: local("2024-03-31T02:30:00"),
            tzid: "Europe/Berlin".to_owned(),
        };
        assert_eq!(
            to_json(&gap, false, None).expect("json")["dateTime"],
            json!("2024-03-31T02:30:00")
        );
    }

    #[test]
    fn conforms_exdates_to_the_series_start() {
        let start = EventTime::Zoned {
            local: local("2024-01-01T09:00:00"),
            tzid: "Europe/Istanbul".to_owned(),
        };
        let utc = EventTime::Utc {
            at: local("2024-01-08T06:00:00").and_utc(),
        };
        assert_eq!(
            conform(utc, &start),
            EventTime::Zoned {
                local: local("2024-01-08T09:00:00"),
                tzid: "Europe/Istanbul".to_owned()
            }
        );
        let ny = EventTime::Zoned {
            local: local("2024-01-08T01:00:00"),
            tzid: "America/New_York".to_owned(),
        };
        assert_eq!(
            conform(ny, &start),
            EventTime::Zoned {
                local: local("2024-01-08T09:00:00"),
                tzid: "Europe/Istanbul".to_owned()
            }
        );
        let date = EventTime::Date {
            date: NaiveDate::from_ymd_opt(2024, 1, 8).expect("date"),
        };
        assert_eq!(conform(date.clone(), &start), date);
    }

    #[test]
    fn compares_occurrences_by_instant() {
        let a = EventTime::Zoned {
            local: local("2024-01-08T09:00:00"),
            tzid: "Europe/Istanbul".to_owned(),
        };
        let b = EventTime::Utc {
            at: local("2024-01-08T06:00:00").and_utc(),
        };
        assert!(same_occurrence(&a, &b));
        assert!(!same_occurrence(
            &a,
            &EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 1, 8).expect("date")
            }
        ));
        assert_eq!(to_query_value(&a).as_deref(), Some("2024-01-08T06:00:00Z"));
    }
}
