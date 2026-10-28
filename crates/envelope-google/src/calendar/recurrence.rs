//! Google's `recurrence` array (`RRULE`, `EXDATE`, `RDATE`, `EXRULE` lines).

use std::collections::BTreeMap;

use chrono::{NaiveDate, NaiveDateTime};
use envelope_ical::{Event, EventTime};

use super::time;

/// The recurrence lines of an event, split into the canonical fields.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Recurrence {
    /// `RRULE` value without the `RRULE:` prefix.
    pub rrule: Option<String>,
    /// `RDATE` values.
    pub rdates: Vec<EventTime>,
    /// `EXDATE` values.
    pub exdates: Vec<EventTime>,
    /// Lines that cannot be represented (`EXRULE`, a second `RRULE`,
    /// unparsable dates), kept verbatim.
    pub unsupported: Vec<String>,
}

/// Parses Google's recurrence lines. Date-time values are converted to the
/// form of `start` where that is lossless (see [`time::conform`]); values
/// without `TZID` inherit the zone of a zoned start.
pub(crate) fn parse(lines: &[String], start: &EventTime) -> Recurrence {
    let mut recurrence = Recurrence::default();
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some(ContentLine {
            name,
            params,
            value,
        }) = split_line(line)
        else {
            recurrence.unsupported.push(line.to_owned());
            continue;
        };
        match name.as_str() {
            "RRULE" if recurrence.rrule.is_none() && !value.trim().is_empty() => {
                recurrence.rrule = Some(value.trim().to_owned());
            }
            "EXDATE" | "RDATE" => match parse_values(value, &params, start) {
                Some(values) if name == "EXDATE" => recurrence.exdates.extend(values),
                Some(values) => recurrence.rdates.extend(values),
                None => recurrence.unsupported.push(line.to_owned()),
            },
            _ => recurrence.unsupported.push(line.to_owned()),
        }
    }
    recurrence
}

/// Formats the recurrence of a series master as Google recurrence lines.
pub(crate) fn format(event: &Event, extra: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(rule) = event
        .rrule
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        let rule = strip_prefix_ignore_case(rule, "RRULE:").unwrap_or(rule);
        lines.push(format!("RRULE:{rule}"));
    }
    lines.extend(format_dates("EXDATE", &event.exdates));
    lines.extend(format_dates("RDATE", &event.rdates));
    lines.extend(extra.iter().cloned());
    lines
}

fn format_dates(name: &str, values: &[EventTime]) -> Vec<String> {
    let mut dates = Vec::new();
    let mut utc = Vec::new();
    let mut floating = Vec::new();
    let mut zoned: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for value in values {
        match value {
            EventTime::Date { date } => dates.push(date.format("%Y%m%d").to_string()),
            EventTime::Utc { at } => utc.push(at.format("%Y%m%dT%H%M%SZ").to_string()),
            EventTime::Floating { local } => {
                floating.push(local.format("%Y%m%dT%H%M%S").to_string())
            }
            EventTime::Zoned { local, tzid } => zoned
                .entry(tzid.as_str())
                .or_default()
                .push(local.format("%Y%m%dT%H%M%S").to_string()),
        }
    }
    let mut lines = Vec::new();
    if !dates.is_empty() {
        lines.push(format!("{name};VALUE=DATE:{}", dates.join(",")));
    }
    for (tzid, values) in zoned {
        lines.push(format!("{name};TZID={tzid}:{}", values.join(",")));
    }
    if !utc.is_empty() {
        lines.push(format!("{name}:{}", utc.join(",")));
    }
    if !floating.is_empty() {
        lines.push(format!("{name}:{}", floating.join(",")));
    }
    lines
}

/// A recurrence line split into its parts.
struct ContentLine<'a> {
    /// Upper-cased property name.
    name: String,
    /// Upper-cased parameter names with their (unquoted) values.
    params: Vec<(String, String)>,
    /// The raw value.
    value: &'a str,
}

/// Splits `NAME;PARAM=VALUE;…:VALUE` into its parts.
fn split_line(line: &str) -> Option<ContentLine<'_>> {
    let mut in_quotes = false;
    let colon = line.char_indices().find_map(|(idx, ch)| match ch {
        '"' => {
            in_quotes = !in_quotes;
            None
        }
        ':' if !in_quotes => Some(idx),
        _ => None,
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|param| {
            let (key, value) = param.split_once('=')?;
            Some((
                key.trim().to_ascii_uppercase(),
                value.trim().trim_matches('"').to_owned(),
            ))
        })
        .collect();
    Some(ContentLine {
        name,
        params,
        value,
    })
}

