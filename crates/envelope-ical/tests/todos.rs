//! Tasks as Apple Reminders, Nextcloud Tasks and Thunderbird store them:
//! reading, editing in place and moving repeating tasks on.

mod common;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use common::{date, dt, text, utc};
use envelope_ical::{
    Component, EventTime, Todo, TodoStatus, parse_components, parse_todos, upsert_todo_at,
};

const FIXTURES: &[(&str, &[u8])] = &[
    (
        "apple_reminder.ics",
        include_bytes!("fixtures/apple_reminder.ics"),
    ),
    (
        "nextcloud_task.ics",
        include_bytes!("fixtures/nextcloud_task.ics"),
    ),
    (
        "thunderbird_task.ics",
        include_bytes!("fixtures/thunderbird_task.ics"),
    ),
];

fn now() -> DateTime<Utc> {
    utc("2026-01-02 03:04")
}

fn fixture(name: &str) -> &'static str {
    let (_, bytes) = FIXTURES.iter().find(|(n, _)| *n == name).expect("fixture");
    text(bytes)
}

fn task(name: &str) -> Todo {
    let mut todos = parse_todos(fixture(name)).expect("calendar");
    assert_eq!(todos.len(), 1, "{name}");
    todos.remove(0)
}

fn vtodo(text: &str) -> Component {
    parse_components(text)
        .into_iter()
        .flat_map(|c| c.components)
        .find(|c| c.is("VTODO"))
        .expect("VTODO")
}

/// Properties of a component except the ones every edit refreshes.
fn stable_properties(component: &Component) -> Vec<String> {
    component
        .properties
        .iter()
        .filter(|p| !matches!(p.name.as_str(), "DTSTAMP" | "LAST-MODIFIED"))
        .map(|p| p.to_content_line())
        .collect()
}

fn zoned(local: &str, tzid: &str) -> EventTime {
    EventTime::Zoned {
        local: dt(local),
        tzid: tzid.to_owned(),
    }
}

#[test]
fn reads_tasks_of_common_clients() {
    let apple = task("apple_reminder.ics");
    assert_eq!(apple.summary, "Call the bank about the mortgage");
    assert_eq!(
        apple.due,
        Some(zoned("2024-03-06 15:00", "Europe/Istanbul"))
    );
    assert_eq!(apple.start, apple.due);
    assert_eq!(apple.priority, 1);
    assert_eq!(apple.sort_order, Some(731_052_900));
    assert_eq!(apple.status, Some(TodoStatus::NeedsAction));
    assert!(!apple.is_done());

    let nextcloud = task("nextcloud_task.ics");
    assert_eq!(
        nextcloud.description.as_deref(),
        Some("The barista one, not the light one.")
    );
    assert_eq!(
        nextcloud.due,
        Some(EventTime::Date {
            date: date("2024-02-12")
        })
    );
    assert!(nextcloud.is_done());
    assert_eq!(nextcloud.completed, Some(utc("2024-02-11 08:00")));
    assert_eq!(
        nextcloud.parent.as_deref(),
        Some("7a1e3e7b-1b55-4c49-9c3b-1d2f0a3e6c11")
    );
    assert_eq!(nextcloud.priority, 5);
    assert_eq!(nextcloud.sort_order, None);

    let thunderbird = task("thunderbird_task.ics");
    assert_eq!(
        thunderbird.rrule.as_deref(),
        Some("FREQ=WEEKLY;COUNT=10;BYDAY=FR")
    );
    assert_eq!(
        thunderbird.start,
        Some(zoned("2024-03-29 09:00", "Europe/Berlin"))
    );
    assert_eq!(
        thunderbird.due,
        Some(zoned("2024-03-29 17:00", "Europe/Berlin"))
    );
    assert_eq!(thunderbird.percent_complete, Some(0));
    assert_eq!(thunderbird.status, None);
}

#[test]
fn unchanged_upsert_only_refreshes_timestamps() {
    for (name, _) in FIXTURES {
        let original = fixture(name);
        let todo = task(name);
        let updated = upsert_todo_at(original, &todo, now()).expect("upsert");
        let (before, after) = (vtodo(original), vtodo(&updated));
        assert_eq!(
            stable_properties(&before),
            stable_properties(&after),
            "{name}"
        );
        assert_eq!(
            before.components, after.components,
            "{name}: alarms changed"
        );
        assert_eq!(after.value("LAST-MODIFIED"), Some("20260102T030400Z"));
        assert_eq!(
            original.matches("BEGIN:VTIMEZONE").count(),
            updated.matches("BEGIN:VTIMEZONE").count(),
            "{name}"
        );
    }
}

#[test]
fn completing_writes_status_and_keeps_the_rest() {
    let original = fixture("apple_reminder.ics");
    let mut todo = task("apple_reminder.ics");
    todo.complete(now());
    let done = upsert_todo_at(original, &todo, now()).expect("upsert");
    let component = vtodo(&done);
    assert_eq!(component.value("STATUS"), Some("COMPLETED"));
    assert_eq!(component.value("COMPLETED"), Some("20260102T030400Z"));
    assert_eq!(component.value("PERCENT-COMPLETE"), Some("100"));
    assert_eq!(component.value("SEQUENCE"), Some("2"));
    assert_eq!(component.value("X-APPLE-SORT-ORDER"), Some("731052900"));
    assert_eq!(component.components, vtodo(original).components);

    let mut reopened = parse_todos(&done).expect("tasks").remove(0);
    reopened.reopen();
    let open = vtodo(&upsert_todo_at(&done, &reopened, now()).expect("upsert"));
    assert_eq!(open.value("STATUS"), Some("NEEDS-ACTION"));
    assert_eq!(open.value("COMPLETED"), None);
    assert_eq!(open.value("PERCENT-COMPLETE"), None);

    // Importance and order are cleared by removing the properties.
    let mut plain = task("apple_reminder.ics");
    plain.priority = 0;
    plain.sort_order = None;
    let plain = vtodo(&upsert_todo_at(original, &plain, now()).expect("upsert"));
    assert_eq!(plain.value("PRIORITY"), None);
    assert_eq!(plain.value("X-APPLE-SORT-ORDER"), None);
}

#[test]
fn repeating_task_moves_on_with_its_alarm() {
    let original = fixture("thunderbird_task.ics");
    let next = task("thunderbird_task.ics")
        .next_occurrence(Tz::Europe__Istanbul)
        .expect("next occurrence");
    let updated = upsert_todo_at(original, &next, now()).expect("upsert");
    let component = vtodo(&updated);
    assert_eq!(
        component.value("DTSTART"),
        Some("20240405T090000"),
        "the wall-clock time stays across the change to summer time"
    );
    assert_eq!(component.value("DUE"), Some("20240405T170000"));
    assert_eq!(
        component.value("RRULE"),
        Some("FREQ=WEEKLY;COUNT=9;BYDAY=FR")
    );
    assert_eq!(component.value("X-MOZ-GENERATION"), Some("3"));
    assert_eq!(component.value("STATUS"), None, "no status is added");
    assert_eq!(component.value("PERCENT-COMPLETE"), Some("0"));
    assert_eq!(component.components, vtodo(original).components);
    assert!(updated.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n"));

    // Moving it by hand to the following Friday uses up one more occurrence.
    let moved = parse_todos(&updated)
        .expect("tasks")
        .remove(0)
        .reschedule(Some(zoned("2024-04-12 17:00", "Europe/Berlin")), Tz::UTC);
    assert_eq!(
        moved.start,
        Some(zoned("2024-04-12 09:00", "Europe/Berlin"))
    );
    assert_eq!(moved.rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=8;BYDAY=FR"));
}
