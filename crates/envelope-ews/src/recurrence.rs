//! Recurrence patterns of calendar items (`t:Recurrence`) and their
//! conversion to and from iCalendar `RRULE` values.
//!
//! | EWS pattern                  | RRULE                                         |
//! |------------------------------|-----------------------------------------------|
//! | `DailyRecurrence`            | `FREQ=DAILY;INTERVAL=n`                       |
//! | `WeeklyRecurrence`           | `FREQ=WEEKLY;INTERVAL=n;BYDAY=…;WKST=…`       |
//! | `AbsoluteMonthlyRecurrence`  | `FREQ=MONTHLY;INTERVAL=n;BYMONTHDAY=d`        |
//! | `RelativeMonthlyRecurrence`  | `FREQ=MONTHLY;BYDAY=2TU` or `BYDAY=…;BYSETPOS=n` |
//! | `AbsoluteYearlyRecurrence`   | `FREQ=YEARLY;BYMONTH=m;BYMONTHDAY=d`          |
//! | `RelativeYearlyRecurrence`   | `FREQ=YEARLY;BYMONTH=m;BYDAY=-1SU`            |
//!
//! Ranges map to `UNTIL` (`EndDateRecurrence`), `COUNT`
//! (`NumberedRecurrence`) or nothing (`NoEndRecurrence`). The MIME
//! content of a series master carries Exchange's own `RRULE`; these
//! conversions serve creating and updating series through properties.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::error::EwsError;
use crate::types::{parse_date, wire_enum};
use crate::xml::{Element, XmlWriter};

wire_enum! {
    /// A day, or a group of days, in a recurrence pattern.
    pub enum DayOfWeek {
        /// Sunday.
        Sunday = "Sunday",
        /// Monday.
        Monday = "Monday",
        /// Tuesday.
        Tuesday = "Tuesday",
        /// Wednesday.
        Wednesday = "Wednesday",
        /// Thursday.
        Thursday = "Thursday",
        /// Friday.
        Friday = "Friday",
        /// Saturday.
        Saturday = "Saturday",
        /// Any day.
        Day = "Day",
        /// Monday to Friday.
        Weekday = "Weekday",
        /// Saturday and Sunday.
        WeekendDay = "WeekendDay",
    }
}

