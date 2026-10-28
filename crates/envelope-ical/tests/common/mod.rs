//! Shared helpers for the integration tests.
#![allow(dead_code)]

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};

/// Every iCalendar fixture with its name.
pub const ICS_FIXTURES: &[(&str, &[u8])] = &[
    (
        "google_export.ics",
        include_bytes!("../fixtures/google_export.ics"),
    ),
    (
        "outlook_invite.ics",
        include_bytes!("../fixtures/outlook_invite.ics"),
    ),
    (
        "outlook_display_tzid.ics",
        include_bytes!("../fixtures/outlook_display_tzid.ics"),
    ),
    (
        "icloud_event.ics",
        include_bytes!("../fixtures/icloud_event.ics"),
    ),
    (
        "thunderbird.ics",
        include_bytes!("../fixtures/thunderbird.ics"),
    ),
    (
        "teams_invite.ics",
        include_bytes!("../fixtures/teams_invite.ics"),
    ),
    (
        "zoom_invite.ics",
        include_bytes!("../fixtures/zoom_invite.ics"),
    ),
    (
        "recurring_dst_ny.ics",
        include_bytes!("../fixtures/recurring_dst_ny.ics"),
    ),
    (
        "recurring_berlin.ics",
        include_bytes!("../fixtures/recurring_berlin.ics"),
    ),
    ("istanbul.ics", include_bytes!("../fixtures/istanbul.ics")),
    ("custom_tz.ics", include_bytes!("../fixtures/custom_tz.ics")),
    (
        "allday_multiday.ics",
        include_bytes!("../fixtures/allday_multiday.ics"),
    ),
    (
        "multi_calendar.ics",
        include_bytes!("../fixtures/multi_calendar.ics"),
    ),
];

/// Every vCard fixture with its name.
pub const VCF_FIXTURES: &[(&str, &[u8])] = &[
    ("icloud.vcf", include_bytes!("../fixtures/icloud.vcf")),
    ("google.vcf", include_bytes!("../fixtures/google.vcf")),
    ("outlook21.vcf", include_bytes!("../fixtures/outlook21.vcf")),
    ("nextcloud.vcf", include_bytes!("../fixtures/nextcloud.vcf")),
];

/// A fixture as text (all fixtures are UTF-8).
pub fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("fixtures are UTF-8")
}

/// `"2024-03-01 16:00"` → naive date-time.
pub fn dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").expect("valid test datetime")
}

/// `"2024-03-01 16:00"` → UTC instant.
pub fn utc(s: &str) -> DateTime<Utc> {
    dt(s).and_utc()
}

/// `"2024-03-01"` → date.
pub fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid test date")
}

/// Formats instants compactly for assertions.
pub fn fmt(at: &DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}
