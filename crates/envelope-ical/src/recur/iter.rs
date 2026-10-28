//! RRULE evaluation over naive wall-clock time.
//!
//! This is a careful port of the python-dateutil `rrule` algorithm, which is
//! the de-facto reference for RFC 5545 semantics, with three deliberate
//! differences:
//!
//! * `BYDAY` lists mixing plain and ordinal weekdays (`MO,1FR`) are a union
//!   as the RFC requires (dateutil intersects them and yields nothing);
//! * every array access is bounds-checked, so malformed rules never panic;
//! * evaluation is bounded by a budget of processed periods, so rules that
//!   can never match (`BYMONTH=2;BYMONTHDAY=30`) terminate.
//!
//! Expansion happens in local wall-clock time; converting instances to UTC
//! (and thereby preserving wall-clock times across DST transitions) is the
//! caller's job. `UNTIL` and `COUNT` are also applied by the caller.

use std::collections::VecDeque;
use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use super::rule::{Frequency, RecurRule};

const MAX_YEAR: i64 = 9999;

/// Immutable, defaults-applied rule parameters (dateutil's `rrule.__init__`).
#[derive(Debug, Clone)]
struct Params {
    freq: Frequency,
    interval: i64,
    wkst: u32,
    bymonth: Vec<u32>,
    byweekno: Vec<i64>,
    byyearday: Vec<i64>,
    bymonthday: Vec<i64>,
    bynmonthday: Vec<i64>,
    byweekday: Vec<u32>,
    bynweekday: Vec<(u32, i64)>,
    byhour: Vec<u32>,
    byminute: Vec<u32>,
    bysecond: Vec<u32>,
    bysetpos: Vec<i64>,
    dtstart: NaiveDateTime,
    fixed_timeset: Vec<NaiveTime>,
}

impl Params {
    fn new(rule: &RecurRule, dtstart: NaiveDateTime) -> Self {
        let freq = rule.freq;
        let mut bymonth: Vec<u32> = rule.by_month.iter().map(|&m| u32::from(m)).collect();
        let mut bymonthday: Vec<i64> = Vec::new();
        let mut bynmonthday: Vec<i64> = Vec::new();
        for &d in &rule.by_month_day {
            if d > 0 {
                bymonthday.push(i64::from(d));
            } else {
                bynmonthday.push(i64::from(d));
            }
        }
        let mut byweekday: Vec<u32> = Vec::new();
        let mut bynweekday: Vec<(u32, i64)> = Vec::new();
        for day in &rule.by_day {
            let wd = day.weekday.num_days_from_monday();
            match day.ordinal {
                Some(n) if freq <= Frequency::Monthly => bynweekday.push((wd, i64::from(n))),
                _ => byweekday.push(wd),
            }
        }
        if rule.by_week_no.is_empty()
            && rule.by_year_day.is_empty()
            && rule.by_month_day.is_empty()
            && rule.by_day.is_empty()
        {
            match freq {
                Frequency::Yearly => {
                    if bymonth.is_empty() {
                        bymonth.push(dtstart.month());
                    }
                    bymonthday.push(i64::from(dtstart.day()));
                }
                Frequency::Monthly => bymonthday.push(i64::from(dtstart.day())),
                Frequency::Weekly => byweekday.push(dtstart.weekday().num_days_from_monday()),
                _ => {}
            }
        }
        let mut byhour: Vec<u32> = rule.by_hour.iter().map(|&h| u32::from(h)).collect();
        if byhour.is_empty() && freq < Frequency::Hourly {
            byhour.push(dtstart.hour());
        }
        let mut byminute: Vec<u32> = rule.by_minute.iter().map(|&m| u32::from(m)).collect();
        if byminute.is_empty() && freq < Frequency::Minutely {
            byminute.push(dtstart.minute());
        }
        let mut bysecond: Vec<u32> = rule.by_second.iter().map(|&s| u32::from(s)).collect();
        if bysecond.is_empty() && freq < Frequency::Secondly {
            bysecond.push(dtstart.second().min(59));
        }
        let mut fixed_timeset = Vec::new();
        if freq < Frequency::Hourly {
            for &h in &byhour {
                for &m in &byminute {
                    for &s in &bysecond {
                        if let Some(t) = NaiveTime::from_hms_opt(h, m, s) {
                            fixed_timeset.push(t);
                        }
                    }
                }
            }
            fixed_timeset.sort_unstable();
            fixed_timeset.dedup();
        }
        Params {
            freq,
            interval: i64::from(rule.interval.max(1)),
            wkst: rule.week_start.num_days_from_monday(),
            bymonth,
            byweekno: rule.by_week_no.iter().map(|&w| i64::from(w)).collect(),
            byyearday: rule.by_year_day.iter().map(|&d| i64::from(d)).collect(),
            bymonthday,
            bynmonthday,
            byweekday,
            bynweekday,
            byhour,
            byminute,
            bysecond,
            bysetpos: rule.by_set_pos.iter().map(|&p| i64::from(p)).collect(),
            dtstart,
            fixed_timeset,
        }
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 30,
    }
}

