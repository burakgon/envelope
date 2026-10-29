//! Exchange Web Services sync: mapping functions, and folder, message,
//! operation and calendar sync against a fake EWS endpoint with a real
//! (temporary) database.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::{NaiveDate, TimeZone, Utc};
use envelope_ical::{
    AttendeeRole, CalendarUserType, EventStatus, EventTime, PartStat, Transparency,
};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, header_regex, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::account::{
    Credentials, Found, account_config, connect, connect_with, looks_like_exchange, probe_url,
    verify,
};
use super::calendar as ews_calendar;
use super::mapping::{self, EventContext};
use super::*;
use crate::calendar::{EditScope, EventInput};
use crate::db::calendar as cdb;
use crate::engine::{EventSink, Paths};

const EWS_PATH: &str = "/EWS/Exchange.asmx";
const NS: &str = concat!(
    r#"xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" "#,
    r#"xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types""#
);

// ---------------------------------------------------------------------------
// Mapping

fn ews_message(fields: Value) -> ews::Message {
    let mut value = json!({
        "id": { "id": "AAMkAD-1", "changeKey": "CQAA" },
        "kind": "Message",
        "to": [], "cc": [], "bcc": [], "replyTo": [], "categories": [], "attachments": []
    });
    if let (Some(base), Some(extra)) = (value.as_object_mut(), fields.as_object()) {
        base.extend(extra.clone());
    }
    serde_json::from_value(value).unwrap_or_else(|e| panic!("message fixture: {e}"))
}

fn person(name: &str, email: &str) -> Value {
    json!({ "name": name, "email": email, "routingType": "SMTP" })
}

#[test]
fn maps_message_headers() {
    let m = ews_message(json!({
        "itemClass": "IPM.Note",
        "subject": "  Quarterly plan ",
        "from": person("Dana Swope", "dana@example.com"),
        "sender": person("Assistant", "assistant@example.com"),
        "to": [person("Jane Doe", "jane@example.com"),
               { "name": "Legacy", "email": "/O=EXAMPLE/OU=EXCHANGE/CN=RECIPIENTS/CN=LEGACY", "routingType": "EX" }],
        "cc": [person("Team", "team@example.com")],
        "replyTo": [person("", "replies@example.com")],
        "received": "2026-10-01T09:00:00Z",
        "sent": "2026-10-01T08:59:00Z",
        "isRead": false,
        "isDraft": false,
        "flag": { "status": "Flagged", "startDate": null, "dueDate": null, "completeDate": null },
        "categories": ["Red category"],
        "internetMessageId": "<plan@example.com>",
        "inReplyTo": "<kickoff@example.com>",
        "references": "<root@example.com> <kickoff@example.com>",
        "conversationId": "AAQkAD-conv",
        "hasAttachments": true,
        "size": 4096,
        "preview": "Here is the plan.\r\n\r\nOn Monday, Dana wrote:\r\n> old"
    }));
    let meta = mapping::message_meta(&m);
    assert_eq!(meta.remote_id.as_deref(), Some("AAMkAD-1"));
    assert_eq!(meta.remote_thread_id.as_deref(), Some("AAQkAD-conv"));
    assert_eq!(meta.message_id.as_deref(), Some("plan@example.com"));
    assert_eq!(meta.in_reply_to.as_deref(), Some("kickoff@example.com"));
    assert_eq!(
        meta.references,
        vec!["root@example.com", "kickoff@example.com"]
    );
    assert_eq!(meta.subject, "Quarterly plan");
    let from = meta.from.expect("from");
    assert_eq!(
        (from.name.as_deref(), from.email.as_str()),
        (Some("Dana Swope"), "dana@example.com")
    );
    assert_eq!(
        meta.to.len(),
        1,
        "addresses without SMTP address are skipped"
    );
    assert_eq!(meta.cc[0].email, "team@example.com");
    assert_eq!(meta.reply_to[0].name, None);
    assert_eq!(
        Some(meta.date),
        Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0)
            .single()
            .map(|d| d.timestamp_millis())
    );
    assert_eq!(meta.flags, Flags::FLAGGED);
    assert!(meta.has_attachments);
    assert_eq!(meta.size, 4096);
    assert_eq!(meta.snippet.as_deref(), Some("Here is the plan."));
    assert_eq!(meta.calendar_method, None);
    assert!(!meta.is_auto);

    let draft = ews_message(json!({
        "isRead": true, "isDraft": true,
        "sender": person("Jane Doe", "jane@example.com"),
        "created": "2026-10-02T08:00:00Z",
        "references": "plain-id"
    }));
    let meta = mapping::message_meta(&draft);
    assert_eq!(meta.flags, Flags::SEEN | Flags::DRAFT);
    assert_eq!(
        meta.from.map(|a| a.email).as_deref(),
        Some("jane@example.com")
    );
    assert_eq!(meta.references, vec!["plain-id"]);
    assert_eq!(meta.remote_thread_id, None);
}

#[test]
fn recognizes_meeting_messages_and_reports() {
    let class = |c: &str| mapping::calendar_method(Some(c));
    assert_eq!(
        class("IPM.Schedule.Meeting.Request").as_deref(),
        Some("REQUEST")
    );
    assert_eq!(
        class("IPM.Schedule.Meeting.Canceled").as_deref(),
        Some("CANCEL")
    );
    assert_eq!(
        class("IPM.Schedule.Meeting.Resp.Pos").as_deref(),
        Some("REPLY")
    );
    assert_eq!(class("IPM.Note"), None);
    let report = ews_message(json!({ "itemClass": "REPORT.IPM.Note.NDR" }));
    assert!(mapping::message_meta(&report).is_auto);
    let request = ews_message(
        json!({ "kind": "MeetingRequest", "itemClass": "IPM.Schedule.Meeting.Request" }),
    );
    assert_eq!(
        mapping::message_meta(&request).calendar_method.as_deref(),
        Some("REQUEST")
    );
    assert!(mapping::is_mail_kind(ews::ItemKind::MeetingRequest));
    assert!(mapping::is_mail_kind(ews::ItemKind::Item));
    assert!(!mapping::is_mail_kind(ews::ItemKind::Contact));
    assert!(!mapping::is_mail_kind(ews::ItemKind::CalendarItem));
}

fn ews_folder(
    id: &str,
    name: &str,
    parent: &str,
    kind: &str,
    class: &str,
    role: Option<&str>,
) -> ews::Folder {
    serde_json::from_value(json!({
        "id": { "id": id }, "parentId": { "id": parent }, "displayName": name,
        "folderClass": class, "kind": kind, "isHidden": false, "role": role
    }))
    .unwrap_or_else(|e| panic!("folder fixture: {e}"))
}

#[test]
fn maps_the_folder_tree_and_categories() {
    let mut hidden = ews_folder(
        "cas",
        "Conversation Action Settings",
        "root",
        "Folder",
        "IPF.Configuration",
        None,
    );
    hidden.is_hidden = true;
    let tree = vec![
        ews_folder(
            "inbox",
            "Inbox",
            "root",
            "Folder",
            "IPF.Note",
            Some("Inbox"),
        ),
        ews_folder(
            "sent",
            "Sent Items",
            "root",
            "Folder",
            "IPF.Note",
            Some("SentItems"),
        ),
        ews_folder(
            "deleted",
            "Deleted Items",
            "root",
            "Folder",
            "IPF.Note",
            Some("DeletedItems"),
        ),
        ews_folder(
            "junk",
            "Junk Email",
            "root",
            "Folder",
            "IPF.Note",
            Some("JunkEmail"),
        ),
        ews_folder(
            "drafts",
            "Drafts",
            "root",
            "Folder",
            "IPF.Note",
            Some("Drafts"),
        ),
        ews_folder(
            "outbox",
            "Outbox",
            "root",
            "Folder",
            "IPF.Note",
            Some("Outbox"),
        ),
        ews_folder(
            "archive",
            "Archive",
            "root",
            "Folder",
            "IPF.Note",
            Some("Archive"),
        ),
        ews_folder(
            "projects",
            " Projects ",
            "inbox",
            "Folder",
            "IPF.Note",
            None,
        ),
        ews_folder(
            "alpha",
            "Alpha",
            "projects",
            "Folder",
            "IPF.Note.Custom",
            None,
        ),
        ews_folder(
            "calendar",
            "Calendar",
            "root",
            "Calendar",
            "IPF.Appointment",
            Some("Calendar"),
        ),
        ews_folder(
            "contacts",
            "Contacts",
            "root",
            "Contacts",
            "IPF.Contact",
            Some("Contacts"),
        ),
        ews_folder("search", "Unread", "root", "Search", "IPF.Note", None),
        hidden,
    ];
    let categories = vec![
        ("Red category".to_string(), Some("#e74856".to_string())),
        ("red CATEGORY".to_string(), None),
        ("Plain".to_string(), None),
    ];
    let upserts = mapping::folder_upserts(&tree, &categories);
    let by_id = |id: &str| upserts.iter().find(|u| u.remote_id == id);
    assert_eq!(by_id("inbox").and_then(|u| u.role), Some(FolderRole::Inbox));
    assert_eq!(by_id("sent").and_then(|u| u.role), Some(FolderRole::Sent));
    assert_eq!(
        by_id("deleted").and_then(|u| u.role),
        Some(FolderRole::Trash)
    );
    assert_eq!(by_id("junk").and_then(|u| u.role), Some(FolderRole::Junk));
    assert_eq!(
        by_id("drafts").and_then(|u| u.role),
        Some(FolderRole::Drafts)
    );
    assert_eq!(
        by_id("archive").and_then(|u| u.role),
        Some(FolderRole::Archive)
    );
    for gone in ["outbox", "calendar", "contacts", "search", "cas"] {
        assert!(by_id(gone).is_none(), "{gone} is not a synced mail folder");
    }
    let alpha = by_id("alpha").expect("alpha");
    assert_eq!(alpha.path, "Inbox/Projects/Alpha");
    assert_eq!(alpha.parent_remote_id.as_deref(), Some("projects"));
    assert_eq!(by_id("projects").map(|u| u.name.as_str()), Some("Projects"));
    assert_eq!(
        by_id("inbox").and_then(|u| u.parent_remote_id.clone()),
        None
    );
    let red = by_id("category:Red category").expect("label");
    assert_eq!(red.kind, "label");
    assert!(!red.selectable);
    assert_eq!(red.color.as_deref(), Some("#e74856"));
    assert!(
        by_id("category:red CATEGORY").is_none(),
        "names are case-insensitive"
    );
    assert_eq!(by_id("category:Plain").and_then(|u| u.color.clone()), None);
}

fn category_list_response(xml: &str) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(xml);
    response(
        "GetUserConfiguration",
        &[success(
            "GetUserConfiguration",
            &format!(
                r#"<m:UserConfiguration><t:UserConfigurationName Name="CategoryList"><t:DistinguishedFolderId Id="calendar"/></t:UserConfigurationName><t:XmlData>{data}</t:XmlData></m:UserConfiguration>"#
            ),
        )],
    )
}

#[test]
fn reads_the_master_category_list() {
    let xml = "\u{feff}<?xml version=\"1.0\"?><categories default=\"Red category\" xmlns=\"CategoryList.xsd\">\
        <category name=\"Red category\" color=\"0\" keyboardShortcut=\"0\"/>\
        <category name=\"Blue &amp; green\" color=\"7\"/>\
        <category name=\"Colorless\" color=\"-1\"/></categories>";
    let list = mapping::category_list(&category_list_response(xml)).expect("categories");
    assert_eq!(
        list,
        vec![
            ("Red category".to_string(), Some("#e74856".to_string())),
            ("Blue & green".to_string(), Some("#3a96dd".to_string())),
            ("Colorless".to_string(), None),
        ]
    );
    let missing = response(
        "GetUserConfiguration",
        &[failure("GetUserConfiguration", "ErrorItemNotFound")],
    );
    assert_eq!(mapping::category_list(&missing), None);
    assert_eq!(mapping::category_color(24).as_deref(), Some("#6e0811"));
    assert_eq!(mapping::category_color(25), None);
}

fn calendar_item(fields: Value) -> ews::CalendarItem {
    let mut value = json!({
        "id": { "id": "AAMkAD-cal", "changeKey": "DwAA" },
        "requiredAttendees": [], "optionalAttendees": [], "resources": [],
        "modifiedOccurrences": [], "deletedOccurrences": [], "categories": []
    });
    if let (Some(base), Some(extra)) = (value.as_object_mut(), fields.as_object()) {
        base.extend(extra.clone());
    }
    serde_json::from_value(value).unwrap_or_else(|e| panic!("calendar fixture: {e}"))
}

fn attendee(name: &str, email: &str, response: &str) -> Value {
    json!({ "mailbox": person(name, email), "response": response })
}

