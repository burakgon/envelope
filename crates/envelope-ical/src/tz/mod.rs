//! Time zone resolution and conversion.
//!
//! A TZID found in calendar data is resolved, in order, by:
//!
//! 1. its name: IANA (any case, also with vendor prefixes such as
//!    `/mozilla.org/20050126_1/Europe/Berlin`), Windows zone ids from the
//!    full CLDR table (`W. Europe Standard Time`), Lotus Notes style ids
//!    (`W. Europe`) and Outlook display names
//!    (`(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna`);
//! 2. the `VTIMEZONE` definition carried by the same calendar (see
//!    [`TzContext`]), evaluated rule by rule;
//! 3. an explicit offset in the TZID (`(UTC+05:00) …`, `GMT+3`) as a fixed zone.
//!
//! Names win over embedded definitions on purpose: embedded `VTIMEZONE`s
//! are frequently outdated (Turkey abolished DST in 2016, Mexico in 2022),
//! while the IANA database is current.

mod custom;
mod generate;
mod names;
pub mod windows;

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::{Arc, LazyLock, Mutex};

use chrono::{
    DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeDelta, TimeZone, Utc,
};
use chrono_tz::{TZ_VARIANTS, Tz};

pub use custom::CustomZone;
pub use generate::generate_vtimezone;
pub use names::{iana_to_windows, is_utc_alias, windows_to_iana};

use crate::component::Component;
use crate::model::EventTime;

/// Resolves a TZID to an IANA zone by name only (no `VTIMEZONE` context).
///
/// ```
/// use chrono_tz::Tz;
/// use envelope_ical::tz::resolve_tzid;
///
/// assert_eq!(resolve_tzid("W. Europe Standard Time"), Some(Tz::Europe__Berlin));
/// assert_eq!(resolve_tzid("/mozilla.org/20050126_1/Europe/Istanbul"), Some(Tz::Europe__Istanbul));
/// assert_eq!(
///     resolve_tzid("(UTC-05:00) Eastern Time (US & Canada)"),
///     Some(Tz::America__New_York)
/// );
/// ```
pub fn resolve_tzid(tzid: &str) -> Option<Tz> {
    names::resolve_strong(tzid).or_else(|| names::resolve_offset_only(tzid))
}

/// A time zone able to convert between wall-clock time and UTC.
#[derive(Debug, Clone, PartialEq)]
pub enum Zone {
    /// A zone of the IANA time zone database.
    Iana(Tz),
    /// A zone known only from a `VTIMEZONE` definition.
    Custom(Arc<CustomZone>),
}

impl Zone {
    /// Offset from UTC in seconds at the given instant.
    pub fn offset_at(&self, utc: DateTime<Utc>) -> i32 {
        match self {
            Zone::Iana(tz) => tz
                .offset_from_utc_datetime(&utc.naive_utc())
                .fix()
                .local_minus_utc(),
            Zone::Custom(zone) => zone.offset_at(utc),
        }
    }

    /// Converts wall-clock time to UTC following RFC 5545 §3.3.5: a time
    /// inside a DST gap is interpreted with the offset in effect before the
    /// gap (so 02:30 on a spring-forward day becomes 03:30), and an ambiguous
    /// time picks its first occurrence.
    pub fn to_utc(&self, local: NaiveDateTime) -> DateTime<Utc> {
        let pseudo = local.and_utc();
        let day = TimeDelta::days(1);
        let before = pseudo.checked_sub_signed(day).map(|t| self.offset_at(t));
        let after = pseudo.checked_add_signed(day).map(|t| self.offset_at(t));
        let mut best: Option<DateTime<Utc>> = None;
        for offset in [before, after].into_iter().flatten() {
            let Some(candidate) = pseudo.checked_sub_signed(TimeDelta::seconds(i64::from(offset)))
            else {
                continue;
            };
            if self.offset_at(candidate) == offset {
                best = Some(best.map_or(candidate, |b| b.min(candidate)));
            }
        }
        if let Some(best) = best {
            return best;
        }
        let offset = before.or(after).unwrap_or(0);
        pseudo
            .checked_sub_signed(TimeDelta::seconds(i64::from(offset)))
            .unwrap_or(pseudo)
    }

    /// Converts a UTC instant to wall-clock time in this zone.
    pub fn to_local(&self, utc: DateTime<Utc>) -> NaiveDateTime {
        let naive = utc.naive_utc();
        naive
            .checked_add_signed(TimeDelta::seconds(i64::from(self.offset_at(utc))))
            .unwrap_or(naive)
    }