fn parse_values(
    value: &str,
    params: &[(String, String)],
    start: &EventTime,
) -> Option<Vec<EventTime>> {
    let param = |key: &str| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let value_type = param("VALUE").map(str::to_ascii_uppercase);
    let tzid = param("TZID").filter(|t| !t.is_empty());
    let start_tz = match start {
        EventTime::Zoned { tzid, .. } => Some(tzid.as_str()),
        _ => None,
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|item| {
            // A PERIOD (`start/end` or `start/duration`) contributes its start.
            let item = item.split('/').next().unwrap_or(item);
            let time = if value_type.as_deref() == Some("DATE") || !item.contains('T') {
                EventTime::Date {
                    date: NaiveDate::parse_from_str(item, "%Y%m%d").ok()?,
                }
            } else if let Some(utc) = item.strip_suffix(['Z', 'z']) {
                EventTime::Utc {
                    at: NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
                        .ok()?
                        .and_utc(),
                }
            } else {
                let local = NaiveDateTime::parse_from_str(item, "%Y%m%dT%H%M%S").ok()?;
                match tzid.or(start_tz) {
                    Some(tzid) => EventTime::Zoned {
                        local,
                        tzid: tzid.to_owned(),
                    },
                    None => EventTime::Floating { local },
                }
            };
            Some(time::conform(time, start))
        })
        .collect()
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &value[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").expect("valid")
    }

    fn zoned(s: &str, tz: &str) -> EventTime {
        EventTime::Zoned {
            local: local(s),
            tzid: tz.to_owned(),
        }
    }

    fn date(s: &str) -> EventTime {
        EventTime::Date {
            date: NaiveDate::parse_from_str(s, "%Y%m%d").expect("valid"),
        }
    }

    #[test]
    fn parses_rrule_exdate_and_rdate_lines() {
        let start = zoned("20240101T090000", "Europe/Istanbul");
        let lines = vec![
            "RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20240331T060000Z".to_owned(),
            "EXDATE;TZID=Europe/Istanbul:20240108T090000,20240110T090000".to_owned(),
            "EXDATE:20240115T060000Z".to_owned(),
            "RDATE;TZID=\"Europe/Istanbul\":20240402T100000".to_owned(),
            "EXRULE:FREQ=MONTHLY".to_owned(),
        ];
        let parsed = parse(&lines, &start);
        assert_eq!(
            parsed.rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20240331T060000Z")
        );
        assert_eq!(
            parsed.exdates,
            vec![
                zoned("20240108T090000", "Europe/Istanbul"),
                zoned("20240110T090000", "Europe/Istanbul"),
                zoned("20240115T090000", "Europe/Istanbul"),
            ]
        );
        assert_eq!(
            parsed.rdates,
            vec![zoned("20240402T100000", "Europe/Istanbul")]
        );
        assert_eq!(parsed.unsupported, vec!["EXRULE:FREQ=MONTHLY".to_owned()]);
    }

    #[test]
    fn parses_all_day_and_floating_values() {
        let start = date("20240101");
        let lines = vec![
            "RRULE:FREQ=YEARLY".to_owned(),
            "EXDATE;VALUE=DATE:20250101,20260101".to_owned(),
            "RDATE;VALUE=PERIOD:20240601T100000Z/PT1H".to_owned(),
            "EXDATE:bogus".to_owned(),
        ];
        let parsed = parse(&lines, &start);
        assert_eq!(parsed.exdates, vec![date("20250101"), date("20260101")]);
        assert_eq!(
            parsed.rdates,
            vec![EventTime::Utc {
                at: local("20240601T100000").and_utc()
            }]
        );
        assert_eq!(parsed.unsupported, vec!["EXDATE:bogus".to_owned()]);

        let floating_start = EventTime::Floating {
            local: local("20240101T090000"),
        };
        let parsed = parse(&["EXDATE:20240102T090000".to_owned()], &floating_start);
        assert_eq!(
            parsed.exdates,
            vec![EventTime::Floating {
                local: local("20240102T090000")
            }]
        );
    }

    #[test]
    fn formats_grouped_lines() {
        let start = zoned("20240101T090000", "Europe/Istanbul");
        let mut event = Event::new("uid", start.clone(), start);
        event.rrule = Some("RRULE:FREQ=DAILY;COUNT=10".to_owned());
        event.exdates = vec![
            zoned("20240103T090000", "Europe/Istanbul"),
            date("20240105"),
            zoned("20240104T090000", "Europe/Istanbul"),
            EventTime::Utc {
                at: local("20240106T060000").and_utc(),
            },
        ];
        event.rdates = vec![zoned("20240201T120000", "Europe/Istanbul")];
        assert_eq!(
            format(&event, &["EXRULE:FREQ=MONTHLY".to_owned()]),
            vec![
                "RRULE:FREQ=DAILY;COUNT=10".to_owned(),
                "EXDATE;VALUE=DATE:20240105".to_owned(),
                "EXDATE;TZID=Europe/Istanbul:20240103T090000,20240104T090000".to_owned(),
                "EXDATE:20240106T060000Z".to_owned(),
                "RDATE;TZID=Europe/Istanbul:20240201T120000".to_owned(),
                "EXRULE:FREQ=MONTHLY".to_owned(),
            ]
        );
    }

    #[test]
    fn round_trips_through_parse_and_format() {
        let start = zoned("20240101T090000", "America/New_York");
        let lines = vec![
            "RRULE:FREQ=WEEKLY;BYDAY=TU".to_owned(),
            "EXDATE;TZID=America/New_York:20240109T090000".to_owned(),
        ];
        let parsed = parse(&lines, &start);
        let mut event = Event::new("uid", start.clone(), start);
        event.rrule = parsed.rrule;
        event.exdates = parsed.exdates;
        assert_eq!(format(&event, &parsed.unsupported), lines);
    }
}
