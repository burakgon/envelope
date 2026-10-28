//! `RRULE` value representation and tolerant parsing (RFC 5545 §3.3.10).

use std::fmt;

use chrono::Weekday;
use serde::{Deserialize, Serialize};

use crate::model::EventTime;
use crate::value::{format_event_time, parse_date, parse_date_time};

/// Recurrence frequency (`FREQ`), ordered from coarsest to finest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Frequency {
    /// `YEARLY`
    Yearly,
    /// `MONTHLY`
    Monthly,
    /// `WEEKLY`
    Weekly,
    /// `DAILY`
    Daily,
    /// `HOURLY`
    Hourly,
    /// `MINUTELY`
    Minutely,
    /// `SECONDLY`
    Secondly,
}

impl Frequency {
    /// The RFC 5545 keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Yearly => "YEARLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Daily => "DAILY",
            Frequency::Hourly => "HOURLY",
            Frequency::Minutely => "MINUTELY",
            Frequency::Secondly => "SECONDLY",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_uppercase().as_str() {
            "YEARLY" => Frequency::Yearly,
            "MONTHLY" => Frequency::Monthly,
            "WEEKLY" => Frequency::Weekly,
            "DAILY" => Frequency::Daily,
            "HOURLY" => Frequency::Hourly,
            "MINUTELY" => Frequency::Minutely,
            "SECONDLY" => Frequency::Secondly,
            _ => return None,
        })
    }
}

/// One `BYDAY` entry: a weekday with an optional ordinal (`-1SU`, `2MO`, `FR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WeekdayNum {
    /// Ordinal within the month or year (`1` = first, `-1` = last).
    pub ordinal: Option<i16>,
    /// The weekday.
    pub weekday: Weekday,
}

/// A parsed recurrence rule.
///
/// Parsing is tolerant: unknown rule parts are ignored and out-of-range
/// values are dropped. Use the original RRULE string for round-tripping; this
/// type is for evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurRule {
    /// `FREQ`
    pub freq: Frequency,
    /// `INTERVAL` (at least 1).
    pub interval: u32,
    /// `COUNT`
    pub count: Option<u32>,
    /// `UNTIL`, as written: a date, a UTC instant or a floating local time.
    pub until: Option<EventTime>,
    /// `BYSECOND`
    pub by_second: Vec<u8>,
    /// `BYMINUTE`
    pub by_minute: Vec<u8>,
    /// `BYHOUR`
    pub by_hour: Vec<u8>,
    /// `BYDAY`
    pub by_day: Vec<WeekdayNum>,
    /// `BYMONTHDAY` (negative counts from the end of the month).
    pub by_month_day: Vec<i8>,
    /// `BYYEARDAY` (negative counts from the end of the year).
    pub by_year_day: Vec<i16>,
    /// `BYWEEKNO` (ISO 8601 style week numbers relative to `WKST`).
    pub by_week_no: Vec<i8>,
    /// `BYMONTH`
    pub by_month: Vec<u8>,
    /// `BYSETPOS`
    pub by_set_pos: Vec<i16>,
    /// `WKST` (defaults to Monday).
    pub week_start: Weekday,
}

impl RecurRule {
    /// A rule with the given frequency and no other parts.
    pub fn new(freq: Frequency) -> Self {
        RecurRule {
            freq,
            interval: 1,
            count: None,
            until: None,
            by_second: Vec::new(),
            by_minute: Vec::new(),
            by_hour: Vec::new(),
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_year_day: Vec::new(),
            by_week_no: Vec::new(),
            by_month: Vec::new(),
            by_set_pos: Vec::new(),
            week_start: Weekday::Mon,
        }
    }