    /// The IANA zone, if this is one.
    pub fn iana(&self) -> Option<Tz> {
        match self {
            Zone::Iana(tz) => Some(*tz),
            Zone::Custom(_) => None,
        }
    }

    /// IANA name or custom TZID.
    pub fn name(&self) -> &str {
        match self {
            Zone::Iana(tz) => tz.name(),
            Zone::Custom(zone) => zone.tzid(),
        }
    }
}

/// TZID resolver for one calendar: name-based resolution plus the
/// calendar's own `VTIMEZONE` definitions as a fallback.
///
/// An empty context (`TzContext::default()`) resolves names only, which is
/// what you want for events loaded from storage.
#[derive(Debug, Clone, Default)]
pub struct TzContext {
    custom: HashMap<String, Arc<CustomZone>>,
    /// Best IANA equivalent of custom zones whose TZID is not a known name.
    matches: HashMap<String, Option<Tz>>,
}

impl TzContext {
    /// An empty context: names only.
    pub fn new() -> Self {
        TzContext::default()
    }

    /// Collects every `VTIMEZONE` directly below `calendar`.
    pub fn from_calendar(calendar: &Component) -> Self {
        let mut ctx = TzContext::new();
        ctx.add_calendar(calendar);
        ctx
    }

    /// Adds every `VTIMEZONE` directly below `calendar`.
    pub fn add_calendar(&mut self, calendar: &Component) {
        for vtimezone in calendar.children("VTIMEZONE") {
            self.add_vtimezone(vtimezone);
        }
    }

    /// Adds one `VTIMEZONE` definition. Returns false if it was unusable.
    ///
    /// Definitions whose TZID resolves by name are not evaluated at all:
    /// names win (see the [module docs](self)), so they would never be used.
    pub fn add_vtimezone(&mut self, vtimezone: &Component) -> bool {
        let Some(tzid) = vtimezone
            .value("TZID")
            .map(|t| t.trim().trim_matches('"'))
            .filter(|t| !t.is_empty())
        else {
            return false;
        };
        if names::resolve_strong(tzid).is_some() {
            return true;
        }
        let Some(zone) = CustomZone::from_vtimezone(vtimezone) else {
            return false;
        };
        let key = tzid.to_ascii_lowercase();
        self.matches.insert(key.clone(), best_iana_match(&zone));
        self.custom.insert(key, Arc::new(zone));
        true
    }

    /// The `VTIMEZONE`-defined zone for a TZID that cannot be resolved by
    /// name, if the calendar defines one.
    pub fn custom_zone(&self, tzid: &str) -> Option<Arc<CustomZone>> {
        self.custom
            .get(&tzid.trim().trim_matches('"').to_ascii_lowercase())
            .cloned()
    }

    /// Resolves a TZID to a zone (see the [module docs](self) for the order).
    pub fn resolve(&self, tzid: &str) -> Option<Zone> {
        if let Some(tz) = names::resolve_strong(tzid) {
            return Some(Zone::Iana(tz));
        }
        if let Some(zone) = self.custom_zone(tzid) {
            return Some(Zone::Custom(zone));
        }
        names::resolve_offset_only(tzid).map(Zone::Iana)
    }

    /// The IANA zone for a TZID: by name, or the IANA zone whose rules match
    /// the calendar's `VTIMEZONE` definition (best effort). `None` if the
    /// TZID can only be evaluated from its definition.
    pub fn resolve_iana(&self, tzid: &str) -> Option<Tz> {
        if let Some(tz) = names::resolve_strong(tzid) {
            return Some(tz);
        }
        let key = tzid.trim().trim_matches('"').to_ascii_lowercase();
        if let Some(found) = self.matches.get(&key) {
            return *found;
        }
        if self.custom.contains_key(&key) {
            return None;
        }
        names::resolve_offset_only(tzid)
    }

    /// Resolves `tzid`, falling back to `default_tz`.
    pub fn zone_or(&self, tzid: &str, default_tz: Tz) -> Zone {
        self.resolve(tzid).unwrap_or(Zone::Iana(default_tz))
    }

    /// The zone in which the wall-clock value of `time` is interpreted
    /// (`default_tz` for dates and floating times, UTC for UTC times).
    pub fn zone_of(&self, time: &EventTime, default_tz: Tz) -> Zone {
        match time {
            EventTime::Zoned { tzid, .. } => self.zone_or(tzid, default_tz),
            EventTime::Utc { .. } => Zone::Iana(Tz::UTC),
            EventTime::Date { .. } | EventTime::Floating { .. } => Zone::Iana(default_tz),
        }
    }