#[test]
fn maps_calendar_items() {
    let berlin: chrono_tz::Tz = "Europe/Berlin".parse().expect("zone");
    let item = calendar_item(json!({
        "uid": "series-1",
        "itemType": "Occurrence",
        "recurrenceId": "2026-07-13T08:00:00Z",
        "subject": "Weekly sync",
        "body": { "bodyType": "Text", "content": "Agenda: https://teams.microsoft.com/l/meetup-join/abc\n" },
        "start": "2026-07-13T08:00:00Z",
        "end": "2026-07-13T08:30:00Z",
        "isAllDay": false,
        "startTimeZone": "W. Europe Standard Time",
        "location": " Room 1 ",
        "organizer": person("Bob Organizer", "Bob@example.com"),
        "requiredAttendees": [attendee("Jane Doe", "jane@example.com", "Unknown"), attendee("Alice", "alice@example.com", "Accept")],
        "optionalAttendees": [attendee("Carol", "carol@example.com", "Tentative")],
        "resources": [attendee("Room 1", "room1@example.com", "Accept")],
        "myResponseType": "Accept",
        "freeBusy": "Free",
        "isMeeting": true,
        "isCancelled": false,
        "sensitivity": "Private",
        "reminderIsSet": true,
        "reminderMinutesBeforeStart": 10,
        "sequence": 3,
        "categories": ["Blue category"]
    }));
    let cx = EventContext {
        me: "jane@example.com",
        default_tz: chrono_tz::Tz::UTC,
        series_rule: Some("FREQ=WEEKLY;BYDAY=MO"),
    };
    let event = mapping::calendar_event(&item, &cx);
    assert_eq!(event.uid, "series-1");
    assert_eq!(
        event.recurrence_id,
        Some(EventTime::Utc {
            at: Utc
                .with_ymd_and_hms(2026, 7, 13, 8, 0, 0)
                .single()
                .expect("time")
        })
    );
    assert_eq!(
        event.start,
        EventTime::Zoned {
            local: NaiveDate::from_ymd_opt(2026, 7, 13)
                .and_then(|d| d.and_hms_opt(10, 0, 0))
                .expect("time"),
            tzid: berlin.name().to_string(),
        }
    );
    assert!(
        !event.is_recurring(),
        "an occurrence is not a series master"
    );
    assert_eq!(event.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
    assert_eq!(event.location.as_deref(), Some("Room 1"));
    assert_eq!(
        event.conference_url.as_deref(),
        Some("https://teams.microsoft.com/l/meetup-join/abc")
    );
    assert_eq!(
        event.organizer.as_ref().map(|o| o.email.as_str()),
        Some("bob@example.com")
    );
    let me = event
        .attendees
        .iter()
        .find(|a| a.email == "jane@example.com")
        .expect("me");
    assert_eq!(me.partstat, PartStat::Accepted, "MyResponseType wins");
    let carol = event
        .attendees
        .iter()
        .find(|a| a.email == "carol@example.com")
        .expect("carol");
    assert_eq!(
        (carol.role, carol.partstat),
        (AttendeeRole::Optional, PartStat::Tentative)
    );
    let room = event
        .attendees
        .iter()
        .find(|a| a.email == "room1@example.com")
        .expect("room");
    assert_eq!(room.cutype, CalendarUserType::Resource);
    assert!(!room.rsvp);
    assert_eq!(event.transparency, Transparency::Transparent);
    assert_eq!(event.class, envelope_ical::EventClass::Private);
    assert_eq!(event.alarms.first().map(|a| a.minutes_before), Some(10));
    assert_eq!(event.sequence, 3);
    assert_eq!(event.categories, vec!["Blue category"]);

    // All-day items span midnights of their zone.
    let holiday = calendar_item(json!({
        "uid": "holiday",
        "itemType": "Single",
        "subject": "Holiday",
        "start": "2026-07-03T22:00:00Z",
        "end": "2026-07-04T22:00:00Z",
        "isAllDay": true,
        "startTimeZone": "W. Europe Standard Time",
        "myResponseType": "Organizer",
        "isCancelled": true
    }));
    let event = mapping::calendar_event(&holiday, &cx);
    assert_eq!(
        event.start,
        EventTime::Date {
            date: NaiveDate::from_ymd_opt(2026, 7, 4).expect("date")
        }
    );
    assert_eq!(
        event.end,
        EventTime::Date {
            date: NaiveDate::from_ymd_opt(2026, 7, 5).expect("date")
        }
    );
    assert_eq!(event.recurrence_id, None);
    assert_eq!(event.rrule, None, "only occurrences carry the series rule");
    assert_eq!(event.status, EventStatus::Cancelled);
    assert!(
        event.attendees.is_empty(),
        "an organizer is not their own attendee"
    );

    // Without a known zone, items stay in UTC; without UID, the id stands in.
    let bare =
        calendar_item(json!({ "start": "2026-07-03T09:00:00Z", "end": "2026-07-03T08:00:00Z" }));
    let event = mapping::calendar_event(&bare, &cx);
    assert_eq!(event.uid, "AAMkAD-cal");
    assert!(matches!(event.start, EventTime::Utc { .. }));
    assert_eq!(event.end, event.start, "an end before the start is clamped");
}

#[test]
fn builds_calendar_items_from_events() {
    let berlin: chrono_tz::Tz = "Europe/Berlin".parse().expect("zone");
    let local = |h: u32| {
        NaiveDate::from_ymd_opt(2026, 11, 2)
            .and_then(|d| d.and_hms_opt(h, 0, 0))
            .expect("time")
    };
    let mut event = envelope_ical::Event::new(
        "uid-1",
        EventTime::Zoned {
            local: local(10),
            tzid: "Europe/Berlin".into(),
        },
        EventTime::Zoned {
            local: local(11),
            tzid: "Europe/Berlin".into(),
        },
    );
    event.summary = "Planning".into();
    event.description = Some("Bring numbers".into());
    event.rrule = Some("FREQ=WEEKLY;BYDAY=MO".into());
    event.organizer = Some(envelope_ical::Organizer {
        email: "jane@example.com".into(),
        name: None,
    });
    let person = |email: &str, role, cutype| envelope_ical::Attendee {
        email: email.into(),
        role,
        cutype,
        ..Default::default()
    };
    event.attendees = vec![
        person(
            "jane@example.com",
            AttendeeRole::Chair,
            CalendarUserType::Individual,
        ),
        person(
            "bob@example.com",
            AttendeeRole::Required,
            CalendarUserType::Individual,
        ),
        person(
            "carol@example.com",
            AttendeeRole::Optional,
            CalendarUserType::Individual,
        ),
        person(
            "room@example.com",
            AttendeeRole::Required,
            CalendarUserType::Room,
        ),
    ];
    event.alarms = vec![envelope_ical::Alarm {
        action: envelope_ical::AlarmAction::Display,
        minutes_before: 15,
        description: None,
    }];
    let item = mapping::new_calendar_item(&event, "jane@example.com", true, berlin).expect("item");
    let emails = |list: &[ews::Mailbox]| -> Vec<String> {
        list.iter().filter_map(|m| m.email.clone()).collect()
    };
    assert_eq!(emails(&item.required_attendees), ["bob@example.com"]);
    assert_eq!(emails(&item.optional_attendees), ["carol@example.com"]);
    assert_eq!(emails(&item.resources), ["room@example.com"]);
    assert_eq!(item.reminder_minutes, Some(15));
    assert_eq!(item.uid.as_deref(), Some("uid-1"));
    assert_eq!(item.free_busy, Some(ews::FreeBusyStatus::Busy));
    match &item.timing {
        ews::EventTiming::Timed {
            start,
            end,
            time_zone,
        } => {
            assert_eq!(
                *start,
                Utc.with_ymd_and_hms(2026, 11, 2, 9, 0, 0)
                    .single()
                    .expect("t")
            );
            assert_eq!(*end - *start, chrono::Duration::hours(1));
            assert_eq!(*time_zone, Some(berlin));
        }
        other => panic!("timed expected, got {other:?}"),
    }
    let recurrence = item.recurrence.as_ref().expect("recurrence");
    assert_eq!(
        recurrence.range.start(),
        NaiveDate::from_ymd_opt(2026, 11, 2).expect("date")
    );
    // Filing someone else's meeting invites nobody.
    let plain =
        mapping::new_calendar_item(&event, "jane@example.com", false, berlin).expect("item");
    assert!(plain.required_attendees.is_empty() && plain.resources.is_empty());

    let mut all_day = event.clone();
    all_day.start = EventTime::Date {
        date: NaiveDate::from_ymd_opt(2026, 12, 24).expect("d"),
    };
    all_day.end = all_day.start.clone();
    all_day.rrule = Some("FREQ=SECONDLY".into());
    assert!(matches!(
        mapping::new_calendar_item(&all_day, "jane@example.com", true, berlin),
        Err(Error::Invalid(_))
    ));
    all_day.rrule = None;
    let item =
        mapping::new_calendar_item(&all_day, "jane@example.com", true, berlin).expect("item");
    assert!(matches!(
        item.timing,
        ews::EventTiming::AllDay { start, end, .. }
            if end == start.succ_opt().expect("d")
    ));
}

#[test]
fn diffs_events_into_updates() {
    let at = |h: u32| EventTime::Utc {
        at: Utc
            .with_ymd_and_hms(2026, 11, 2, h, 0, 0)
            .single()
            .expect("t"),
    };
    let mut old = envelope_ical::Event::new("u", at(9), at(10));
    old.summary = "Planning".into();
    old.location = Some("Room 1".into());
    old.attendees = vec![envelope_ical::Attendee {
        email: "bob@example.com".into(),
        ..Default::default()
    }];
    let mut new = old.clone();
    assert_eq!(
        mapping::calendar_update(&old, &new, "jane@example.com"),
        ews::CalendarItemUpdate::default()
    );
    assert!(!mapping::times_differ(&old, &new, chrono_tz::Tz::UTC));
    // The same instant written in another zone is no change.
    new.start = EventTime::Zoned {
        local: NaiveDate::from_ymd_opt(2026, 11, 2)
            .and_then(|d| d.and_hms_opt(10, 0, 0))
            .expect("t"),
        tzid: "Europe/Berlin".into(),
    };
    assert!(!mapping::times_differ(&old, &new, chrono_tz::Tz::UTC));
    new.end = at(11);
    assert!(mapping::times_differ(&old, &new, chrono_tz::Tz::UTC));
    new.summary = "Planning (moved)".into();
    new.location = None;
    new.attendees.push(envelope_ical::Attendee {
        email: "carol@example.com".into(),
        role: AttendeeRole::Optional,
        ..Default::default()
    });
    new.transparency = Transparency::Transparent;
    let update = mapping::calendar_update(&old, &new, "jane@example.com");
    assert_eq!(update.subject.as_deref(), Some("Planning (moved)"));
    assert_eq!(update.location, Some(None));
    assert!(
        update.required_attendees.is_none(),
        "unchanged lists are left alone"
    );
    assert_eq!(update.optional_attendees.map(|l| l.len()), Some(1));
    assert_eq!(update.free_busy, Some(ews::FreeBusyStatus::Free));
    assert!(update.timing.is_none() && update.recurrence.is_none());

    // A series moves by as much as one of its occurrences did.
    let master =
        calendar_item(json!({ "start": "2026-07-06T08:00:00Z", "end": "2026-07-06T08:30:00Z" }));
    let mut moved = old.clone();
    moved.start = at(10);
    moved.end = at(12);
    match mapping::shifted_timing(&master, &old, &moved, chrono_tz::Tz::UTC) {
        ews::EventTiming::Timed { start, end, .. } => {
            assert_eq!(
                start,
                Utc.with_ymd_and_hms(2026, 7, 6, 9, 0, 0)
                    .single()
                    .expect("t")
            );
            assert_eq!(
                end,
                Utc.with_ymd_and_hms(2026, 7, 6, 11, 0, 0)
                    .single()
                    .expect("t")
            );
        }
        other => panic!("timed expected, got {other:?}"),
    }
    let monday = NaiveDate::from_ymd_opt(2026, 7, 6).expect("date");
    assert!(mapping::same_rule(None, Some(" "), monday));
    assert!(mapping::same_rule(
        Some("FREQ=WEEKLY;INTERVAL=1;BYDAY=MO;WKST=MO"),
        Some("FREQ=WEEKLY;BYDAY=MO"),
        monday
    ));
    assert!(!mapping::same_rule(
        Some("FREQ=WEEKLY;BYDAY=MO"),
        Some("FREQ=WEEKLY;BYDAY=MO,WE"),
        monday
    ));
    assert!(!mapping::same_rule(Some("FREQ=DAILY"), None, monday));
    assert_eq!(
        mapping::meeting_response(PartStat::Accepted),
        ews::MeetingResponse::Accept
    );
    assert_eq!(
        mapping::meeting_response(PartStat::Declined),
        ews::MeetingResponse::Decline
    );
    assert_eq!(
        mapping::meeting_response(PartStat::NeedsAction),
        ews::MeetingResponse::Tentative
    );
}

#[test]
fn writes_raw_requests() {
    let now = Utc
        .with_ymd_and_hms(2026, 10, 1, 9, 0, 0)
        .single()
        .expect("t");
    let request = mapping::last_verb_request(&["A&B".into(), "C".into()], true, now);
    assert!(request.starts_with("<m:UpdateItem MessageDisposition=\"SaveOnly\""));
    assert!(request.contains("<t:ItemId Id=\"A&amp;B\"/>"));
    assert!(
        request.contains("PropertyTag=\"0x1081\" PropertyType=\"Integer\"/><t:Value>104</t:Value>")
    );
    assert!(
        request.contains("PropertyTag=\"0x1080\" PropertyType=\"Integer\"/><t:Value>262</t:Value>")
    );
    assert!(request.contains("<t:Value>2026-10-01T09:00:00Z</t:Value>"));
    assert_eq!(request.matches("<t:ItemChange>").count(), 2);
    let request =
        mapping::occurrence_times_request("occ", now, now + chrono::Duration::hours(1), true);
    assert!(request.contains("SendMeetingInvitationsOrCancellations=\"SendToAllAndSaveCopy\""));
    assert!(request.contains("<t:Start>2026-10-01T09:00:00Z</t:Start>"));
    assert!(request.contains("<t:End>2026-10-01T10:00:00Z</t:End>"));

    assert_eq!(
        mapping::raw_error(&response("UpdateItem", &[success("UpdateItem", "")])),
        None
    );
    assert_eq!(
        mapping::raw_error(&response(
            "UpdateItem",
            &[
                success("UpdateItem", ""),
                failure("UpdateItem", "ErrorInvalidPropertySet")
            ]
        ))
        .as_deref(),
        Some("ErrorInvalidPropertySet")
    );
}

#[test]
fn classifies_ews_failures() {
    let unauthorized = ews_error(EwsError::Unauthorized { detail: None });
    assert!(unauthorized.is_auth());
    let no_ntlm = ews_error(EwsError::Unauthorized {
        detail: Some("the server does not offer NTLM (offered: basic)".into()),
    });
    assert!(no_ntlm.is_auth());
    assert!(ntlm_unavailable(&no_ntlm));
    assert!(!ntlm_unavailable(&unauthorized));
    let http = |status: u16| EwsError::Http {
        status,
        url: "https://mail.example.com/EWS/Exchange.asmx".into(),
        body: String::new(),
        retry_after: None,
    };
    assert!(ews_error(http(503)).is_transient());
    assert!(ews_error(http(429)).is_transient());
    assert!(!ews_error(http(400)).is_transient());
    let response = |code: ews::ResponseCode| EwsError::from(ews::ResponseError::new(code, "x"));
    assert!(matches!(
        ews_error(response(ews::ResponseCode::ErrorItemNotFound)),
        Error::NotFound(_)
    ));
    assert!(ews_error(response(ews::ResponseCode::ErrorServerBusy)).is_transient());
    assert!(matches!(
        ews_error(response(ews::ResponseCode::ErrorMessageSizeExceeded)),
        Error::Invalid(_)
    ));
    let denied = ews_error(EwsError::Forbidden {
        url: "https://mail.example.com".into(),
    });
    assert!(!denied.is_auth() && !denied.is_transient());
    assert!(denied.to_string().contains("EWS"));
}

#[test]
fn item_state_survives_the_folder_state() {
    let mut state = FolderSyncState::default();
    assert_eq!(
        ItemState::load(&state),
        ItemState::fresh(),
        "new folders start with a listing"
    );
    let items = ItemState {
        sync_state: Some("H4sIAAAA".into()),
        current: true,
        listing: None,
        prune: true,
    };
    items.store(&mut state);
    assert_eq!(ItemState::load(&state), items);
    state.cursor = Some("not json".into());
    assert_eq!(ItemState::load(&state), ItemState::fresh());
    assert_eq!(sync_properties(false).len(), 2);
    assert_eq!(sync_properties(true).len(), 5);
}

#[test]
fn recognizes_exchange_autodiscover_answers() {
    use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};
    let mut ntlm = HeaderMap::new();
    ntlm.append(WWW_AUTHENTICATE, HeaderValue::from_static("Negotiate"));
    ntlm.append(WWW_AUTHENTICATE, HeaderValue::from_static("NTLM"));
    assert!(looks_like_exchange(401, &ntlm));
    let mut basic = HeaderMap::new();
    basic.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"cpanel\""),
    );
    assert!(!looks_like_exchange(401, &basic));
    assert!(!looks_like_exchange(200, &HeaderMap::new()));
    let mut labeled = HeaderMap::new();
    labeled.insert("x-feserver", HeaderValue::from_static("EXCH01"));
    assert!(looks_like_exchange(200, &labeled));
}

