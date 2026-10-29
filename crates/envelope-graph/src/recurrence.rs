//! Conversion between Graph `patternedRecurrence` and iCalendar `RRULE`
//! values (RFC 5545 section 3.3.10).
//!
//! Graph pattern types map onto RRULE frequencies like this:
//!
//! | Graph pattern     | RRULE                                                   |
//! |-------------------|---------------------------------------------------------|
//! | `daily`           | `FREQ=DAILY`                                            |
//! | `weekly`          | `FREQ=WEEKLY;BYDAY=…;WKST=…`                            |
//! | `absoluteMonthly` | `FREQ=MONTHLY;BYMONTHDAY=d`                             |
//! | `relativeMonthly` | `FREQ=MONTHLY;BYDAY=2TU` or `BYDAY=MO,TU;BYSETPOS=n`    |
//! | `absoluteYearly`  | `FREQ=YEARLY;BYMONTH=m;BYMONTHDAY=d`                    |
//! | `relativeYearly`  | `FREQ=YEARLY;BYMONTH=m;BYDAY=-1WE` (or `BYSETPOS`)      |
//!
//! Ranges map to `UNTIL` (`endDate`), `COUNT` (`numbered`) or nothing
//! (`noEnd`). Graph's `endDate` is an inclusive *local* date, whereas a
//! timed RRULE needs a UTC `UNTIL`: the end of that local day is converted
//! with the recurrence time zone when it can be resolved (see
//! [`TimeZoneResolver`](crate::TimeZoneResolver)) or with the UTC offset of
//! the series start when the client knows it; otherwise `T235959Z` is used,
//! which still round-trips the date exactly.
//!
//! Known semantic difference: Exchange moves a monthly or yearly
//! `dayOfMonth` that a month lacks (29–31) to the month's last day, while
//! `BYMONTHDAY` skips such months.

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Timelike, Utc, Weekday,
};
use chrono_tz::Tz;
use envelope_ical::EventTime;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::RecurrenceError;
use crate::serde_util::{graph_enum, is_zero, null_default};

graph_enum! {
    /// Kind of a recurrence pattern.
    pub enum RecurrencePatternType {
        /// Every `interval` days.
        Daily = "daily",
        /// On `days_of_week`, every `interval` weeks.
        Weekly = "weekly",
        /// On `day_of_month`, every `interval` months.
        AbsoluteMonthly = "absoluteMonthly",
        /// On the `index`-th of `days_of_week`, every `interval` months.
        RelativeMonthly = "relativeMonthly",
        /// On `day_of_month` of `month`, every `interval` years.
        AbsoluteYearly = "absoluteYearly",
        /// On the `index`-th of `days_of_week` in `month`, every `interval` years.
        RelativeYearly = "relativeYearly",
    }
}

graph_enum! {
    /// Day of the week.
    pub enum DayOfWeek {
        /// Sunday.
        Sunday = "sunday",
        /// Monday.
        Monday = "monday",
        /// Tuesday.
        Tuesday = "tuesday",
        /// Wednesday.
        Wednesday = "wednesday",
        /// Thursday.
        Thursday = "thursday",
        /// Friday.
        Friday = "friday",
        /// Saturday.
        Saturday = "saturday",
    }
}

impl DayOfWeek {
    /// The two-letter RRULE code (`SU`, `MO`, …).
    pub fn rrule_code(self) -> &'static str {
        match self {
            DayOfWeek::Sunday => "SU",
            DayOfWeek::Monday => "MO",
            DayOfWeek::Tuesday => "TU",
            DayOfWeek::Wednesday => "WE",
            DayOfWeek::Thursday => "TH",
            DayOfWeek::Friday => "FR",
            DayOfWeek::Saturday => "SA",
        }
    }

    /// Parses a two-letter RRULE code, ignoring case.
    pub fn from_rrule_code(code: &str) -> Option<Self> {
        DayOfWeek::ALL
            .iter()
            .copied()
            .find(|d| d.rrule_code().eq_ignore_ascii_case(code))
    }

    /// The equivalent [`chrono::Weekday`].
    pub fn to_weekday(self) -> Weekday {
        match self {
            DayOfWeek::Sunday => Weekday::Sun,
            DayOfWeek::Monday => Weekday::Mon,
            DayOfWeek::Tuesday => Weekday::Tue,
            DayOfWeek::Wednesday => Weekday::Wed,
            DayOfWeek::Thursday => Weekday::Thu,
            DayOfWeek::Friday => Weekday::Fri,
            DayOfWeek::Saturday => Weekday::Sat,
        }
    }

    /// Converts a [`chrono::Weekday`].
    pub fn from_weekday(weekday: Weekday) -> Self {
        match weekday {
            Weekday::Sun => DayOfWeek::Sunday,
            Weekday::Mon => DayOfWeek::Monday,
            Weekday::Tue => DayOfWeek::Tuesday,
            Weekday::Wed => DayOfWeek::Wednesday,
            Weekday::Thu => DayOfWeek::Thursday,
            Weekday::Fri => DayOfWeek::Friday,
            Weekday::Sat => DayOfWeek::Saturday,
        }
    }
}

graph_enum! {
    /// Which instance of the allowed days a relative pattern selects.
    pub enum WeekIndex {
        /// The first.
        First = "first",
        /// The second.
        Second = "second",
        /// The third.
        Third = "third",
        /// The fourth.
        Fourth = "fourth",
        /// The last.
        Last = "last",
    }
}

impl WeekIndex {
    /// The RRULE ordinal (`1`–`4`, or `-1` for last).
    pub fn ordinal(self) -> i32 {
        match self {
            WeekIndex::First => 1,
            WeekIndex::Second => 2,
            WeekIndex::Third => 3,
            WeekIndex::Fourth => 4,
            WeekIndex::Last => -1,
        }
    }

    /// The index for an RRULE ordinal, if Graph can express it.
    pub fn from_ordinal(ordinal: i32) -> Option<Self> {
        match ordinal {
            1 => Some(WeekIndex::First),
            2 => Some(WeekIndex::Second),
            3 => Some(WeekIndex::Third),
            4 => Some(WeekIndex::Fourth),
            -1 => Some(WeekIndex::Last),
            _ => None,
        }
    }
}

graph_enum! {
    /// How a recurrence ends.
    pub enum RecurrenceRangeType {
        /// On `end_date` (inclusive).
        EndDate = "endDate",
        /// Never.
        NoEnd = "noEnd",
        /// After `number_of_occurrences` occurrences.
        Numbered = "numbered",
    }
}