fn year_weekday(year: i64) -> Option<u32> {
    let year = i32::try_from(year).ok()?;
    Some(
        NaiveDate::from_ymd_opt(year, 1, 1)?
            .weekday()
            .num_days_from_monday(),
    )
}

/// Month layout of a year: month, day of month and negative day of month
/// for every day index (followed by two weeks of the next January, for
/// weekly periods crossing the year), and where each month starts.
#[derive(Debug)]
struct MonthLayout {
    mmask: Vec<u32>,
    mdaymask: Vec<i64>,
    nmdaymask: Vec<i64>,
    mrange: [usize; 13],
}

impl MonthLayout {
    fn new(leap_year: bool) -> Self {
        // Any common or leap year has the same layout.
        let year = if leap_year { 2000 } else { 2001 };
        let mut layout = MonthLayout {
            mmask: Vec::with_capacity(380),
            mdaymask: Vec::with_capacity(380),
            nmdaymask: Vec::with_capacity(380),
            mrange: [0; 13],
        };
        let mut start = 0usize;
        for month in 1..=12u32 {
            let dim = days_in_month(year, i64::from(month));
            layout.mrange[month as usize - 1] = start;
            for d in 1..=dim {
                layout.mmask.push(month);
                layout.mdaymask.push(d);
                layout.nmdaymask.push(d - dim - 1);
            }
            start += dim as usize;
        }
        layout.mrange[12] = start;
        for d in 1..=14 {
            layout.mmask.push(1);
            layout.mdaymask.push(d);
            layout.nmdaymask.push(d - 32);
        }
        layout
    }
}

static COMMON_YEAR: LazyLock<MonthLayout> = LazyLock::new(|| MonthLayout::new(false));
static LEAP_YEAR: LazyLock<MonthLayout> = LazyLock::new(|| MonthLayout::new(true));
/// Weekday (Monday = 0) of day index `i` of a year starting on a Monday;
/// slicing it at a year's first weekday gives that year's weekday mask.
static WEEKDAYS: LazyLock<Vec<u32>> =
    LazyLock::new(|| (0..7 + 366 + 14).map(|i| (i % 7) as u32).collect());

/// Per-year (and per-month) lookup masks (dateutil's `_iterinfo`). The
/// month and weekday masks are shared statics, so moving to another year
/// costs nothing unless `BYWEEKNO` or ordinal `BYDAY` values are used.
#[derive(Debug)]
struct YearInfo {
    year: Option<i64>,
    month: Option<i64>,
    yearlen: usize,
    nextyearlen: usize,
    first: Option<NaiveDate>,
    mmask: &'static [u32],
    mdaymask: &'static [i64],
    nmdaymask: &'static [i64],
    wdaymask: &'static [u32],
    mrange: &'static [usize; 13],
    wnomask: Vec<bool>,
    nwdaymask: Vec<bool>,
}

impl Default for YearInfo {
    fn default() -> Self {
        let layout: &'static MonthLayout = LazyLock::force(&COMMON_YEAR);
        YearInfo {
            year: None,
            month: None,
            yearlen: 365,
            nextyearlen: 365,
            first: None,
            mmask: &layout.mmask,
            mdaymask: &layout.mdaymask,
            nmdaymask: &layout.nmdaymask,
            wdaymask: LazyLock::force(&WEEKDAYS),
            mrange: &layout.mrange,
            wnomask: Vec::new(),
            nwdaymask: Vec::new(),
        }
    }
}

impl YearInfo {
    fn rebuild(&mut self, year: i64, month: i64, params: &Params) {
        let year_changed = self.year != Some(year);
        if year_changed {
            self.build_year(year, params);
        }
        if !params.bynweekday.is_empty() && (year_changed || self.month != Some(month)) {
            self.build_month(month, params);
        }
        self.month = Some(month);
    }

    fn build_year(&mut self, year: i64, params: &Params) {
        self.year = Some(year);
        self.yearlen = if is_leap(year) { 366 } else { 365 };
        self.nextyearlen = if is_leap(year + 1) { 366 } else { 365 };
        self.first = i32::try_from(year)
            .ok()
            .and_then(|y| NaiveDate::from_ymd_opt(y, 1, 1));
        let yearweekday = year_weekday(year).unwrap_or(0);

        let layout: &'static MonthLayout = if is_leap(year) {
            LazyLock::force(&LEAP_YEAR)
        } else {
            LazyLock::force(&COMMON_YEAR)
        };
        self.mmask = &layout.mmask;
        self.mdaymask = &layout.mdaymask;
        self.nmdaymask = &layout.nmdaymask;
        self.mrange = &layout.mrange;
        let weekdays: &'static [u32] = LazyLock::force(&WEEKDAYS);
        self.wdaymask = weekdays.get(yearweekday as usize..).unwrap_or(weekdays);