// ---------------------------------------------------------------------------
// A fake Exchange server

fn envelope(body: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">"#,
            r#"<s:Header><h:ServerVersionInfo MajorVersion="15" MinorVersion="1" MajorBuildNumber="2507" MinorBuildNumber="6" Version="V2017_07_11" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/></s:Header>"#,
            r#"<s:Body>{}</s:Body></s:Envelope>"#
        ),
        body
    )
}

/// A response of operation `op` carrying the given response messages.
fn response(op: &str, messages: &[String]) -> String {
    envelope(&format!(
        "<m:{op}Response {NS}><m:ResponseMessages>{}</m:ResponseMessages></m:{op}Response>",
        messages.concat()
    ))
}

fn success(op: &str, inner: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:{op}ResponseMessage>"#
    )
}

fn failure(op: &str, code: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Error"><m:MessageText>{code}</m:MessageText><m:ResponseCode>{code}</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:{op}ResponseMessage>"#
    )
}

fn soap(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/xml; charset=utf-8")
        .set_body_string(body)
}

fn mailbox_xml(name: &str, email: &str) -> String {
    format!(
        "<t:Mailbox><t:Name>{name}</t:Name><t:EmailAddress>{email}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox>"
    )
}

fn capture(pattern: &str, text: &str) -> Option<String> {
    Regex::new(pattern)
        .ok()?
        .captures(text)?
        .get(1)
        .map(|m| m.as_str().to_string())
}