    /// Parses an RRULE value (with or without the `RRULE:` prefix).
    /// Returns `None` when there is no valid `FREQ`.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let value = match value.get(..6) {
            Some(prefix) if prefix.eq_ignore_ascii_case("RRULE:") => &value[6..],
            _ => value,
        };
        let mut freq = None;
        let mut rule = RecurRule::new(Frequency::Daily);
        for part in value.split(';') {
            let Some((key, val)) = part.split_once('=') else {
                continue;
            };
            let val = val.trim();
            match key.trim().to_ascii_uppercase().as_str() {
                "FREQ" => freq = Frequency::parse(val),
                "INTERVAL" => {
                    rule.interval = val.parse::<u32>().ok().filter(|i| *i > 0).unwrap_or(1);
                }
                "COUNT" => rule.count = val.parse::<u32>().ok().map(|c| c.max(1)),
                "UNTIL" => rule.until = parse_until(val),
                "BYSECOND" => rule.by_second = parse_list(val, 0, 59),
                "BYMINUTE" => rule.by_minute = parse_list(val, 0, 59),
                "BYHOUR" => rule.by_hour = parse_list(val, 0, 23),
                "BYDAY" => rule.by_day = val.split(',').filter_map(parse_weekday_num).collect(),
                "BYMONTHDAY" => rule.by_month_day = parse_signed_list(val, 31),
                "BYYEARDAY" => rule.by_year_day = parse_signed_list(val, 366),
                "BYWEEKNO" => rule.by_week_no = parse_signed_list(val, 53),
                "BYMONTH" => rule.by_month = parse_list(val, 1, 12),
                "BYSETPOS" => rule.by_set_pos = parse_signed_list(val, 366),
                "WKST" => {
                    if let Some(day) = parse_weekday(val) {
                        rule.week_start = day;
                    }
                }
                _ => {}
            }
        }
        rule.freq = freq?;
        Some(rule)
    }
}

impl fmt::Display for RecurRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FREQ={}", self.freq.as_str())?;
        if let Some(until) = &self.until {
            write!(f, ";UNTIL={}", format_event_time(until))?;
        }
        if let Some(count) = self.count {
            write!(f, ";COUNT={count}")?;
        }
        if self.interval > 1 {
            write!(f, ";INTERVAL={}", self.interval)?;
        }
        fn list<T: fmt::Display>(
            f: &mut fmt::Formatter<'_>,
            key: &str,
            items: &[T],
        ) -> fmt::Result {
            if items.is_empty() {
                return Ok(());
            }
            let joined: Vec<String> = items.iter().map(ToString::to_string).collect();
            write!(f, ";{key}={}", joined.join(","))
        }
        list(f, "BYSECOND", &self.by_second)?;
        list(f, "BYMINUTE", &self.by_minute)?;
        list(f, "BYHOUR", &self.by_hour)?;
        let days: Vec<String> = self
            .by_day
            .iter()
            .map(|d| match d.ordinal {
                Some(n) => format!("{n}{}", weekday_code(d.weekday)),
                None => weekday_code(d.weekday).to_owned(),
            })
            .collect();
        list(f, "BYDAY", &days)?;
        list(f, "BYMONTHDAY", &self.by_month_day)?;
        list(f, "BYYEARDAY", &self.by_year_day)?;
        list(f, "BYWEEKNO", &self.by_week_no)?;
        list(f, "BYMONTH", &self.by_month)?;
        list(f, "BYSETPOS", &self.by_set_pos)?;
        if self.week_start != Weekday::Mon {
            write!(f, ";WKST={}", weekday_code(self.week_start))?;
        }
        Ok(())
    }
}

fn parse_until(value: &str) -> Option<EventTime> {
    if !value.contains(['T', 't']) {
        return parse_date(value).map(|date| EventTime::Date { date });
    }
    let (local, utc) = parse_date_time(value)?;
    Some(if utc {
        EventTime::Utc {
            at: local.and_utc(),
        }
    } else {
        EventTime::Floating { local }
    })
}