    /// Converts an [`EventTime`] to a UTC instant. All-day dates map to
    /// local midnight and floating times to wall-clock time in `default_tz`;
    /// unresolvable TZIDs also fall back to `default_tz`.
    pub fn to_utc(&self, time: &EventTime, default_tz: Tz) -> DateTime<Utc> {
        match time {
            EventTime::Utc { at } => *at,
            other => self.zone_of(other, default_tz).to_utc(wall_clock(other)),
        }
    }

    /// Expresses `time` in the same form as `like`: a date for all-day
    /// values, wall-clock time in `like`'s TZID, a UTC instant or a floating
    /// time. Used to write `EXDATE` and `RECURRENCE-ID` values that match a
    /// series' `DTSTART`.
    pub fn express_like(&self, time: &EventTime, like: &EventTime, default_tz: Tz) -> EventTime {
        let time_of_day = wall_clock(like).time();
        let instant = || match time {
            EventTime::Date { date } => self
                .zone_of(like, default_tz)
                .to_utc(date.and_time(time_of_day)),
            other => self.to_utc(other, default_tz),
        };
        match like {
            EventTime::Date { .. } => EventTime::Date {
                date: wall_clock(time).date(),
            },
            EventTime::Zoned { tzid, .. } => {
                if let EventTime::Zoned { tzid: own, .. } = time
                    && own == tzid
                {
                    return time.clone();
                }
                let zone = self.zone_or(tzid, default_tz);
                EventTime::Zoned {
                    local: zone.to_local(instant()),
                    tzid: tzid.clone(),
                }
            }
            EventTime::Utc { .. } => EventTime::Utc { at: instant() },
            EventTime::Floating { .. } => match time {
                EventTime::Floating { local } => EventTime::Floating { local: *local },
                EventTime::Date { date } => EventTime::Floating {
                    local: date.and_time(time_of_day),
                },
                other => EventTime::Floating {
                    local: Zone::Iana(default_tz).to_local(self.to_utc(other, default_tz)),
                },
            },
        }
    }

    /// True if `a` and `b` denote the same occurrence: same date when either
    /// is all-day, same instant otherwise.
    pub fn same_instant(&self, a: &EventTime, b: &EventTime, default_tz: Tz) -> bool {
        if a.is_date() || b.is_date() {
            return wall_clock(a).date() == wall_clock(b).date();
        }
        self.to_utc(a, default_tz) == self.to_utc(b, default_tz)
    }

    /// Replaces resolvable TZIDs by their IANA name and UTC aliases by UTC
    /// values; other values are returned unchanged.
    pub fn normalize(&self, time: EventTime) -> EventTime {
        match time {
            EventTime::Zoned { local, tzid } => {
                if is_utc_alias(&tzid) {
                    return EventTime::Utc {
                        at: local.and_utc(),
                    };
                }
                match self.resolve_iana(&tzid) {
                    Some(tz) if tz == Tz::UTC || tz.name() == "Etc/UTC" => EventTime::Utc {
                        at: local.and_utc(),
                    },
                    Some(tz) => EventTime::Zoned {
                        local,
                        tzid: tz.name().to_owned(),
                    },
                    None => EventTime::Zoned { local, tzid },
                }
            }
            other => other,
        }
    }
}

/// Converts an [`EventTime`] to UTC with a calendar's resolver; see
/// [`TzContext::to_utc`].
pub fn to_utc(time: &EventTime, ctx: &TzContext, default_tz: Tz) -> DateTime<Utc> {
    ctx.to_utc(time, default_tz)
}

/// The wall-clock value of an [`EventTime`] (midnight for dates, the naive
/// UTC time for UTC values).
pub fn wall_clock(time: &EventTime) -> NaiveDateTime {
    match time {
        EventTime::Date { date } => date.and_time(NaiveTime::MIN),
        EventTime::Zoned { local, .. } | EventTime::Floating { local } => *local,
        EventTime::Utc { at } => at.naive_utc(),
    }
}

/// An [`EventTime`] of the same form as `like` (date, same TZID, UTC or
/// floating) carrying the wall-clock value `local`.
pub fn with_wall_clock(local: NaiveDateTime, like: &EventTime) -> EventTime {
    match like {
        EventTime::Date { .. } => EventTime::Date { date: local.date() },
        EventTime::Zoned { tzid, .. } => EventTime::Zoned {
            local,
            tzid: tzid.clone(),
        },
        EventTime::Utc { .. } => EventTime::Utc {
            at: local.and_utc(),
        },
        EventTime::Floating { .. } => EventTime::Floating { local },
    }
}