/// Ids a request names, in order (`ItemId`s and the occurrences of
/// `RecurringMasterItemId`s).
fn requested_ids(body: &str) -> Vec<String> {
    let Ok(re) = Regex::new(r#"<t:(?:ItemId Id|RecurringMasterItemId OccurrenceId)="([^"]+)""#)
    else {
        return Vec::new();
    };
    re.captures_iter(body)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn folder_xml(tag: &str, id: &str, parent: &str, class: &str, name: &str, hidden: bool) -> String {
    let hidden = if hidden {
        r#"<t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x10f4" PropertyType="Boolean"/><t:Value>true</t:Value></t:ExtendedProperty>"#
    } else {
        ""
    };
    format!(
        r#"<t:{tag}><t:FolderId Id="{id}" ChangeKey="ck"/><t:ParentFolderId Id="{parent}"/><t:FolderClass>{class}</t:FolderClass><t:DisplayName>{name}</t:DisplayName><t:TotalCount>0</t:TotalCount><t:ChildFolderCount>0</t:ChildFolderCount>{hidden}</t:{tag}>"#
    )
}

/// The ids of the well-known folders, in the order the client asks.
fn well_known_folders() -> String {
    let found = |tag: &str, id: &str| {
        success(
            "GetFolder",
            &format!(
                r#"<m:Folders><t:{tag}><t:FolderId Id="{id}" ChangeKey="ck"/></t:{tag}></m:Folders>"#
            ),
        )
    };
    response(
        "GetFolder",
        &[
            found("Folder", "inbox-id"),
            found("Folder", "drafts-id"),
            found("Folder", "sentitems-id"),
            found("Folder", "deleteditems-id"),
            found("Folder", "junkemail-id"),
            found("Folder", "outbox-id"),
            found("CalendarFolder", "calendar-id"),
            found("ContactsFolder", "contacts-id"),
            failure("GetFolder", "ErrorFolderNotFound"),
            failure("GetFolder", "ErrorFolderNotFound"),
            found("Folder", "msgfolderroot-id"),
        ],
    )
}

fn folder_tree() -> String {
    let root = "msgfolderroot-id";
    let folders = [
        folder_xml("Folder", "inbox-id", root, "IPF.Note", "Inbox", false),
        folder_xml(
            "Folder",
            "projects-id",
            "inbox-id",
            "IPF.Note",
            "Projects",
            false,
        ),
        folder_xml("Folder", "drafts-id", root, "IPF.Note", "Drafts", false),
        folder_xml(
            "Folder",
            "sentitems-id",
            root,
            "IPF.Note",
            "Sent Items",
            false,
        ),
        folder_xml(
            "Folder",
            "deleteditems-id",
            root,
            "IPF.Note",
            "Deleted Items",
            false,
        ),
        folder_xml(
            "Folder",
            "junkemail-id",
            root,
            "IPF.Note",
            "Junk Email",
            false,
        ),
        folder_xml("Folder", "outbox-id", root, "IPF.Note", "Outbox", false),
        folder_xml("Folder", "archive-id", root, "IPF.Note", "Archive", false),
        folder_xml(
            "CalendarFolder",
            "calendar-id",
            root,
            "IPF.Appointment",
            "Calendar",
            false,
        ),
        folder_xml(
            "CalendarFolder",
            "birthdays-id",
            "calendar-id",
            "IPF.Appointment.Birthday",
            "Birthdays",
            false,
        ),
        folder_xml(
            "ContactsFolder",
            "contacts-id",
            root,
            "IPF.Contact",
            "Contacts",
            false,
        ),
        folder_xml(
            "Folder",
            "cas-id",
            root,
            "IPF.Configuration",
            "Conversation Action Settings",
            true,
        ),
    ]
    .concat();
    response(
        "FindFolder",
        &[success(
            "FindFolder",
            &format!(
                r#"<m:RootFolder TotalItemsInView="12" IncludesLastItemInRange="true"><t:Folders>{folders}</t:Folders></m:RootFolder>"#
            ),
        )],
    )
}

/// A test message, rendered the ways Exchange reports messages.
#[derive(Clone)]
struct Msg {
    id: &'static str,
    folder: &'static str,
    conversation: &'static str,
    message_id: &'static str,
    subject: &'static str,
    from: (&'static str, &'static str),
    to: (&'static str, &'static str),
    received: &'static str,
    read: bool,
    flagged: bool,
    categories: Vec<&'static str>,
}

impl Msg {
    fn categories_xml(&self) -> String {
        if self.categories.is_empty() {
            return String::new();
        }
        let list: String = self
            .categories
            .iter()
            .map(|c| format!("<t:String>{c}</t:String>"))
            .collect();
        format!("<t:Categories>{list}</t:Categories>")
    }

    fn flag_property(&self) -> String {
        format!(
            r#"<t:ExtendedProperty><t:ExtendedFieldURI PropertyTag="0x1090" PropertyType="Integer"/><t:Value>{}</t:Value></t:ExtendedProperty>"#,
            if self.flagged { 2 } else { 0 }
        )
    }

    /// As `GetItem` returns it with the detail properties (and, given, the
    /// MIME content).
    fn detail_with(&self, mime: Option<&[u8]>) -> String {
        let mime = mime
            .map(|raw| {
                format!(
                    r#"<t:MimeContent CharacterSet="UTF-8">{}</t:MimeContent>"#,
                    base64::engine::general_purpose::STANDARD.encode(raw)
                )
            })
            .unwrap_or_default();
        format!(
            concat!(
                r#"<t:Message>{mime}<t:ItemId Id="{id}" ChangeKey="ck-{id}"/><t:ParentFolderId Id="{folder}"/>"#,
                r#"<t:ItemClass>IPM.Note</t:ItemClass><t:Subject>{subject}</t:Subject>"#,
                r#"<t:DateTimeReceived>{received}</t:DateTimeReceived><t:Size>2048</t:Size>{categories}"#,
                r#"<t:IsDraft>false</t:IsDraft><t:HasAttachments>false</t:HasAttachments>{flag}"#,
                r#"<t:ConversationId Id="{conversation}"/>"#,
                r#"<t:Flag><t:FlagStatus>{status}</t:FlagStatus></t:Flag><t:Preview>{subject} preview</t:Preview>"#,
                r#"<t:ToRecipients>{to}</t:ToRecipients><t:From>{from}</t:From>"#,
                r#"<t:InternetMessageId>&lt;{message_id}&gt;</t:InternetMessageId><t:IsRead>{read}</t:IsRead></t:Message>"#
            ),
            mime = mime,
            id = self.id,
            folder = self.folder,
            subject = self.subject,
            received = self.received,
            categories = self.categories_xml(),
            flag = self.flag_property(),
            conversation = self.conversation,
            status = if self.flagged {
                "Flagged"
            } else {
                "NotFlagged"
            },
            to = mailbox_xml(self.to.0, self.to.1),
            from = mailbox_xml(self.from.0, self.from.1),
            message_id = self.message_id,
            read = self.read,
        )
    }

    fn detail(&self) -> String {
        self.detail_with(None)
    }

    /// As `SyncFolderItems` reports it, with read state, categories and flag.
    fn change(&self) -> String {
        format!(
            r#"<t:Message><t:ItemId Id="{id}" ChangeKey="ck-{id}"/><t:ItemClass>IPM.Note</t:ItemClass><t:DateTimeReceived>{received}</t:DateTimeReceived>{categories}{flag}<t:IsRead>{read}</t:IsRead></t:Message>"#,
            id = self.id,
            received = self.received,
            categories = self.categories_xml(),
            flag = self.flag_property(),
            read = self.read,
        )
    }

    /// As a listing (`FindItem`) returns it.
    fn listed(&self) -> String {
        format!(
            r#"<t:Message><t:ItemId Id="{id}" ChangeKey="ck-{id}"/><t:ItemClass>IPM.Note</t:ItemClass></t:Message>"#,
            id = self.id
        )
    }
}

fn plan() -> Msg {
    Msg {
        id: "m1",
        folder: "inbox-id",
        conversation: "c1",
        message_id: "plan@example.com",
        subject: "Quarterly plan",
        from: ("Dana Swope", "dana@example.com"),
        to: ("Jane Doe", "jane@example.com"),
        received: "2026-10-01T09:00:00Z",
        read: false,
        flagged: true,
        categories: vec!["Red category"],
    }
}

fn deals() -> Msg {
    Msg {
        id: "m2",
        folder: "inbox-id",
        conversation: "c2",
        message_id: "deal@shop.example",
        subject: "Weekend deals",
        from: ("Shop", "news@shop.example"),
        to: ("Jane Doe", "jane@example.com"),
        received: "2026-09-30T12:00:00Z",
        read: true,
        flagged: false,
        categories: Vec::new(),
    }
}

fn reply() -> Msg {
    Msg {
        id: "m3",
        folder: "sentitems-id",
        conversation: "c1",
        message_id: "re-plan@example.com",
        subject: "RE: Quarterly plan",
        from: ("Jane Doe", "jane@example.com"),
        to: ("Dana Swope", "dana@example.com"),
        received: "2026-10-02T10:00:00Z",
        read: true,
        flagged: false,
        categories: Vec::new(),
    }
}

fn lunch() -> Msg {
    Msg {
        id: "m4",
        folder: "inbox-id",
        conversation: "c4",
        message_id: "lunch@example.com",
        subject: "Lunch?",
        from: ("Alex Wilber", "alex@example.com"),
        to: ("Jane Doe", "jane@example.com"),
        received: "2026-10-03T11:00:00Z",
        read: false,
        flagged: false,
        categories: Vec::new(),
    }
}

fn create(xml: String) -> String {
    format!("<t:Create>{xml}</t:Create>")
}

fn update(xml: String) -> String {
    format!("<t:Update>{xml}</t:Update>")
}

fn delete(id: &str) -> String {
    format!(r#"<t:Delete><t:ItemId Id="{id}" ChangeKey="ck"/></t:Delete>"#)
}

fn read_flag(id: &str, read: bool) -> String {
    format!(
        r#"<t:ReadFlagChange><t:ItemId Id="{id}" ChangeKey="ck"/><t:IsRead>{read}</t:IsRead></t:ReadFlagChange>"#
    )
}

/// What the fake server knows and has been asked.
#[derive(Default)]
struct FakeState {
    /// `GetItem` answers: `id` (message headers), `mime:id`, `cal:id`
    /// (calendar details) and `master:occurrence id`.
    items: HashMap<String, String>,
    /// `FindItem` answers by folder id (listings and calendar views).
    listings: HashMap<String, Vec<String>>,
    /// `SyncFolderItems` rounds by folder and state sent: changes, next
    /// state, whether the round is the last.
    rounds: HashMap<(String, Option<String>), (String, String, bool)>,
    /// Sync states the server no longer accepts.
    expired: std::collections::HashSet<String>,
    /// Whole response bodies by operation, overriding the defaults.
    canned: HashMap<String, String>,
    /// Every request: operation and body.
    requests: Vec<(String, String)>,
}

#[derive(Clone, Default)]
struct FakeExchange(Arc<Mutex<FakeState>>);

impl wiremock::Respond for FakeExchange {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&request.body).into_owned();
        let op = capture(r"<soap:Body><m:(\w+)", &body).unwrap_or_default();
        let mut state = self.0.lock();
        state.requests.push((op.clone(), body.clone()));
        if let Some(answer) = state.canned.get(&op) {
            return soap(answer.clone());
        }
        let ids = requested_ids(&body);
        let xml = match op.as_str() {
            "GetFolder" if body.contains(r#"Id="archive""#) => {
                response("GetFolder", &[failure("GetFolder", "ErrorFolderNotFound")])
            }
            "GetFolder" => well_known_folders(),
            "FindFolder" => folder_tree(),
            "GetUserConfiguration" => category_list_response(
                r#"<?xml version="1.0"?><categories xmlns="CategoryList.xsd"><category name="Red category" color="0"/><category name="Blue category" color="7"/></categories>"#,
            ),
            "SyncFolderItems" => {
                let folder = capture(r#"<m:SyncFolderId><t:FolderId Id="([^"]+)""#, &body)
                    .unwrap_or_default();
                let sent = capture(r"<m:SyncState>([^<]*)</m:SyncState>", &body);
                if sent.as_ref().is_some_and(|s| state.expired.contains(s)) {
                    return soap(response(
                        "SyncFolderItems",
                        &[failure("SyncFolderItems", "ErrorInvalidSyncStateData")],
                    ));
                }
                let (changes, next, last) = state
                    .rounds
                    .get(&(folder, sent.clone()))
                    .cloned()
                    .unwrap_or_else(|| {
                        (String::new(), sent.unwrap_or_else(|| "empty".into()), true)
                    });
                response(
                    "SyncFolderItems",
                    &[success(
                        "SyncFolderItems",
                        &format!(
                            "<m:SyncState>{next}</m:SyncState><m:IncludesLastItemInRange>{last}</m:IncludesLastItemInRange><m:Changes>{changes}</m:Changes>"
                        ),
                    )],
                )
            }
            "FindItem" => {
                let folder = capture(r#"<m:ParentFolderIds><t:FolderId Id="([^"]+)""#, &body)
                    .unwrap_or_default();
                let items = state.listings.get(&folder).cloned().unwrap_or_default();
                response(
                    "FindItem",
                    &[success(
                        "FindItem",
                        &format!(
                            r#"<m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="true"><t:Items>{}</t:Items></m:RootFolder>"#,
                            items.len(),
                            items.concat()
                        ),
                    )],
                )
            }
            "GetItem" => {
                let prefix = if body.contains("RecurringMasterItemId") {
                    "master:"
                } else if body.contains("<t:IncludeMimeContent>true</t:IncludeMimeContent>") {
                    "mime:"
                } else if body.contains("calendar:Start") {
                    "cal:"
                } else {
                    ""
                };
                let messages: Vec<String> = ids
                    .iter()
                    .map(|id| match state.items.get(&format!("{prefix}{id}")) {
                        Some(xml) => success("GetItem", &format!("<m:Items>{xml}</m:Items>")),
                        None => failure("GetItem", "ErrorItemNotFound"),
                    })
                    .collect();
                response("GetItem", &messages)
            }
            "UpdateItem" => {
                let changes: Vec<String> = ids
                    .iter()
                    .map(|id| {
                        success(
                            "UpdateItem",
                            &format!(r#"<m:Items><t:Message><t:ItemId Id="{id}" ChangeKey="ck-new"/></t:Message></m:Items><m:ConflictResults><t:Count>0</t:Count></m:ConflictResults>"#),
                        )
                    })
                    .collect();
                response("UpdateItem", &changes)
            }
            "MoveItem" => {
                let moved: Vec<String> = ids
                    .iter()
                    .map(|id| {
                        success(
                            "MoveItem",
                            &format!(r#"<m:Items><t:Message><t:ItemId Id="{id}-moved" ChangeKey="ck"/></t:Message></m:Items>"#),
                        )
                    })
                    .collect();
                response("MoveItem", &moved)
            }
            "DeleteItem" => {
                let deleted: Vec<String> = ids.iter().map(|_| success("DeleteItem", "")).collect();
                response("DeleteItem", &deleted)
            }
            "CreateItem"
                if body.contains("MessageDisposition=\"SaveOnly\"")
                    && body.contains("<t:MimeContent") =>
            {
                response(
                    "CreateItem",
                    &[success(
                        "CreateItem",
                        r#"<m:Items><t:Message><t:ItemId Id="new-draft" ChangeKey="ck"/></t:Message></m:Items>"#,
                    )],
                )
            }
            "CreateItem" if body.contains("<t:CalendarItem>") => response(
                "CreateItem",
                &[success(
                    "CreateItem",
                    r#"<m:Items><t:CalendarItem><t:ItemId Id="new-event" ChangeKey="ck"/></t:CalendarItem></m:Items>"#,
                )],
            ),
            "CreateItem" => response("CreateItem", &[success("CreateItem", "<m:Items/>")]),
            other => {
                return ResponseTemplate::new(500)
                    .set_body_string(format!("unexpected operation {other}"));
            }
        };
        soap(xml)
    }
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

impl Events {
    fn new_mail(&self) -> Vec<Vec<ThreadSummary>> {
        self.0
            .lock()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::NewMail { threads, .. } => Some(threads.clone()),
                _ => None,
            })
            .collect()
    }
}

struct Fixture {
    _server: MockServer,
    fake: FakeExchange,
    engine: Engine,
    events: Events,
    account: AccountRow,
    ews: EwsMail,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    let fake = FakeExchange::default();
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .respond_with(fake.clone())
        .mount(&server)
        .await;
    let dir = std::env::temp_dir().join(format!("envelope-ews-{}", uuid::Uuid::new_v4().simple()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .expect("engine");
    let url = format!("{}{EWS_PATH}", server.uri());
    let config = AccountConfig {
        auth: AuthMethod::Password,
        exchange: Some(ExchangeConfig {
            ews_url: url.clone(),
            auth: ExchangeAuth::Basic,
            ..ExchangeConfig::default()
        }),
        ..AccountConfig::default()
    };
    let id = engine
        .db
        .write(|tx| {
            let id = mail::insert_account(
                tx,
                AccountKind::Exchange,
                "exchange",
                "jane@example.com",
                "Jane Doe",
                "#0ea5e9",
                &config,
            )?;
            // Keep the calendar scheduler and the account worker away.
            tx.execute("UPDATE accounts SET enabled = 0 WHERE id = ?", [id])?;
            Ok(id)
        })
        .expect("account");
    // Writes made through the engine look the password up.
    engine
        .secrets
        .preload(&crate::secrets::password_key(id), "secret");
    let account = engine.account_row(id).expect("account row");
    let client = EwsClient::builder(url, ews::Auth::basic("jane@example.com", "secret"))
        .retry_policy(ews::RetryPolicy::none())
        .build()
        .expect("client");
    Fixture {
        _server: server,
        fake,
        engine,
        events,
        account,
        ews: EwsMail::new(client),
        dir,
    }
}

impl Fixture {
    fn session(&self) -> Session {
        Session::new(
            self.engine.clone(),
            self.account.clone(),
            self.ews.clone(),
            CancellationToken::new(),
        )
    }

    fn state(&self) -> parking_lot::MutexGuard<'_, FakeState> {
        self.fake.0.lock()
    }

    fn serve(&self, msg: &Msg) {
        self.state().items.insert(msg.id.to_string(), msg.detail());
    }

    fn round(&self, folder: &str, sent: Option<&str>, changes: &[String], next: &str) {
        self.state().rounds.insert(
            (folder.to_string(), sent.map(str::to_string)),
            (changes.concat(), next.to_string(), true),
        );
    }

    /// Bodies of the requests for an operation.
    fn requests(&self, op: &str) -> Vec<String> {
        self.state()
            .requests
            .iter()
            .filter(|(o, _)| o == op)
            .map(|(_, b)| b.clone())
            .collect()
    }

    fn folder(&self, remote_id: &str) -> FolderRow {
        self.engine
            .db
            .read(|c| mail::folder_by_remote(c, self.account.id, remote_id))
            .expect("query")
            .unwrap_or_else(|| panic!("folder {remote_id}"))
    }

    fn has_folder(&self, remote_id: &str) -> bool {
        self.engine
            .db
            .read(|c| mail::folder_by_remote(c, self.account.id, remote_id))
            .expect("query")
            .is_some()
    }

    fn message(&self, remote_id: &str) -> Option<MessageView> {
        let found = self
            .engine
            .db
            .read(|c| mail::message_by_remote(c, self.account.id, remote_id))
            .expect("query");
        found.map(|(id, _, _)| {
            self.engine
                .db
                .read(|c| mail::get_message(c, id))
                .expect("message")
        })
    }

    fn folder_counts(&self, id: i64) -> (i64, i64, Option<i64>, Option<String>, bool) {
        self.engine
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT total_count, unread_count, parent_id, color, hidden FROM folders WHERE id = ?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0)),
                )?)
            })
            .expect("folder")
    }

    /// The first sync of the mailbox: the inbox is listed (m1, m2) and
    /// enumerated, sent items only enumerated (m3, a reply in m1's
    /// conversation).
    async fn first_sync(&self) -> Session {
        for msg in [plan(), deals(), reply()] {
            self.serve(&msg);
        }
        self.state()
            .listings
            .insert("inbox-id".into(), vec![plan().listed(), deals().listed()]);
        self.round(
            "inbox-id",
            None,
            &[create(plan().change()), create(deals().change())],
            "inbox-1",
        );
        self.round("sentitems-id", None, &[create(reply().change())], "sent-1");
        let mut session = self.session();
        session.reload_folders().expect("folders");
        session.sync_folder_list().await.expect("folder list");
        let (more, clean) = session.sync_folders(None).await.expect("first sync");
        assert!(!more && clean);
        session.tidy().await.expect("tidy");
        session
    }
}

