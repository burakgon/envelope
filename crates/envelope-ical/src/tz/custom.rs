//! Time zones defined only by a `VTIMEZONE` component.
//!
//! The observances (`STANDARD` / `DAYLIGHT` with `TZOFFSETFROM`,
//! `TZOFFSETTO`, `DTSTART`, `RRULE` and `RDATE`) are expanded once into a
//! sorted transition table, which makes UTC offset lookups a binary search.

use chrono::{DateTime, Datelike, NaiveDateTime, NaiveTime, TimeDelta, Utc};

use crate::component::Component;
use crate::model::EventTime;
use crate::recur::rule::RecurRule;
use crate::recur::{InstanceBounds, local_instances};
use crate::value::{parse_utc_offset, property_time, property_times};

/// Transitions are expanded up to this year.
const HORIZON_YEAR: i32 = 2200;
/// Rule-based onsets before this year are not expanded (Outlook writes
/// observances starting in 1601).
const FIRST_YEAR: i32 = 1900;
/// Safety limit on the onsets taken from one observance.
const MAX_ONSETS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Transition {
    /// UTC instant (seconds since the epoch) of the transition.
    at: i64,
    /// Offset (seconds east of UTC) in effect from `at` on.
    offset: i32,
}

/// A zone evaluated from a `VTIMEZONE` definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CustomZone {
    tzid: String,
    initial_offset: i32,
    transitions: Vec<Transition>,
}

impl CustomZone {
    /// Builds a zone from a `VTIMEZONE` component. Returns `None` when it has
    /// no `TZID` or no usable observance.
    pub fn from_vtimezone(vtimezone: &Component) -> Option<Self> {
        let tzid = vtimezone.value("TZID")?.trim().trim_matches('"').to_owned();
        if tzid.is_empty() {
            return None;
        }
        let mut onsets: Vec<(i64, i32, i32)> = Vec::new();
        for observance in vtimezone
            .components
            .iter()
            .filter(|c| c.is("STANDARD") || c.is("DAYLIGHT"))
        {
            let Some(to) = observance.value("TZOFFSETTO").and_then(parse_utc_offset) else {
                continue;
            };
            let from = observance
                .value("TZOFFSETFROM")
                .and_then(parse_utc_offset)
                .unwrap_or(to);
            let start = observance
                .property("DTSTART")
                .and_then(property_time)
                .map(|t| local_of(&t, from))
                .or_else(|| {
                    chrono::NaiveDate::from_ymd_opt(1970, 1, 1).map(|d| d.and_time(NaiveTime::MIN))
                });
            let Some(start) = start else {
                continue;
            };
            let mut locals = vec![start];
            for rdate in observance.properties_named("RDATE") {
                locals.extend(property_times(rdate).iter().map(|t| local_of(t, from)));
            }
            if let Some(rule) = observance.value("RRULE").and_then(RecurRule::parse) {
                let until = rule.until.clone();
                let (instances, _) = local_instances(
                    &rule,
                    start,
                    |local| {
                        local.year() > HORIZON_YEAR
                            || until.as_ref().is_some_and(|u| past_until(local, from, u))
                    },
                    InstanceBounds {
                        from: chrono::NaiveDate::from_ymd_opt(FIRST_YEAR, 1, 1),
                        until: chrono::NaiveDate::from_ymd_opt(HORIZON_YEAR, 12, 31),
                        max: MAX_ONSETS,
                        budget: 50_000,
                    },
                );
                locals.extend(instances);
            }
            for local in locals {
                if let Some(utc) = local.checked_sub_signed(TimeDelta::seconds(i64::from(from))) {
                    onsets.push((utc.and_utc().timestamp(), to, from));
                }
            }
        }
        if onsets.is_empty() {
            return None;
        }
        onsets.sort_by_key(|(at, _, _)| *at);
        onsets.dedup_by_key(|(at, _, _)| *at);
        let initial_offset = onsets[0].2;
        let transitions = onsets
            .into_iter()
            .map(|(at, offset, _)| Transition { at, offset })
            .collect();
        Some(CustomZone {
            tzid,
            initial_offset,
            transitions,
        })
    }

    /// The `TZID` of the definition.
    pub fn tzid(&self) -> &str {
        &self.tzid
    }

    /// Offset from UTC, in seconds, in effect at `utc`.
    pub fn offset_at(&self, utc: DateTime<Utc>) -> i32 {
        let ts = utc.timestamp();
        match self.transitions.partition_point(|t| t.at <= ts) {
            0 => self.initial_offset,
            n => self.transitions[n - 1].offset,
        }
    }

    /// UTC instants of all transitions in `[from, to)`.
    pub(crate) fn transitions_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>> {
        let (from, to) = (from.timestamp(), to.timestamp());
        self.transitions
            .iter()
            .filter(|t| t.at >= from && t.at < to)
            .filter_map(|t| DateTime::from_timestamp(t.at, 0))
            .collect()
    }

