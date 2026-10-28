//! Recurrence expansion (RFC 5545 §3.8.5).
//!
//! Instances are generated in the series' wall-clock time and converted to
//! UTC one by one, so a weekly 09:00 meeting stays at 09:00 across DST
//! transitions. `RDATE`s are added, `EXDATE`s removed, and overridden
//! instances (`RECURRENCE-ID`) replaced by their override, which may move
//! an occurrence into or out of the requested window; cancelled overrides
//! remove their occurrence.

mod iter;
pub mod rule;

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::model::{Event, EventStatus, EventTime};
use crate::tz::{TzContext, Zone, wall_clock, with_wall_clock};

pub(crate) use iter::RuleIter;
pub use rule::{Frequency, RecurRule, WeekdayNum};

/// Default for [`ExpandOptions::max_occurrences`].
pub const DEFAULT_MAX_OCCURRENCES: usize = 5_000;

/// Upper bound on instances generated internally for one series, including
/// those before the window that are needed to honour `COUNT`.
const MAX_GENERATED: usize = 500_000;

/// Upper bound on rule periods (years, months, weeks, days, …) evaluated.
const PERIOD_BUDGET: u64 = 2_000_000;

/// Options for [`expand`] and [`expand_events`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpandOptions {
    /// Zone used for floating times, all-day dates and unresolvable TZIDs
    /// (normally the user's zone).
    pub default_tz: Tz,
    /// Hard cap on the occurrences returned by one call.
    pub max_occurrences: usize,
}

impl Default for ExpandOptions {
    fn default() -> Self {
        ExpandOptions {
            default_tz: Tz::UTC,
            max_occurrences: DEFAULT_MAX_OCCURRENCES,
        }
    }
}

impl ExpandOptions {
    /// Options with the given default zone and the default cap.
    pub fn with_default_tz(default_tz: Tz) -> Self {
        ExpandOptions {
            default_tz,
            ..ExpandOptions::default()
        }
    }
}

/// One concrete occurrence of an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    /// Start instant.
    pub start: DateTime<Utc>,
    /// End instant (exclusive).
    pub end: DateTime<Utc>,
    /// Start in its original form (series wall-clock time, all-day date, …;
    /// the override's own value for overridden occurrences).
    pub start_time: EventTime,
    /// End in its original form.
    pub end_time: EventTime,
    /// The occurrence's `RECURRENCE-ID`, in the same form as the series'
    /// `DTSTART`. `None` for non-recurring events.
    pub recurrence_id: Option<EventTime>,
    /// Index into the overrides passed to [`expand`] when an override
    /// replaces this occurrence.
    pub override_index: Option<usize>,
}

/// Result of [`expand`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expansion {
    /// Occurrences overlapping the window, sorted by start.
    pub occurrences: Vec<Occurrence>,
    /// True if a limit was hit and occurrences may be missing.
    pub truncated: bool,
}

/// An occurrence produced by [`expand_events`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventOccurrence {
    /// Index of the event (series master, override or single event) whose
    /// details apply to this occurrence.
    pub event_index: usize,
    /// The occurrence.
    pub occurrence: Occurrence,
}

/// Result of [`expand_events`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsExpansion {
    /// Occurrences of all events overlapping the window, sorted by start.
    pub occurrences: Vec<EventOccurrence>,
    /// True if a limit was hit and occurrences may be missing.
    pub truncated: bool,
}

/// How an instance's end is derived from its start.
#[derive(Debug, Clone, Copy)]
enum Span {
    /// All-day: a number of days.
    Days(i64),
    /// Wall-clock difference in the series' zone (keeps 09:00–10:00 intact
    /// across DST).
    Wall(TimeDelta),
    /// Exact duration (start and end in different zones or UTC).
    Exact(TimeDelta),
}

impl Span {
    fn approximate(self) -> TimeDelta {
        match self {
            Span::Days(days) => TimeDelta::try_days(days).unwrap_or(TimeDelta::zero()),
            Span::Wall(d) | Span::Exact(d) => d,
        }
    }
}

fn span_of(event: &Event, ctx: &TzContext, tz: Tz) -> Span {
    match (&event.start, &event.end) {
        (EventTime::Date { date: start }, end) => {
            Span::Days((wall_clock(end).date() - *start).num_days().max(1))
        }
        (EventTime::Zoned { local: s, tzid: a }, EventTime::Zoned { local: e, tzid: b })
            if a == b =>
        {
            Span::Wall((*e - *s).max(TimeDelta::zero()))
        }
        (EventTime::Floating { local: s }, EventTime::Floating { local: e }) => {
            Span::Wall((*e - *s).max(TimeDelta::zero()))
        }
        (start, end) => {
            Span::Exact((ctx.to_utc(end, tz) - ctx.to_utc(start, tz)).max(TimeDelta::zero()))
        }
    }
}