        self.wnomask.clear();
        if !params.byweekno.is_empty() {
            self.build_weekno(year, yearweekday, params);
        }
        self.nwdaymask.clear();
    }

    fn build_weekno(&mut self, year: i64, yearweekday: u32, params: &Params) {
        let total = self.yearlen + 14;
        self.wnomask = vec![false; total];
        let wkst = i64::from(params.wkst);
        let yearlen = self.yearlen as i64;
        let yw = i64::from(yearweekday);
        let firstwkst = (7 - yw + wkst).rem_euclid(7);
        let (no1wkst, wyearlen) = if firstwkst >= 4 {
            // Week 1 includes days from the previous year.
            (0, yearlen + (yw - wkst).rem_euclid(7))
        } else {
            (firstwkst, yearlen - firstwkst)
        };
        let numweeks = wyearlen / 7 + (wyearlen % 7) / 4;
        let mark_week = |mask: &mut Vec<bool>, wdaymask: &[u32], mut i: i64| {
            for _ in 0..7 {
                if let Some(slot) = usize::try_from(i).ok().and_then(|i| mask.get_mut(i)) {
                    *slot = true;
                }
                i += 1;
                match usize::try_from(i).ok().and_then(|i| wdaymask.get(i)) {
                    Some(&wd) if i64::from(wd) == wkst => break,
                    Some(_) => {}
                    None => break,
                }
            }
        };
        for &n0 in &params.byweekno {
            let mut n = n0;
            if n < 0 {
                n += numweeks + 1;
            }
            if !(0 < n && n <= numweeks) {
                continue;
            }
            let i = if n > 1 {
                let mut i = no1wkst + (n - 1) * 7;
                if no1wkst != firstwkst {
                    i -= 7 - firstwkst;
                }
                i
            } else {
                no1wkst
            };
            mark_week(&mut self.wnomask, self.wdaymask, i);
        }
        if params.byweekno.contains(&1) {
            // Week number 1 of the next year may start in this year.
            let mut i = no1wkst + numweeks * 7;
            if no1wkst != firstwkst {
                i -= 7 - firstwkst;
            }
            if i < yearlen {
                mark_week(&mut self.wnomask, self.wdaymask, i);
            }
        }
        if no1wkst != 0 {
            // Days before week 1 belong to the last week of the previous year.
            let lnumweeks = if params.byweekno.contains(&-1) {
                -1
            } else {
                let lyearweekday = i64::from(year_weekday(year - 1).unwrap_or(0));
                let lno1wkst = (7 - lyearweekday + wkst).rem_euclid(7);
                let lyearlen = if is_leap(year - 1) { 366 } else { 365 };
                if lno1wkst >= 4 {
                    52 + ((lyearlen + (lyearweekday - wkst).rem_euclid(7)) % 7) / 4
                } else {
                    52 + ((yearlen - no1wkst) % 7) / 4
                }
            };
            if params.byweekno.contains(&lnumweeks) {
                for slot in self.wnomask.iter_mut().take(no1wkst as usize) {
                    *slot = true;
                }
            }
        }
    }

    fn build_month(&mut self, month: i64, params: &Params) {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let month_range = |m: i64| -> Option<(usize, usize)> {
            let m = usize::try_from(m).ok().filter(|m| (1..=12).contains(m))?;
            Some((self.mrange[m - 1], self.mrange[m]))
        };
        match params.freq {
            Frequency::Yearly => {
                if params.bymonth.is_empty() {
                    ranges.push((0, self.yearlen));
                } else {
                    ranges.extend(
                        params
                            .bymonth
                            .iter()
                            .filter_map(|&m| month_range(i64::from(m))),
                    );
                }
            }
            Frequency::Monthly => ranges.extend(month_range(month)),
            _ => {}
        }
        self.nwdaymask.clear();
        if ranges.is_empty() {
            return;
        }
        self.nwdaymask = vec![false; self.yearlen];
        for (first, last) in ranges {
            if last == 0 {
                continue;
            }
            let first = first as i64;
            let last = last as i64 - 1;
            for &(wday, n) in &params.bynweekday {
                let base = if n < 0 {
                    last + (n + 1) * 7
                } else {
                    first + (n - 1) * 7
                };
                let Some(&mask_wd) = usize::try_from(base)
                    .ok()
                    .and_then(|b| self.wdaymask.get(b))
                else {
                    continue;
                };
                let i = if n < 0 {
                    base - (i64::from(mask_wd) - i64::from(wday)).rem_euclid(7)
                } else {
                    base + (7 - i64::from(mask_wd) + i64::from(wday)).rem_euclid(7)
                };
                if first <= i
                    && i <= last
                    && let Some(slot) = usize::try_from(i)
                        .ok()
                        .and_then(|i| self.nwdaymask.get_mut(i))
                {
                    *slot = true;
                }
            }
        }
    }

    fn date_at(&self, index: usize) -> Option<NaiveDate> {
        self.first?
            .checked_add_days(chrono::Days::new(index as u64))
    }
}

