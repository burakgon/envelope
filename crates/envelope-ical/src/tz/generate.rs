//! `VTIMEZONE` generation from the IANA database.

use chrono::{DateTime, Datelike, NaiveDateTime, Offset, TimeDelta, TimeZone, Timelike, Utc};
use chrono_tz::{OffsetName, Tz};

use crate::component::{Component, Property};
use crate::recur::rule::weekday_code;
use crate::value::{format_date_time, format_utc, format_utc_offset};

#[derive(Debug, Clone, Copy)]
struct Transition {
    at: DateTime<Utc>,
    from: i32,
    to: i32,
}

impl Transition {
    /// Wall-clock time of the transition as seen before it happens.
    fn local(&self) -> Option<NaiveDateTime> {
        self.at
            .naive_utc()
            .checked_add_signed(TimeDelta::seconds(i64::from(self.from)))
    }
}

fn offset_at(tz: Tz, at: DateTime<Utc>) -> i32 {
    tz.offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc()
}

fn abbreviation(tz: Tz, at: DateTime<Utc>) -> Option<String> {
    tz.offset_from_utc_datetime(&at.naive_utc())
        .abbreviation()
        .map(str::to_owned)
        .filter(|a| !a.is_empty())
}

/// All offset changes in `[start, end)`, located to the second.
fn transitions(tz: Tz, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<Transition> {
    let mut found = Vec::new();
    let step = TimeDelta::hours(12);
    let mut cursor = start;
    let mut current = offset_at(tz, cursor);
    while cursor < end {
        let Some(next) = cursor.checked_add_signed(step) else {
            break;
        };
        let next_offset = offset_at(tz, next);
        if next_offset != current {
            // Binary search for the first second with the new offset.
            let (mut lo, mut hi) = (cursor.timestamp(), next.timestamp());
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                let at = DateTime::from_timestamp(mid, 0).unwrap_or(next);
                if offset_at(tz, at) == current {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            if let Some(at) = DateTime::from_timestamp(hi, 0) {
                found.push(Transition {
                    at,
                    from: current,
                    to: next_offset,
                });
            }
            current = next_offset;
        }
        cursor = next;
    }
    found
}

/// A yearly `BYMONTH`/`BYDAY` rule describing all transitions, if one exists.
fn yearly_rule(group: &[Transition]) -> Option<String> {
    if group.len() < 2 {
        return None;
    }
    let locals: Vec<NaiveDateTime> = group.iter().map(Transition::local).collect::<Option<_>>()?;
    let first = locals[0];
    let same_shape = group
        .iter()
        .zip(&locals)
        .enumerate()
        .all(|(i, (t, local))| {
            t.from == group[0].from
                && t.to == group[0].to
                && local.month() == first.month()
                && local.weekday() == first.weekday()
                && local.time() == first.time()
                && local.year() == first.year() + i as i32
        });
    if !same_shape {
        return None;
    }
    let is_last = |d: &NaiveDateTime| {
        let dim = days_in_month(d.year(), d.month());
        d.day() + 7 > dim
    };
    let ordinal = |d: &NaiveDateTime| (d.day() - 1) / 7 + 1;
    let day = weekday_code(first.weekday());
    let byday = if locals.iter().all(is_last) {
        format!("-1{day}")
    } else if locals.iter().all(|d| ordinal(d) == ordinal(&first)) {
        format!("{}{day}", ordinal(&first))
    } else {
        return None;
    };
    Some(format!(
        "FREQ=YEARLY;BYMONTH={};BYDAY={byday}",
        first.month()
    ))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|d| d.pred_opt())
        .map_or(31, |d| d.day())
}

fn observance(
    name: &str,
    dtstart: NaiveDateTime,
    from: i32,
    to: i32,
    tzname: Option<String>,
) -> Component {
    let mut c = Component::new(name);
    c.push(Property::new("DTSTART", format_date_time(dtstart)));
    c.push(Property::new("TZOFFSETFROM", format_utc_offset(from)));
    c.push(Property::new("TZOFFSETTO", format_utc_offset(to)));
    if let Some(tzname) = tzname {
        c.push(Property::text("TZNAME", &tzname));
    }
    c
}

/// Generates a `VTIMEZONE` for `tz` that is valid for events around
/// `around` (from the start of the previous year on).
///
/// Transitions are taken from the IANA database. When the daylight and
/// standard transitions follow a yearly weekday rule (as in the EU and the
/// US) the observances carry an `RRULE`; otherwise each transition becomes
/// its own observance. Zones without DST get a single `STANDARD`.
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use envelope_ical::tz::generate_vtimezone;
///
/// let vtz = generate_vtimezone(chrono_tz::Europe::Berlin, Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap());
/// let text = vtz.to_ical();
/// assert!(text.contains("RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU"));
/// assert!(text.contains("TZOFFSETTO:+0200"));
/// ```
pub fn generate_vtimezone(tz: Tz, around: DateTime<Utc>) -> Component {
    let mut vtimezone = Component::new("VTIMEZONE");
    vtimezone.push(Property::new("TZID", tz.name()));
    vtimezone.push(Property::new("X-LIC-LOCATION", tz.name()));

    let year = around.year();
    let range = (
        Utc.with_ymd_and_hms(year - 1, 1, 1, 0, 0, 0).single(),
        Utc.with_ymd_and_hms(year + 3, 1, 1, 0, 0, 0).single(),
    );
    let found = match range {
        (Some(start), Some(end)) => transitions(tz, start, end),
        _ => Vec::new(),
    };
    if found.is_empty() {
        let offset = offset_at(tz, around);
        let start = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default();
        vtimezone.components.push(observance(
            "STANDARD",
            start,
            offset,
            offset,
            abbreviation(tz, around),
        ));
        return vtimezone;
    }

    let last_year = year + 2;
    for (name, daylight) in [("DAYLIGHT", true), ("STANDARD", false)] {
        let group: Vec<Transition> = found
            .iter()
            .copied()
            .filter(|t| (t.to > t.from) == daylight)
            .collect();
        let Some(first) = group.first() else {
            continue;
        };
        let Some(first_local) = first.local() else {
            continue;
        };
        if let Some(rule) = yearly_rule(&group) {
            let mut c = observance(
                name,
                first_local,
                first.from,
                first.to,
                abbreviation(tz, first.at),
            );
            let mut rrule = rule;
            if let Some(last) = group.last().filter(|t| t.at.year() < last_year) {
                // The rule stopped (e.g. DST was abolished): bound it.
                rrule.push_str(&format!(";UNTIL={}", format_utc(last.at)));
            }
            c.push(Property::new("RRULE", rrule));
            vtimezone.components.push(c);
        } else {
            for t in &group {
                if let Some(local) = t.local() {
                    vtimezone.components.push(observance(
                        name,
                        local,
                        t.from,
                        t.to,
                        abbreviation(tz, t.at),
                    ));
                }
            }
        }
    }
    // Keep observances in chronological order for readability.
    vtimezone.components.sort_by_key(|c| {
        c.value("DTSTART")
            .and_then(|v| crate::value::parse_date_time(v).map(|(d, _)| d))
            .map(|d| (d.date(), d.hour()))
    });
    vtimezone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tz::{CustomZone, Zone};

    fn utc(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0)
            .single()
            .expect("valid date")
    }

    /// The generated definition must agree with the IANA zone everywhere in
    /// the years it is meant for.
    fn assert_round_trip(tz: Tz, around: DateTime<Utc>) {
        let vtz = generate_vtimezone(tz, around);
        let zone = CustomZone::from_vtimezone(&vtz).expect("valid definition");
        let iana = Zone::Iana(tz);
        let custom = Zone::Custom(std::sync::Arc::new(zone));
        let start = utc(around.year(), 1, 1);
        let mut at = start;
        let end = utc(around.year() + 3, 1, 1);
        while at < end {
            assert_eq!(
                custom.offset_at(at),
                iana.offset_at(at),
                "{} at {at}\n{}",
                tz.name(),
                vtz.to_ical()
            );
            at += TimeDelta::hours(5);
        }
    }

    #[test]
    fn generated_definitions_match_iana() {
        let around = utc(2025, 6, 1);
        for tz in [
            Tz::Europe__Berlin,
            Tz::America__New_York,
            Tz::America__Los_Angeles,
            Tz::Europe__London,
            Tz::Europe__Istanbul,
            Tz::Australia__Sydney,
            Tz::Pacific__Auckland,
            Tz::Asia__Kolkata,
            Tz::Asia__Tokyo,
            Tz::America__Sao_Paulo,
            Tz::Africa__Casablanca,
            Tz::Asia__Tehran,
            Tz::Europe__Dublin,
            Tz::America__Santiago,
            Tz::Australia__Lord_Howe,
            Tz::America__Mexico_City,
            Tz::UTC,
        ] {
            assert_round_trip(tz, around);
        }
        // A year in which a zone abolished DST.
        assert_round_trip(Tz::Europe__Istanbul, utc(2016, 3, 1));
        assert_round_trip(Tz::America__Mexico_City, utc(2022, 3, 1));
    }

    #[test]
    fn eu_and_us_rules_use_rrules() {
        let berlin = generate_vtimezone(Tz::Europe__Berlin, utc(2025, 6, 1)).to_ical();
        assert!(berlin.contains("BEGIN:DAYLIGHT\r\nDTSTART:20240331T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nTZNAME:CEST\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\n"));
        assert!(berlin.contains("RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU"));
        let ny = generate_vtimezone(Tz::America__New_York, utc(2025, 6, 1)).to_ical();
        assert!(ny.contains("RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU"));
        assert!(ny.contains("RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU"));
        assert!(ny.contains("TZID:America/New_York"));
    }

    #[test]
    fn zones_without_dst_get_a_single_standard() {
        let ist = generate_vtimezone(Tz::Europe__Istanbul, utc(2025, 6, 1));
        assert_eq!(ist.components.len(), 1);
        assert_eq!(ist.components[0].name, "STANDARD");
        assert_eq!(ist.components[0].value("TZOFFSETTO"), Some("+0300"));
        assert_eq!(ist.components[0].value("TZOFFSETFROM"), Some("+0300"));
    }
}