/// End of an instance starting at `start` (in the series' form).
fn instance_end(
    start: &EventTime,
    start_utc: DateTime<Utc>,
    span: Span,
    template_end: &EventTime,
    ctx: &TzContext,
    tz: Tz,
) -> (EventTime, DateTime<Utc>) {
    match span {
        Span::Days(days) => {
            let date = wall_clock(start).date();
            let end = date
                .checked_add_signed(TimeDelta::try_days(days).unwrap_or(TimeDelta::zero()))
                .unwrap_or(date);
            let end_time = EventTime::Date { date: end };
            let end_utc = ctx.to_utc(&end_time, tz);
            (end_time, end_utc)
        }
        Span::Wall(delta) => {
            let local = wall_clock(start)
                .checked_add_signed(delta)
                .unwrap_or_else(|| wall_clock(start));
            let end_time = match template_end {
                EventTime::Zoned { tzid, .. } => EventTime::Zoned {
                    local,
                    tzid: tzid.clone(),
                },
                _ => EventTime::Floating { local },
            };
            let end_utc = ctx.to_utc(&end_time, tz).max(start_utc);
            (end_time, end_utc)
        }
        Span::Exact(delta) => {
            let end_utc = start_utc.checked_add_signed(delta).unwrap_or(start_utc);
            let end_time = ctx.express_like(&EventTime::Utc { at: end_utc }, template_end, tz);
            (end_time, end_utc)
        }
    }
}

/// `UNTIL` interpreted against the series' `DTSTART` form, including the
/// common real-world mismatches.
#[derive(Debug, Clone, Copy)]
enum UntilBound {
    Unbounded,
    Instant(DateTime<Utc>),
    Local(NaiveDateTime),
    Date(NaiveDate),
    /// All-day series with a DATE-TIME `UNTIL`: a date is included if it is
    /// not after the written UTC date, or if its local midnight is not after
    /// the instant (producers that wrote local midnight in UTC). An `UNTIL`
    /// at 23:59 UTC or later is read as "end of the written date".
    AllDay(NaiveDate, Option<DateTime<Utc>>),
}

impl UntilBound {
    fn new(rule: &RecurRule, start: &EventTime) -> Self {
        let Some(until) = &rule.until else {
            return UntilBound::Unbounded;
        };
        match (start, until) {
            (EventTime::Date { .. }, EventTime::Date { date }) => UntilBound::Date(*date),
            (EventTime::Date { .. }, EventTime::Utc { at }) => {
                let end_of_day = NaiveTime::from_hms_opt(23, 59, 0).unwrap_or(NaiveTime::MIN);
                let instant = (at.time() < end_of_day).then_some(*at);
                UntilBound::AllDay(at.date_naive(), instant)
            }
            (EventTime::Date { .. }, other) => UntilBound::Date(wall_clock(other).date()),
            (_, EventTime::Utc { at }) => UntilBound::Instant(*at),
            (_, EventTime::Date { date }) => UntilBound::Date(*date),
            (_, EventTime::Zoned { local, .. } | EventTime::Floating { local }) => {
                UntilBound::Local(*local)
            }
        }
    }

    fn excludes(self, local: NaiveDateTime, utc: DateTime<Utc>) -> bool {
        match self {
            UntilBound::Unbounded => false,
            UntilBound::Instant(at) => utc > at,
            UntilBound::Local(bound) => local > bound,
            UntilBound::Date(date) => local.date() > date,
            UntilBound::AllDay(date, instant) => {
                local.date() > date && !instant.is_some_and(|at| utc <= at)
            }
        }
    }
}

/// Key identifying an occurrence for `EXDATE` and override matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RidKey {
    Date(NaiveDate),
    Instant(DateTime<Utc>),
}

fn rid_key(time: &EventTime, all_day_series: bool, ctx: &TzContext, tz: Tz) -> RidKey {
    if all_day_series || time.is_date() {
        RidKey::Date(wall_clock(time).date())
    } else {
        RidKey::Instant(ctx.to_utc(time, tz))
    }
}

