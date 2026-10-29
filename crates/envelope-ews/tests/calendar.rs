//! Calendar views, calendar items with iCalendar content, event writes
//! and meeting responses against recorded EWS responses.

mod common;

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use common::{bodies, on, requests, setup, soap_ok};
use envelope_ews::{
    CalendarItemType, CalendarItemUpdate, DayOfWeek, DeleteType, DistinguishedFolder, EventTiming,
    FreeBusyStatus, GetCalendarItemsOptions, ItemId, ItemRef, Mailbox, MeetingResponse,
    NewCalendarItem, Recurrence, RecurrencePattern, RecurrenceRange, ResponseType,
    SendCancellations, SendInvitations, SendUpdates,
};
use wiremock::ResponseTemplate;
use wiremock::matchers::{body_string_contains, method};

#[tokio::test]
async fn calendar_view_returns_sorted_occurrences() {
    let (server, client) = setup().await;
    on(&server, "<m:CalendarView", soap_ok("calendar_view.xml")).await;
    let start = Utc.with_ymd_and_hms(2024, 7, 1, 0, 0, 0).single().unwrap();
    let end = Utc.with_ymd_and_hms(2024, 8, 1, 0, 0, 0).single().unwrap();
    let items = client
        .calendar_view(DistinguishedFolder::Calendar, start, end)
        .await
        .unwrap();
    let subjects: Vec<(&str, Option<CalendarItemType>)> = items
        .iter()
        .map(|i| (i.subject.as_deref().unwrap_or_default(), i.item_type))
        .collect();
    assert_eq!(
        subjects,
        [
            ("Weekly sync", Some(CalendarItemType::Exception)),
            ("Company holiday", Some(CalendarItemType::Single)),
            ("Weekly sync", Some(CalendarItemType::Occurrence)),
        ]
    );
    assert_eq!(
        items[0].uid, items[2].uid,
        "occurrences share the series UID"
    );
    assert_eq!(items[0].recurrence_id, items[0].start);
    assert_eq!(items[1].is_all_day, Some(true));
    assert_eq!(items[0].my_response_type, Some(ResponseType::Accept));
    assert_eq!(items[0].free_busy, Some(FreeBusyStatus::Busy));
    let body = &bodies(&server).await[0];
    assert!(body.contains(r#"StartDate="2024-07-01T00:00:00Z" EndDate="2024-08-01T00:00:00Z""#));
    assert!(body.contains(r#"<t:FieldURI FieldURI="calendar:UID"/>"#));
}

#[tokio::test]
async fn truncated_calendar_views_are_split() {
    let (server, client) = setup().await;
    let truncated = common::fixture("calendar_view.xml").replace(
        r#"IncludesLastItemInRange="true""#,
        r#"IncludesLastItemInRange="false""#,
    );
    // The full month is truncated; each half fits.
    wiremock::Mock::given(method("POST"))
        .and(body_string_contains(
            r#"StartDate="2024-07-01T00:00:00Z" EndDate="2024-08-01T00:00:00Z""#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(truncated))
        .mount(&server)
        .await;
    on(&server, "<m:CalendarView", soap_ok("calendar_view.xml")).await;
    let start = Utc.with_ymd_and_hms(2024, 7, 1, 0, 0, 0).single().unwrap();
    let end = Utc.with_ymd_and_hms(2024, 8, 1, 0, 0, 0).single().unwrap();
    let items = client
        .calendar_view(DistinguishedFolder::Calendar, start, end)
        .await
        .unwrap();
    // Both halves return the same three items; duplicates are dropped.
    assert_eq!(items.len(), 3);
    assert_eq!(requests(&server).await.len(), 3);
    assert!(
        client
            .calendar_view(DistinguishedFolder::Calendar, end, start)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn calendar_items_come_with_ics_that_envelope_ical_understands() {
    let (server, client) = setup().await;
    on(&server, "<m:GetItem>", soap_ok("get_item_calendar.xml")).await;
    let item = client
        .get_calendar_item(
            ItemRef::MasterOf(ItemId::new("AAMkADk1-occ-1")),
            &GetCalendarItemsOptions {
                include_mime_content: true,
                ..GetCalendarItemsOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(item.item_type, Some(CalendarItemType::RecurringMaster));
    assert_eq!(
        item.start_time_zone.as_deref(),
        Some("W. Europe Standard Time")
    );
    assert_eq!(item.start_tz(), Some(Tz::Europe__Berlin));
    assert_eq!(
        item.required_attendees[0].response,
        Some(ResponseType::Accept)
    );
    assert_eq!(
        item.optional_attendees[0].response,
        Some(ResponseType::NoResponseReceived)
    );
    assert_eq!(item.sequence, Some(2));
    assert_eq!(item.reminder_minutes_before_start, Some(15));
    assert_eq!(item.modified_occurrences.len(), 1);
    assert_eq!(item.deleted_occurrences.len(), 1);
    let recurrence = item.recurrence.as_ref().unwrap();
    assert_eq!(
        recurrence.to_rrule(item.start_tz(), false),
        "FREQ=WEEKLY;BYDAY=MO;WKST=MO"
    );

    let ics = item.ics().unwrap();
    let calendar = envelope_ical::parse_calendar(&ics).expect("valid iCalendar");
    let event = &calendar.events[0];
    assert_eq!(event.summary, "Weekly sync");
    assert_eq!(Some(event.uid.as_str()), item.uid.as_deref());
    // The Windows TZID resolves to Berlin: 10:00 local is 08:00 UTC.
    let start_utc = envelope_ical::to_utc(&event.start, &calendar.tz, Tz::UTC);
    assert_eq!(Some(start_utc), item.start);

    let body = &bodies(&server).await[0];
    assert!(body.contains(r#"<t:RecurringMasterItemId OccurrenceId="AAMkADk1-occ-1"/>"#));
    assert!(body.contains(r#"<t:FieldURI FieldURI="calendar:StartTimeZone"/>"#));
}

#[tokio::test]
async fn creates_updates_and_deletes_meetings() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:CreateItem SendMeetingInvitations",
        soap_ok("create_item_calendar.xml"),
    )
    .await;
    on(
        &server,
        "<m:UpdateItem",
        soap_ok("update_item_calendar.xml"),
    )
    .await;
    on(&server, "<m:DeleteItem", soap_ok("delete_item.xml")).await;

    let berlin: Tz = "Europe/Berlin".parse().unwrap();
    let mut meeting = NewCalendarItem::new(
        "Planning",
        EventTiming::Timed {
            start: Utc.with_ymd_and_hms(2024, 7, 1, 8, 0, 0).single().unwrap(),
            end: Utc.with_ymd_and_hms(2024, 7, 1, 9, 0, 0).single().unwrap(),
            time_zone: Some(berlin),
        },
    );
    meeting.required_attendees = vec![Mailbox::with_name("Alice", "alice@example.com")];
    meeting.recurrence = Some(
        Recurrence::from_rrule(
            "FREQ=WEEKLY;BYDAY=MO;COUNT=4",
            NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(),
        )
        .unwrap(),
    );
    let id = client
        .create_calendar_item(
            DistinguishedFolder::Calendar,
            &meeting,
            SendInvitations::SendToAllAndSaveCopy,
        )
        .await
        .unwrap();
    assert_eq!(id.id, "AAMkADk1-new-event");

    let moved = client
        .update_calendar_item(
            id.clone(),
            &CalendarItemUpdate {
                timing: Some(EventTiming::AllDay {
                    start: NaiveDate::from_ymd_opt(2024, 7, 2).unwrap(),
                    end: NaiveDate::from_ymd_opt(2024, 7, 3).unwrap(),
                    time_zone: Some(berlin),
                }),
                location: Some(Some("Offsite".into())),
                ..CalendarItemUpdate::default()
            },
            SendUpdates::SendToAllAndSaveCopy,
        )
        .await
        .unwrap();
    assert_eq!(moved.change_key.as_deref(), Some("DwAAABYAAAD-v2"));

    client
        .delete_calendar_item(
            moved,
            DeleteType::MoveToDeletedItems,
            SendCancellations::SendToAllAndSaveCopy,
        )
        .await
        .unwrap();

    let bodies = bodies(&server).await;
    let create = &bodies[0];
    assert!(create.contains(r#"<m:CreateItem SendMeetingInvitations="SendToAllAndSaveCopy">"#));
    assert!(create.contains("<t:Start>2024-07-01T08:00:00Z</t:Start>"));
    assert!(create.contains(r#"<t:StartTimeZone Id="W. Europe Standard Time"/>"#));
    assert!(create.contains("<t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Monday</t:DaysOfWeek></t:WeeklyRecurrence>"));
    assert!(create.contains("<t:NumberedRecurrence><t:StartDate>2024-07-01</t:StartDate><t:NumberOfOccurrences>4</t:NumberOfOccurrences></t:NumberedRecurrence>"));
    let update = &bodies[1];
    assert!(update.contains(r#"SendMeetingInvitationsOrCancellations="SendToAllAndSaveCopy""#));
    // Berlin midnight on July 2nd is 22:00 UTC the day before.
    assert!(update.contains("<t:Start>2024-07-01T22:00:00Z</t:Start>"));
    assert!(update.contains("<t:IsAllDayEvent>true</t:IsAllDayEvent>"));
    let zone = update.find("calendar:StartTimeZone").unwrap();
    let start = update.find("calendar:Start\"").unwrap();
    assert!(zone < start, "time zone is set before the start");
    assert!(bodies[2].contains(r#"SendMeetingCancellations="SendToAllAndSaveCopy""#));

    assert!(matches!(
        RecurrencePattern::Weekly {
            interval: 1,
            days: vec![DayOfWeek::Monday],
            first_day_of_week: None
        },
        RecurrencePattern::Weekly { .. }
    ));
    let _ = RecurrenceRange::NoEnd {
        start: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
    };
}

#[tokio::test]
async fn answers_meeting_requests() {
    let (server, client) = setup().await;
    on(&server, "<t:AcceptItem>", soap_ok("respond_accept.xml")).await;
    on(&server, "<t:DeclineItem>", soap_ok("create_item_sent.xml")).await;
    let accepted = client
        .respond_to_meeting(
            &ItemId::new("AAMkADk1-mr-1"),
            MeetingResponse::Accept,
            Some("See you there"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(accepted.map(|id| id.id), Some("AAMkADk1-cal-1".to_owned()));
    let declined = client
        .respond_to_meeting(
            &ItemId::new("AAMkADk1-mr-2"),
            MeetingResponse::Decline,
            None,
            false,
        )
        .await
        .unwrap();
    assert!(declined.is_none());
    let bodies = bodies(&server).await;
    assert!(bodies[0].contains(r#"<m:CreateItem MessageDisposition="SendAndSaveCopy"><m:Items><t:AcceptItem><t:Body BodyType="Text">See you there</t:Body><t:ReferenceItemId Id="AAMkADk1-mr-1"/></t:AcceptItem>"#));
    assert!(bodies[1].contains(r#"MessageDisposition="SaveOnly""#));
}

#[tokio::test]
async fn proposes_new_meeting_times_from_exchange_2013_sp1_on() {
    let (server, client) = setup().await;
    on(
        &server,
        "<t:TentativelyAcceptItem>",
        soap_ok("create_item_sent.xml"),
    )
    .await;
    let at = |h: u32| Utc.with_ymd_and_hms(2026, 11, 5, h, 0, 0).single().unwrap();
    client
        .propose_new_time(
            &ItemId::new("AAMkADk1-mr-1"),
            MeetingResponse::Tentative,
            at(11),
            at(12),
            Some("Could we meet later?"),
        )
        .await
        .unwrap();
    let sent = bodies(&server).await;
    assert!(sent[0].contains(r#"<t:RequestServerVersion Version="Exchange2013_SP1"/>"#));
    assert!(sent[0].contains(r#"<m:CreateItem MessageDisposition="SendAndSaveCopy"><m:Items><t:TentativelyAcceptItem><t:Body BodyType="Text">Could we meet later?</t:Body><t:ReferenceItemId Id="AAMkADk1-mr-1"/><t:ProposedStart>2026-11-05T11:00:00Z</t:ProposedStart><t:ProposedEnd>2026-11-05T12:00:00Z</t:ProposedEnd></t:TentativelyAcceptItem>"#));
    // Older servers don't know the elements; nothing is sent to them.
    client.set_version(envelope_ews::ExchangeVersion::Exchange2013);
    let refused = client
        .propose_new_time(
            &ItemId::new("AAMkADk1-mr-1"),
            MeetingResponse::Decline,
            at(11),
            at(12),
            None,
        )
        .await;
    assert!(matches!(
        refused,
        Err(envelope_ews::EwsError::InvalidInput(_))
    ));
    assert_eq!(bodies(&server).await.len(), 1);
}