/// Lazily yields the instances of an RRULE, in ascending local time, that
/// are not before `dtstart`. Instances are *not* limited by `UNTIL` or
/// `COUNT` (see module docs). `DTSTART` itself is only yielded if it matches
/// the rule.
#[derive(Debug)]
pub(crate) struct RuleIter {
    params: Params,
    info: YearInfo,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    weekday: u32,
    timeset: Vec<NaiveTime>,
    pending: VecDeque<NaiveDateTime>,
    finished: bool,
    budget: u64,
    exhausted: bool,
    stop_after: Option<NaiveDate>,
}

impl RuleIter {
    /// Creates an iterator that processes at most `budget` periods (years,
    /// months, weeks, days, … depending on `FREQ`).
    pub(crate) fn new(rule: &RecurRule, dtstart: NaiveDateTime, budget: u64) -> Self {
        let params = Params::new(rule, dtstart);
        let mut iter = RuleIter {
            year: i64::from(dtstart.year()),
            month: i64::from(dtstart.month()),
            day: i64::from(dtstart.day()),
            hour: i64::from(dtstart.hour()),
            minute: i64::from(dtstart.minute()),
            second: i64::from(dtstart.second().min(59)),
            weekday: dtstart.weekday().num_days_from_monday(),
            params,
            info: YearInfo::default(),
            timeset: Vec::new(),
            pending: VecDeque::new(),
            finished: false,
            budget,
            exhausted: false,
            stop_after: None,
        };
        iter.info.rebuild(iter.year, iter.month, &iter.params);
        iter.timeset = iter.initial_timeset();
        iter
    }

    /// Time set of the first sub-daily period: empty when the current time
    /// itself is excluded by `BYHOUR`/`BYMINUTE`/`BYSECOND`.
    fn initial_timeset(&self) -> Vec<NaiveTime> {
        let p = &self.params;
        if p.freq < Frequency::Hourly {
            return Vec::new();
        }
        let allowed = |list: &[u32], value: i64| {
            list.is_empty() || u32::try_from(value).is_ok_and(|v| list.contains(&v))
        };
        let hour_ok = allowed(&p.byhour, self.hour);
        let minute_ok = p.freq < Frequency::Minutely || allowed(&p.byminute, self.minute);
        let second_ok = p.freq < Frequency::Secondly || allowed(&p.bysecond, self.second);
        if hour_ok && minute_ok && second_ok {
            self.current_timeset()
        } else {
            Vec::new()
        }
    }

    /// True if iteration stopped because the period budget ran out.
    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Ends iteration once a period would start after `date`, so that rules
    /// which rarely (or never) match do not run to the end of the calendar.
    pub(crate) fn stop_after(&mut self, date: NaiveDate) {
        self.stop_after = Some(date);
    }

    fn past_stop(&self) -> bool {
        let Some(stop) = self.stop_after else {
            return false;
        };
        let (year, month) = (i64::from(stop.year()), i64::from(stop.month()));
        match self.params.freq {
            Frequency::Yearly => self.year > year,
            Frequency::Monthly => (self.year, self.month) > (year, month),
            _ => self.current_date().is_some_and(|current| current > stop),
        }
    }