impl DayOfWeek {
    /// The days a value stands for.
    pub fn weekdays(self) -> Vec<Weekday> {
        match self {
            DayOfWeek::Sunday => vec![Weekday::Sun],
            DayOfWeek::Monday => vec![Weekday::Mon],
            DayOfWeek::Tuesday => vec![Weekday::Tue],
            DayOfWeek::Wednesday => vec![Weekday::Wed],
            DayOfWeek::Thursday => vec![Weekday::Thu],
            DayOfWeek::Friday => vec![Weekday::Fri],
            DayOfWeek::Saturday => vec![Weekday::Sat],
            DayOfWeek::Day => vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ],
            DayOfWeek::Weekday => vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            DayOfWeek::WeekendDay => vec![Weekday::Sat, Weekday::Sun],
        }
    }

    /// A single day.
    pub fn from_weekday(day: Weekday) -> Self {
        match day {
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

wire_enum! {
    /// Which of the matching days a relative pattern selects.
    pub enum DayOfWeekIndex {
        /// The first.
        First = "First",
        /// The second.
        Second = "Second",
        /// The third.
        Third = "Third",
        /// The fourth.
        Fourth = "Fourth",
        /// The last.
        Last = "Last",
    }
}

impl DayOfWeekIndex {
    /// The `BYSETPOS` value (`1`–`4`, `-1`).
    pub fn ordinal(self) -> i32 {
        match self {
            DayOfWeekIndex::First => 1,
            DayOfWeekIndex::Second => 2,
            DayOfWeekIndex::Third => 3,
            DayOfWeekIndex::Fourth => 4,
            DayOfWeekIndex::Last => -1,
        }
    }

    /// Parses a `BYSETPOS` value.
    pub fn from_ordinal(ordinal: i32) -> Option<Self> {
        match ordinal {
            1 => Some(DayOfWeekIndex::First),
            2 => Some(DayOfWeekIndex::Second),
            3 => Some(DayOfWeekIndex::Third),
            4 => Some(DayOfWeekIndex::Fourth),
            -1 => Some(DayOfWeekIndex::Last),
            _ => None,
        }
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// How a series repeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum RecurrencePattern {
    /// Every `interval` days.
    Daily {
        /// Days between occurrences.
        interval: u32,
    },
    /// On `days`, every `interval` weeks.
    Weekly {
        /// Weeks between occurrences.
        interval: u32,
        /// The days of the week.
        days: Vec<DayOfWeek>,
        /// First day of the week (Exchange 2010 SP1 and later).
        first_day_of_week: Option<DayOfWeek>,
    },
    /// On day `day_of_month`, every `interval` months.
    AbsoluteMonthly {
        /// Months between occurrences.
        interval: u32,
        /// Day of the month (1–31).
        day_of_month: u32,
    },
    /// On the `index`-th of `days`, every `interval` months.
    RelativeMonthly {
        /// Months between occurrences.
        interval: u32,
        /// The days considered.
        days: Vec<DayOfWeek>,
        /// Which of them.
        index: DayOfWeekIndex,
    },
    /// Yearly on a date.
    AbsoluteYearly {
        /// Day of the month (1–31).
        day_of_month: u32,
        /// Month (1–12).
        month: u32,
    },
    /// Yearly on the `index`-th of `days` in `month`.
    RelativeYearly {
        /// The days considered.
        days: Vec<DayOfWeek>,
        /// Which of them.
        index: DayOfWeekIndex,
        /// Month (1–12).
        month: u32,
    },
}

/// How long a series runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum RecurrenceRange {
    /// Forever.
    NoEnd {
        /// Date of the first occurrence.
        start: NaiveDate,
    },
    /// Until a date (inclusive, local to the series' time zone).
    EndDate {
        /// Date of the first occurrence.
        start: NaiveDate,
        /// Last possible date.
        end: NaiveDate,
    },
    /// A number of occurrences.
    Numbered {
        /// Date of the first occurrence.
        start: NaiveDate,
        /// Number of occurrences.
        count: u32,
    },
}

impl RecurrenceRange {
    /// Date of the first occurrence.
    pub fn start(&self) -> NaiveDate {
        match self {
            RecurrenceRange::NoEnd { start }
            | RecurrenceRange::EndDate { start, .. }
            | RecurrenceRange::Numbered { start, .. } => *start,
        }
    }
}

/// A recurrence: pattern plus range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recurrence {
    /// How the series repeats.
    pub pattern: RecurrencePattern,
    /// How long it runs.
    pub range: RecurrenceRange,
}

impl Recurrence {
    /// Parses a `Recurrence` element. Task regeneration patterns are not
    /// supported and yield `None`.
    pub(crate) fn from_element(element: &Element) -> Option<Recurrence> {
        let mut pattern = None;
        let mut range = None;
        for child in element.elements() {
            let interval = || child.child_u32("Interval").unwrap_or(1).max(1);
            let days = || parse_days(child.child_text("DaysOfWeek").as_deref());
            let index = || {
                child
                    .child_text("DayOfWeekIndex")
                    .and_then(|i| DayOfWeekIndex::parse(&i))
            };
            let month = || child.child_text("Month").and_then(|m| month_number(&m));
            match child.name.as_str() {
                "DailyRecurrence" => {
                    pattern = Some(RecurrencePattern::Daily {
                        interval: interval(),
                    });
                }
                "WeeklyRecurrence" => {
                    pattern = Some(RecurrencePattern::Weekly {
                        interval: interval(),
                        days: days(),
                        first_day_of_week: child
                            .child_text("FirstDayOfWeek")
                            .and_then(|d| DayOfWeek::parse(&d)),
                    });
                }
                "AbsoluteMonthlyRecurrence" => {
                    pattern = Some(RecurrencePattern::AbsoluteMonthly {
                        interval: interval(),
                        day_of_month: child.child_u32("DayOfMonth")?,
                    });
                }
                "RelativeMonthlyRecurrence" => {
                    pattern = Some(RecurrencePattern::RelativeMonthly {
                        interval: interval(),
                        days: days(),
                        index: index()?,
                    });
                }
                "AbsoluteYearlyRecurrence" => {
                    pattern = Some(RecurrencePattern::AbsoluteYearly {
                        day_of_month: child.child_u32("DayOfMonth")?,
                        month: month()?,
                    });
                }
                "RelativeYearlyRecurrence" => {
                    pattern = Some(RecurrencePattern::RelativeYearly {
                        days: days(),
                        index: index()?,
                        month: month()?,
                    });
                }
                "NoEndRecurrence" => {
                    range = Some(RecurrenceRange::NoEnd {
                        start: child.child_text("StartDate").and_then(|d| parse_date(&d))?,
                    });
                }
                "EndDateRecurrence" => {
                    range = Some(RecurrenceRange::EndDate {
                        start: child.child_text("StartDate").and_then(|d| parse_date(&d))?,
                        end: child.child_text("EndDate").and_then(|d| parse_date(&d))?,
                    });
                }
                "NumberedRecurrence" => {
                    range = Some(RecurrenceRange::Numbered {
                        start: child.child_text("StartDate").and_then(|d| parse_date(&d))?,
                        count: child.child_u32("NumberOfOccurrences")?,
                    });
                }
                _ => {}
            }
        }
        Some(Recurrence {
            pattern: pattern?,
            range: range?,
        })
    }

    /// Writes the `t:Recurrence` element. `FirstDayOfWeek` is only written
    /// when `with_first_day` is set (Exchange 2013 and later).
    pub(crate) fn write(&self, w: &mut XmlWriter, with_first_day: bool) {
        w.start("t:Recurrence", &[]);
        match &self.pattern {
            RecurrencePattern::Daily { interval } => {
                w.start("t:DailyRecurrence", &[])
                    .text_element("t:Interval", &interval.max(&1).to_string())
                    .end("t:DailyRecurrence");
            }
            RecurrencePattern::Weekly {
                interval,
                days,
                first_day_of_week,
            } => {
                w.start("t:WeeklyRecurrence", &[])
                    .text_element("t:Interval", &interval.max(&1).to_string())
                    .text_element("t:DaysOfWeek", &format_days(days));
                if with_first_day && let Some(first) = first_day_of_week {
                    w.text_element("t:FirstDayOfWeek", first.as_str());
                }
                w.end("t:WeeklyRecurrence");
            }
            RecurrencePattern::AbsoluteMonthly {
                interval,
                day_of_month,
            } => {
                w.start("t:AbsoluteMonthlyRecurrence", &[])
                    .text_element("t:Interval", &interval.max(&1).to_string())
                    .text_element("t:DayOfMonth", &day_of_month.to_string())
                    .end("t:AbsoluteMonthlyRecurrence");
            }
            RecurrencePattern::RelativeMonthly {
                interval,
                days,
                index,
            } => {
                w.start("t:RelativeMonthlyRecurrence", &[])
                    .text_element("t:Interval", &interval.max(&1).to_string())
                    .text_element("t:DaysOfWeek", &format_days(days))
                    .text_element("t:DayOfWeekIndex", index.as_str())
                    .end("t:RelativeMonthlyRecurrence");
            }
            RecurrencePattern::AbsoluteYearly {
                day_of_month,
                month,
            } => {
                w.start("t:AbsoluteYearlyRecurrence", &[])
                    .text_element("t:DayOfMonth", &day_of_month.to_string())
                    .text_element("t:Month", month_name(*month))
                    .end("t:AbsoluteYearlyRecurrence");
            }
            RecurrencePattern::RelativeYearly { days, index, month } => {
                w.start("t:RelativeYearlyRecurrence", &[])
                    .text_element("t:DaysOfWeek", &format_days(days))
                    .text_element("t:DayOfWeekIndex", index.as_str())
                    .text_element("t:Month", month_name(*month))
                    .end("t:RelativeYearlyRecurrence");
            }
        }
        let date = |d: &NaiveDate| d.format("%Y-%m-%d").to_string();
        match &self.range {
            RecurrenceRange::NoEnd { start } => {
                w.start("t:NoEndRecurrence", &[])
                    .text_element("t:StartDate", &date(start))
                    .end("t:NoEndRecurrence");
            }
            RecurrenceRange::EndDate { start, end } => {
                w.start("t:EndDateRecurrence", &[])
                    .text_element("t:StartDate", &date(start))
                    .text_element("t:EndDate", &date(end))
                    .end("t:EndDateRecurrence");
            }
            RecurrenceRange::Numbered { start, count } => {
                w.start("t:NumberedRecurrence", &[])
                    .text_element("t:StartDate", &date(start))
                    .text_element("t:NumberOfOccurrences", &count.to_string())
                    .end("t:NumberedRecurrence");
            }
        }
        w.end("t:Recurrence");
    }

    /// The equivalent `RRULE` value (without the `RRULE:` prefix).
    ///
    /// `UNTIL` is a date for all-day series. For timed series it is the end
    /// of the last day in `tz` (or UTC when unknown), expressed in UTC.
    pub fn to_rrule(&self, tz: Option<Tz>, all_day: bool) -> String {
        let mut parts: Vec<String> = Vec::new();
        match &self.pattern {
            RecurrencePattern::Daily { interval } => {
                parts.push("FREQ=DAILY".into());
                push_interval(&mut parts, *interval);
            }
            RecurrencePattern::Weekly {
                interval,
                days,
                first_day_of_week,
            } => {
                parts.push("FREQ=WEEKLY".into());
                push_interval(&mut parts, *interval);
                if !days.is_empty() {
                    parts.push(format!("BYDAY={}", rrule_days(days, None)));
                }
                if let Some(first) = first_day_of_week
                    && let [day] = first.weekdays().as_slice()
                {
                    parts.push(format!("WKST={}", weekday_code(*day)));
                }
            }
            RecurrencePattern::AbsoluteMonthly {
                interval,
                day_of_month,
            } => {
                parts.push("FREQ=MONTHLY".into());
                push_interval(&mut parts, *interval);
                parts.push(format!("BYMONTHDAY={day_of_month}"));
            }
            RecurrencePattern::RelativeMonthly {
                interval,
                days,
                index,
            } => {
                parts.push("FREQ=MONTHLY".into());
                push_interval(&mut parts, *interval);
                push_relative(&mut parts, days, *index);
            }
            RecurrencePattern::AbsoluteYearly {
                day_of_month,
                month,
            } => {
                parts.push("FREQ=YEARLY".into());
                parts.push(format!("BYMONTH={month}"));
                parts.push(format!("BYMONTHDAY={day_of_month}"));
            }
            RecurrencePattern::RelativeYearly { days, index, month } => {
                parts.push("FREQ=YEARLY".into());
                parts.push(format!("BYMONTH={month}"));
                push_relative(&mut parts, days, *index);
            }
        }
        match &self.range {
            RecurrenceRange::NoEnd { .. } => {}
            RecurrenceRange::Numbered { count, .. } => parts.push(format!("COUNT={count}")),
            RecurrenceRange::EndDate { end, .. } => {
                if all_day {
                    parts.push(format!("UNTIL={}", end.format("%Y%m%d")));
                } else {
                    let local =
                        end.and_time(NaiveTime::from_hms_opt(23, 59, 59).unwrap_or_default());
                    let utc = match tz {
                        Some(tz) => tz
                            .from_local_datetime(&local)
                            .earliest()
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|| local.and_utc()),
                        None => local.and_utc(),
                    };
                    parts.push(format!("UNTIL={}", utc.format("%Y%m%dT%H%M%SZ")));
                }
            }
        }
        parts.join(";")
    }

    /// Converts an `RRULE` value (with or without `RRULE:` prefix) for a
    /// series whose first occurrence is on `start` (local date).
    ///
    /// Rules Exchange cannot represent (hourly or secondly frequencies,
    /// several month days, yearly intervals, `BYWEEKNO`, ...) are rejected
    /// with [`EwsError::InvalidInput`]. A UTC `UNTIL` is taken as a UTC date;
    /// see [`Recurrence::from_rrule_in`] for series planned in another zone.
    pub fn from_rrule(rrule: &str, start: NaiveDate) -> Result<Recurrence, EwsError> {
        Recurrence::from_rrule_in(rrule, start, None)
    }

    /// [`Recurrence::from_rrule`] for a series planned in `tz`: a UTC
    /// `UNTIL` (RFC 5545 wants one for zoned series, usually the end of the
    /// last day there) becomes the date it falls on in `tz`, which is what
    /// Exchange's end date means.
    pub fn from_rrule_in(
        rrule: &str,
        start: NaiveDate,
        tz: Option<Tz>,
    ) -> Result<Recurrence, EwsError> {
        let unsupported = |what: &str| {
            EwsError::InvalidInput(format!("RRULE {what} cannot be represented in Exchange"))
        };
        let text = rrule.trim();
        let text = text
            .strip_prefix("RRULE:")
            .or_else(|| text.strip_prefix("rrule:"))
            .unwrap_or(text);
        let mut freq = None;
        let mut interval = 1u32;
        let mut by_day: Vec<(Option<i32>, Weekday)> = Vec::new();
        let mut by_month_day: Vec<i32> = Vec::new();
        let mut by_month: Vec<u32> = Vec::new();
        let mut by_set_pos: Vec<i32> = Vec::new();
        let mut count = None;
        let mut until = None;
        let mut wkst = None;
        for part in text.split(';').filter(|p| !p.trim().is_empty()) {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| EwsError::InvalidInput(format!("malformed RRULE part `{part}`")))?;
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim();
            let invalid = || EwsError::InvalidInput(format!("invalid RRULE {key} value `{value}`"));
            match key.as_str() {
                "FREQ" => freq = Some(value.to_ascii_uppercase()),
                "INTERVAL" => interval = value.parse().map_err(|_| invalid())?,
                "COUNT" => count = Some(value.parse::<u32>().map_err(|_| invalid())?),
                "UNTIL" => until = Some(parse_until(value, tz).ok_or_else(invalid)?),
                "WKST" => wkst = Some(parse_weekday(value).ok_or_else(invalid)?),
                "BYDAY" => {
                    for day in value.split(',') {
                        by_day.push(parse_by_day(day).ok_or_else(invalid)?);
                    }
                }
                "BYMONTHDAY" => {
                    for day in value.split(',') {
                        by_month_day.push(day.trim().parse().map_err(|_| invalid())?);
                    }
                }
                "BYMONTH" => {
                    for month in value.split(',') {
                        by_month.push(month.trim().parse().map_err(|_| invalid())?);
                    }
                }
                "BYSETPOS" => {
                    for pos in value.split(',') {
                        by_set_pos.push(pos.trim().parse().map_err(|_| invalid())?);
                    }
                }
                other => return Err(unsupported(other)),
            }
        }
        let interval = interval.max(1);
        if by_month_day.len() > 1 || by_month.len() > 1 || by_set_pos.len() > 1 {
            return Err(unsupported(
                "with several BYMONTHDAY, BYMONTH or BYSETPOS values",
            ));
        }
        let plain_days: Vec<Weekday> = by_day.iter().map(|(_, d)| *d).collect();
        let ordinal = match (by_day.as_slice(), by_set_pos.first()) {
            ([(Some(n), _)], None) => Some(*n),
            (days, Some(pos)) if days.iter().all(|(n, _)| n.is_none()) => Some(*pos),
            (days, None) if days.iter().all(|(n, _)| n.is_none()) => None,
            _ => return Err(unsupported("with mixed BYDAY ordinals")),
        };
        let month = by_month.first().copied().unwrap_or_else(|| start.month());
        if !(1..=12).contains(&month) {
            return Err(EwsError::InvalidInput(format!("invalid month {month}")));
        }

        let pattern = match freq.as_deref() {
            Some("DAILY") if plain_days.is_empty() => RecurrencePattern::Daily { interval },
            Some("DAILY") if interval == 1 && ordinal.is_none() => RecurrencePattern::Weekly {
                interval: 1,
                days: explicit_days(&plain_days),
                first_day_of_week: None,
            },
            Some("WEEKLY") if ordinal.is_none() => RecurrencePattern::Weekly {
                interval,
                days: if plain_days.is_empty() {
                    vec![DayOfWeek::from_weekday(start.weekday())]
                } else {
                    explicit_days(&plain_days)
                },
                first_day_of_week: wkst.map(DayOfWeek::from_weekday),
            },
            Some("MONTHLY") if !plain_days.is_empty() => {
                let index = ordinal
                    .and_then(DayOfWeekIndex::from_ordinal)
                    .ok_or_else(|| unsupported("monthly rule without a supported position"))?;
                RecurrencePattern::RelativeMonthly {
                    interval,
                    days: relative_days(&plain_days)?,
                    index,
                }
            }
            Some("MONTHLY") => RecurrencePattern::AbsoluteMonthly {
                interval,
                day_of_month: month_day(by_month_day.first().copied(), start)?,
            },
            Some("YEARLY") if interval != 1 => {
                return Err(unsupported("with a yearly interval"));
            }
            Some("YEARLY") if !plain_days.is_empty() => {
                let index = ordinal
                    .and_then(DayOfWeekIndex::from_ordinal)
                    .ok_or_else(|| unsupported("yearly rule without a supported position"))?;
                RecurrencePattern::RelativeYearly {
                    days: relative_days(&plain_days)?,
                    index,
                    month,
                }
            }
            Some("YEARLY") => RecurrencePattern::AbsoluteYearly {
                day_of_month: month_day(by_month_day.first().copied(), start)?,
                month,
            },
            Some(other) => return Err(unsupported(&format!("FREQ={other}"))),
            None => return Err(EwsError::InvalidInput("RRULE without FREQ".into())),
        };
        let range = match (count, until) {
            (Some(_), Some(_)) => {
                return Err(EwsError::InvalidInput(
                    "RRULE with both COUNT and UNTIL".into(),
                ));
            }
            (Some(count), None) => RecurrenceRange::Numbered { start, count },
            (None, Some(end)) => RecurrenceRange::EndDate { start, end },
            (None, None) => RecurrenceRange::NoEnd { start },
        };
        Ok(Recurrence { pattern, range })
    }
}