/// Wall-clock value of an `RDATE` in the series' zone.
fn rdate_local(
    rdate: &EventTime,
    master_start: &EventTime,
    zone: &Zone,
    ctx: &TzContext,
    tz: Tz,
) -> NaiveDateTime {
    let start_wall = wall_clock(master_start);
    match (rdate, master_start) {
        (_, EventTime::Date { .. }) => wall_clock(rdate).date().and_time(NaiveTime::MIN),
        (EventTime::Date { date }, _) => date.and_time(start_wall.time()),
        (EventTime::Floating { local }, EventTime::Floating { .. }) => *local,
        (EventTime::Zoned { local, tzid }, EventTime::Zoned { tzid: own, .. }) if tzid == own => {
            *local
        }
        _ => zone.to_local(ctx.to_utc(rdate, tz)),
    }
}

fn overlaps(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> bool {
    if end <= start {
        start >= window_start && start < window_end
    } else {
        start < window_end && end > window_start
    }
}

/// Bounds for [`local_instances`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct InstanceBounds {
    /// Skip instances before this date (ignored when the rule has `COUNT`).
    pub from: Option<NaiveDate>,
    /// No instance after this date is needed.
    pub until: Option<NaiveDate>,
    /// Maximum number of instances.
    pub max: usize,
    /// Maximum number of rule periods evaluated.
    pub budget: u64,
}

/// Instances of `rule` in wall-clock time with RFC 5545 semantics: `DTSTART`
/// is always the first instance and counts towards `COUNT`. Generation stops
/// when `beyond` returns true or a bound is reached; the flag is true if a
/// limit (not the rule) ended generation.
pub(crate) fn local_instances(
    rule: &RecurRule,
    dtstart: NaiveDateTime,
    mut beyond: impl FnMut(&NaiveDateTime) -> bool,
    bounds: InstanceBounds,
) -> (Vec<NaiveDateTime>, bool) {
    let mut out = vec![dtstart];
    let limit = rule.count.map_or(usize::MAX, |c| c as usize);
    let max = bounds.max;
    if out.len() >= limit {
        return (out, false);
    }
    let mut iter = RuleIter::new(rule, dtstart, bounds.budget);
    if let (Some(from), None) = (bounds.from, rule.count) {
        iter.skip_to(from);
    }
    if let Some(until) = bounds.until {
        iter.stop_after(until);
    }
    for instance in iter.by_ref() {
        if instance <= dtstart {
            continue;
        }
        if beyond(&instance) {
            return (out, false);
        }
        out.push(instance);
        if out.len() >= limit {
            return (out, false);
        }
        if out.len() >= max {
            return (out, true);
        }
    }
    let exhausted = iter.exhausted();
    (out, exhausted)
}