/// A recurrence: how often (`pattern`) and for how long (`range`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternedRecurrence {
    /// The repetition pattern.
    pub pattern: RecurrencePattern,
    /// The duration of the recurrence.
    pub range: RecurrenceRange,
}

/// The repetition part of a recurrence. Graph fills unused fields with
/// placeholders (`month: 0`, `index: "first"`, …) in responses; only the
/// fields relevant to [`RecurrencePattern::kind`] matter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurrencePattern {
    /// Pattern kind.
    #[serde(rename = "type")]
    pub kind: RecurrencePatternType,
    /// Number of units (days, weeks, months, years) between occurrences.
    #[serde(default, deserialize_with = "null_default")]
    pub interval: u32,
    /// Month (1–12) for yearly patterns.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_zero"
    )]
    pub month: u32,
    /// Day of the month (1–31) for absolute patterns.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_zero"
    )]
    pub day_of_month: u32,
    /// Days for weekly and relative patterns.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub days_of_week: Vec<DayOfWeek>,
    /// First day of the week (weekly patterns; Graph default Sunday).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_day_of_week: Option<DayOfWeek>,
    /// Instance selector for relative patterns (default first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<WeekIndex>,
}

/// The duration part of a recurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecurrenceRange {
    /// How the recurrence ends.
    #[serde(rename = "type")]
    pub kind: RecurrenceRangeType,
    /// First day of the recurrence; must equal the start date of the series.
    #[serde(
        default,
        deserialize_with = "lenient_date",
        skip_serializing_if = "Option::is_none"
    )]
    pub start_date: Option<NaiveDate>,
    /// Last day (inclusive) for [`RecurrenceRangeType::EndDate`].
    #[serde(
        default,
        deserialize_with = "lenient_date",
        skip_serializing_if = "Option::is_none"
    )]
    pub end_date: Option<NaiveDate>,
    /// Time zone of `start_date`/`end_date` (defaults to the event's zone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_time_zone: Option<String>,
    /// Number of occurrences for [`RecurrenceRangeType::Numbered`].
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_zero"
    )]
    pub number_of_occurrences: u32,
}

/// Accepts `null`, `"2017-09-04"` and timestamps starting with a date.
fn lenient_date<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<NaiveDate>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|v| {
        v.get(..10)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    }))
}

impl PatternedRecurrence {
    /// Converts to an RRULE value (without the `RRULE:` prefix). See
    /// [`to_rrule`].
    pub fn to_rrule(&self, start: &EventTime, tz: Option<Tz>) -> Result<String, RecurrenceError> {
        to_rrule(self, start, tz)
    }

    /// Builds a recurrence from an RRULE value. See [`from_rrule`].
    pub fn from_rrule(
        rrule: &str,
        start: &EventTime,
        tz: Option<Tz>,
    ) -> Result<Self, RecurrenceError> {
        from_rrule(rrule, start, tz)
    }
}

/// How the wall-clock times of a series map to UTC: a resolved zone, or a
/// fixed offset derived from the series start when the zone name cannot be
/// resolved (exact unless DST differs between the start and `UNTIL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeriesZone {
    /// A resolved time zone.
    Named(Tz),
    /// A fixed UTC offset.
    Fixed(FixedOffset),
}

impl SeriesZone {
    fn to_utc(self, local: NaiveDateTime) -> Option<DateTime<Utc>> {
        match self {
            // 23:59:59 can fall into a DST gap; an hour earlier cannot.
            SeriesZone::Named(tz) => tz
                .from_local_datetime(&local)
                .earliest()
                .or_else(|| {
                    local
                        .checked_sub_signed(Duration::hours(1))
                        .and_then(|earlier| tz.from_local_datetime(&earlier).earliest())
                })
                .map(|dt| dt.with_timezone(&Utc)),
            SeriesZone::Fixed(offset) => offset
                .from_local_datetime(&local)
                .single()
                .map(|dt| dt.with_timezone(&Utc)),
        }
    }

    fn to_local(self, at: DateTime<Utc>) -> NaiveDateTime {
        match self {
            SeriesZone::Named(tz) => at.with_timezone(&tz).naive_local(),
            SeriesZone::Fixed(offset) => at.with_timezone(&offset).naive_local(),
        }
    }
}

/// Converts a Graph recurrence into an RRULE value (without `RRULE:`).
///
/// `start` is the series start (`DTSTART`); `tz` is the resolved recurrence
/// time zone, used to turn the inclusive local `endDate` into a UTC `UNTIL`.
pub fn to_rrule(
    recurrence: &PatternedRecurrence,
    start: &EventTime,
    tz: Option<Tz>,
) -> Result<String, RecurrenceError> {
    to_rrule_in(recurrence, start, tz.map(SeriesZone::Named))
}

/// [`to_rrule`] with a zone that may be a fixed offset.
pub(crate) fn to_rrule_in(
    recurrence: &PatternedRecurrence,
    start: &EventTime,
    zone: Option<SeriesZone>,
) -> Result<String, RecurrenceError> {
    let pattern = &recurrence.pattern;
    let local_start = local_start(start, zone);
    let mut parts: Vec<String> = Vec::new();
    let freq = match pattern.kind {
        RecurrencePatternType::Daily => "DAILY",
        RecurrencePatternType::Weekly => "WEEKLY",
        RecurrencePatternType::AbsoluteMonthly | RecurrencePatternType::RelativeMonthly => {
            "MONTHLY"
        }
        RecurrencePatternType::AbsoluteYearly | RecurrencePatternType::RelativeYearly => "YEARLY",
    };
    parts.push(format!("FREQ={freq}"));
    let interval = pattern.interval.max(1);
    if interval > 1 {
        parts.push(format!("INTERVAL={interval}"));
    }
    match pattern.kind {
        RecurrencePatternType::Daily => {}
        RecurrencePatternType::Weekly => {
            if !pattern.days_of_week.is_empty() {
                parts.push(format!("BYDAY={}", day_list(&pattern.days_of_week)));
            }
            // Monday is the RFC 5545 default and needs no WKST.
            if let Some(first) = pattern
                .first_day_of_week
                .filter(|d| *d != DayOfWeek::Monday)
            {
                parts.push(format!("WKST={}", first.rrule_code()));
            }
        }
        RecurrencePatternType::AbsoluteMonthly => {
            parts.push(format!(
                "BYMONTHDAY={}",
                day_of_month(pattern, local_start.date())?
            ));
        }
        RecurrencePatternType::RelativeMonthly => relative_days(pattern, &mut parts)?,
        RecurrencePatternType::AbsoluteYearly => {
            parts.push(format!("BYMONTH={}", month(pattern, local_start.date())?));
            parts.push(format!(
                "BYMONTHDAY={}",
                day_of_month(pattern, local_start.date())?
            ));
        }
        RecurrencePatternType::RelativeYearly => {
            parts.push(format!("BYMONTH={}", month(pattern, local_start.date())?));
            relative_days(pattern, &mut parts)?;
        }
    }
    match recurrence.range.kind {
        RecurrenceRangeType::EndDate => {
            let end = recurrence.range.end_date.ok_or_else(|| {
                RecurrenceError::Invalid("endDate range without an endDate".into())
            })?;
            parts.push(format!("UNTIL={}", until_value(end, start, zone)));
        }
        RecurrenceRangeType::Numbered => {
            let count = recurrence.range.number_of_occurrences;
            if count == 0 {
                return Err(RecurrenceError::Invalid(
                    "numbered range without numberOfOccurrences".into(),
                ));
            }
            parts.push(format!("COUNT={count}"));
        }
        RecurrenceRangeType::NoEnd => {}
    }
    Ok(parts.join(";"))
}

