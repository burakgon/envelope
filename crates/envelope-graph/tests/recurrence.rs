//! RRULE <-> patternedRecurrence conversions over the recurrence patterns of
//! the Graph "schedule repeating appointments" guide.

mod common;

use envelope_graph::recurrence::{from_rrule, to_rrule};
use envelope_graph::{PatternedRecurrence, RecurrencePatternType, resolve_iana_time_zone};
use envelope_ical::EventTime;
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    start: EventTime,
    recurrence: PatternedRecurrence,
    rrule: String,
}

fn cases() -> Vec<Case> {
    serde_json::from_value(common::static_fixture("recurrence_patterns.json")).expect("cases")
}

/// The zone used for UNTIL conversion, as the client resolves it.
fn zone(case: &Case) -> Option<chrono_tz::Tz> {
    case.recurrence
        .range
        .recurrence_time_zone
        .as_deref()
        .and_then(resolve_iana_time_zone)
        .or_else(|| match &case.start {
            EventTime::Zoned { tzid, .. } => resolve_iana_time_zone(tzid),
            EventTime::Utc { .. } => Some(chrono_tz::Tz::UTC),
            _ => None,
        })
}

#[test]
fn graph_patterns_become_the_expected_rrules() {
    for case in cases() {
        let rule = to_rrule(&case.recurrence, &case.start, zone(&case))
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        assert_eq!(rule, case.rrule, "{}", case.name);
    }
}

#[test]
fn rrules_round_trip_through_graph() {
    for case in cases() {
        let tz = zone(&case);
        let graph = from_rrule(&case.rrule, &case.start, tz)
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let original = &case.recurrence;
        assert_eq!(graph.pattern.kind, original.pattern.kind, "{}", case.name);
        assert_eq!(
            graph.pattern.interval, original.pattern.interval,
            "{}",
            case.name
        );
        assert_eq!(
            graph.pattern.days_of_week, original.pattern.days_of_week,
            "{}",
            case.name
        );
        assert_eq!(graph.range.kind, original.range.kind, "{}", case.name);
        assert_eq!(
            graph.range.start_date, original.range.start_date,
            "{}",
            case.name
        );
        assert_eq!(
            graph.range.end_date, original.range.end_date,
            "{}",
            case.name
        );
        assert_eq!(
            graph.range.number_of_occurrences, original.range.number_of_occurrences,
            "{}",
            case.name
        );
        match original.pattern.kind {
            RecurrencePatternType::AbsoluteMonthly | RecurrencePatternType::AbsoluteYearly => {
                assert_eq!(
                    graph.pattern.day_of_month, original.pattern.day_of_month,
                    "{}",
                    case.name
                );
                assert_eq!(graph.pattern.month, original.pattern.month, "{}", case.name);
            }
            RecurrencePatternType::RelativeMonthly | RecurrencePatternType::RelativeYearly => {
                assert_eq!(graph.pattern.index, original.pattern.index, "{}", case.name);
                assert_eq!(graph.pattern.month, original.pattern.month, "{}", case.name);
            }
            RecurrencePatternType::Weekly | RecurrencePatternType::Daily => {}
        }
        // And back again: the rule is stable.
        let again =
            to_rrule(&graph, &case.start, tz).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        assert_eq!(again, case.rrule, "{}", case.name);
    }
}

#[test]
fn graph_accepts_the_generated_json() {
    // Every converted recurrence serializes with only the properties its
    // pattern type uses (Graph validates even ignored properties).
    for case in cases() {
        let graph = from_rrule(&case.rrule, &case.start, zone(&case)).expect("convert");
        let json = serde_json::to_value(&graph).expect("serialize");
        let pattern = &json["pattern"];
        match graph.pattern.kind {
            RecurrencePatternType::Daily => {
                assert!(pattern.get("daysOfWeek").is_none() && pattern.get("index").is_none());
            }
            RecurrencePatternType::Weekly => {
                assert!(pattern["firstDayOfWeek"].is_string(), "{}", case.name);
                assert!(pattern.get("dayOfMonth").is_none());
            }
            RecurrencePatternType::AbsoluteMonthly | RecurrencePatternType::AbsoluteYearly => {
                assert!(pattern["dayOfMonth"].is_number());
                assert!(pattern.get("index").is_none());
            }
            RecurrencePatternType::RelativeMonthly | RecurrencePatternType::RelativeYearly => {
                assert!(pattern["index"].is_string());
                assert!(pattern.get("dayOfMonth").is_none());
            }
        }
        assert!(json["range"]["startDate"].is_string());
    }
}