fn parse_list(value: &str, min: u8, max: u8) -> Vec<u8> {
    let mut out: Vec<u8> = value
        .split(',')
        .filter_map(|v| v.trim().parse::<u8>().ok())
        .filter(|v| (min..=max).contains(v))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn parse_signed_list<T>(value: &str, max_abs: i16) -> Vec<T>
where
    T: TryFrom<i16> + Ord + Copy,
{
    let mut out: Vec<T> = value
        .split(',')
        .filter_map(|v| v.trim().trim_start_matches('+').parse::<i16>().ok())
        .filter(|v| *v != 0 && v.abs() <= max_abs)
        .filter_map(|v| T::try_from(v).ok())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Parses a two-letter weekday code (`MO`, `tu`, …).
pub(crate) fn parse_weekday(code: &str) -> Option<Weekday> {
    Some(match code.trim().to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// The two-letter code of a weekday.
pub(crate) fn weekday_code(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

fn parse_weekday_num(item: &str) -> Option<WeekdayNum> {
    let item = item.trim();
    if item.len() < 2 || !item.is_char_boundary(item.len() - 2) {
        return None;
    }
    let (number, code) = item.split_at(item.len() - 2);
    let weekday = parse_weekday(code)?;
    let number = number.trim();
    let ordinal = if number.is_empty() {
        None
    } else {
        let n: i16 = number.trim_start_matches('+').parse().ok()?;
        if n == 0 || n.abs() > 53 {
            return None;
        }
        Some(n)
    };
    Some(WeekdayNum { ordinal, weekday })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn parses_full_rule() {
        let rule = RecurRule::parse(
            "RRULE:FREQ=MONTHLY;INTERVAL=2;COUNT=10;BYDAY=1SU,-1SU,+2MO,TU;BYMONTHDAY=1,-1;BYSETPOS=-1;WKST=SU",
        )
        .expect("valid rule");
        assert_eq!(rule.freq, Frequency::Monthly);
        assert_eq!(rule.interval, 2);
        assert_eq!(rule.count, Some(10));
        assert_eq!(rule.by_day.len(), 4);
        assert_eq!(
            rule.by_day[0],
            WeekdayNum {
                ordinal: Some(1),
                weekday: Weekday::Sun
            }
        );
        assert_eq!(rule.by_day[1].ordinal, Some(-1));
        assert_eq!(rule.by_day[2].ordinal, Some(2));
        assert_eq!(rule.by_day[3].ordinal, None);
        assert_eq!(rule.by_month_day, vec![-1, 1]);
        assert_eq!(rule.by_set_pos, vec![-1]);
        assert_eq!(rule.week_start, Weekday::Sun);
    }

    #[test]
    fn tolerant_parsing() {
        let rule =
            RecurRule::parse("freq=weekly;interval=0;byday=mo, we ,xx;bymonth=13,2;X-NAME=1;FOO")
                .expect("valid rule");
        assert_eq!(rule.freq, Frequency::Weekly);
        assert_eq!(rule.interval, 1);
        assert_eq!(rule.by_day.len(), 2);
        assert_eq!(rule.by_month, vec![2]);
        assert!(RecurRule::parse("INTERVAL=2").is_none());
        assert!(RecurRule::parse("FREQ=FORTNIGHTLY").is_none());
        assert!(RecurRule::parse("").is_none());
    }

    #[test]
    fn until_forms() {
        let date = RecurRule::parse("FREQ=DAILY;UNTIL=20240315").expect("rule");
        assert_eq!(
            date.until,
            Some(EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 3, 15).expect("date")
            })
        );
        let utc = RecurRule::parse("FREQ=DAILY;UNTIL=20240315T225959Z").expect("rule");
        assert!(matches!(utc.until, Some(EventTime::Utc { .. })));
        let floating = RecurRule::parse("FREQ=DAILY;UNTIL=20240315T090000").expect("rule");
        assert!(matches!(floating.until, Some(EventTime::Floating { .. })));
    }

    #[test]
    fn display_is_canonical() {
        let rule =
            RecurRule::parse("FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU;WKST=SU;INTERVAL=1").expect("rule");
        assert_eq!(rule.to_string(), "FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3;WKST=SU");
        let reparsed = RecurRule::parse(&rule.to_string()).expect("rule");
        assert_eq!(reparsed, rule);
    }
}