fn day_list(days: &[DayOfWeek]) -> String {
    days.iter()
        .map(|d| d.rrule_code())
        .collect::<Vec<_>>()
        .join(",")
}

fn day_of_month(pattern: &RecurrencePattern, start: NaiveDate) -> Result<u32, RecurrenceError> {
    match pattern.day_of_month {
        0 => Ok(start.day()),
        day @ 1..=31 => Ok(day),
        other => Err(RecurrenceError::Invalid(format!("dayOfMonth {other}"))),
    }
}

fn month(pattern: &RecurrencePattern, start: NaiveDate) -> Result<u32, RecurrenceError> {
    match pattern.month {
        0 => Ok(start.month()),
        month @ 1..=12 => Ok(month),
        other => Err(RecurrenceError::Invalid(format!("month {other}"))),
    }
}

fn relative_days(
    pattern: &RecurrencePattern,
    parts: &mut Vec<String>,
) -> Result<(), RecurrenceError> {
    let ordinal = pattern.index.unwrap_or(WeekIndex::First).ordinal();
    match pattern.days_of_week.as_slice() {
        [] => Err(RecurrenceError::Invalid(
            "relative pattern without daysOfWeek".into(),
        )),
        [day] => {
            parts.push(format!("BYDAY={ordinal}{}", day.rrule_code()));
            Ok(())
        }
        days => {
            parts.push(format!("BYDAY={}", day_list(days)));
            parts.push(format!("BYSETPOS={ordinal}"));
            Ok(())
        }
    }
}

/// Wall-clock start of the series in its own zone.
fn local_start(start: &EventTime, zone: Option<SeriesZone>) -> NaiveDateTime {
    match start {
        EventTime::Date { date } => date.and_time(NaiveTime::MIN),
        EventTime::Zoned { local, .. } | EventTime::Floating { local } => *local,
        EventTime::Utc { at } => zone.map_or_else(|| at.naive_utc(), |zone| zone.to_local(*at)),
    }
}

/// `UNTIL` for an inclusive local end date, matching the value type of DTSTART.
fn until_value(end: NaiveDate, start: &EventTime, zone: Option<SeriesZone>) -> String {
    let compact = end.format("%Y%m%d");
    match start {
        EventTime::Date { .. } => compact.to_string(),
        EventTime::Floating { .. } => format!("{compact}T235959"),
        EventTime::Zoned { .. } | EventTime::Utc { .. } => {
            let local_end =
                end.and_time(NaiveTime::from_hms_opt(23, 59, 59).unwrap_or(NaiveTime::MIN));
            let utc = zone
                .and_then(|zone| zone.to_utc(local_end))
                .unwrap_or_else(|| local_end.and_utc());
            utc.format("%Y%m%dT%H%M%SZ").to_string()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Until {
    Date(NaiveDate),
    Floating(NaiveDateTime),
    Utc(DateTime<Utc>),
}

#[derive(Debug, Default)]
struct ParsedRule {
    freq: Option<Freq>,
    interval: Option<u32>,
    count: Option<u32>,
    until: Option<Until>,
    by_day: Vec<(Option<i32>, DayOfWeek)>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
    by_set_pos: Vec<i32>,
    wkst: Option<DayOfWeek>,
    by_hour: Vec<u32>,
    by_minute: Vec<u32>,
    by_second: Vec<u32>,
}

fn invalid(message: impl Into<String>) -> RecurrenceError {
    RecurrenceError::Invalid(message.into())
}

fn unsupported(message: impl Into<String>) -> RecurrenceError {
    RecurrenceError::Unsupported(message.into())
}

fn parse_list<T: std::str::FromStr>(key: &str, value: &str) -> Result<Vec<T>, RecurrenceError> {
    value
        .split(',')
        .map(|v| {
            v.trim()
                .trim_start_matches('+')
                .parse::<T>()
                .map_err(|_| invalid(format!("bad {key} value `{v}`")))
        })
        .collect()
}

fn parse_by_day(value: &str) -> Result<Vec<(Option<i32>, DayOfWeek)>, RecurrenceError> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            let split = item
                .len()
                .checked_sub(2)
                .ok_or_else(|| invalid(format!("bad BYDAY `{item}`")))?;
            let (ordinal, code) = item
                .split_at_checked(split)
                .ok_or_else(|| invalid(format!("bad BYDAY `{item}`")))?;
            let day = DayOfWeek::from_rrule_code(code)
                .ok_or_else(|| invalid(format!("bad BYDAY `{item}`")))?;
            let ordinal = match ordinal.trim_start_matches('+') {
                "" => None,
                n => Some(
                    n.parse::<i32>()
                        .map_err(|_| invalid(format!("bad BYDAY `{item}`")))?,
                ),
            };
            if ordinal == Some(0) {
                return Err(invalid(format!("bad BYDAY `{item}`")));
            }
            Ok((ordinal, day))
        })
        .collect()
}