/// Expands a series master and its overrides within `[window_start,
/// window_end)`.
///
/// Occurrences overlapping the window are returned sorted by start. An
/// override applies to the instance whose start equals its
/// `RECURRENCE-ID` (compared as instants, or as dates for all-day series);
/// overrides whose `RECURRENCE-ID` matches no instance (orphans left behind
/// by other clients) are ignored, unless the series' `RRULE` could not be
/// parsed at all. `EXDATE` wins over overrides. Non-recurring events yield
/// at most one occurrence.
pub fn expand(
    master: &Event,
    overrides: &[Event],
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    ctx: &TzContext,
    options: &ExpandOptions,
) -> Expansion {
    let tz = options.default_tz;
    let mut expansion = Expansion::default();
    if window_end <= window_start {
        return expansion;
    }

    if !master.is_recurring() {
        let start = ctx.to_utc(&master.start, tz);
        let end = ctx.to_utc(&master.end, tz).max(start);
        if overlaps(start, end, window_start, window_end) {
            expansion.occurrences.push(Occurrence {
                start,
                end,
                start_time: master.start.clone(),
                end_time: master.end.clone(),
                recurrence_id: master.recurrence_id.clone(),
                override_index: None,
            });
        }
        return expansion;
    }

    let all_day = master.start.is_date();
    let zone = ctx.zone_of(&master.start, tz);
    let dtstart = wall_clock(&master.start);
    let span = span_of(master, ctx, tz);

    // Overrides by recurrence id; the highest SEQUENCE wins among duplicates.
    let mut by_rid: HashMap<RidKey, usize> = HashMap::new();
    let mut latest_rid: Option<DateTime<Utc>> = None;
    let mut earliest_rid: Option<DateTime<Utc>> = None;
    for (index, ov) in overrides.iter().enumerate() {
        let Some(rid) = &ov.recurrence_id else {
            continue;
        };
        let rid_utc = ctx.to_utc(rid, tz);
        latest_rid = latest_rid.max(Some(rid_utc));
        earliest_rid = Some(earliest_rid.map_or(rid_utc, |e| e.min(rid_utc)));
        let key = rid_key(rid, all_day, ctx, tz);
        match by_rid.get(&key) {
            Some(&existing) if overrides[existing].sequence > ov.sequence => {}
            _ => {
                by_rid.insert(key, index);
            }
        }
    }

    // Generation horizon: the window, plus far enough to see every override.
    let margin = TimeDelta::days(2);
    let horizon = latest_rid
        .and_then(|r| r.checked_add_signed(TimeDelta::seconds(1)))
        .map_or(window_end, |r| r.max(window_end));
    let lookback = window_start
        .checked_sub_signed(span.approximate())
        .and_then(|t| t.checked_sub_signed(margin))
        .unwrap_or(window_start);
    let skip_target = earliest_rid
        .and_then(|r| r.checked_sub_signed(margin))
        .map_or(lookback, |r| r.min(lookback));

    let rule = master.rrule.as_deref().and_then(RecurRule::parse);
    let mut locals: Vec<NaiveDateTime> = vec![dtstart];
    if let Some(rule) = &rule {
        let until = UntilBound::new(rule, &master.start);
        let mut iter = RuleIter::new(rule, dtstart, PERIOD_BUDGET);
        if rule.count.is_none() {
            iter.skip_to(skip_target.date_naive());
        }
        // No instance can start after the horizon (in local time, with a
        // day of slack for the zone offset), nor after UNTIL.
        let mut stop = zone
            .to_local(horizon)
            .date()
            .succ_opt()
            .unwrap_or(NaiveDate::MAX);
        if let Some(until) = &rule.until {
            stop = stop.min(
                wall_clock(until)
                    .date()
                    .succ_opt()
                    .unwrap_or(NaiveDate::MAX),
            );
        }
        iter.stop_after(stop);
        let limit = rule.count.map_or(usize::MAX, |c| c as usize);
        // Each instance yields at most one occurrence and every override can
        // add at most one more, so more in-window instances are never needed.
        let enough = options
            .max_occurrences
            .saturating_add(overrides.len())
            .saturating_add(1);
        let mut produced = 1usize;
        let mut in_window = 0usize;
        if produced < limit {
            for instance in iter.by_ref() {
                if instance <= dtstart {
                    continue;
                }
                let instance_utc = zone.to_utc(instance);
                if until.excludes(instance, instance_utc) || instance_utc >= horizon {
                    break;
                }
                locals.push(instance);
                produced += 1;
                if produced >= limit {
                    break;
                }
                if instance_utc >= window_start {
                    in_window += 1;
                }
                // Past the cap, keep going only while overrides further out
                // still need to be validated.
                let saturated =
                    in_window >= enough && latest_rid.is_none_or(|rid| instance_utc > rid);
                if saturated || locals.len() >= MAX_GENERATED {
                    expansion.truncated = true;
                    break;
                }
            }
            if iter.exhausted() {
                expansion.truncated = true;
            }
        }
    }
    for rdate in &master.rdates {
        locals.push(rdate_local(rdate, &master.start, &zone, ctx, tz));
    }
    locals.sort_unstable();
    locals.dedup();

    let exdates: Vec<RidKey> = master
        .exdates
        .iter()
        .map(|ex| rid_key(ex, all_day, ctx, tz))
        .collect();

    let mut matched = vec![false; overrides.len()];
    for local in locals {
        let start_utc = zone.to_utc(local);
        let instant_key = if all_day {
            RidKey::Date(local.date())
        } else {
            RidKey::Instant(start_utc)
        };
        let date_key = RidKey::Date(local.date());
        if exdates.contains(&instant_key) || exdates.contains(&date_key) {
            continue;
        }
        let rid = with_wall_clock(local, &master.start);
        let override_index = by_rid
            .get(&instant_key)
            .or_else(|| by_rid.get(&date_key))
            .copied();
        if let Some(index) = override_index {
            matched[index] = true;
            let ov = &overrides[index];
            if ov.status == EventStatus::Cancelled {
                continue;
            }
            let start = ctx.to_utc(&ov.start, tz);
            let end = ctx.to_utc(&ov.end, tz).max(start);
            if overlaps(start, end, window_start, window_end) {
                expansion.occurrences.push(Occurrence {
                    start,
                    end,
                    start_time: ov.start.clone(),
                    end_time: ov.end.clone(),
                    recurrence_id: Some(rid),
                    override_index: Some(index),
                });
            }
            continue;
        }
        let (end_time, end_utc) = instance_end(&rid, start_utc, span, &master.end, ctx, tz);
        if overlaps(start_utc, end_utc, window_start, window_end) {
            expansion.occurrences.push(Occurrence {
                start: start_utc,
                end: end_utc,
                start_time: rid.clone(),
                end_time,
                recurrence_id: Some(rid),
                override_index: None,
            });
        }
    }

    // An unparseable RRULE leaves us unable to validate overrides: show them.
    if master.rrule.is_some() && rule.is_none() {
        for (index, ov) in overrides.iter().enumerate() {
            if matched[index] || ov.status == EventStatus::Cancelled {
                continue;
            }
            let Some(rid) = &ov.recurrence_id else {
                continue;
            };
            if exdates.contains(&rid_key(rid, all_day, ctx, tz)) {
                continue;
            }
            let start = ctx.to_utc(&ov.start, tz);
            let end = ctx.to_utc(&ov.end, tz).max(start);
            if overlaps(start, end, window_start, window_end) {
                expansion.occurrences.push(Occurrence {
                    start,
                    end,
                    start_time: ov.start.clone(),
                    end_time: ov.end.clone(),
                    recurrence_id: Some(rid.clone()),
                    override_index: Some(index),
                });
            }
        }
    }

    expansion
        .occurrences
        .sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    if expansion.occurrences.len() > options.max_occurrences {
        expansion.occurrences.truncate(options.max_occurrences);
        expansion.truncated = true;
    }
    expansion
}