    /// Skips whole periods so that iteration resumes at the last period that
    /// starts on or before `target`. Only valid before iteration started and
    /// for rules without `COUNT` (skipped instances are not counted).
    pub(crate) fn skip_to(&mut self, target: NaiveDate) {
        if !self.pending.is_empty() || self.finished {
            return;
        }
        let interval = self.params.interval;
        let Some(current) = self.current_date() else {
            return;
        };
        if target <= current {
            return;
        }
        match self.params.freq {
            Frequency::Yearly => {
                let k = (i64::from(target.year()) - self.year).div_euclid(interval);
                if k >= 1 {
                    self.year += k * interval;
                    self.info.rebuild(self.year, self.month, &self.params);
                }
            }
            Frequency::Monthly => {
                let months = (i64::from(target.year()) - self.year) * 12
                    + (i64::from(target.month()) - self.month);
                let k = months.div_euclid(interval);
                if k >= 1 {
                    let index = self.year * 12 + (self.month - 1) + k * interval;
                    self.year = index.div_euclid(12);
                    self.month = index.rem_euclid(12) + 1;
                    self.info.rebuild(self.year, self.month, &self.params);
                }
            }
            Frequency::Weekly => {
                let offset = (i64::from(self.weekday) - i64::from(self.params.wkst)).rem_euclid(7);
                let Some(week_start) = current.checked_sub_days(chrono::Days::new(offset as u64))
                else {
                    return;
                };
                let k = (target - week_start).num_days().div_euclid(7 * interval);
                if k >= 1
                    && let Some(date) =
                        week_start.checked_add_days(chrono::Days::new((k * 7 * interval) as u64))
                {
                    self.set_date(date);
                    self.weekday = self.params.wkst;
                }
            }
            Frequency::Daily => {
                let k = (target - current).num_days().div_euclid(interval);
                if k >= 1
                    && let Some(date) =
                        current.checked_add_days(chrono::Days::new((k * interval) as u64))
                {
                    self.set_date(date);
                    self.weekday = date.weekday().num_days_from_monday();
                }
            }
            Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => {
                // Instances lie on the lattice DTSTART + k × INTERVAL (filters
                // only remove points), so jumping whole steps keeps alignment.
                let unit = interval
                    * match self.params.freq {
                        Frequency::Hourly => 3_600,
                        Frequency::Minutely => 60,
                        _ => 1,
                    };
                let (Ok(h), Ok(m), Ok(sec)) = (
                    u32::try_from(self.hour),
                    u32::try_from(self.minute),
                    u32::try_from(self.second),
                ) else {
                    return;
                };
                let Some(now) = current.and_hms_opt(h, m, sec) else {
                    return;
                };
                let k = (target.and_time(NaiveTime::MIN) - now)
                    .num_seconds()
                    .div_euclid(unit);
                if k < 1 {
                    return;
                }
                let Some(next) = now.checked_add_signed(chrono::TimeDelta::seconds(k * unit))
                else {
                    return;
                };
                self.set_date(next.date());
                self.weekday = next.weekday().num_days_from_monday();
                self.hour = i64::from(next.hour());
                self.minute = i64::from(next.minute());
                self.second = i64::from(next.second());
                self.timeset = self.initial_timeset();
            }
        }
    }

    fn set_date(&mut self, date: NaiveDate) {
        self.year = i64::from(date.year());
        self.month = i64::from(date.month());
        self.day = i64::from(date.day());
        self.info.rebuild(self.year, self.month, &self.params);
    }

    fn current_date(&self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(
            i32::try_from(self.year).ok()?,
            u32::try_from(self.month).ok()?,
            u32::try_from(self.day).ok()?,
        )
    }

    fn current_timeset(&self) -> Vec<NaiveTime> {
        let p = &self.params;
        let (Ok(hour), Ok(minute), Ok(second)) = (
            u32::try_from(self.hour),
            u32::try_from(self.minute),
            u32::try_from(self.second),
        ) else {
            return Vec::new();
        };
        let mut set = Vec::new();
        match p.freq {
            Frequency::Hourly => {
                for &m in &p.byminute {
                    for &s in &p.bysecond {
                        set.extend(NaiveTime::from_hms_opt(hour, m, s));
                    }
                }
            }
            Frequency::Minutely => {
                for &s in &p.bysecond {
                    set.extend(NaiveTime::from_hms_opt(hour, minute, s));
                }
            }
            _ => set.extend(NaiveTime::from_hms_opt(hour, minute, second)),
        }
        set.sort_unstable();
        set
    }

    /// Indices (day of year, possibly past the end for weekly sets) of the
    /// current period, and whether any day was filtered out.
    fn period_days(&self) -> (Vec<usize>, bool) {
        let info = &self.info;
        let candidates: Vec<usize> = match self.params.freq {
            // Days outside BYMONTH can never match: only look at those months.
            Frequency::Yearly if !self.params.bymonth.is_empty() => {
                let mut months = self.params.bymonth.clone();
                months.sort_unstable();
                months.dedup();
                months
                    .iter()
                    .filter_map(|&m| usize::try_from(m).ok().filter(|m| (1..=12).contains(m)))
                    .flat_map(|m| info.mrange[m - 1]..info.mrange[m])
                    .collect()
            }
            Frequency::Yearly => (0..info.yearlen).collect(),
            Frequency::Monthly => {
                let m = usize::try_from(self.month).unwrap_or(1).clamp(1, 12);
                (info.mrange[m - 1]..info.mrange[m]).collect()
            }
            Frequency::Weekly => {
                let Some(start) = self.current_date().map(|d| d.ordinal0() as usize) else {
                    return (Vec::new(), true);
                };
                let mut set = Vec::with_capacity(7);
                let mut i = start;
                for _ in 0..7 {
                    set.push(i);
                    i += 1;
                    match info.wdaymask.get(i) {
                        Some(&wd) if wd == self.params.wkst => break,
                        Some(_) => {}
                        None => break,
                    }
                }
                set
            }
            _ => match self.current_date() {
                Some(d) => vec![d.ordinal0() as usize],
                None => Vec::new(),
            },
        };
        let total = candidates.len();
        let kept: Vec<usize> = candidates
            .into_iter()
            .filter(|&i| self.day_matches(i))
            .collect();
        let filtered = kept.len() != total;
        (kept, filtered)
    }

