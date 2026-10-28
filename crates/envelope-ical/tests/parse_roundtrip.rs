//! Parser / serializer round trips over every fixture.

mod common;

use common::{ICS_FIXTURES, text};
use envelope_ical::{
    Component, parse_calendar, parse_components, parse_components_bytes, write_components,
};

fn assert_well_formed(output: &str) {
    assert!(output.ends_with("\r\n"), "output must end with CRLF");
    for line in output.split("\r\n") {
        assert!(
            !line.contains('\n') && !line.contains('\r'),
            "bare line break in {line:?}"
        );
        assert!(
            line.len() <= 75,
            "line longer than 75 octets: {} {line:?}",
            line.len()
        );
    }
}

fn find<'a>(component: &'a Component, name: &str) -> Vec<&'a Component> {
    let mut found = Vec::new();
    if component.is(name) {
        found.push(component);
    }
    for child in &component.components {
        found.extend(find(child, name));
    }
    found
}

#[test]
fn every_fixture_round_trips_losslessly() {
    for (name, bytes) in ICS_FIXTURES {
        let parsed = parse_components_bytes(bytes);
        assert!(!parsed.is_empty(), "{name}: nothing parsed");
        let written = write_components(&parsed);
        assert_well_formed(&written);
        let reparsed = parse_components(&written);
        assert_eq!(parsed, reparsed, "{name}: tree changed after a round trip");
        // Writing is idempotent.
        assert_eq!(
            write_components(&reparsed),
            written,
            "{name}: output not stable"
        );
        // Text and byte entry points agree.
        assert_eq!(
            parse_components(text(bytes)),
            parsed,
            "{name}: str/bytes mismatch"
        );
        // Events survive the round trip unchanged.
        let before = parse_calendar(text(bytes)).expect("calendar").events;
        let after = parse_calendar(&written).expect("calendar").events;
        assert_eq!(before, after, "{name}: events changed after a round trip");
    }
}

#[test]
fn unknown_properties_parameters_and_components_survive() {
    let bytes = include_bytes!("fixtures/icloud_event.ics");
    let written = write_components(&parse_components_bytes(bytes));
    let reparsed = parse_components(&written);
    let vevent = find(&reparsed[0], "VEVENT")[0];
    let location = vevent
        .property("X-APPLE-STRUCTURED-LOCATION")
        .expect("vendor property kept");
    assert_eq!(
        location.param("X-ADDRESS"),
        Some("Kemankeş Karamustafa Paşa, Kemankeş Cd. No:37/A, 34425 Beyoğlu/İstanbul, Türkiye")
    );
    assert_eq!(location.param("X-TITLE"), Some("Karaköy Lokantası"));
    assert_eq!(location.value, "geo:41.024830,28.978690");
    let alarms = find(vevent, "VALARM");
    assert_eq!(alarms.len(), 2);
    assert_eq!(alarms[1].value("X-APPLE-DEFAULT-ALARM"), Some("TRUE"));

    let bytes = include_bytes!("fixtures/multi_calendar.ics");
    let written = write_components(&parse_components_bytes(bytes));
    assert!(written.contains("X-CUSTOM-PROP;X-PARAM=\"a,b\";X-OTHER=c:Unknown value\\, kept\r\n"));
    assert!(written.contains("BEGIN:X-VENDOR-WIDGET\r\nX-WIDGET-ID:42\r\nEND:X-VENDOR-WIDGET\r\n"));

    let bytes = include_bytes!("fixtures/outlook_invite.ics");
    let written = write_components(&parse_components_bytes(bytes));
    for needle in [
        "X-MICROSOFT-CDO-OWNERAPPTID:2122391234",
        "X-MICROSOFT-SKYPETEAMSPROPERTIES:",
        "SUMMARY;LANGUAGE=de-DE:Quartalsplanung Q2",
        "PRIORITY:5",
    ] {
        assert!(
            written.replace("\r\n ", "").contains(needle),
            "lost {needle}"
        );
    }
}

#[test]
fn multiple_calendars_lowercase_bom_and_missing_end() {
    let bytes = include_bytes!("fixtures/multi_calendar.ics");
    assert_eq!(&bytes[..3], b"\xEF\xBB\xBF", "fixture starts with a BOM");
    let components = parse_components_bytes(bytes);
    assert_eq!(components.len(), 2);
    assert!(components.iter().all(|c| c.name == "VCALENDAR"));
    let data = parse_calendar(text(bytes)).expect("calendar");
    let uids: Vec<&str> = data.events.iter().map(|e| e.uid.as_str()).collect();
    assert_eq!(
        uids,
        [
            "lower-1@example.com",
            "second-1@example.com",
            "second-2@example.com"
        ]
    );
    // Lower-case TZIDs resolve too.
    assert!(matches!(
        &data.events[0].start,
        envelope_ical::EventTime::Zoned { tzid, .. } if tzid == "Europe/Istanbul"
    ));
}

#[test]
fn folded_input_variants_unfold_identically() {
    // The Thunderbird fixture is LF-terminated and folded with tabs.
    let bytes = include_bytes!("fixtures/thunderbird.ics");
    assert!(!bytes.windows(2).any(|w| w == b"\r\n"));
    let data = parse_calendar(text(bytes)).expect("calendar");
    assert_eq!(data.events.len(), 2);
    assert_eq!(data.events[0].categories, ["Persönlich", "Gesundheit"]);
    // The Google fixture folds long descriptions at 75 octets with CRLF.
    let google =
        parse_calendar(text(include_bytes!("fixtures/google_export.ics"))).expect("calendar");
    let description = google.events[0]
        .description
        .as_deref()
        .expect("description");
    assert!(description.starts_with("Weekly sync.\n\n-::~:~::~"));
    assert!(description.contains("Join with Google Meet: https://meet.google.com/abc-mnop-xyz\n"));
}