fn parse_until(value: &str) -> Result<Until, RecurrenceError> {
    let value = value.trim();
    if let Some(utc) = value.strip_suffix(['Z', 'z']) {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
            .map(|dt| Until::Utc(dt.and_utc()))
            .map_err(|_| invalid(format!("bad UNTIL `{value}`")));
    }
    if value.len() == 8 {
        return NaiveDate::parse_from_str(value, "%Y%m%d")
            .map(Until::Date)
            .map_err(|_| invalid(format!("bad UNTIL `{value}`")));
    }
    NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .map(Until::Floating)
        .map_err(|_| invalid(format!("bad UNTIL `{value}`")))
}

impl ParsedRule {
    fn parse(rrule: &str) -> Result<Self, RecurrenceError> {
        let trimmed = rrule.trim();
        let body = match trimmed.get(..6) {
            Some(prefix) if prefix.eq_ignore_ascii_case("RRULE:") => &trimmed[6..],
            _ => trimmed,
        };
        let mut rule = ParsedRule::default();
        for part in body.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| invalid(format!("malformed rule part `{part}`")))?;
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim();
            match key.as_str() {
                "FREQ" => {
                    rule.freq = Some(match value.to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        "HOURLY" | "MINUTELY" | "SECONDLY" => {
                            return Err(unsupported(format!("FREQ={value}")));
                        }
                        _ => return Err(invalid(format!("unknown FREQ `{value}`"))),
                    });
                }
                "INTERVAL" => {
                    let interval = value
                        .parse::<u32>()
                        .ok()
                        .filter(|i| *i > 0)
                        .ok_or_else(|| invalid(format!("bad INTERVAL `{value}`")))?;
                    rule.interval = Some(interval);
                }
                "COUNT" => {
                    let count = value
                        .parse::<u32>()
                        .ok()
                        .filter(|c| *c > 0)
                        .ok_or_else(|| invalid(format!("bad COUNT `{value}`")))?;
                    rule.count = Some(count);
                }
                "UNTIL" => rule.until = Some(parse_until(value)?),
                "BYDAY" => rule.by_day = parse_by_day(value)?,
                "BYMONTHDAY" => rule.by_month_day = parse_list(&key, value)?,
                "BYMONTH" => rule.by_month = parse_list(&key, value)?,
                "BYSETPOS" => rule.by_set_pos = parse_list(&key, value)?,
                "BYHOUR" => rule.by_hour = parse_list(&key, value)?,
                "BYMINUTE" => rule.by_minute = parse_list(&key, value)?,
                "BYSECOND" => rule.by_second = parse_list(&key, value)?,
                "WKST" => {
                    rule.wkst = Some(
                        DayOfWeek::from_rrule_code(value)
                            .ok_or_else(|| invalid(format!("bad WKST `{value}`")))?,
                    );
                }
                "BYYEARDAY" | "BYWEEKNO" | "RSCALE" | "SKIP" => {
                    return Err(unsupported(format!("{key} is not supported by Graph")));
                }
                other if other.starts_with("X-") => {}
                other => return Err(invalid(format!("unknown rule part `{other}`"))),
            }
        }
        if rule.count.is_some() && rule.until.is_some() {
            return Err(invalid("COUNT and UNTIL are mutually exclusive"));
        }
        if rule.by_month.iter().any(|m| !(1..=12).contains(m)) {
            return Err(invalid("BYMONTH out of range"));
        }
        if rule
            .by_month_day
            .iter()
            .any(|d| *d == 0 || !(-31..=31).contains(d))
        {
            return Err(invalid("BYMONTHDAY out of range"));
        }
        Ok(rule)
    }

    /// BYHOUR/BYMINUTE/BYSECOND are accepted only when they just restate the
    /// start time.
    fn check_time_parts(&self, start: NaiveDateTime) -> Result<(), RecurrenceError> {
        let matches = |values: &[u32], actual: u32| values.is_empty() || values == [actual];
        if matches(&self.by_hour, start.hour())
            && matches(&self.by_minute, start.minute())
            && matches(&self.by_second, start.second())
        {
            Ok(())
        } else {
            Err(unsupported(
                "BYHOUR/BYMINUTE/BYSECOND with several occurrences per day",
            ))
        }
    }

    fn plain_days(&self) -> Result<Vec<DayOfWeek>, RecurrenceError> {
        self.by_day
            .iter()
            .map(|(ordinal, day)| match ordinal {
                None => Ok(*day),
                Some(_) => Err(unsupported(
                    "BYDAY ordinals require a monthly or yearly rule",
                )),
            })
            .collect()
    }
}