    fn day_matches(&self, i: usize) -> bool {
        let p = &self.params;
        let info = &self.info;
        let (Some(&month), Some(&mday), Some(&nmday), Some(&wday)) = (
            info.mmask.get(i),
            info.mdaymask.get(i),
            info.nmdaymask.get(i),
            info.wdaymask.get(i),
        ) else {
            return false;
        };
        if !p.bymonth.is_empty() && !p.bymonth.contains(&month) {
            return false;
        }
        if !p.byweekno.is_empty() && !info.wnomask.get(i).copied().unwrap_or(false) {
            return false;
        }
        let has_plain = !p.byweekday.is_empty();
        let has_ordinal = !info.nwdaymask.is_empty();
        if has_plain || has_ordinal {
            let plain = has_plain && p.byweekday.contains(&wday);
            let ordinal = has_ordinal && info.nwdaymask.get(i).copied().unwrap_or(false);
            if !plain && !ordinal {
                return false;
            }
        }
        if (!p.bymonthday.is_empty() || !p.bynmonthday.is_empty())
            && !p.bymonthday.contains(&mday)
            && !p.bynmonthday.contains(&nmday)
        {
            return false;
        }
        if !p.byyearday.is_empty() {
            let yearlen = info.yearlen as i64;
            let idx = i as i64;
            let ok = if i < info.yearlen {
                p.byyearday.contains(&(idx + 1)) || p.byyearday.contains(&(idx - yearlen))
            } else {
                p.byyearday.contains(&(idx + 1 - yearlen))
                    || p.byyearday
                        .contains(&(idx - yearlen - info.nextyearlen as i64))
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Emits the instances of the current period and advances to the next.
    fn step(&mut self) {
        let (days, filtered) = self.period_days();
        let timeset: &[NaiveTime] = if self.params.freq < Frequency::Hourly {
            &self.params.fixed_timeset
        } else {
            &self.timeset
        };
        let mut results: Vec<NaiveDateTime> = Vec::new();
        if !self.params.bysetpos.is_empty() && !timeset.is_empty() {
            let n = timeset.len() as i64;
            for &pos in &self.params.bysetpos {
                let (daypos, timepos) = if pos < 0 {
                    (pos.div_euclid(n), pos.rem_euclid(n))
                } else {
                    ((pos - 1).div_euclid(n), (pos - 1).rem_euclid(n))
                };
                let index = if daypos < 0 {
                    days.len() as i64 + daypos
                } else {
                    daypos
                };
                let Some(&day_index) = usize::try_from(index).ok().and_then(|i| days.get(i)) else {
                    continue;
                };
                let (Some(date), Some(&time)) = (
                    self.info.date_at(day_index),
                    usize::try_from(timepos).ok().and_then(|t| timeset.get(t)),
                ) else {
                    continue;
                };
                let instance = date.and_time(time);
                if !results.contains(&instance) {
                    results.push(instance);
                }
            }
            results.sort_unstable();
        } else {
            for &day_index in &days {
                if let Some(date) = self.info.date_at(day_index) {
                    results.extend(timeset.iter().map(|&t| date.and_time(t)));
                }
            }
        }
        let dtstart = self.params.dtstart;
        self.pending
            .extend(results.into_iter().filter(|r| *r >= dtstart));
        self.advance(filtered);
    }

    fn finish(&mut self) {
        self.finished = true;
    }

    fn advance(&mut self, filtered: bool) {
        let interval = self.params.interval;
        let mut fixday = false;
        match self.params.freq {
            Frequency::Yearly => {
                self.year += interval;
                if self.year > MAX_YEAR {
                    return self.finish();
                }
                self.info.rebuild(self.year, self.month, &self.params);
            }
            Frequency::Monthly => {
                let index = self.year * 12 + (self.month - 1) + interval;
                self.year = index.div_euclid(12);
                self.month = index.rem_euclid(12) + 1;
                if self.year > MAX_YEAR {
                    return self.finish();
                }
                self.info.rebuild(self.year, self.month, &self.params);
            }
            Frequency::Weekly => {
                let wkst = i64::from(self.params.wkst);
                let wd = i64::from(self.weekday);
                self.day += if wkst > wd {
                    -(wd + 1 + (6 - wkst)) + interval * 7
                } else {
                    -(wd - wkst) + interval * 7
                };
                self.weekday = self.params.wkst;
                fixday = true;
            }
            Frequency::Daily => {
                self.day += interval;
                fixday = true;
            }
            Frequency::Hourly => {
                if filtered {
                    // Jump to the last iteration of the day.
                    self.hour += ((23 - self.hour) / interval) * interval;
                }
                let ndays = if self.params.byhour.is_empty() {
                    let total = self.hour + interval;
                    self.hour = total.rem_euclid(24);
                    total.div_euclid(24)
                } else {
                    match mod_distance(self.hour, &self.params.byhour, 24, interval) {
                        Some((days, hour)) => {
                            self.hour = hour;
                            days
                        }
                        None => return self.finish(),
                    }
                };
                if ndays != 0 {
                    self.day += ndays;
                    fixday = true;
                }
                self.timeset = self.current_timeset();
            }
            Frequency::Minutely => {
                if filtered {
                    self.minute += ((1439 - (self.hour * 60 + self.minute)) / interval) * interval;
                }
                let repeat = 1440 / gcd(interval, 1440);
                let mut valid = false;
                for _ in 0..repeat {
                    let nhours = if self.params.byminute.is_empty() {
                        let total = self.minute + interval;
                        self.minute = total.rem_euclid(60);
                        total.div_euclid(60)
                    } else {
                        match mod_distance(self.minute, &self.params.byminute, 60, interval) {
                            Some((hours, minute)) => {
                                self.minute = minute;
                                hours
                            }
                            None => return self.finish(),
                        }
                    };
                    let total = self.hour + nhours;
                    self.hour = total.rem_euclid(24);
                    let ndays = total.div_euclid(24);
                    if ndays != 0 {
                        self.day += ndays;
                        fixday = true;
                    }
                    if self.params.byhour.is_empty()
                        || self.params.byhour.contains(&(self.hour as u32))
                    {
                        valid = true;
                        break;
                    }
                }
                if !valid {
                    return self.finish();
                }
                self.timeset = self.current_timeset();
            }
            Frequency::Secondly => {
                if filtered {
                    self.second += ((86_399
                        - (self.hour * 3_600 + self.minute * 60 + self.second))
                        / interval)
                        * interval;
                }
                let repeat = 86_400 / gcd(interval, 86_400);
                let mut valid = false;
                for _ in 0..repeat {
                    let nminutes = if self.params.bysecond.is_empty() {
                        let total = self.second + interval;
                        self.second = total.rem_euclid(60);
                        total.div_euclid(60)
                    } else {
                        match mod_distance(self.second, &self.params.bysecond, 60, interval) {
                            Some((minutes, second)) => {
                                self.second = second;
                                minutes
                            }
                            None => return self.finish(),
                        }
                    };
                    let total = self.minute + nminutes;
                    self.minute = total.rem_euclid(60);
                    let carry = total.div_euclid(60);
                    if carry != 0 {
                        let total = self.hour + carry;
                        self.hour = total.rem_euclid(24);
                        let ndays = total.div_euclid(24);
                        if ndays != 0 {
                            self.day += ndays;
                            fixday = true;
                        }
                    }
                    let p = &self.params;
                    if (p.byhour.is_empty() || p.byhour.contains(&(self.hour as u32)))
                        && (p.byminute.is_empty() || p.byminute.contains(&(self.minute as u32)))
                        && (p.bysecond.is_empty() || p.bysecond.contains(&(self.second as u32)))
                    {
                        valid = true;
                        break;
                    }
                }
                if !valid {
                    return self.finish();
                }
                self.timeset = self.current_timeset();
            }
        }
        if fixday && self.day > 28 {
            let mut dim = days_in_month(self.year, self.month);
            if self.day > dim {
                while self.day > dim {
                    self.day -= dim;
                    self.month += 1;
                    if self.month == 13 {
                        self.month = 1;
                        self.year += 1;
                        if self.year > MAX_YEAR {
                            return self.finish();
                        }
                    }
                    dim = days_in_month(self.year, self.month);
                }
                self.info.rebuild(self.year, self.month, &self.params);
            }
        }
    }
}

/// Smallest step count (in units of `interval`) after which `value` lands on
/// an element of `allowed`, modulo `base`: returns `(carry, new_value)`.
fn mod_distance(value: i64, allowed: &[u32], base: i64, interval: i64) -> Option<(i64, i64)> {
    let mut accumulator = 0;
    let mut value = value;
    for _ in 0..base {
        let total = value + interval;
        accumulator += total.div_euclid(base);
        value = total.rem_euclid(base);
        if u32::try_from(value).is_ok_and(|v| allowed.contains(&v)) {
            return Some((accumulator, value));
        }
    }
    None
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

impl Iterator for RuleIter {
    type Item = NaiveDateTime;

    fn next(&mut self) -> Option<NaiveDateTime> {
        loop {
            if let Some(next) = self.pending.pop_front() {
                return Some(next);
            }
            if self.finished {
                return None;
            }
            if self.past_stop() {
                self.finished = true;
                return None;
            }
            if self.budget == 0 {
                self.exhausted = true;
                self.finished = true;
                return None;
            }
            self.budget -= 1;
            self.step();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").expect("valid test datetime")
    }

    fn run(rule: &str, start: &str, n: usize) -> Vec<String> {
        let rule = RecurRule::parse(rule).expect("valid rule");
        RuleIter::new(&rule, dt(start), 100_000)
            .take(n)
            .map(|d| d.format("%Y%m%dT%H%M%S").to_string())
            .collect()
    }

    #[test]
    fn daily_basic() {
        assert_eq!(
            run("FREQ=DAILY", "20240130T090000", 3),
            vec!["20240130T090000", "20240131T090000", "20240201T090000"]
        );
    }

    #[test]
    fn never_matching_rule_terminates() {
        let rule = RecurRule::parse("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30").expect("rule");
        let mut iter = RuleIter::new(&rule, dt("20240101T000000"), 1_000);
        assert_eq!(iter.next(), None);
        assert!(iter.exhausted());

        let rule = RecurRule::parse("FREQ=HOURLY;INTERVAL=2;BYHOUR=3").expect("rule");
        let mut iter = RuleIter::new(&rule, dt("20240101T000000"), 1_000);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn mixed_ordinal_and_plain_byday_is_a_union() {
        // Every Monday plus the first Friday of each month.
        assert_eq!(
            run("FREQ=MONTHLY;BYDAY=MO,1FR", "20240101T090000", 6),
            vec![
                "20240101T090000",
                "20240105T090000",
                "20240108T090000",
                "20240115T090000",
                "20240122T090000",
                "20240129T090000",
            ]
        );
    }

    #[test]
    fn weekly_crossing_year_boundary() {
        assert_eq!(
            run("FREQ=WEEKLY;BYDAY=MO,TH", "20241230T100000", 4),
            vec![
                "20241230T100000",
                "20250102T100000",
                "20250106T100000",
                "20250109T100000"
            ]
        );
    }

    #[test]
    fn skip_to_keeps_alignment() {
        let rule = RecurRule::parse("FREQ=WEEKLY;INTERVAL=2;BYDAY=TU").expect("rule");
        let all: Vec<_> = RuleIter::new(&rule, dt("20200107T090000"), 10_000)
            .take_while(|d| d.year() < 2027)
            .collect();
        let mut skipped = RuleIter::new(&rule, dt("20200107T090000"), 10_000);
        skipped.skip_to(NaiveDate::from_ymd_opt(2026, 3, 1).expect("date"));
        let first = skipped.next().expect("instance");
        assert!(all.contains(&first));
        assert!(
            first.date()
                <= NaiveDate::from_ymd_opt(2026, 3, 1).expect("date") + chrono::Days::new(14)
        );

        for (freq, start) in [
            ("FREQ=DAILY;INTERVAL=3", "20200101T080000"),
            ("FREQ=MONTHLY;INTERVAL=5;BYMONTHDAY=31", "20200131T080000"),
            (
                "FREQ=YEARLY;INTERVAL=4;BYMONTH=2;BYMONTHDAY=29",
                "19960229T080000",
            ),
            ("FREQ=HOURLY;INTERVAL=7", "20310101T050000"),
            (
                "FREQ=HOURLY;INTERVAL=5;BYDAY=MO,FR;BYHOUR=1,6,11",
                "20310102T060000",
            ),
            ("FREQ=MINUTELY;INTERVAL=37;BYHOUR=9,10", "20310301T091500"),
            (
                "FREQ=SECONDLY;INTERVAL=13;BYMINUTE=0,30;BYSECOND=5,18,31",
                "20310610T000005",
            ),
        ] {
            let rule = RecurRule::parse(freq).expect("rule");
            let all: Vec<_> = RuleIter::new(&rule, dt(start), 100_000)
                .take_while(|d| d.year() < 2040)
                .collect();
            let mut skipped = RuleIter::new(&rule, dt(start), 100_000);
            let target = NaiveDate::from_ymd_opt(2031, 6, 15).expect("date");
            skipped.skip_to(target);
            let rest: Vec<_> = skipped.take_while(|d| d.year() < 2040).collect();
            let expected: Vec<_> = all
                .iter()
                .copied()
                .filter(|d| d.date() >= rest.first().map(|r| r.date()).unwrap_or(target))
                .collect();
            assert_eq!(rest, expected, "{freq}");
            assert!(
                rest.first()
                    .is_some_and(|r| r.date() <= target + chrono::Days::new(5 * 31 * 4 * 12))
            );
        }
    }
}