    /// Instant of the first transition, if any.
    pub(crate) fn first_transition(&self) -> Option<DateTime<Utc>> {
        self.transitions
            .first()
            .and_then(|t| DateTime::from_timestamp(t.at, 0))
    }
}

/// Local wall time of an observance onset as written. UTC values (invalid
/// but seen) are converted with the observance's "from" offset.
fn local_of(time: &EventTime, from: i32) -> NaiveDateTime {
    match time {
        EventTime::Date { date } => date.and_time(NaiveTime::MIN),
        EventTime::Zoned { local, .. } | EventTime::Floating { local } => *local,
        EventTime::Utc { at } => at
            .naive_utc()
            .checked_add_signed(TimeDelta::seconds(i64::from(from)))
            .unwrap_or(at.naive_utc()),
    }
}

/// True if the onset at `local` (in offset `from`) lies after `UNTIL`.
fn past_until(local: &NaiveDateTime, from: i32, until: &EventTime) -> bool {
    match until {
        EventTime::Utc { at } => local
            .checked_sub_signed(TimeDelta::seconds(i64::from(from)))
            .is_some_and(|utc| utc.and_utc() > *at),
        EventTime::Date { date } => local.date() > *date,
        EventTime::Zoned { local: until, .. } | EventTime::Floating { local: until } => {
            local > until
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_components;
    use chrono::TimeZone;

    const EASTERN: &str = "BEGIN:VTIMEZONE\r\nTZID:Custom Eastern\r\nBEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nRRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nRRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";

    fn zone(text: &str) -> CustomZone {
        let components = parse_components(text);
        CustomZone::from_vtimezone(&components[0]).expect("valid zone")
    }

    #[test]
    fn outlook_style_rules() {
        let z = zone(EASTERN);
        assert_eq!(z.tzid(), "Custom Eastern");
        // 2024-03-10 07:00Z is the spring-forward instant.
        let before = Utc
            .with_ymd_and_hms(2024, 3, 10, 6, 59, 59)
            .single()
            .expect("time");
        let after = Utc
            .with_ymd_and_hms(2024, 3, 10, 7, 0, 0)
            .single()
            .expect("time");
        assert_eq!(z.offset_at(before), -5 * 3600);
        assert_eq!(z.offset_at(after), -4 * 3600);
        // 2024-11-03 06:00Z falls back.
        let before = Utc
            .with_ymd_and_hms(2024, 11, 3, 5, 59, 59)
            .single()
            .expect("time");
        let after = Utc
            .with_ymd_and_hms(2024, 11, 3, 6, 0, 0)
            .single()
            .expect("time");
        assert_eq!(z.offset_at(before), -4 * 3600);
        assert_eq!(z.offset_at(after), -5 * 3600);
        // Far future still follows the rule.
        let summer = Utc
            .with_ymd_and_hms(2150, 7, 1, 0, 0, 0)
            .single()
            .expect("time");
        assert_eq!(z.offset_at(summer), -4 * 3600);
    }

    #[test]
    fn rdate_and_until_observances() {
        let text = "BEGIN:VTIMEZONE\nTZID:Hist\nBEGIN:DAYLIGHT\nDTSTART:19670430T020000\nRRULE:FREQ=YEARLY;BYMONTH=4;BYDAY=-1SU;UNTIL=19730429T070000Z\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nEND:DAYLIGHT\nBEGIN:STANDARD\nDTSTART:19671029T020000\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19740106T020000\nRDATE:19750223T020000\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nEND:DAYLIGHT\nEND:VTIMEZONE\n";
        let z = zone(text);
        let summer_1972 = Utc
            .with_ymd_and_hms(1972, 7, 1, 0, 0, 0)
            .single()
            .expect("time");
        assert_eq!(z.offset_at(summer_1972), -4 * 3600);
        // UNTIL ended the April rule in 1973: in 1976 April-October is standard.
        let summer_1976 = Utc
            .with_ymd_and_hms(1976, 7, 1, 0, 0, 0)
            .single()
            .expect("time");
        assert_eq!(z.offset_at(summer_1976), -5 * 3600);
        // RDATE onset in 1975.
        let march_1975 = Utc
            .with_ymd_and_hms(1975, 3, 1, 0, 0, 0)
            .single()
            .expect("time");
        assert_eq!(z.offset_at(march_1975), -4 * 3600);
    }

    #[test]
    fn rejects_unusable_definitions() {
        let components = parse_components("BEGIN:VTIMEZONE\nTZID:X\nEND:VTIMEZONE\n");
        assert!(CustomZone::from_vtimezone(&components[0]).is_none());
        let components = parse_components(
            "BEGIN:VTIMEZONE\nBEGIN:STANDARD\nTZOFFSETTO:+0100\nEND:STANDARD\nEND:VTIMEZONE\n",
        );
        assert!(CustomZone::from_vtimezone(&components[0]).is_none());
    }
}