/// Converts an RRULE value (with or without the `RRULE:` prefix) into a Graph
/// recurrence.
///
/// `start` is the series start; `tz` the resolved zone of a timed start, used
/// to turn a UTC `UNTIL` into Graph's local `endDate`. Rules Graph cannot
/// express (sub-daily frequencies, `BYWEEKNO`, several ordinals, fifth
/// weekdays, …) yield [`RecurrenceError::Unsupported`].
pub fn from_rrule(
    rrule: &str,
    start: &EventTime,
    tz: Option<Tz>,
) -> Result<PatternedRecurrence, RecurrenceError> {
    let zone = tz.map(SeriesZone::Named);
    let rule = ParsedRule::parse(rrule)?;
    let local = local_start(start, zone);
    rule.check_time_parts(local)?;
    let start_date = local.date();
    let interval = rule.interval.unwrap_or(1);
    let freq = rule.freq.ok_or_else(|| invalid("missing FREQ"))?;
    let base = |kind| RecurrencePattern {
        kind,
        interval,
        month: 0,
        day_of_month: 0,
        days_of_week: Vec::new(),
        first_day_of_week: None,
        index: None,
    };
    let weekly = |days: Vec<DayOfWeek>, interval: u32| RecurrencePattern {
        kind: RecurrencePatternType::Weekly,
        interval,
        month: 0,
        day_of_month: 0,
        days_of_week: days,
        first_day_of_week: Some(rule.wkst.unwrap_or(DayOfWeek::Monday)),
        index: None,
    };

    let pattern = match freq {
        Freq::Daily => {
            if !rule.by_month_day.is_empty()
                || !rule.by_month.is_empty()
                || !rule.by_set_pos.is_empty()
            {
                return Err(unsupported("DAILY with BYMONTHDAY/BYMONTH/BYSETPOS"));
            }
            if rule.by_day.is_empty() {
                base(RecurrencePatternType::Daily)
            } else if interval == 1 {
                weekly(rule.plain_days()?, 1)
            } else {
                return Err(unsupported("DAILY with BYDAY and INTERVAL > 1"));
            }
        }
        Freq::Weekly => {
            if !rule.by_month_day.is_empty()
                || !rule.by_month.is_empty()
                || !rule.by_set_pos.is_empty()
            {
                return Err(unsupported("WEEKLY with BYMONTHDAY/BYMONTH/BYSETPOS"));
            }
            let days = if rule.by_day.is_empty() {
                vec![DayOfWeek::from_weekday(start_date.weekday())]
            } else {
                rule.plain_days()?
            };
            weekly(days, interval)
        }
        Freq::Monthly => {
            if !rule.by_month.is_empty() {
                return Err(unsupported("MONTHLY with BYMONTH"));
            }
            match monthly_or_yearly(&rule, start_date, None, interval)? {
                Some(pattern) => pattern,
                None => weekly(rule.plain_days()?, 1),
            }
        }
        Freq::Yearly => match rule.by_month.as_slice() {
            [month] => monthly_or_yearly(&rule, start_date, Some(*month), interval)?
                .ok_or_else(|| unsupported("YEARLY with BYDAY but no ordinal"))?,
            [] if rule.by_day.is_empty() && rule.by_month_day.is_empty() => {
                // Plain FREQ=YEARLY: the month and day of DTSTART.
                monthly_or_yearly(&rule, start_date, Some(start_date.month()), interval)?
                    .ok_or_else(|| unsupported("BYSETPOS without BYDAY"))?
            }
            [] if rule.by_day.is_empty() => {
                // BYMONTHDAY without BYMONTH repeats in every month.
                if interval != 1 {
                    return Err(unsupported(
                        "YEARLY;BYMONTHDAY without BYMONTH and INTERVAL > 1",
                    ));
                }
                monthly_or_yearly(&rule, start_date, None, 1)?
                    .ok_or_else(|| unsupported("YEARLY;BYMONTHDAY with BYSETPOS"))?
            }
            [] => {
                // BYDAY without BYMONTH counts weekdays across the whole year:
                // the first four fall into January, the last into December.
                let set_position = match rule.by_set_pos.as_slice() {
                    [pos] => Some(*pos),
                    _ => None,
                };
                let position = rule
                    .by_day
                    .first()
                    .and_then(|(ordinal, _)| *ordinal)
                    .or(set_position);
                match position {
                    Some(1..=4) => monthly_or_yearly(&rule, start_date, Some(1), interval)?
                        .ok_or_else(|| unsupported("YEARLY with BYDAY but no ordinal"))?,
                    Some(-1) => monthly_or_yearly(&rule, start_date, Some(12), interval)?
                        .ok_or_else(|| unsupported("YEARLY with BYDAY but no ordinal"))?,
                    Some(other) => {
                        return Err(unsupported(format!(
                            "weekday position {other} within a year"
                        )));
                    }
                    None if interval == 1 && rule.by_set_pos.is_empty() => {
                        weekly(rule.plain_days()?, 1)
                    }
                    None => return Err(unsupported("YEARLY with BYDAY but no ordinal")),
                }
            }
            _ => return Err(unsupported("YEARLY with several BYMONTH values")),
        },
    };

    let range = if let Some(count) = rule.count {
        RecurrenceRange {
            kind: RecurrenceRangeType::Numbered,
            start_date: Some(start_date),
            end_date: None,
            recurrence_time_zone: None,
            number_of_occurrences: count,
        }
    } else if let Some(until) = rule.until {
        let end_date = match until {
            Until::Date(date) => date,
            Until::Floating(local) => local.date(),
            Until::Utc(at) => zone.map_or_else(|| at.date_naive(), |zone| zone.to_local(at).date()),
        };
        if end_date < start_date {
            return Err(invalid("UNTIL lies before the start"));
        }
        RecurrenceRange {
            kind: RecurrenceRangeType::EndDate,
            start_date: Some(start_date),
            end_date: Some(end_date),
            recurrence_time_zone: None,
            number_of_occurrences: 0,
        }
    } else {
        RecurrenceRange {
            kind: RecurrenceRangeType::NoEnd,
            start_date: Some(start_date),
            end_date: None,
            recurrence_time_zone: None,
            number_of_occurrences: 0,
        }
    };
    Ok(PatternedRecurrence { pattern, range })
}