/// Expands a set of events (for example every `VEVENT` of a calendar
/// resource), grouping series masters with their overrides by UID.
///
/// Overrides without a master in the set (an invitation to a single
/// instance) are treated as standalone occurrences. When several masters
/// share a UID, the one with the highest `SEQUENCE` wins.
pub fn expand_events(
    events: &[Event],
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    ctx: &TzContext,
    options: &ExpandOptions,
) -> EventsExpansion {
    let mut result = EventsExpansion::default();
    let mut order: Vec<&str> = Vec::new();
    let mut groups: HashMap<&str, (Option<usize>, Vec<usize>)> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        let entry = groups.entry(event.uid.as_str()).or_insert_with(|| {
            order.push(event.uid.as_str());
            (None, Vec::new())
        });
        if event.recurrence_id.is_some() {
            entry.1.push(index);
        } else {
            match entry.0 {
                Some(existing) if events[existing].sequence >= event.sequence => {}
                _ => entry.0 = Some(index),
            }
        }
    }
    for uid in order {
        let Some((master, override_indices)) = groups.get(uid) else {
            continue;
        };
        match master {
            Some(master_index) => {
                let overrides: Vec<Event> = override_indices
                    .iter()
                    .map(|&i| events[i].clone())
                    .collect();
                let expansion = expand(
                    &events[*master_index],
                    &overrides,
                    window_start,
                    window_end,
                    ctx,
                    options,
                );
                result.truncated |= expansion.truncated;
                for occurrence in expansion.occurrences {
                    let event_index = occurrence
                        .override_index
                        .and_then(|i| override_indices.get(i).copied())
                        .unwrap_or(*master_index);
                    result.occurrences.push(EventOccurrence {
                        event_index,
                        occurrence,
                    });
                }
            }
            None => {
                for &index in override_indices {
                    let event = &events[index];
                    if event.status == EventStatus::Cancelled {
                        continue;
                    }
                    let start = ctx.to_utc(&event.start, options.default_tz);
                    let end = ctx.to_utc(&event.end, options.default_tz).max(start);
                    if overlaps(start, end, window_start, window_end) {
                        result.occurrences.push(EventOccurrence {
                            event_index: index,
                            occurrence: Occurrence {
                                start,
                                end,
                                start_time: event.start.clone(),
                                end_time: event.end.clone(),
                                recurrence_id: event.recurrence_id.clone(),
                                override_index: None,
                            },
                        });
                    }
                }
            }
        }
    }
    result.occurrences.sort_by(|a, b| {
        a.occurrence
            .start
            .cmp(&b.occurrence.start)
            .then(a.occurrence.end.cmp(&b.occurrence.end))
            .then(a.event_index.cmp(&b.event_index))
    });
    if result.occurrences.len() > options.max_occurrences {
        result.occurrences.truncate(options.max_occurrences);
        result.truncated = true;
    }
    result
}