#[tokio::test]
async fn syncs_folders_and_messages() {
    let f = fixture().await;
    let mut session = f.first_sync().await;

    // Folders: roles, hierarchy, categories as labels; no outbox and no
    // calendar, contact or hidden folders.
    let inbox = f.folder("inbox-id");
    assert_eq!(inbox.role, Some(FolderRole::Inbox));
    assert_eq!(f.folder("drafts-id").role, Some(FolderRole::Drafts));
    assert_eq!(f.folder("sentitems-id").role, Some(FolderRole::Sent));
    assert_eq!(f.folder("deleteditems-id").role, Some(FolderRole::Trash));
    assert_eq!(f.folder("junkemail-id").role, Some(FolderRole::Junk));
    assert_eq!(f.folder("archive-id").role, Some(FolderRole::Archive));
    for absent in [
        "outbox-id",
        "calendar-id",
        "contacts-id",
        "cas-id",
        "birthdays-id",
    ] {
        assert!(!f.has_folder(absent), "{absent}");
    }
    let projects = f.folder("projects-id");
    assert_eq!(projects.path, "Inbox/Projects");
    assert_eq!(f.folder_counts(projects.id).2, Some(inbox.id));
    let red = f.folder("category:Red category");
    let blue = f.folder("category:Blue category");
    assert_eq!(red.kind, "label");
    assert_eq!(f.folder_counts(red.id).3.as_deref(), Some("#e74856"));
    assert!(!f.folder_counts(red.id).4, "a category in use is shown");
    assert!(
        f.folder_counts(blue.id).4,
        "unused categories stay out of the way"
    );

    // Messages, flags, labels, threads and counts.
    let m1 = f.message("m1").expect("m1");
    let m2 = f.message("m2").expect("m2");
    let m3 = f.message("m3").expect("m3");
    assert!(!m1.flags.seen && m1.flags.flagged);
    assert!(m2.flags.seen && !m2.flags.flagged);
    assert_eq!(m1.thread_id, m3.thread_id, "conversation ids group threads");
    assert_eq!(m1.message_id.as_deref(), Some("plan@example.com"));
    assert_eq!(m1.snippet, "Quarterly plan preview");
    assert_eq!(m1.to[0].email, "jane@example.com");
    assert!(m1.folder_ids.contains(&inbox.id) && m1.folder_ids.contains(&red.id));
    assert_eq!(m3.folder_ids, vec![f.folder("sentitems-id").id]);
    let (total, unread, ..) = f.folder_counts(inbox.id);
    assert_eq!((total, unread), (2, 1));

    // The listing fetched the inbox headers once; the enumeration found the
    // messages known and fetched only the sent reply.
    let header_requests = f.requests("GetItem");
    assert_eq!(
        header_requests
            .iter()
            .filter(|b| b.contains(r#"Id="m1""#))
            .count(),
        1
    );
    assert_eq!(
        header_requests
            .iter()
            .filter(|b| b.contains(r#"Id="m3""#))
            .count(),
        1
    );
    let sync = f.requests("SyncFolderItems");
    assert!(
        sync.iter()
            .any(|b| b.contains("message:IsRead") && b.contains("0x1090"))
    );

    // Sync states are kept per folder; the listing produced no notifications.
    let state = ItemState::load(&f.folder("inbox-id").sync_state);
    assert_eq!(state.sync_state.as_deref(), Some("inbox-1"));
    assert!(state.current && state.listing.is_none());
    assert!(f.folder("inbox-id").sync_state.complete);
    assert!(f.events.new_mail().is_empty());

    // Changes: m1 read elsewhere, m2 flagged and categorized, m4 arrives.
    let mut changed_m2 = deals();
    changed_m2.flagged = true;
    changed_m2.categories = vec!["Blue category", "Brand new"];
    f.serve(&lunch());
    f.round(
        "inbox-id",
        Some("inbox-1"),
        &[
            read_flag("m1", true),
            update(changed_m2.change()),
            create(lunch().change()),
        ],
        "inbox-2",
    );
    let quick = BTreeSet::from([inbox.id]);
    session.sync_folders(Some(&quick)).await.expect("changes");
    assert!(f.message("m1").expect("m1").flags.seen);
    let m2 = f.message("m2").expect("m2");
    assert!(m2.flags.flagged && m2.flags.seen);
    assert!(m2.folder_ids.contains(&blue.id));
    let brand_new = f.folder("category:Brand new");
    assert!(
        m2.folder_ids.contains(&brand_new.id),
        "unknown categories become labels"
    );
    let m4 = f.message("m4").expect("m4");
    assert!(!m4.flags.seen);
    let notified: Vec<i64> = f.events.new_mail().concat().iter().map(|t| t.id).collect();
    assert_eq!(notified, vec![m4.thread_id]);
    let state = ItemState::load(&f.folder("inbox-id").sync_state);
    assert_eq!(state.sync_state.as_deref(), Some("inbox-2"));
}

#[tokio::test]
async fn messages_moved_elsewhere_keep_their_local_copy() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let before = f.message("m2").expect("m2");
    // Moved to the archive by another client: it leaves the inbox and shows
    // up in the archive with a new id.
    let mut moved = deals();
    moved.id = "m2-elsewhere";
    moved.folder = "archive-id";
    f.serve(&moved);
    f.round("inbox-id", Some("inbox-1"), &[delete("m2")], "inbox-2");
    f.round(
        "archive-id",
        Some("empty"),
        &[create(moved.change())],
        "archive-2",
    );
    session.last_full = None;
    let (_, clean) = session.sync_folders(None).await.expect("sync");
    assert!(clean);
    session.tidy().await.expect("tidy");
    assert!(f.message("m2").is_none());
    let after = f.message("m2-elsewhere").expect("moved message");
    assert_eq!(after.id, before.id, "the local copy (and its body) is kept");
    assert_eq!(after.folder_ids, vec![f.folder("archive-id").id]);
}

#[tokio::test]
async fn new_mail_shows_up_during_a_long_first_sync() {
    let f = fixture().await;
    let mut session = f.session();
    session.reload_folders().expect("folders");
    session.sync_folder_list().await.expect("folder list");
    // The inbox is halfway through its first listing and enumeration.
    let inbox = f.folder("inbox-id");
    let mut state = inbox.sync_state.clone();
    ItemState {
        sync_state: Some("partial".into()),
        current: false,
        listing: Some(200),
        prune: false,
    }
    .store(&mut state);
    f.engine
        .db
        .write(|tx| mail::set_folder_sync_state(tx, inbox.id, &state))
        .expect("state");
    f.serve(&lunch());
    f.state()
        .listings
        .insert("inbox-id".into(), vec![lunch().listed()]);
    f.state().rounds.insert(
        ("inbox-id".into(), Some("partial".into())),
        (String::new(), "partial-2".into(), false),
    );
    let quick = BTreeSet::from([inbox.id]);
    session.sync_folders(Some(&quick)).await.expect("sync");
    assert!(
        f.message("m4").is_some(),
        "the newest message is stored at once"
    );
    let probes = f
        .requests("FindItem")
        .iter()
        .filter(|b| b.contains(r#"MaxEntriesReturned="25" Offset="0""#))
        .count();
    assert_eq!(probes, 1);
    assert!(
        f.events.new_mail().is_empty(),
        "no notifications during a first sync"
    );
}

#[tokio::test]
async fn resynchronizes_after_an_expired_sync_state() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let inbox = f.folder("inbox-id");
    // The server lost the state; enumerating again finds only the plan (the
    // deals were deleted while we were away).
    f.state().expired.insert("inbox-1".into());
    f.state()
        .listings
        .insert("inbox-id".into(), vec![plan().listed()]);
    f.round("inbox-id", None, &[create(plan().change())], "inbox-fresh");
    let quick = BTreeSet::from([inbox.id]);
    session.sync_folders(Some(&quick)).await.expect("resync");
    assert!(f.message("m1").is_some());
    assert!(
        f.message("m2").expect("m2").folder_ids.is_empty(),
        "pruned from the inbox, deleted by the next tidy"
    );
    let state = ItemState::load(&f.folder("inbox-id").sync_state);
    assert_eq!(state.sync_state.as_deref(), Some("inbox-fresh"));
    assert!(state.current && !state.prune && state.listing.is_none());
    session.tidy().await.expect("tidy");
    assert!(f.message("m2").is_none());
}

#[tokio::test]
async fn replays_local_changes() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    let m2 = f.message("m2").expect("m2");
    let blue = f.folder("category:Blue category");
    f.engine
        .set_flags(
            &[],
            &[m1.id],
            &FlagChange {
                seen: Some(true),
                flagged: Some(false),
            },
        )
        .expect("flags");
    f.engine
        .move_threads(&[m2.thread_id], None, &MoveTarget::Archive)
        .expect("archive");
    f.engine
        .set_label(&[m1.thread_id], blue.id, true)
        .expect("label");
    // Skip the undo window.
    f.engine
        .db
        .write(|tx| Ok(tx.execute("UPDATE pending_ops SET not_before = 0", [])?))
        .expect("due");
    assert_eq!(
        f.message("m2").expect("m2").folder_ids,
        vec![f.folder("archive-id").id],
        "optimistic move"
    );
    session.process_ops().await.expect("ops");
    let left = f
        .engine
        .db
        .read(|c| misc::queued_ops(c, f.account.id))
        .expect("ops");
    assert!(left.is_empty(), "{left:?}");

    let updates = f.requests("UpdateItem");
    assert!(
        updates
            .iter()
            .any(|b| b.contains("<t:IsRead>true</t:IsRead>") && b.contains(r#"Id="m1""#))
    );
    assert!(
        updates
            .iter()
            .any(|b| b.contains("<t:FlagStatus>NotFlagged</t:FlagStatus>"))
    );
    let categories = updates
        .iter()
        .find(|b| b.contains("item:Categories"))
        .expect("categories update");
    assert!(
        categories.contains("<t:String>Red category</t:String><t:String>Blue category</t:String>"),
        "categories are merged with the server's: {categories}"
    );
    let moves = f.requests("MoveItem");
    assert_eq!(moves.len(), 1);
    assert!(moves[0].contains(r#"<m:ToFolderId><t:FolderId Id="archive-id"/>"#));
    assert!(moves[0].contains(r#"<t:ItemId Id="m2"/>"#));
    // A moved message gets a new id.
    assert!(f.message("m2").is_none());
    let moved = f.message("m2-moved").expect("new id");
    assert_eq!(moved.id, m2.id);
    let m1 = f.message("m1").expect("m1");
    assert!(m1.flags.seen && !m1.flags.flagged);

    // Deleting from the trash deletes for good (into Recoverable Items).
    let trash = f.folder("deleteditems-id");
    f.engine
        .db
        .write(|tx| mail::set_only_folder(tx, moved.id, trash.id).map(|_| ()))
        .expect("trash");
    f.engine.delete_forever(&[moved.thread_id]).expect("delete");
    session.process_ops().await.expect("ops");
    let deletes = f.requests("DeleteItem");
    assert!(
        deletes
            .iter()
            .any(|b| b.contains(r#"DeleteType="SoftDelete""#) && b.contains(r#"Id="m2-moved""#))
    );
}

#[tokio::test]
async fn gives_up_on_rejected_changes_and_restores_the_server_state() {
    let f = fixture().await;
    let mut session = f.first_sync().await;
    let m2 = f.message("m2").expect("m2");
    let inbox = f.folder("inbox-id");
    f.engine
        .move_threads(&[m2.thread_id], Some(inbox.id), &MoveTarget::Archive)
        .expect("archive");
    // The server keeps refusing: this is the last attempt.
    f.engine
        .db
        .write(|tx| Ok(tx.execute("UPDATE pending_ops SET not_before = 0, attempts = 4", [])?))
        .expect("due");
    f.state().canned.insert(
        "MoveItem".into(),
        response("MoveItem", &[failure("MoveItem", "ErrorMoveCopyFailed")]),
    );
    session.process_ops().await.expect("ops");
    assert!(
        f.engine
            .db
            .read(|c| misc::queued_ops(c, f.account.id))
            .expect("ops")
            .is_empty()
    );
    assert_eq!(
        f.message("m2").expect("m2").folder_ids,
        vec![inbox.id],
        "back where the server has it"
    );
}

const SOURCE: &str = "From: Dana Swope <dana@example.com>\r\n\
To: Jane Doe <jane@example.com>\r\n\
Subject: Quarterly plan\r\n\
Message-ID: <plan@example.com>\r\n\
In-Reply-To: <kickoff@example.com>\r\n\
References: <kickoff@example.com>\r\n\
List-Unsubscribe: <https://example.com/u>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=outer\r\n\
\r\n\
--outer\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Here is the plan.</p>\r\n\
--outer\r\n\
Content-Type: application/pdf; name=plan.pdf\r\n\
Content-Disposition: attachment; filename=plan.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--outer--\r\n";

#[tokio::test]
async fn downloads_bodies_and_attachments_from_mime() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    f.state().items.insert(
        "mime:m1".into(),
        plan().detail_with(Some(SOURCE.as_bytes())),
    );
    let threads = download_bodies(&f.engine, &f.ews, f.account.id, &[m1.id], true)
        .await
        .expect("bodies");
    assert!(threads.contains(&m1.thread_id));
    let m1 = f.message("m1").expect("m1");
    assert!(
        m1.body
            .expect("body")
            .html
            .unwrap_or_default()
            .contains("Here is the plan.")
    );
    assert_eq!(m1.in_reply_to.as_deref(), Some("kickoff@example.com"));
    assert!(m1.unsubscribe.is_some());
    let pdf = m1
        .attachments
        .iter()
        .find(|a| a.filename == "plan.pdf")
        .expect("pdf");
    let row = f
        .engine
        .db
        .read(|c| mail::get_attachment(c, pdf.id))
        .expect("row");
    let data = attachment_with(&f.engine, &f.ews, f.account.id, m1.id, &row.part_id)
        .await
        .expect("pdf data");
    assert_eq!(data, b"%PDF-");
    let mime_requests = f
        .requests("GetItem")
        .into_iter()
        .filter(|b| b.contains("IncludeMimeContent"))
        .count();
    assert_eq!(mime_requests, 1, "attachments come from the cached source");

    // Messages the server no longer has are marked, not retried forever.
    let m2 = f.message("m2").expect("m2");
    download_bodies(&f.engine, &f.ews, f.account.id, &[m2.id], false)
        .await
        .expect("a missing message is not an error");
    let state: i64 = f
        .engine
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT body_state FROM messages WHERE id = ?",
                [m2.id],
                |r| r.get(0),
            )?)
        })
        .expect("state");
    assert_eq!(state, 2);
}

fn sent_mime(body: &str) -> String {
    let encoded = capture(
        r#"<t:MimeContent CharacterSet="UTF-8">([^<]*)</t:MimeContent>"#,
        body,
    )
    .expect("MIME content");
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .expect("base64");
    String::from_utf8_lossy(&raw).into_owned()
}

#[tokio::test]
async fn sends_mime_and_keeps_a_copy() {
    let f = fixture().await;
    let mime = b"From: jane@example.com\r\nTo: dana@example.com\r\nSubject: hi\r\n\r\nhello\r\n";
    send_with(
        &f.ews,
        mime,
        &["dana@example.com".into(), "secret@example.com".into()],
    )
    .await
    .expect("sent");
    let sent = f.requests("CreateItem");
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(r#"MessageDisposition="SendAndSaveCopy""#));
    assert!(sent[0].contains(r#"<t:DistinguishedFolderId Id="sentitems"/>"#));
    let text = sent_mime(&sent[0]);
    assert!(
        text.starts_with("Bcc: secret@example.com\r\nFrom: jane@example.com"),
        "{text}"
    );

    f.state().canned.insert(
        "CreateItem".into(),
        response(
            "CreateItem",
            &[failure("CreateItem", "ErrorMessageSizeExceeded")],
        ),
    );
    let err = send_with(&f.ews, mime, &[]).await.expect_err("too large");
    assert!(matches!(err, Error::Invalid(_)), "{err}");
}

#[tokio::test]
async fn uploads_drafts_and_replaces_the_old_copy() {
    let f = fixture().await;
    let session = f.first_sync().await;
    let draft = DraftInput {
        account_id: f.account.id,
        to: vec![Address::new(None, "dana@example.com")],
        bcc: vec![Address::new(None, "hidden@example.com")],
        subject: "Draft".into(),
        html: "<p>Work in progress</p>".into(),
        ..Default::default()
    };
    let draft_id = f
        .engine
        .db
        .write(|tx| {
            let id = misc::draft_save(tx, None, &draft)?;
            misc::draft_set_remote(tx, id, None, Some("old-draft"))?;
            Ok(id)
        })
        .expect("draft");
    session.save_draft(draft_id).await.expect("saved");
    let row = f
        .engine
        .db
        .read(|c| misc::draft_get(c, draft_id))
        .expect("draft row");
    assert_eq!(row.remote_id.as_deref(), Some("new-draft"));
    let created = f.requests("CreateItem");
    assert!(created[0].contains(r#"MessageDisposition="SaveOnly""#));
    let mime = sent_mime(&created[0]);
    assert!(mime.starts_with("Bcc: hidden@example.com\r\n"), "{mime}");
    assert!(mime.contains("Subject: Draft"));
    let deleted = f.requests("DeleteItem");
    assert!(
        deleted
            .iter()
            .any(|b| b.contains(r#"<t:ItemId Id="old-draft"/>"#))
    );
}

// ---------------------------------------------------------------------------
// Calendar

fn calendar_xml(
    id: &str,
    kind: &str,
    uid: &str,
    subject: &str,
    start: &str,
    end: &str,
    recurrence_id: Option<&str>,
    details: bool,
) -> String {
    let all_day = kind == "Holiday";
    let item_type = if all_day { "Single" } else { kind };
    let rid = recurrence_id
        .map(|r| format!("<t:RecurrenceId>{r}</t:RecurrenceId>"))
        .unwrap_or_default();
    let extra = if details {
        format!(
            concat!(
                "<t:ReminderIsSet>true</t:ReminderIsSet><t:ReminderMinutesBeforeStart>10</t:ReminderMinutesBeforeStart>",
                "<t:RequiredAttendees><t:Attendee>{}<t:ResponseType>Unknown</t:ResponseType></t:Attendee>",
                "<t:Attendee>{}<t:ResponseType>Accept</t:ResponseType></t:Attendee></t:RequiredAttendees>",
                r#"<t:StartTimeZone Id="W. Europe Standard Time"/><t:EndTimeZone Id="W. Europe Standard Time"/>"#,
                "<t:Body BodyType=\"Text\">Agenda</t:Body>"
            ),
            mailbox_xml("Jane Doe", "jane@example.com"),
            mailbox_xml("Alice", "alice@example.com"),
        )
    } else {
        String::new()
    };
    let (organizer, response) = if all_day {
        (mailbox_xml("Jane Doe", "jane@example.com"), "Organizer")
    } else {
        (mailbox_xml("Bob Organizer", "bob@example.com"), "Accept")
    };
    format!(
        concat!(
            r#"<t:CalendarItem><t:ItemId Id="{id}" ChangeKey="ck-{id}"/><t:ItemClass>IPM.Appointment</t:ItemClass>"#,
            "<t:Subject>{subject}</t:Subject><t:UID>{uid}</t:UID>{rid}<t:Start>{start}</t:Start><t:End>{end}</t:End>",
            "<t:IsAllDayEvent>{all_day}</t:IsAllDayEvent><t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus>",
            "<t:Location>Room 1</t:Location><t:IsMeeting>{meeting}</t:IsMeeting><t:IsCancelled>false</t:IsCancelled>",
            "<t:IsRecurring>{recurring}</t:IsRecurring><t:CalendarItemType>{item_type}</t:CalendarItemType>",
            "<t:MyResponseType>{response}</t:MyResponseType><t:Organizer>{organizer}</t:Organizer>{extra}</t:CalendarItem>"
        ),
        id = id,
        subject = subject,
        uid = uid,
        rid = rid,
        start = start,
        end = end,
        all_day = all_day,
        meeting = !all_day,
        recurring = !all_day,
        item_type = item_type,
        response = response,
        organizer = organizer,
        extra = extra,
    )
}

fn weekly_master() -> String {
    concat!(
        r#"<t:CalendarItem><t:ItemId Id="master-1" ChangeKey="ck-master"/><t:Subject>Weekly sync</t:Subject><t:UID>weekly</t:UID>"#,
        "<t:Start>2026-07-06T08:00:00Z</t:Start><t:End>2026-07-06T08:30:00Z</t:End><t:IsAllDayEvent>false</t:IsAllDayEvent>",
        "<t:CalendarItemType>RecurringMaster</t:CalendarItemType>",
        "<t:Recurrence><t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Monday</t:DaysOfWeek><t:FirstDayOfWeek>Monday</t:FirstDayOfWeek></t:WeeklyRecurrence>",
        "<t:NoEndRecurrence><t:StartDate>2026-07-06</t:StartDate></t:NoEndRecurrence></t:Recurrence>",
        r#"<t:StartTimeZone Id="W. Europe Standard Time"/></t:CalendarItem>"#
    )
    .to_string()
}

impl Fixture {
    /// A calendar with a weekly meeting (one occurrence moved) and an
    /// all-day holiday.
    fn serve_calendar(&self) {
        let items = [
            (
                "occ-1",
                "Exception",
                "weekly",
                "Weekly sync (moved)",
                "2026-07-06T09:00:00Z",
                "2026-07-06T09:30:00Z",
                Some("2026-07-06T08:00:00Z"),
            ),
            (
                "occ-2",
                "Occurrence",
                "weekly",
                "Weekly sync",
                "2026-07-13T08:00:00Z",
                "2026-07-13T08:30:00Z",
                Some("2026-07-13T08:00:00Z"),
            ),
            (
                "holiday",
                "Holiday",
                "holiday",
                "Company holiday",
                "2026-07-03T22:00:00Z",
                "2026-07-04T22:00:00Z",
                None,
            ),
        ];
        let mut state = self.state();
        let mut view = Vec::new();
        for (id, kind, uid, subject, start, end, rid) in items {
            view.push(calendar_xml(id, kind, uid, subject, start, end, rid, false));
            state.items.insert(
                format!("cal:{id}"),
                calendar_xml(id, kind, uid, subject, start, end, rid, true),
            );
            state.items.insert(format!("master:{id}"), weekly_master());
        }
        state.listings.insert("calendar-id".into(), view);
    }

    fn event_row(&self, calendar_id: i64, remote: &str) -> cdb::EventRow {
        self.engine
            .db
            .read(|c| cdb::rows_by_remote(c, calendar_id, remote))
            .expect("rows")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("event {remote}"))
    }
}

fn ms(y: i32, m: u32, d: u32) -> i64 {
    Utc.with_ymd_and_hms(y, m, d, 0, 0, 0)
        .single()
        .map(|t| t.timestamp_millis())
        .unwrap_or_default()
}

#[tokio::test]
async fn syncs_calendars_and_writes_with_meeting_semantics() {
    let f = fixture().await;
    f.serve_calendar();
    let changed = ews_calendar::sync_with(&f.engine, &f.account, &f.ews)
        .await
        .expect("calendar sync");
    let calendars = f
        .engine
        .db
        .read(|c| cdb::account_calendars(c, f.account.id))
        .expect("calendars");
    let main = calendars
        .iter()
        .find(|c| c.remote_id == "calendar-id")
        .expect("calendar");
    let birthdays = calendars
        .iter()
        .find(|c| c.remote_id == "birthdays-id")
        .expect("birthdays");
    assert_eq!(main.kind, "ews");
    assert!(main.is_default && !main.read_only);
    assert!(birthdays.read_only && !birthdays.is_default);
    assert_eq!(changed, vec![main.id], "only calendars with events changed");

    let moved = f.event_row(main.id, "occ-1");
    assert_eq!(moved.event.summary, "Weekly sync (moved)");
    assert_eq!(
        moved.etag.as_ref().map(String::len),
        Some(24),
        "a view signature"
    );
    assert!(!moved.is_recurring, "occurrences are stored as instances");
    assert_eq!(
        moved.recurrence_id,
        Some(EventTime::Utc {
            at: Utc
                .with_ymd_and_hms(2026, 7, 6, 8, 0, 0)
                .single()
                .expect("t")
        })
    );
    assert!(
        moved
            .event
            .rrule
            .as_deref()
            .is_some_and(|r| r.contains("FREQ=WEEKLY") && r.contains("BYDAY=MO"))
    );
    let me = moved
        .event
        .attendees
        .iter()
        .find(|a| a.email == "jane@example.com")
        .expect("me");
    assert_eq!(me.partstat, PartStat::Accepted);
    assert_eq!(
        moved.event.alarms.first().map(|a| a.minutes_before),
        Some(10)
    );
    assert_eq!(moved.event.description.as_deref(), Some("Agenda"));
    let pd = moved.provider_data.clone().expect("provider data");
    assert_eq!(pd["itemType"], "Exception");
    assert_eq!(pd["masterId"], "master-1");
    assert_eq!(pd["isOrganizer"], false);
    let holiday = f.event_row(main.id, "holiday");
    assert_eq!(
        holiday.event.start,
        EventTime::Date {
            date: NaiveDate::from_ymd_opt(2026, 7, 4).expect("d")
        }
    );
    assert_eq!(
        holiday.event.end,
        EventTime::Date {
            date: NaiveDate::from_ymd_opt(2026, 7, 5).expect("d")
        }
    );

    // Calendars of disabled accounts are not shown.
    f.engine
        .db
        .write(|tx| Ok(tx.execute("UPDATE accounts SET enabled = 1", [])?))
        .expect("enable");
    let occurrences = f
        .engine
        .occurrences(ms(2026, 7, 1), ms(2026, 8, 1))
        .expect("occurrences");
    let titles: Vec<&str> = occurrences.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Company holiday", "Weekly sync (moved)", "Weekly sync"]
    );
    assert!(occurrences[1].is_recurring && !occurrences[0].is_recurring);
    assert_eq!(occurrences[2].my_status, Some(PartStat::Accepted));

    // Unchanged items are not read again.
    let reads = f.requests("GetItem").len();
    ews_calendar::sync_with(&f.engine, &f.account, &f.ews)
        .await
        .expect("second sync");
    assert_eq!(f.requests("GetItem").len(), reads);

    // A change the calendar view shows is read even when the occurrence's
    // change key stays the same.
    {
        let mut state = f.state();
        let rename = |xml: &mut String| {
            *xml = xml.replace(
                "<t:Subject>Weekly sync</t:Subject>",
                "<t:Subject>Weekly sync (renamed)</t:Subject>",
            );
        };
        if let Some(view) = state.listings.get_mut("calendar-id") {
            view.iter_mut().for_each(rename);
        }
        if let Some(detail) = state.items.get_mut("cal:occ-2") {
            rename(detail);
        }
    }
    ews_calendar::sync_with(&f.engine, &f.account, &f.ews)
        .await
        .expect("third sync");
    assert_eq!(
        f.event_row(main.id, "occ-2").event.summary,
        "Weekly sync (renamed)"
    );
    assert_eq!(
        f.event_row(main.id, "occ-1").event.summary,
        "Weekly sync (moved)",
        "the other occurrence is untouched"
    );

    // Answering an occurrence answers the series.
    let occurrence = f.event_row(main.id, "occ-2");
    f.engine
        .respond_event(occurrence.id, PartStat::Declined, Some("Away".into()))
        .await
        .expect("respond");
    let answers = f.requests("CreateItem");
    let decline = answers
        .iter()
        .find(|b| b.contains("<t:DeclineItem>"))
        .expect("decline");
    assert!(
        decline.contains(r#"<t:ReferenceItemId Id="master-1"/>"#),
        "{decline}"
    );
    assert!(decline.contains(r#"MessageDisposition="SendAndSaveCopy""#));

    // Deleting someone else's meeting declines it; deleting one's own
    // appointment deletes it.
    f.engine
        .delete_event(
            occurrence.id,
            occurrence.recurrence_id.clone(),
            EditScope::This,
        )
        .await
        .expect("delete occurrence");
    let declines = f
        .requests("CreateItem")
        .iter()
        .filter(|b| b.contains(r#"<t:DeclineItem><t:ReferenceItemId Id="occ-2"/>"#))
        .count();
    assert_eq!(declines, 1);
    f.engine
        .delete_event(holiday.id, None, EditScope::All)
        .await
        .expect("delete holiday");
    let deletes = f.requests("DeleteItem");
    assert!(
        deletes
            .iter()
            .any(|b| b.contains(r#"<t:ItemId Id="holiday"/>"#)
                && b.contains(r#"SendMeetingCancellations="SendToNone""#))
    );

    // Moving one occurrence of one's own series changes just its times.
    let weekly = f.event_row(main.id, "occ-1");
    f.engine
        .db
        .write(|tx| {
            let mut pd = weekly.provider_data.clone().unwrap_or_default();
            pd["isOrganizer"] = json!(true);
            tx.execute(
                "UPDATE events SET provider_data = ? WHERE id = ?",
                rusqlite::params![pd.to_string(), weekly.id],
            )?;
            Ok(())
        })
        .expect("organizer");
    f.engine
        .move_event(
            weekly.id,
            weekly.recurrence_id.clone(),
            EditScope::This,
            weekly.start_ts + 3_600_000,
            weekly.end_ts + 3_600_000,
        )
        .await
        .expect("move");
    let moved_times = f.requests("UpdateItem");
    let times = moved_times
        .iter()
        .find(|b| b.contains("calendar:Start"))
        .expect("times update");
    assert!(
        times.contains("<t:Start>2026-07-06T10:00:00Z</t:Start>"),
        "{times}"
    );
    assert!(times.contains(r#"SendMeetingInvitationsOrCancellations="SendToAllAndSaveCopy""#));

    // New meetings invite their attendees (but not the organizer).
    let input = EventInput {
        calendar_id: main.id,
        summary: "Planning".into(),
        description: None,
        location: None,
        all_day: false,
        start: ms(2026, 7, 20) + 9 * 3_600_000,
        end: ms(2026, 7, 20) + 10 * 3_600_000,
        start_date: None,
        end_date: None,
        timezone: Some("Europe/Berlin".into()),
        rrule: None,
        attendees: vec![Address::new(Some("Bob".into()), "bob@example.com")],
        alarms: vec![15],
        color: None,
        transparent: false,
        add_conference: false,
    };
    f.engine.create_event(input).await.expect("create");
    let created = f
        .requests("CreateItem")
        .into_iter()
        .find(|b| b.contains("<t:CalendarItem>"))
        .expect("create");
    assert!(created.contains(r#"SendMeetingInvitations="SendToAllAndSaveCopy""#));
    assert!(created.contains("<t:EmailAddress>bob@example.com</t:EmailAddress>"));
    assert!(!created.contains("<t:EmailAddress>jane@example.com</t:EmailAddress>"));
    assert!(created.contains(r#"<t:StartTimeZone Id="W. Europe Standard Time"/>"#));
}

#[tokio::test]
async fn answers_meeting_requests_through_exchange() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    f.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE messages SET calendar_method = 'REQUEST' WHERE id = ?",
                [m1.id],
            )?;
            Ok(())
        })
        .expect("request");
    f.engine
        .respond_invite(m1.id, PartStat::Accepted, None)
        .await
        .expect("respond");
    let accept = f
        .requests("CreateItem")
        .into_iter()
        .find(|b| b.contains("<t:AcceptItem>"))
        .expect("accept");
    assert!(accept.contains(r#"<t:ReferenceItemId Id="m1"/>"#));
    // The calendar is read again to show the meeting.
    assert!(
        f.requests("FindItem")
            .iter()
            .any(|b| b.contains("<m:CalendarView"))
    );
}

#[tokio::test]
async fn proposes_new_times_through_exchange_2013_sp1_and_later() {
    let f = fixture().await;
    let _session = f.first_sync().await;
    let m1 = f.message("m1").expect("m1");
    let set = |sql: &str, id: i64| {
        f.engine
            .db
            .write(|tx| {
                tx.execute(sql, [id])?;
                Ok(())
            })
            .expect("update")
    };
    set(
        "UPDATE messages SET calendar_method = 'REQUEST' WHERE id = ?",
        m1.id,
    );
    let at = |h: u32| Utc.with_ymd_and_hms(2026, 11, 5, h, 0, 0).unwrap();
    // The account speaks the default schema, Exchange 2013 SP1.
    assert!(ews_calendar::takes_proposals(&f.account));
    for (status, element) in [
        (PartStat::Tentative, "<t:TentativelyAcceptItem>"),
        (PartStat::Declined, "<t:DeclineItem>"),
    ] {
        let proposed = ews_calendar::propose_for_request(
            &f.engine,
            m1.id,
            status,
            at(11),
            at(12),
            Some("Could we meet later?"),
        )
        .await
        .expect("propose");
        assert!(proposed);
        let sent = f
            .requests("CreateItem")
            .into_iter()
            .find(|b| b.contains(element))
            .expect("answer");
        assert!(sent.contains(r#"MessageDisposition="SendAndSaveCopy""#));
        assert!(sent.contains(r#"<t:ReferenceItemId Id="m1"/><t:ProposedStart>2026-11-05T11:00:00Z</t:ProposedStart><t:ProposedEnd>2026-11-05T12:00:00Z</t:ProposedEnd>"#), "{sent}");
        assert!(sent.contains("Could we meet later?"));
    }
    // Messages that aren't meeting requests go the iCalendar way.
    let m2 = f.message("m2").expect("m2");
    set(
        "UPDATE messages SET calendar_method = 'CANCEL' WHERE id = ?",
        m2.id,
    );
    assert!(
        !ews_calendar::propose_for_request(
            &f.engine,
            m2.id,
            PartStat::Tentative,
            at(11),
            at(12),
            None
        )
        .await
        .expect("not a request")
    );
    // Exchange 2010 doesn't take proposals: nothing is sent to it.
    set(
        "UPDATE accounts SET config = json_set(config, '$.exchange.version', 'Exchange2010_SP2') WHERE id = ?",
        f.account.id,
    );
    let account = f.engine.account_row(f.account.id).expect("account");
    assert!(!ews_calendar::takes_proposals(&account));
    let before = f.requests("CreateItem").len();
    assert!(
        ews_calendar::propose_for_request(
            &f.engine,
            m1.id,
            PartStat::Tentative,
            at(11),
            at(12),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(f.requests("CreateItem").len(), before);
}

// ---------------------------------------------------------------------------
// The worker

fn stream(events: &str) -> String {
    let message = |inner: &str| {
        format!(
            r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><soap11:Body xmlns:soap11="http://schemas.xmlsoap.org/soap/envelope/"><m:GetStreamingEventsResponse {NS}><m:ResponseMessages><m:GetStreamingEventsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:GetStreamingEventsResponseMessage></m:ResponseMessages></m:GetStreamingEventsResponse></soap11:Body></Envelope>"#
        )
    };
    [
        message("<m:ConnectionStatus>OK</m:ConnectionStatus>"),
        message(&format!(
            "<m:Notifications><m:Notification><t:SubscriptionId>sub-1</t:SubscriptionId>{events}</m:Notification></m:Notifications>"
        )),
        message("<m:ConnectionStatus>Closed</m:ConnectionStatus>"),
    ]
    .concat()
}

#[tokio::test]
async fn the_worker_syncs_follows_notifications_replays_changes_and_stops() {
    let f = fixture().await;
    for msg in [plan(), deals()] {
        f.serve(&msg);
    }
    f.state()
        .listings
        .insert("inbox-id".into(), vec![plan().listed(), deals().listed()]);
    f.round(
        "inbox-id",
        None,
        &[create(plan().change()), create(deals().change())],
        "inbox-1",
    );
    f.state().canned.insert(
        "Subscribe".into(),
        response(
            "Subscribe",
            &[success(
                "Subscribe",
                "<m:SubscriptionId>sub-1</m:SubscriptionId>",
            )],
        ),
    );
    f.state().canned.insert(
        "GetStreamingEvents".into(),
        stream(r#"<t:NewMailEvent><t:TimeStamp>2026-10-03T11:00:00Z</t:TimeStamp><t:ItemId Id="m4" ChangeKey="ck"/><t:ParentFolderId Id="inbox-id"/></t:NewMailEvent>"#),
    );
    let wake = Arc::new(Notify::new());
    let stop = CancellationToken::new();
    let worker = tokio::spawn({
        let engine = f.engine.clone();
        let account = f.account.clone();
        let ews = f.ews.clone();
        let wake = wake.clone();
        let stop = stop.clone();
        async move { run_once(&engine, account, ews, &wake, &stop).await.is_ok() }
    });
    let synced = async {
        while f.message("m2").is_none() || f.engine.sync_state(f.account.id) != SyncState::Idle {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), synced)
        .await
        .expect("first sync");
    assert!(
        !f.requests("Subscribe").is_empty(),
        "notifications are subscribed"
    );
    // The notification names the inbox: it is looked at again right away.
    let inbox_syncs = || {
        f.requests("SyncFolderItems")
            .iter()
            .filter(|b| b.contains(r#"<m:SyncFolderId><t:FolderId Id="inbox-id"/>"#))
            .count()
    };
    let followed = async {
        while inbox_syncs() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), followed)
        .await
        .expect("notification followed");

    // A local change wakes the worker, which replays it right away.
    let m1 = f.message("m1").expect("m1");
    f.engine
        .set_flags(
            &[],
            &[m1.id],
            &FlagChange {
                seen: Some(true),
                flagged: None,
            },
        )
        .expect("flags");
    wake.notify_one();
    let replayed = async {
        while !f
            .engine
            .db
            .read(|c| misc::queued_ops(c, f.account.id))
            .expect("ops")
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), replayed)
        .await
        .expect("replayed");
    assert!(
        f.requests("UpdateItem")
            .iter()
            .any(|b| b.contains("<t:IsRead>true</t:IsRead>"))
    );
    stop.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("stops")
        .expect("no panic");
    assert!(ended);
}

/// The fake server behind Basic authentication only.
struct BasicOnly(FakeExchange);

impl wiremock::Respond for BasicOnly {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let basic = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Basic "));
        if !basic {
            return ResponseTemplate::new(401)
                .insert_header("www-authenticate", "Basic realm=\"mail.example.com\"");
        }
        self.0.respond(request)
    }
}

#[tokio::test]
async fn automatic_sign_in_falls_back_to_basic() {
    let f = fixture().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .respond_with(BasicOnly(f.fake.clone()))
        .mount(&server)
        .await;
    let url = format!("{}{EWS_PATH}", server.uri());
    let config = AccountConfig {
        auth: AuthMethod::Password,
        exchange: Some(ExchangeConfig {
            ews_url: url.clone(),
            auth: ExchangeAuth::Auto,
            ..ExchangeConfig::default()
        }),
        ..AccountConfig::default()
    };
    f.engine
        .db
        .write(|tx| {
            tx.execute(
                "UPDATE accounts SET config = ? WHERE id = ?",
                rusqlite::params![serde_json::to_string(&config)?, f.account.id],
            )?;
            Ok(())
        })
        .expect("config");
    for msg in [plan(), deals()] {
        f.serve(&msg);
    }
    f.state()
        .listings
        .insert("inbox-id".into(), vec![plan().listed(), deals().listed()]);
    let wake = Arc::new(Notify::new());
    let stop = CancellationToken::new();
    let worker = tokio::spawn(run(
        f.engine.clone(),
        f.account.id,
        wake.clone(),
        stop.clone(),
    ));
    let synced = async {
        while f.message("m2").is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), synced)
        .await
        .expect("synced with Basic");
    assert!(basic_only().lock().contains(&(f.account.id, url)));
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("stops")
        .expect("no panic");
}

// ---------------------------------------------------------------------------
// Adding accounts

fn credentials(auth: ExchangeAuth, password: &str) -> Credentials {
    Credentials {
        email: "jane@example.com".into(),
        username: "CORP\\jane".into(),
        password: password.into(),
        auth,
        accept_invalid_certs: false,
    }
}

async fn basic_only_server() -> MockServer {
    let server = MockServer::start().await;
    let token = base64::engine::general_purpose::STANDARD.encode("CORP\\jane:secret");
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .and(header("authorization", format!("Basic {token}").as_str()))
        .respond_with(soap(response(
            "GetFolder",
            &[success(
                "GetFolder",
                r#"<m:Folders><t:Folder><t:FolderId Id="inbox-id"/><t:DisplayName>Inbox</t:DisplayName></t:Folder></m:Folders>"#,
            )],
        )))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("www-authenticate", "Basic realm=\"mail.example.com\""),
        )
        .with_priority(5)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn verifies_the_sign_in_and_falls_back_to_basic() {
    let server = basic_only_server().await;
    let url = format!("{}{EWS_PATH}", server.uri());
    let (found, version) = verify(&credentials(ExchangeAuth::Auto, "secret"), &url, None)
        .await
        .expect("signed in with Basic");
    assert_eq!(found, url);
    assert_eq!(
        version,
        Some(ews::ExchangeVersion::Exchange2013Sp1),
        "keeps the schema version that worked"
    );
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().any(|r| r
            .headers
            .get("authorization")
            .is_some_and(|v| v.to_str().is_ok_and(|v| v.starts_with("NTLM ")))),
        "NTLM is tried first"
    );

    let wrong = verify(&credentials(ExchangeAuth::Auto, "wrong"), &url, None)
        .await
        .expect_err("wrong password");
    assert!(wrong.is_auth());
    assert!(
        wrong
            .to_string()
            .contains("did not accept the user name or password"),
        "{wrong}"
    );

    let ntlm_only = verify(&credentials(ExchangeAuth::Ntlm, "secret"), &url, None)
        .await
        .expect_err("no NTLM");
    assert!(ntlm_only.is_auth());
    assert!(
        ntlm_only
            .to_string()
            .contains("does not offer a sign-in method"),
        "{ntlm_only}"
    );

    let missing = verify(
        &credentials(ExchangeAuth::Basic, "secret"),
        &format!("{}/owa/", server.uri()),
        None,
    )
    .await
    .expect_err("not EWS");
    assert!(
        matches!(missing, Error::Auth(_) | Error::Invalid(_)),
        "{missing}"
    );
}

#[tokio::test]
async fn finds_the_server_with_autodiscover() {
    let server = basic_only_server().await;
    let settings =
        include_str!("../../../../envelope-ews/tests/fixtures/autodiscover_settings.xml")
            // Outside the office the external address does not answer; the
            // internal one does.
            .replace("{{external}}", "http://127.0.0.1:9/EWS/Exchange.asmx")
            .replace("{{internal}}", &format!("{}{EWS_PATH}", server.uri()));
    Mock::given(method("POST"))
        .and(path("/autodiscover/autodiscover.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/xml; charset=utf-8")
                .set_body_string(settings),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    let endpoint =
        ews::Url::parse(&format!("{}/autodiscover/autodiscover.xml", server.uri())).expect("url");
    let endpoints = |_: &str| vec![ews::AutodiscoverEndpoint::Pox(endpoint.clone())];
    let creds = credentials(ExchangeAuth::Basic, "secret");
    let found = connect_with(&creds, None, &endpoints).await.expect("found");
    assert_eq!(found.url, format!("{}{EWS_PATH}", server.uri()));
    assert_eq!(found.display_name.as_deref(), Some("Jane Doe"));
    assert_eq!(found.version, Some(ews::ExchangeVersion::Exchange2016));
    let config = account_config(&creds, &found);
    let exchange = config.exchange.expect("exchange settings");
    assert_eq!(exchange.ews_url, found.url);
    assert_eq!(exchange.username, "CORP\\jane");
    assert_eq!(exchange.auth, ExchangeAuth::Basic);
    assert_eq!(exchange.version.as_deref(), Some("Exchange2016"));
    assert_eq!(config.auth, AuthMethod::Password);

    // The e-mail address is the default login name and is not stored.
    let by_email = Credentials {
        username: "Jane@Example.com".into(),
        ..credentials(ExchangeAuth::Auto, "secret")
    };
    let found = Found {
        url: "https://mail.example.com/EWS/Exchange.asmx".into(),
        version: None,
        display_name: None,
    };
    assert_eq!(
        account_config(&by_email, &found)
            .exchange
            .map(|e| e.username),
        Some(String::new())
    );

    // A wrong password is a sign-in error, not a missing server.
    let wrong = connect_with(&credentials(ExchangeAuth::Basic, "wrong"), None, &endpoints)
        .await
        .expect_err("rejected");
    assert!(wrong.is_auth(), "{wrong}");
    // Nothing answering is reported as a server that cannot be found.
    let nowhere = |_: &str| {
        vec![ews::AutodiscoverEndpoint::Pox(
            ews::Url::parse("http://127.0.0.1:9/autodiscover/autodiscover.xml").expect("url"),
        )]
    };
    let missing = connect_with(&creds, None, &nowhere)
        .await
        .expect_err("not found");
    assert!(matches!(missing, Error::NotFound(_)), "{missing}");
    assert!(missing.to_string().contains("Advanced"), "{missing}");
}

#[tokio::test]
async fn a_pasted_web_address_leads_to_ews() {
    let server = basic_only_server().await;
    let found = connect(
        &credentials(ExchangeAuth::Basic, "secret"),
        Some(&format!("{}/owa/", server.uri())),
    )
    .await
    .expect("found next to Outlook on the web");
    assert_eq!(found.url, format!("{}{EWS_PATH}", server.uri()));
}

#[tokio::test]
async fn explains_what_is_wrong_with_a_server_address() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let url = format!("{}/custom/ews", server.uri());
    let missing = verify(&credentials(ExchangeAuth::Basic, "secret"), &url, None)
        .await
        .expect_err("404");
    assert!(matches!(missing, Error::Invalid(_)));
    assert!(
        missing.to_string().contains("/EWS/Exchange.asmx"),
        "{missing}"
    );

    let unreachable = verify(
        &credentials(ExchangeAuth::Basic, "secret"),
        "http://127.0.0.1:9/EWS/Exchange.asmx",
        None,
    )
    .await
    .expect_err("nothing listens");
    assert!(unreachable.is_transient(), "{unreachable}");
}

#[tokio::test]
async fn probes_autodiscover_without_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/exchange/autodiscover/autodiscover.xml"))
        .respond_with(
            ResponseTemplate::new(401)
                .append_header("www-authenticate", "Negotiate")
                .append_header("www-authenticate", "NTLM"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cpanel/autodiscover/autodiscover.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<Autodiscover/>"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header_regex("authorization", ".+"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(
        probe_url(&format!(
            "{}/exchange/autodiscover/autodiscover.xml",
            server.uri()
        ))
        .await
    );
    assert!(
        !probe_url(&format!(
            "{}/cpanel/autodiscover/autodiscover.xml",
            server.uri()
        ))
        .await
    );
    assert!(!probe_url("http://127.0.0.1:9/autodiscover/autodiscover.xml").await);
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("authorization"))
    );
}

// ---------------------------------------------------------------------------
// Folder and category management

impl Fixture {
    fn folder_rows(&self) -> Vec<FolderRow> {
        self.engine
            .db
            .read(|c| mail::list_folder_rows(c, self.account.id))
            .expect("folders")
    }

    fn answer(&self, op: &str, inner: &str) {
        self.state()
            .canned
            .insert(op.to_string(), response(op, &[success(op, inner)]));
    }
}

/// The master category list a `UpdateUserConfiguration` request stores.
fn stored_categories(body: &str) -> String {
    let data = capture(r"<t:XmlData>([^<]*)</t:XmlData>", body).expect("XmlData");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .expect("base64");
    String::from_utf8(bytes).expect("utf-8")
}

#[tokio::test]
async fn exchange_folders_are_created_renamed_moved_and_deleted() {
    let f = fixture().await;
    let mut session = f.session();
    session.reload_folders().expect("folders");
    session.sync_folder_list().await.expect("folder list");
    let projects = f.folder("projects-id");

    f.answer(
        "CreateFolder",
        r#"<m:Folders><t:Folder><t:FolderId Id="clients-id" ChangeKey="ck1"/></t:Folder></m:Folders>"#,
    );
    let id = f
        .engine
        .ews_create_folder(
            &f.ews,
            f.account.id,
            &f.folder_rows(),
            Some(&projects),
            "Clients",
        )
        .await
        .expect("create");
    let clients = f.folder("clients-id");
    assert_eq!(clients.id, id);
    assert_eq!(clients.path, format!("{}/Clients", projects.path));
    let body = f.requests("CreateFolder").pop().expect("request");
    assert!(
        body.contains("<t:DisplayName>Clients</t:DisplayName>") && body.contains("projects-id")
    );

    f.answer(
        "UpdateFolder",
        r#"<m:Folders><t:Folder><t:FolderId Id="clients-id" ChangeKey="ck2"/></t:Folder></m:Folders>"#,
    );
    f.engine
        .ews_rename_folder(&f.ews, &f.folder_rows(), &clients, "Customers")
        .await
        .expect("rename");
    assert_eq!(f.folder("clients-id").name, "Customers");

    f.answer(
        "MoveFolder",
        r#"<m:Folders><t:Folder><t:FolderId Id="clients-id" ChangeKey="ck3"/></t:Folder></m:Folders>"#,
    );
    f.engine
        .ews_move_folder(&f.ews, &f.folder_rows(), &f.folder("clients-id"), None)
        .await
        .expect("to the top");
    let top = f.folder("clients-id");
    assert_eq!((top.parent_id, top.path.as_str()), (None, "Customers"));
    assert!(f.requests("MoveFolder")[0].contains("msgfolderroot"));

    f.answer("DeleteFolder", "");
    let trash = f.folder("deleteditems-id");
    f.engine
        .ews_delete_folder(&f.ews, &top, Some(&trash))
        .await
        .expect("delete");
    assert!(f.requests("DeleteFolder")[0].contains(r#"DeleteType="MoveToDeletedItems""#));
    let deleted = f.folder("clients-id");
    assert_eq!(
        deleted.parent_id,
        Some(trash.id),
        "it waits in Deleted Items"
    );
}

#[tokio::test]
async fn exchange_categories_change_in_the_master_list() {
    let f = fixture().await;
    f.answer("UpdateUserConfiguration", "");
    let changed = ews_sync_edit(&f, "blue CATEGORY", CategoryEdit::Recolor(4)).await;
    assert!(changed);
    let stored = stored_categories(&f.requests("UpdateUserConfiguration")[0]);
    assert!(
        stored.contains(r#"<category name="Blue category" color="4"/>"#),
        "{stored}"
    );
    assert!(
        stored.contains(r#"<category name="Red category" color="0"/>"#),
        "{stored}"
    );

    assert!(ews_sync_edit(&f, "Red category", CategoryEdit::Remove).await);
    let stored = stored_categories(&f.requests("UpdateUserConfiguration")[1]);
    assert!(
        !stored.contains("Red category") && stored.contains("Blue category"),
        "{stored}"
    );

    // Only on messages: nothing to change in the list.
    assert!(!ews_sync_edit(&f, "Somewhere else", CategoryEdit::Remove).await);
    assert_eq!(f.requests("UpdateUserConfiguration").len(), 2);

    // A refusal is reported.
    f.state().canned.insert(
        "UpdateUserConfiguration".into(),
        response(
            "UpdateUserConfiguration",
            &[failure("UpdateUserConfiguration", "ErrorAccessDenied")],
        ),
    );
    assert!(
        super::edit_category(&f.ews, "Blue category", CategoryEdit::Recolor(1))
            .await
            .is_err()
    );
}

async fn ews_sync_edit(f: &Fixture, name: &str, edit: CategoryEdit) -> bool {
    super::edit_category(&f.ews, name, edit)
        .await
        .expect("edit")
}

#[test]
fn category_list_edits_keep_everything_else() {
    let xml = r#"<?xml version="1.0"?><categories default="Red category" xmlns="CategoryList.xsd"><category name="Red category" color="0" keyboardShortcut="0" guid="{1}"><extra a="b"/></category><category name="Plain" guid="{2}"/></categories>"#;
    let removed =
        mapping::edit_category_list(xml, "red category", CategoryEdit::Remove).expect("removed");
    assert_eq!(
        removed,
        r#"<?xml version="1.0"?><categories default="Red category" xmlns="CategoryList.xsd"><category name="Plain" guid="{2}"/></categories>"#
    );
    let colored =
        mapping::edit_category_list(xml, "Plain", CategoryEdit::Recolor(7)).expect("colored");
    assert!(
        colored.contains(r#"<category name="Plain" guid="{2}" color="7"/>"#),
        "{colored}"
    );
    assert!(colored.contains(r#"keyboardShortcut="0" guid="{1}"><extra a="b"/></category>"#));
    assert_eq!(
        mapping::edit_category_list(xml, "Nope", CategoryEdit::Remove),
        None
    );
}

#[tokio::test]
async fn exchange_folders_are_emptied_with_their_subfolders() {
    let f = fixture().await;
    f.answer("EmptyFolder", "");
    super::empty_folder(&f.ews, "deleteditems-id")
        .await
        .expect("empty");
    let body = f.requests("EmptyFolder").pop().expect("request");
    assert!(
        body.contains(r#"DeleteSubFolders="true""#) && body.contains("deleteditems-id"),
        "{body}"
    );
}