fn push_interval(parts: &mut Vec<String>, interval: u32) {
    if interval > 1 {
        parts.push(format!("INTERVAL={interval}"));
    }
}

fn push_relative(parts: &mut Vec<String>, days: &[DayOfWeek], index: DayOfWeekIndex) {
    let expanded: Vec<Weekday> = days.iter().flat_map(|d| d.weekdays()).collect();
    if let [day] = expanded.as_slice() {
        parts.push(format!("BYDAY={}{}", index.ordinal(), weekday_code(*day)));
    } else {
        parts.push(format!("BYDAY={}", rrule_days(days, None)));
        parts.push(format!("BYSETPOS={}", index.ordinal()));
    }
}

fn rrule_days(days: &[DayOfWeek], ordinal: Option<i32>) -> String {
    let mut out: Vec<String> = Vec::new();
    for day in days.iter().flat_map(|d| d.weekdays()) {
        let code = match ordinal {
            Some(n) => format!("{n}{}", weekday_code(day)),
            None => weekday_code(day).to_owned(),
        };
        if !out.contains(&code) {
            out.push(code);
        }
    }
    out.join(",")
}

fn weekday_code(day: Weekday) -> &'static str {
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

fn parse_weekday(code: &str) -> Option<Weekday> {
    match code.trim().to_ascii_uppercase().as_str() {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

/// Parses a `BYDAY` entry such as `MO`, `2TU` or `-1FR`.
fn parse_by_day(entry: &str) -> Option<(Option<i32>, Weekday)> {
    let entry = entry.trim();
    if entry.len() < 2 {
        return None;
    }
    let (ordinal, code) = entry.split_at(entry.len() - 2);
    let day = parse_weekday(code)?;
    let ordinal = if ordinal.is_empty() {
        None
    } else {
        Some(ordinal.trim_start_matches('+').parse::<i32>().ok()?)
    };
    Some((ordinal, day))
}

/// The last date of an `UNTIL` value; a UTC one is read in `tz` when known.
fn parse_until(value: &str, tz: Option<Tz>) -> Option<NaiveDate> {
    let value = value.trim();
    if let (Some(tz), Some(utc)) = (tz, value.strip_suffix(['Z', 'z'])) {
        let at = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some(at.and_utc().with_timezone(&tz).date_naive());
    }
    let date = value.get(..8)?;
    NaiveDate::parse_from_str(date, "%Y%m%d").ok()
}

fn month_day(explicit: Option<i32>, start: NaiveDate) -> Result<u32, EwsError> {
    match explicit {
        None => Ok(start.day()),
        Some(day @ 1..=31) => Ok(day as u32),
        Some(day) => Err(EwsError::InvalidInput(format!(
            "RRULE BYMONTHDAY={day} cannot be represented in Exchange"
        ))),
    }
}

/// Single days, Monday first, without duplicates.
fn explicit_days(days: &[Weekday]) -> Vec<DayOfWeek> {
    let mut sorted: Vec<Weekday> = days.to_vec();
    sorted.sort_by_key(Weekday::num_days_from_monday);
    sorted.dedup();
    sorted.into_iter().map(DayOfWeek::from_weekday).collect()
}

/// Collapses a set of days into the EWS groups where possible.
fn group_days(days: &[Weekday]) -> Vec<DayOfWeek> {
    let mut sorted: Vec<Weekday> = days.to_vec();
    sorted.sort_by_key(Weekday::num_days_from_monday);
    sorted.dedup();
    for group in [DayOfWeek::Day, DayOfWeek::Weekday, DayOfWeek::WeekendDay] {
        let mut members = group.weekdays();
        members.sort_by_key(Weekday::num_days_from_monday);
        if members == sorted {
            return vec![group];
        }
    }
    sorted.into_iter().map(DayOfWeek::from_weekday).collect()
}

/// Relative patterns accept one day or one of the day groups.
fn relative_days(days: &[Weekday]) -> Result<Vec<DayOfWeek>, EwsError> {
    let grouped = group_days(days);
    if grouped.len() == 1 {
        Ok(grouped)
    } else {
        Err(EwsError::InvalidInput(
            "RRULE with this set of days and a position cannot be represented in Exchange".into(),
        ))
    }
}

fn parse_days(text: Option<&str>) -> Vec<DayOfWeek> {
    text.map(|t| t.split_whitespace().filter_map(DayOfWeek::parse).collect())
        .unwrap_or_default()
}

fn format_days(days: &[DayOfWeek]) -> String {
    days.iter()
        .map(|d| d.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn month_number(name: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(name.trim()))
        .map(|i| i as u32 + 1)
}

fn month_name(month: u32) -> &'static str {
    MONTHS
        .get(month.clamp(1, 12) as usize - 1)
        .copied()
        .unwrap_or("January")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn written(recurrence: &Recurrence, first_day: bool) -> String {
        let mut w = XmlWriter::new();
        recurrence.write(&mut w, first_day);
        w.into_string()
    }

    #[test]
    fn parses_exchange_patterns() {
        let el = xml::parse(
            r#"<t:Recurrence xmlns:t="t">
                 <t:WeeklyRecurrence><t:Interval>2</t:Interval><t:DaysOfWeek>Monday Wednesday</t:DaysOfWeek><t:FirstDayOfWeek>Monday</t:FirstDayOfWeek></t:WeeklyRecurrence>
                 <t:EndDateRecurrence><t:StartDate>2024-05-06+02:00</t:StartDate><t:EndDate>2024-06-30+02:00</t:EndDate></t:EndDateRecurrence>
               </t:Recurrence>"#,
        )
        .unwrap();
        let recurrence = Recurrence::from_element(&el).unwrap();
        assert_eq!(
            recurrence,
            Recurrence {
                pattern: RecurrencePattern::Weekly {
                    interval: 2,
                    days: vec![DayOfWeek::Monday, DayOfWeek::Wednesday],
                    first_day_of_week: Some(DayOfWeek::Monday),
                },
                range: RecurrenceRange::EndDate {
                    start: date(2024, 5, 6),
                    end: date(2024, 6, 30),
                },
            }
        );
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        assert_eq!(
            recurrence.to_rrule(Some(tz), false),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;WKST=MO;UNTIL=20240630T215959Z"
        );
        assert_eq!(
            recurrence.to_rrule(None, true),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;WKST=MO;UNTIL=20240630"
        );

        let el = xml::parse(
            r#"<Recurrence><RelativeYearlyRecurrence><DaysOfWeek>Sunday</DaysOfWeek><DayOfWeekIndex>Last</DayOfWeekIndex><Month>March</Month></RelativeYearlyRecurrence><NoEndRecurrence><StartDate>2024-03-31Z</StartDate></NoEndRecurrence></Recurrence>"#,
        )
        .unwrap();
        let recurrence = Recurrence::from_element(&el).unwrap();
        assert_eq!(
            recurrence.to_rrule(None, false),
            "FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU"
        );

        let el = xml::parse(
            r#"<Recurrence><RelativeMonthlyRecurrence><Interval>1</Interval><DaysOfWeek>Weekday</DaysOfWeek><DayOfWeekIndex>Last</DayOfWeekIndex></RelativeMonthlyRecurrence><NumberedRecurrence><StartDate>2024-01-31</StartDate><NumberOfOccurrences>10</NumberOfOccurrences></NumberedRecurrence></Recurrence>"#,
        )
        .unwrap();
        let recurrence = Recurrence::from_element(&el).unwrap();
        assert_eq!(
            recurrence.to_rrule(None, false),
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=10"
        );
        // Task regeneration patterns are not calendar recurrences.
        let el = xml::parse("<Recurrence><DailyRegeneration><Interval>1</Interval></DailyRegeneration><NoEndRecurrence><StartDate>2024-01-01</StartDate></NoEndRecurrence></Recurrence>").unwrap();
        assert!(Recurrence::from_element(&el).is_none());
    }

    #[test]
    fn writes_schema_ordered_patterns() {
        let recurrence = Recurrence {
            pattern: RecurrencePattern::Weekly {
                interval: 1,
                days: vec![DayOfWeek::Tuesday, DayOfWeek::Thursday],
                first_day_of_week: Some(DayOfWeek::Monday),
            },
            range: RecurrenceRange::Numbered {
                start: date(2024, 5, 7),
                count: 8,
            },
        };
        assert_eq!(
            written(&recurrence, true),
            "<t:Recurrence><t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Tuesday Thursday</t:DaysOfWeek><t:FirstDayOfWeek>Monday</t:FirstDayOfWeek></t:WeeklyRecurrence><t:NumberedRecurrence><t:StartDate>2024-05-07</t:StartDate><t:NumberOfOccurrences>8</t:NumberOfOccurrences></t:NumberedRecurrence></t:Recurrence>"
        );
        assert!(!written(&recurrence, false).contains("FirstDayOfWeek"));
        let yearly = Recurrence {
            pattern: RecurrencePattern::AbsoluteYearly {
                day_of_month: 24,
                month: 12,
            },
            range: RecurrenceRange::NoEnd {
                start: date(2024, 12, 24),
            },
        };
        assert_eq!(
            written(&yearly, true),
            "<t:Recurrence><t:AbsoluteYearlyRecurrence><t:DayOfMonth>24</t:DayOfMonth><t:Month>December</t:Month></t:AbsoluteYearlyRecurrence><t:NoEndRecurrence><t:StartDate>2024-12-24</t:StartDate></t:NoEndRecurrence></t:Recurrence>"
        );
    }

    #[test]
    fn rrule_round_trips() {
        let start = date(2024, 5, 6);
        for (rule, expected) in [
            (
                "FREQ=DAILY;INTERVAL=3;COUNT=5",
                "FREQ=DAILY;INTERVAL=3;COUNT=5",
            ),
            (
                "RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR",
                "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            ),
            ("FREQ=WEEKLY", "FREQ=WEEKLY;BYDAY=MO"),
            (
                "FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR",
                "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
            ),
            ("FREQ=MONTHLY;BYDAY=2TU", "FREQ=MONTHLY;BYDAY=2TU"),
            (
                "FREQ=MONTHLY;BYDAY=SA,SU;BYSETPOS=1",
                "FREQ=MONTHLY;BYDAY=SA,SU;BYSETPOS=1",
            ),
            (
                "FREQ=MONTHLY;INTERVAL=2;BYMONTHDAY=15",
                "FREQ=MONTHLY;INTERVAL=2;BYMONTHDAY=15",
            ),
            ("FREQ=MONTHLY", "FREQ=MONTHLY;BYMONTHDAY=6"),
            ("FREQ=YEARLY", "FREQ=YEARLY;BYMONTH=5;BYMONTHDAY=6"),
            (
                "FREQ=YEARLY;BYMONTH=11;BYDAY=4TH",
                "FREQ=YEARLY;BYMONTH=11;BYDAY=4TH",
            ),
            (
                "FREQ=WEEKLY;BYDAY=TU;UNTIL=20240630",
                "FREQ=WEEKLY;BYDAY=TU;UNTIL=20240630",
            ),
            (
                "FREQ=WEEKLY;BYDAY=SU,MO,TU,WE,TH,FR,SA;WKST=SU",
                "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;WKST=SU",
            ),
        ] {
            let recurrence =
                Recurrence::from_rrule(rule, start).unwrap_or_else(|e| panic!("{rule}: {e}"));
            assert_eq!(recurrence.to_rrule(None, true), expected, "{rule}");
        }
        let until = Recurrence::from_rrule("FREQ=DAILY;UNTIL=20240630T215959Z", start).unwrap();
        assert_eq!(
            until.range,
            RecurrenceRange::EndDate {
                start,
                end: date(2024, 6, 30)
            }
        );
    }

    #[test]
    fn rules_of_the_event_editor() {
        // A series on Tuesday 10 November 2026, the month's second Tuesday.
        let start = date(2026, 11, 10);
        let new_york: Tz = "America/New_York".parse().unwrap();
        let second_tuesday =
            Recurrence::from_rrule("FREQ=MONTHLY;BYDAY=2TU;COUNT=5", start).unwrap();
        assert_eq!(
            second_tuesday,
            Recurrence {
                pattern: RecurrencePattern::RelativeMonthly {
                    interval: 1,
                    days: vec![DayOfWeek::Tuesday],
                    index: DayOfWeekIndex::Second,
                },
                range: RecurrenceRange::Numbered { start, count: 5 },
            }
        );
        assert_eq!(
            written(&second_tuesday, true),
            "<t:Recurrence><t:RelativeMonthlyRecurrence><t:Interval>1</t:Interval>\
             <t:DaysOfWeek>Tuesday</t:DaysOfWeek><t:DayOfWeekIndex>Second</t:DayOfWeekIndex>\
             </t:RelativeMonthlyRecurrence><t:NumberedRecurrence><t:StartDate>2026-11-10</t:StartDate>\
             <t:NumberOfOccurrences>5</t:NumberOfOccurrences></t:NumberedRecurrence></t:Recurrence>"
        );
        for rule in [
            "FREQ=MONTHLY;BYDAY=2TU;COUNT=5",
            "FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR",
            "FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=31",
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR",
            "FREQ=DAILY;INTERVAL=4;COUNT=10",
            "FREQ=YEARLY;BYMONTH=11;BYMONTHDAY=10",
            // The end of 31 December in New York, in UTC.
            "FREQ=WEEKLY;BYDAY=TU;UNTIL=20270101T045959Z",
        ] {
            let recurrence = Recurrence::from_rrule_in(rule, start, Some(new_york))
                .unwrap_or_else(|e| panic!("{rule}: {e}"));
            assert_eq!(recurrence.to_rrule(Some(new_york), false), rule);
        }
        // Exchange keeps the date the series ends on where it is planned,
        // not the UTC one.
        let until = Recurrence::from_rrule_in(
            "FREQ=WEEKLY;BYDAY=TU;UNTIL=20270101T045959Z",
            start,
            Some(new_york),
        )
        .unwrap();
        assert_eq!(
            until.range,
            RecurrenceRange::EndDate {
                start,
                end: date(2026, 12, 31)
            }
        );
        let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
        let until =
            Recurrence::from_rrule_in("FREQ=DAILY;UNTIL=20261230T150000Z", start, Some(tokyo))
                .unwrap();
        assert_eq!(
            until.range,
            RecurrenceRange::EndDate {
                start,
                end: date(2026, 12, 31)
            }
        );
        // An all-day series ends on a date.
        let all_day =
            Recurrence::from_rrule_in("FREQ=DAILY;INTERVAL=2;UNTIL=20261231", start, None).unwrap();
        assert_eq!(
            all_day.to_rrule(None, true),
            "FREQ=DAILY;INTERVAL=2;UNTIL=20261231"
        );
        // Every other year is the editor's one rule Exchange cannot keep.
        assert!(Recurrence::from_rrule_in("FREQ=YEARLY;INTERVAL=2", start, None).is_err());
    }

    #[test]
    fn rejects_rules_exchange_cannot_store() {
        let start = date(2024, 5, 6);
        for rule in [
            "FREQ=HOURLY",
            "FREQ=YEARLY;INTERVAL=2",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
            "FREQ=MONTHLY;BYMONTHDAY=-1",
            "FREQ=MONTHLY;BYDAY=MO,WE;BYSETPOS=2",
            "FREQ=MONTHLY;BYDAY=5MO",
            "FREQ=WEEKLY;BYWEEKNO=3",
            "FREQ=DAILY;COUNT=2;UNTIL=20240601",
            "INTERVAL=2",
            "FREQ=DAILY;INTERVAL=x",
        ] {
            assert!(
                matches!(
                    Recurrence::from_rrule(rule, start),
                    Err(EwsError::InvalidInput(_))
                ),
                "{rule}"
            );
        }
    }

    #[test]
    fn day_groups() {
        assert_eq!(
            group_days(&[Weekday::Sat, Weekday::Sun]),
            vec![DayOfWeek::WeekendDay]
        );
        assert_eq!(DayOfWeek::Weekday.weekdays().len(), 5);
        assert_eq!(DayOfWeekIndex::from_ordinal(-1), Some(DayOfWeekIndex::Last));
        assert_eq!(DayOfWeekIndex::from_ordinal(5), None);
        assert_eq!(month_number("august"), Some(8));
        assert_eq!(month_name(13), "December");
        assert_eq!(parse_by_day("+3WE"), Some((Some(3), Weekday::Wed)));
        assert_eq!(parse_by_day("X"), None);
    }
}