/// Monthly (`month == None`) or yearly patterns. Returns `Ok(None)` for a
/// monthly rule on plain weekdays every month, which equals a weekly rule.
fn monthly_or_yearly(
    rule: &ParsedRule,
    start: NaiveDate,
    month: Option<u32>,
    interval: u32,
) -> Result<Option<RecurrencePattern>, RecurrenceError> {
    let (absolute, relative) = match month {
        Some(_) => (
            RecurrencePatternType::AbsoluteYearly,
            RecurrencePatternType::RelativeYearly,
        ),
        None => (
            RecurrencePatternType::AbsoluteMonthly,
            RecurrencePatternType::RelativeMonthly,
        ),
    };
    if !rule.by_day.is_empty() {
        if !rule.by_month_day.is_empty() {
            return Err(unsupported("BYDAY combined with BYMONTHDAY"));
        }
        let ordinals: Vec<Option<i32>> = rule.by_day.iter().map(|(o, _)| *o).collect();
        let days: Vec<DayOfWeek> = rule.by_day.iter().map(|(_, d)| *d).collect();
        let index = if ordinals.iter().all(Option::is_none) {
            match rule.by_set_pos.as_slice() {
                [pos] => WeekIndex::from_ordinal(*pos)
                    .ok_or_else(|| unsupported(format!("BYSETPOS={pos}")))?,
                [] if month.is_none() && interval == 1 => return Ok(None),
                [] => return Err(unsupported("BYDAY without ordinal or BYSETPOS")),
                _ => return Err(unsupported("several BYSETPOS values")),
            }
        } else {
            let first = ordinals.first().copied().flatten();
            if !rule.by_set_pos.is_empty() || ordinals.iter().any(|o| *o != first) {
                return Err(unsupported("BYDAY with differing ordinals"));
            }
            // `1MO,1TU` is two occurrences (first Monday and first Tuesday);
            // a Graph pattern with several days picks one of them.
            if days.len() > 1 {
                return Err(unsupported("several BYDAY entries with an ordinal"));
            }
            let ordinal = first.unwrap_or(1);
            WeekIndex::from_ordinal(ordinal)
                .ok_or_else(|| unsupported(format!("BYDAY ordinal {ordinal}")))?
        };
        return Ok(Some(RecurrencePattern {
            kind: relative,
            interval,
            month: month.unwrap_or(0),
            day_of_month: 0,
            days_of_week: days,
            first_day_of_week: None,
            index: Some(index),
        }));
    }
    if !rule.by_set_pos.is_empty() {
        return Err(unsupported("BYSETPOS without BYDAY"));
    }
    let day_of_month = match rule.by_month_day.as_slice() {
        [] => start.day(),
        [-1] => 31,
        [day] if (1..=31).contains(day) => day.unsigned_abs(),
        _ => return Err(unsupported("BYMONTHDAY with several or negative values")),
    };
    Ok(Some(RecurrencePattern {
        kind: absolute,
        interval,
        month: month.unwrap_or(0),
        day_of_month,
        days_of_week: Vec::new(),
        first_day_of_week: None,
        index: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap_or_default()
    }

    fn zoned(y: i32, m: u32, d: u32, h: u32, tzid: &str) -> EventTime {
        EventTime::Zoned {
            local: date(y, m, d).and_hms_opt(h, 0, 0).unwrap_or_default(),
            tzid: tzid.into(),
        }
    }

    fn graph(json: &str) -> PatternedRecurrence {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("fixture: {e}"))
    }

    fn rrule(json: &str, start: &EventTime, tz: Option<Tz>) -> String {
        to_rrule(&graph(json), start, tz).unwrap_or_else(|e| panic!("to_rrule: {e}"))
    }

    fn back(rule: &str, start: &EventTime, tz: Option<Tz>) -> PatternedRecurrence {
        from_rrule(rule, start, tz).unwrap_or_else(|e| panic!("from_rrule({rule}): {e}"))
    }

    /// Converts Graph -> RRULE -> Graph and checks the relevant fields survive.
    fn assert_round_trip(json: &str, start: &EventTime, tz: Option<Tz>) {
        let original = graph(json);
        let rule = rrule(json, start, tz);
        let again = back(&rule, start, tz);
        let p = &original.pattern;
        let q = &again.pattern;
        assert_eq!(q.kind, p.kind, "{rule}");
        assert_eq!(q.interval, p.interval.max(1), "{rule}");
        assert_eq!(q.days_of_week, p.days_of_week, "{rule}");
        match p.kind {
            RecurrencePatternType::AbsoluteMonthly => assert_eq!(q.day_of_month, p.day_of_month),
            RecurrencePatternType::AbsoluteYearly => {
                assert_eq!((q.month, q.day_of_month), (p.month, p.day_of_month));
            }
            RecurrencePatternType::RelativeMonthly => assert_eq!(q.index, p.index),
            RecurrencePatternType::RelativeYearly => {
                assert_eq!((q.month, q.index), (p.month, p.index));
            }
            RecurrencePatternType::Weekly => assert_eq!(
                q.first_day_of_week,
                Some(p.first_day_of_week.unwrap_or(DayOfWeek::Monday))
            ),
            RecurrencePatternType::Daily => {}
        }
        assert_eq!(again.range.kind, original.range.kind, "{rule}");
        assert_eq!(again.range.start_date, original.range.start_date, "{rule}");
        if original.range.kind == RecurrenceRangeType::EndDate {
            assert_eq!(again.range.end_date, original.range.end_date, "{rule}");
        }
        if original.range.kind == RecurrenceRangeType::Numbered {
            assert_eq!(
                again.range.number_of_occurrences,
                original.range.number_of_occurrences
            );
        }
    }

    #[test]
    fn daily_every_three_days_numbered() {
        let json = r#"{"pattern":{"type":"daily","interval":3},
            "range":{"type":"numbered","startDate":"2017-04-02","numberOfOccurrences":10}}"#;
        let start = zoned(2017, 4, 2, 9, "Pacific Standard Time");
        assert_eq!(rrule(json, &start, None), "FREQ=DAILY;INTERVAL=3;COUNT=10");
        assert_round_trip(json, &start, None);
    }

    #[test]
    fn weekly_with_graph_response_placeholders() {
        // Shape of a real Graph response: unused fields filled in.
        let json = r#"{"pattern":{"type":"weekly","interval":1,"month":0,"dayOfMonth":0,
              "daysOfWeek":["monday"],"firstDayOfWeek":"sunday","index":"first"},
            "range":{"type":"endDate","startDate":"2017-09-04","endDate":"2017-12-31",
              "recurrenceTimeZone":"Pacific Standard Time","numberOfOccurrences":0}}"#;
        let start = zoned(2017, 9, 4, 12, "Pacific Standard Time");
        // Windows zone unresolved: end of the local day written as UTC.
        assert_eq!(
            rrule(json, &start, None),
            "FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20171231T235959Z"
        );
        // Resolved zone: the exact instant.
        assert_eq!(
            rrule(json, &start, Some(chrono_tz::America::Los_Angeles)),
            "FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20180101T075959Z"
        );
        assert_round_trip(json, &start, None);
        assert_round_trip(json, &start, Some(chrono_tz::America::Los_Angeles));
    }

    #[test]
    fn weekly_every_other_monday_tuesday() {
        let json = r#"{"pattern":{"type":"weekly","interval":2,"daysOfWeek":["Monday","Tuesday"],
              "firstDayOfWeek":"Sunday"},
            "range":{"type":"noEnd","startDate":"2017-04-03"}}"#;
        let start = zoned(2017, 4, 3, 10, "W. Europe Standard Time");
        assert_eq!(
            rrule(json, &start, None),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TU;WKST=SU"
        );
        assert_round_trip(json, &start, None);
    }

    #[test]
    fn absolute_monthly_quarterly() {
        let json = r#"{"pattern":{"type":"absoluteMonthly","interval":3,"dayOfMonth":7},
            "range":{"type":"noEnd","startDate":"2017-04-07"}}"#;
        let start = zoned(2017, 4, 7, 8, "UTC");
        assert_eq!(
            rrule(json, &start, None),
            "FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=7"
        );
        assert_round_trip(json, &start, None);
    }

    #[test]
    fn relative_monthly_single_and_multiple_days() {
        let second_wednesday = r#"{"pattern":{"type":"relativeMonthly","interval":1,
              "daysOfWeek":["wednesday"],"index":"second"},
            "range":{"type":"noEnd","startDate":"2017-04-12"}}"#;
        let start = zoned(2017, 4, 12, 9, "Tokyo Standard Time");
        assert_eq!(
            rrule(second_wednesday, &start, None),
            "FREQ=MONTHLY;BYDAY=2WE"
        );
        assert_round_trip(second_wednesday, &start, None);

        let first_thu_or_fri = r#"{"pattern":{"type":"relativeMonthly","interval":1,
              "daysOfWeek":["thursday","friday"],"index":"first"},
            "range":{"type":"noEnd","startDate":"2017-04-06"}}"#;
        let start = zoned(2017, 4, 6, 9, "UTC");
        assert_eq!(
            rrule(first_thu_or_fri, &start, None),
            "FREQ=MONTHLY;BYDAY=TH,FR;BYSETPOS=1"
        );
        assert_round_trip(first_thu_or_fri, &start, None);
    }

    #[test]
    fn absolute_and_relative_yearly() {
        let april_15 = r#"{"pattern":{"type":"absoluteYearly","interval":1,"dayOfMonth":15,"month":4},
            "range":{"type":"noEnd","startDate":"2017-04-15"}}"#;
        let start = EventTime::Date {
            date: date(2017, 4, 15),
        };
        assert_eq!(
            rrule(april_15, &start, None),
            "FREQ=YEARLY;BYMONTH=4;BYMONTHDAY=15"
        );
        assert_round_trip(april_15, &start, None);

        let last_wed_nov = r#"{"pattern":{"type":"relativeYearly","interval":1,
              "daysOfWeek":["wednesday"],"index":"last","month":11},
            "range":{"type":"endDate","startDate":"2017-11-29","endDate":"2025-11-30"}}"#;
        let start = EventTime::Date {
            date: date(2017, 11, 29),
        };
        assert_eq!(
            rrule(last_wed_nov, &start, None),
            "FREQ=YEARLY;BYMONTH=11;BYDAY=-1WE;UNTIL=20251130"
        );
        assert_round_trip(last_wed_nov, &start, None);
    }

    #[test]
    fn relative_monthly_every_other_month_with_end() {
        let json = r#"{"pattern":{"type":"relativeMonthly","interval":2,
              "daysOfWeek":["thursday"],"index":"first"},
            "range":{"type":"endDate","startDate":"2017-04-06","endDate":"2017-12-31"}}"#;
        let start = EventTime::Utc {
            at: date(2017, 4, 6)
                .and_hms_opt(16, 0, 0)
                .unwrap_or_default()
                .and_utc(),
        };
        assert_eq!(
            rrule(json, &start, Some(Tz::UTC)),
            "FREQ=MONTHLY;INTERVAL=2;BYDAY=1TH;UNTIL=20171231T235959Z"
        );
        assert_round_trip(json, &start, Some(Tz::UTC));
    }

    #[test]
    fn floating_start_keeps_floating_until() {
        let json = r#"{"pattern":{"type":"daily","interval":1},
            "range":{"type":"endDate","startDate":"2024-01-01","endDate":"2024-01-31"}}"#;
        let start = EventTime::Floating {
            local: date(2024, 1, 1).and_hms_opt(7, 30, 0).unwrap_or_default(),
        };
        assert_eq!(
            rrule(json, &start, None),
            "FREQ=DAILY;UNTIL=20240131T235959"
        );
        assert_round_trip(json, &start, None);
    }

    #[test]
    fn rrule_variants_map_to_graph() {
        let start = zoned(2024, 3, 4, 9, "Europe/Istanbul");
        let tz = Some(chrono_tz::Europe::Istanbul);

        let every_weekday = back("RRULE:FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR", &start, tz);
        assert_eq!(every_weekday.pattern.kind, RecurrencePatternType::Weekly);
        assert_eq!(every_weekday.pattern.days_of_week.len(), 5);
        assert_eq!(
            every_weekday.pattern.first_day_of_week,
            Some(DayOfWeek::Monday)
        );

        let plain_weekly = back("FREQ=WEEKLY", &start, tz);
        assert_eq!(plain_weekly.pattern.days_of_week, vec![DayOfWeek::Monday]);

        let last_day = back("FREQ=MONTHLY;BYMONTHDAY=-1", &start, tz);
        assert_eq!(last_day.pattern.day_of_month, 31);

        let last_friday = back("FREQ=MONTHLY;BYDAY=-1FR;COUNT=6", &start, tz);
        assert_eq!(
            last_friday.pattern.kind,
            RecurrencePatternType::RelativeMonthly
        );
        assert_eq!(last_friday.pattern.index, Some(WeekIndex::Last));
        assert_eq!(last_friday.range.number_of_occurrences, 6);

        let monthly_default = back("FREQ=MONTHLY", &start, tz);
        assert_eq!(monthly_default.pattern.day_of_month, 4);

        let yearly_default = back("FREQ=YEARLY;INTERVAL=2", &start, tz);
        assert_eq!(
            (
                yearly_default.pattern.month,
                yearly_default.pattern.day_of_month
            ),
            (3, 4)
        );

        // UNTIL as the exact last occurrence (09:00 Istanbul = 06:00Z).
        let until = back("FREQ=DAILY;UNTIL=20240310T060000Z", &start, tz);
        assert_eq!(until.range.end_date, Some(date(2024, 3, 10)));
        // Restating the start time is fine.
        assert!(from_rrule("FREQ=DAILY;BYHOUR=9;BYMINUTE=0", &start, tz).is_ok());

        // YEARLY without BYMONTH: BYMONTHDAY repeats monthly, weekday
        // positions count within the year.
        let every_15th = back("FREQ=YEARLY;BYMONTHDAY=15", &start, tz);
        assert_eq!(
            every_15th.pattern.kind,
            RecurrencePatternType::AbsoluteMonthly
        );
        assert_eq!(every_15th.pattern.day_of_month, 15);
        let first_monday = back("FREQ=YEARLY;BYDAY=1MO", &start, tz);
        assert_eq!(
            first_monday.pattern.kind,
            RecurrencePatternType::RelativeYearly
        );
        assert_eq!(
            (first_monday.pattern.month, first_monday.pattern.index),
            (1, Some(WeekIndex::First))
        );
        let last_weekend_day = back("FREQ=YEARLY;BYDAY=SA,SU;BYSETPOS=-1", &start, tz);
        assert_eq!(last_weekend_day.pattern.month, 12);
        assert_eq!(last_weekend_day.pattern.index, Some(WeekIndex::Last));
    }

    #[test]
    fn fixed_offsets_give_exact_until() {
        // 17:00 Pacific (UTC-8 in December): the last occurrence is at
        // 01:00Z on the next day and must stay inside UNTIL.
        let json = r#"{"pattern":{"type":"weekly","interval":1,"daysOfWeek":["monday"],"firstDayOfWeek":"sunday"},
            "range":{"type":"endDate","startDate":"2017-12-04","endDate":"2017-12-25"}}"#;
        let start = zoned(2017, 12, 4, 17, "Pacific Standard Time");
        let offset = FixedOffset::west_opt(8 * 3600).map(SeriesZone::Fixed);
        let rule = to_rrule_in(&graph(json), &start, offset).unwrap_or_default();
        assert_eq!(rule, "FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20171226T075959Z");
    }

    #[test]
    fn unsupported_and_invalid_rules() {
        let start = zoned(2024, 3, 4, 9, "UTC");
        for rule in [
            "FREQ=HOURLY",
            "FREQ=MONTHLY;BYDAY=5MO",
            "FREQ=MONTHLY;BYDAY=1MO,3WE",
            "FREQ=MONTHLY;BYDAY=1MO,1TU",
            "FREQ=YEARLY;BYDAY=-1SA,-1SU",
            "FREQ=YEARLY;INTERVAL=2;BYMONTHDAY=15",
            "FREQ=YEARLY;BYDAY=10MO",
            "FREQ=YEARLY;BYWEEKNO=20",
            "FREQ=YEARLY;BYMONTH=1,7",
            "FREQ=DAILY;INTERVAL=2;BYDAY=MO",
            "FREQ=DAILY;BYHOUR=9,17",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
        ] {
            assert!(
                matches!(
                    from_rrule(rule, &start, None),
                    Err(RecurrenceError::Unsupported(_))
                ),
                "{rule}"
            );
        }
        for rule in [
            "INTERVAL=2",
            "FREQ=DAILY;COUNT=2;UNTIL=20240401",
            "FREQ=FORTNIGHTLY",
            "FREQ=WEEKLY;BYDAY=XX",
            "FREQ=DAILY;UNTIL=20200101",
            "garbage",
        ] {
            assert!(
                matches!(
                    from_rrule(rule, &start, None),
                    Err(RecurrenceError::Invalid(_))
                ),
                "{rule}"
            );
        }
    }

    #[test]
    fn rules_of_the_event_editor() {
        // Tuesday 10 November 2026 at 10:00 in New York, the month's second Tuesday.
        let new_york = Some(chrono_tz::America::New_York);
        let start = zoned(2026, 11, 10, 10, "America/New_York");
        let to_graph =
            |rule: &str| serde_json::to_value(back(rule, &start, new_york)).unwrap_or_default();
        assert_eq!(
            to_graph("FREQ=MONTHLY;BYDAY=2TU;COUNT=5"),
            serde_json::json!({
                "pattern": {"type": "relativeMonthly", "interval": 1, "daysOfWeek": ["tuesday"],
                            "index": "second"},
                "range": {"type": "numbered", "startDate": "2026-11-10", "numberOfOccurrences": 5}
            })
        );
        assert_eq!(
            to_graph("FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR"),
            serde_json::json!({
                "pattern": {"type": "relativeMonthly", "interval": 2, "daysOfWeek": ["friday"],
                            "index": "last"},
                "range": {"type": "noEnd", "startDate": "2026-11-10"}
            })
        );
        assert_eq!(
            to_graph("FREQ=MONTHLY;BYMONTHDAY=31"),
            serde_json::json!({
                "pattern": {"type": "absoluteMonthly", "interval": 1, "dayOfMonth": 31},
                "range": {"type": "noEnd", "startDate": "2026-11-10"}
            })
        );
        // The end of 31 December in New York ends the series on that date.
        assert_eq!(
            to_graph("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20270101T045959Z"),
            serde_json::json!({
                "pattern": {"type": "weekly", "interval": 2, "daysOfWeek": ["monday", "wednesday"],
                            "firstDayOfWeek": "monday"},
                "range": {"type": "endDate", "startDate": "2026-11-10", "endDate": "2026-12-31"}
            })
        );
        assert_eq!(
            to_graph("FREQ=YEARLY;INTERVAL=2"),
            serde_json::json!({
                "pattern": {"type": "absoluteYearly", "interval": 2, "month": 11, "dayOfMonth": 10},
                "range": {"type": "noEnd", "startDate": "2026-11-10"}
            })
        );
        assert_eq!(
            to_graph("FREQ=DAILY;INTERVAL=3;COUNT=12"),
            serde_json::json!({
                "pattern": {"type": "daily", "interval": 3},
                "range": {"type": "numbered", "startDate": "2026-11-10", "numberOfOccurrences": 12}
            })
        );
        // And back: Graph's patterns read as the same rules.
        for rule in [
            "FREQ=MONTHLY;BYDAY=2TU;COUNT=5",
            "FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR",
            "FREQ=MONTHLY;BYMONTHDAY=31",
            "FREQ=DAILY;INTERVAL=3;COUNT=12",
        ] {
            let graph = back(rule, &start, new_york);
            assert_eq!(to_rrule(&graph, &start, new_york).as_deref(), Ok(rule));
        }
        let weekly = back(
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20270101T045959Z",
            &start,
            new_york,
        );
        assert_eq!(
            to_rrule(&weekly, &start, new_york).as_deref(),
            Ok("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20270101T045959Z")
        );
        // A fifth weekday has no Graph index; the editor never offers one.
        assert!(matches!(
            from_rrule("FREQ=MONTHLY;BYDAY=5TU", &start, new_york),
            Err(RecurrenceError::Unsupported(_))
        ));
    }

    #[test]
    fn serializes_only_relevant_fields() {
        let start = zoned(2024, 3, 4, 9, "UTC");
        let recurrence = back(
            "FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20240630T235959Z",
            &start,
            None,
        );
        let json = serde_json::to_value(&recurrence).unwrap_or_default();
        assert_eq!(
            json,
            serde_json::json!({
                "pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday", "wednesday"],
                            "firstDayOfWeek": "monday"},
                "range": {"type": "endDate", "startDate": "2024-03-04", "endDate": "2024-06-30"}
            })
        );
    }
}