/// Results of [`find_iana_match`], keyed by a hash of the definition. Mail
/// and CalDAV data repeat the same custom definition in every object, so
/// the (comparatively expensive) search runs once per distinct zone.
static MATCH_CACHE: LazyLock<Mutex<HashMap<u64, Option<Tz>>>> = LazyLock::new(Default::default);

/// Upper bound on cached definitions.
const MATCH_CACHE_LIMIT: usize = 256;

/// Cached [`find_iana_match`].
fn best_iana_match(zone: &CustomZone) -> Option<Tz> {
    let key = {
        let mut hasher = DefaultHasher::new();
        zone.hash(&mut hasher);
        hasher.finish()
    };
    if let Some(found) = MATCH_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).copied())
    {
        return found;
    }
    let found = find_iana_match(zone);
    if let Ok(mut cache) = MATCH_CACHE.lock() {
        if cache.len() >= MATCH_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, found);
    }
    found
}

/// Finds an IANA zone whose offsets agree with a custom zone between 2008
/// and 2037 (from its first transition on). Candidates sharing a word with
/// the TZID are tried first, then the primary zones of all Windows zones,
/// then every IANA zone.
fn find_iana_match(zone: &CustomZone) -> Option<Tz> {
    let start_year = 2008;
    let end_year = 2037;
    let range_start = Utc.with_ymd_and_hms(start_year, 1, 1, 0, 0, 0).single()?;
    let range_end = Utc.with_ymd_and_hms(end_year, 12, 31, 0, 0, 0).single()?;
    let from = zone
        .first_transition()
        .map_or(range_start, |first| first.max(range_start));
    let mut samples: Vec<DateTime<Utc>> = Vec::new();
    for at in zone.transitions_between(from, range_end) {
        samples.extend(at.checked_sub_signed(TimeDelta::hours(2)));
        samples.extend(at.checked_add_signed(TimeDelta::hours(2)));
    }
    for year in from.year()..=end_year {
        for month in [1, 4, 7, 10] {
            if let Some(sample) = NaiveDate::from_ymd_opt(year, month, 15)
                .and_then(|d| d.and_hms_opt(12, 0, 0))
                .map(|d| d.and_utc())
                && sample >= from
            {
                samples.push(sample);
            }
        }
    }
    let matches = |tz: Tz| {
        let candidate = Zone::Iana(tz);
        samples
            .iter()
            .all(|s| candidate.offset_at(*s) == zone.offset_at(*s))
    };
    let words: Vec<String> = zone
        .tzid()
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(str::to_owned)
        .collect();
    let named = TZ_VARIANTS.iter().copied().filter(|tz| {
        let name = tz.name().to_ascii_lowercase();
        words.iter().any(|w| name.contains(w.as_str()))
    });
    let primary = windows::WINDOWS_ZONES
        .iter()
        .filter_map(|z| Tz::from_str(z.iana).ok());
    named
        .chain(primary)
        .chain(TZ_VARIANTS.iter().copied())
        .find(|tz| matches(*tz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_components;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").expect("valid test datetime")
    }

    fn utc(s: &str) -> DateTime<Utc> {
        dt(s).and_utc()
    }

    #[test]
    fn gap_and_ambiguity_follow_rfc5545() {
        let ny = Zone::Iana(Tz::America__New_York);
        // Spring forward 2024-03-10: 02:30 does not exist → 03:30 EDT.
        assert_eq!(ny.to_utc(dt("2024-03-10 02:30")), utc("2024-03-10 07:30"));
        // Fall back 2024-11-03: 01:30 happens twice → first (EDT).
        assert_eq!(ny.to_utc(dt("2024-11-03 01:30")), utc("2024-11-03 05:30"));
        assert_eq!(ny.to_utc(dt("2024-07-01 09:00")), utc("2024-07-01 13:00"));
        assert_eq!(ny.to_local(utc("2024-12-01 14:00")), dt("2024-12-01 09:00"));

        let berlin = Zone::Iana(Tz::Europe__Berlin);
        assert_eq!(
            berlin.to_utc(dt("2024-03-31 02:30")),
            utc("2024-03-31 01:30")
        );
        assert_eq!(
            berlin.to_utc(dt("2024-10-27 02:30")),
            utc("2024-10-27 00:30")
        );
    }

    #[test]
    fn custom_zone_conversion_matches_iana() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:My Berlin\nBEGIN:STANDARD\nDTSTART:19701025T030000\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19700329T020000\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0200\nEND:DAYLIGHT\nEND:VTIMEZONE\nEND:VCALENDAR\n";
        let cal = &parse_components(text)[0];
        let ctx = TzContext::from_calendar(cal);
        let custom = ctx.resolve("My Berlin").expect("resolvable");
        assert!(matches!(custom, Zone::Custom(_)));
        let berlin = Zone::Iana(Tz::Europe__Berlin);
        for local in [
            "2024-03-31 02:30",
            "2024-10-27 02:30",
            "2024-06-01 12:00",
            "2025-01-01 00:00",
        ] {
            assert_eq!(
                custom.to_utc(dt(local)),
                berlin.to_utc(dt(local)),
                "{local}"
            );
        }
        // The definition is recognised as Berlin-like for normalization.
        let matched = ctx.resolve_iana("My Berlin").expect("matched");
        assert_eq!(Zone::Iana(matched).offset_at(utc("2024-07-01 00:00")), 7200);
        assert_eq!(matched, Tz::Europe__Berlin);
    }

    #[test]
    fn names_win_over_outdated_definitions() {
        // Istanbul with pre-2016 DST rules embedded: the IANA zone (UTC+3 all
        // year since 2016) must be used.
        let text = "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Europe/Istanbul\nBEGIN:STANDARD\nDTSTART:19701025T040000\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\nTZOFFSETFROM:+0300\nTZOFFSETTO:+0200\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19700329T030000\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0300\nEND:DAYLIGHT\nEND:VTIMEZONE\nEND:VCALENDAR\n";
        let ctx = TzContext::from_calendar(&parse_components(text)[0]);
        let t = EventTime::Zoned {
            local: dt("2024-01-15 10:00"),
            tzid: "Europe/Istanbul".into(),
        };
        assert_eq!(ctx.to_utc(&t, Tz::UTC), utc("2024-01-15 07:00"));
    }

    #[test]
    fn to_utc_for_every_form() {
        let ctx = TzContext::new();
        let date = EventTime::Date {
            date: NaiveDate::from_ymd_opt(2024, 6, 1).expect("date"),
        };
        assert_eq!(
            ctx.to_utc(&date, Tz::Europe__Istanbul),
            utc("2024-05-31 21:00")
        );
        let floating = EventTime::Floating {
            local: dt("2024-06-01 09:00"),
        };
        assert_eq!(
            ctx.to_utc(&floating, Tz::America__New_York),
            utc("2024-06-01 13:00")
        );
        let unknown = EventTime::Zoned {
            local: dt("2024-06-01 09:00"),
            tzid: "Nowhere/Special".into(),
        };
        assert_eq!(
            ctx.to_utc(&unknown, Tz::Europe__Berlin),
            utc("2024-06-01 07:00")
        );
        let windows = EventTime::Zoned {
            local: dt("2024-06-01 09:00"),
            tzid: "Pacific Standard Time".into(),
        };
        assert_eq!(to_utc(&windows, &ctx, Tz::UTC), utc("2024-06-01 16:00"));
    }

    #[test]
    fn normalize_and_express_like() {
        let ctx = TzContext::new();
        let windows = EventTime::Zoned {
            local: dt("2024-06-01 09:00"),
            tzid: "W. Europe Standard Time".into(),
        };
        assert_eq!(
            ctx.normalize(windows.clone()),
            EventTime::Zoned {
                local: dt("2024-06-01 09:00"),
                tzid: "Europe/Berlin".into()
            }
        );
        let utc_zoned = EventTime::Zoned {
            local: dt("2024-06-01 09:00"),
            tzid: "UTC".into(),
        };
        assert_eq!(
            ctx.normalize(utc_zoned),
            EventTime::Utc {
                at: utc("2024-06-01 09:00")
            }
        );

        let like = EventTime::Zoned {
            local: dt("2024-01-01 09:00"),
            tzid: "America/New_York".into(),
        };
        let instant = EventTime::Utc {
            at: utc("2024-06-01 13:00"),
        };
        assert_eq!(
            ctx.express_like(&instant, &like, Tz::UTC),
            EventTime::Zoned {
                local: dt("2024-06-01 09:00"),
                tzid: "America/New_York".into()
            }
        );
        let date_like = EventTime::Date {
            date: NaiveDate::from_ymd_opt(2024, 1, 1).expect("date"),
        };
        assert_eq!(
            ctx.express_like(&windows, &date_like, Tz::UTC),
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 6, 1).expect("date")
            }
        );
        assert!(!ctx.same_instant(&instant, &like.clone(), Tz::UTC));
        assert!(ctx.same_instant(
            &instant,
            &EventTime::Zoned {
                local: dt("2024-06-01 09:00"),
                tzid: "America/New_York".into()
            },
            Tz::UTC
        ));
    }
}
