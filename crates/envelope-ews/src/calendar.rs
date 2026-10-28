//! Calendar: calendar views with expanded occurrences, calendar items with
//! their iCalendar MIME content, creating, updating and deleting events
//! (with meeting invitations and cancellations) and answering meeting
//! requests.
//!
//! All instants travel in UTC. The zone of an event is named separately by
//! its Windows id (`StartTimeZone` / `EndTimeZone`, Exchange 2010 and
//! later), mapped from and to IANA zones with [`crate::timezone`].

use std::collections::HashSet;

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseCode, ResponseError};
use crate::folders::DeleteType;
use crate::items::{
    FieldUpdate, FindItemQuery, GET_ITEM_BATCH_SIZE, GET_ITEM_MIME_BATCH_SIZE, ItemView,
    SendCancellations, UpdateAttributes, delete_item_request, first_item_id,
};
use crate::messages::decode_base64;
use crate::recurrence::Recurrence;
use crate::soap;
use crate::timezone::{resolve_time_zone, windows_zone_id};
use crate::types::{
    BaseShape, Body, BodyType, FolderId, FolderRef, Importance, ItemId, ItemRef, ItemShape,
    Mailbox, PropertyPath, Sensitivity, child_datetime, format_datetime, string_list, wire_enum,
};
use crate::version::ExchangeVersion;
use crate::xml::{Element, XmlWriter};

/// Entries requested per calendar view; larger ranges are split.
const CALENDAR_VIEW_MAX_ENTRIES: u32 = 1000;

/// Ranges are not split below this length.
const MIN_SPLIT_RANGE: Duration = Duration::hours(1);

wire_enum! {
    /// An attendee's answer to a meeting request.
    pub enum ResponseType {
        /// Unknown.
        Unknown = "Unknown",
        /// The attendee is the organizer.
        Organizer = "Organizer",
        /// Tentatively accepted.
        Tentative = "Tentative",
        /// Accepted.
        Accept = "Accept",
        /// Declined.
        Decline = "Decline",
        /// No answer yet.
        NoResponseReceived = "NoResponseReceived",
    }
}

wire_enum! {
    /// How an event shows on the free/busy schedule.
    pub enum FreeBusyStatus {
        /// Free.
        Free = "Free",
        /// Tentative.
        Tentative = "Tentative",
        /// Busy.
        Busy = "Busy",
        /// Out of office.
        Oof = "OOF",
        /// Working elsewhere.
        WorkingElsewhere = "WorkingElsewhere",
        /// No information.
        NoData = "NoData",
    }
}

wire_enum! {
    /// Role of a calendar item in a series.
    pub enum CalendarItemType {
        /// A non-recurring event.
        Single = "Single",
        /// An unmodified occurrence of a series.
        Occurrence = "Occurrence",
        /// A modified occurrence of a series.
        Exception = "Exception",
        /// The series itself.
        RecurringMaster = "RecurringMaster",
    }
}

/// An attendee with response state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    /// Address and name.
    pub mailbox: Mailbox,
    /// The attendee's answer.
    pub response: Option<ResponseType>,
    /// When the answer arrived.
    pub last_response_time: Option<DateTime<Utc>>,
}

impl Attendee {
    fn from_element(element: &Element) -> Option<Self> {
        Some(Attendee {
            mailbox: element.child("Mailbox").map(Mailbox::from_element)?,
            response: element
                .child_text("ResponseType")
                .and_then(|r| ResponseType::parse(&r)),
            last_response_time: child_datetime(element, "LastResponseTime"),
        })
    }

    fn list(element: Option<&Element>) -> Vec<Self> {
        element
            .map(|list| {
                list.children_named("Attendee")
                    .filter_map(Attendee::from_element)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A modified occurrence listed on a series master.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OccurrenceInfo {
    /// Id of the occurrence.
    pub id: ItemId,
    /// Its start.
    pub start: Option<DateTime<Utc>>,
    /// Its end.
    pub end: Option<DateTime<Utc>>,
    /// Where the series would have placed it.
    pub original_start: Option<DateTime<Utc>>,
}

/// An appointment or meeting.
///
/// Every property is optional because what is filled depends on the
/// request: calendar views return a summary, [`EwsClient::get_calendar_items`]
/// the details and, on request, the iCalendar MIME content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarItem {
    /// Item id and change key (occurrences have ids of their own).
    pub id: ItemId,
    /// The calendar holding the item.
    pub parent_folder_id: Option<FolderId>,
    /// `IPM.Appointment`, ...
    pub item_class: Option<String>,
    /// iCalendar `UID` (shared by all items of a series).
    pub uid: Option<String>,
    /// iCalendar `RECURRENCE-ID` of an occurrence or exception.
    pub recurrence_id: Option<DateTime<Utc>>,
    /// Subject.
    pub subject: Option<String>,
    /// Body (when requested).
    pub body: Option<Body>,
    /// Start.
    pub start: Option<DateTime<Utc>>,
    /// End.
    pub end: Option<DateTime<Utc>>,
    /// Original start of an exception.
    pub original_start: Option<DateTime<Utc>>,
    /// Whether the event lasts whole days.
    pub is_all_day: Option<bool>,
    /// Windows id of the start time zone.
    pub start_time_zone: Option<String>,
    /// Windows id of the end time zone.
    pub end_time_zone: Option<String>,
    /// Display name of the time zone (`TimeZone`).
    pub time_zone_name: Option<String>,
    /// Location.
    pub location: Option<String>,
    /// Organizer.
    pub organizer: Option<Mailbox>,
    /// Required attendees.
    pub required_attendees: Vec<Attendee>,
    /// Optional attendees.
    pub optional_attendees: Vec<Attendee>,
    /// Resources (rooms, equipment).
    pub resources: Vec<Attendee>,
    /// The mailbox owner's own answer.
    pub my_response_type: Option<ResponseType>,
    /// Free/busy status.
    pub free_busy: Option<FreeBusyStatus>,
    /// Role in a series.
    pub item_type: Option<CalendarItemType>,
    /// Whether the item belongs to a series.
    pub is_recurring: Option<bool>,
    /// Whether the item has attendees.
    pub is_meeting: Option<bool>,
    /// Whether the meeting was cancelled.
    pub is_cancelled: Option<bool>,
    /// Whether it is an online meeting.
    pub is_online_meeting: Option<bool>,
    /// Join URL of a Skype for Business / Lync meeting (Exchange 2013 and
    /// later). Teams links live in the body.
    pub join_url: Option<String>,
    /// The recurrence of a series master.
    pub recurrence: Option<Recurrence>,
    /// Modified occurrences of a series master.
    pub modified_occurrences: Vec<OccurrenceInfo>,
    /// Starts of deleted occurrences of a series master.
    pub deleted_occurrences: Vec<DateTime<Utc>>,
    /// Categories.
    pub categories: Vec<String>,
    /// Sensitivity.
    pub sensitivity: Option<Sensitivity>,
    /// Importance.
    pub importance: Option<Importance>,
    /// Whether a reminder is set.
    pub reminder_is_set: Option<bool>,
    /// Reminder lead time.
    pub reminder_minutes_before_start: Option<i32>,
    /// iCalendar `SEQUENCE`.
    pub sequence: Option<i32>,
    /// iCalendar `DTSTAMP`.
    pub date_time_stamp: Option<DateTime<Utc>>,
    /// Last modification.
    pub last_modified: Option<DateTime<Utc>>,
    /// iCalendar MIME content (when requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_content: Option<Vec<u8>>,
}

impl CalendarItem {
    pub(crate) fn from_element(element: &Element) -> Option<CalendarItem> {
        if !element.is("CalendarItem") {
            return None;
        }
        let id = element.child("ItemId").and_then(ItemId::from_element)?;
        let zone = |name: &str| {
            element.child(name).and_then(|z| {
                z.attr("Id")
                    .or_else(|| z.attr("Name"))
                    .filter(|v| !v.trim().is_empty())
                    .map(str::to_owned)
            })
        };
        Some(CalendarItem {
            id,
            parent_folder_id: element
                .child("ParentFolderId")
                .and_then(FolderId::from_element),
            item_class: element.child_text("ItemClass"),
            uid: element.child_text("UID"),
            recurrence_id: child_datetime(element, "RecurrenceId"),
            subject: element.child_raw_text("Subject"),
            body: element.child("Body").map(Body::from_element),
            start: child_datetime(element, "Start"),
            end: child_datetime(element, "End"),
            original_start: child_datetime(element, "OriginalStart"),
            is_all_day: element.child_bool("IsAllDayEvent"),
            start_time_zone: zone("StartTimeZone")
                .or_else(|| element.child_text("StartTimeZoneId")),
            end_time_zone: zone("EndTimeZone").or_else(|| element.child_text("EndTimeZoneId")),
            time_zone_name: element.child_text("TimeZone"),
            location: element.child_text("Location"),
            organizer: element.child("Organizer").and_then(Mailbox::from_single),
            required_attendees: Attendee::list(element.child("RequiredAttendees")),
            optional_attendees: Attendee::list(element.child("OptionalAttendees")),
            resources: Attendee::list(element.child("Resources")),
            my_response_type: element
                .child_text("MyResponseType")
                .and_then(|r| ResponseType::parse(&r)),
            free_busy: element
                .child_text("LegacyFreeBusyStatus")
                .and_then(|s| FreeBusyStatus::parse(&s)),
            item_type: element
                .child_text("CalendarItemType")
                .and_then(|t| CalendarItemType::parse(&t)),
            is_recurring: element.child_bool("IsRecurring"),
            is_meeting: element.child_bool("IsMeeting"),
            is_cancelled: element.child_bool("IsCancelled"),
            is_online_meeting: element.child_bool("IsOnlineMeeting"),
            join_url: element.child_text("JoinOnlineMeetingUrl"),
            recurrence: element
                .child("Recurrence")
                .and_then(Recurrence::from_element),
            modified_occurrences: element
                .child("ModifiedOccurrences")
                .map(|list| {
                    list.children_named("Occurrence")
                        .filter_map(|o| {
                            Some(OccurrenceInfo {
                                id: o.child("ItemId").and_then(ItemId::from_element)?,
                                start: child_datetime(o, "Start"),
                                end: child_datetime(o, "End"),
                                original_start: child_datetime(o, "OriginalStart"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            deleted_occurrences: element
                .child("DeletedOccurrences")
                .map(|list| {
                    list.children_named("DeletedOccurrence")
                        .filter_map(|o| child_datetime(o, "Start"))
                        .collect()
                })
                .unwrap_or_default(),
            categories: string_list(element.child("Categories")),
            sensitivity: element
                .child_text("Sensitivity")
                .and_then(|s| Sensitivity::parse(&s)),
            importance: element
                .child_text("Importance")
                .and_then(|i| Importance::parse(&i)),
            reminder_is_set: element.child_bool("ReminderIsSet"),
            reminder_minutes_before_start: element.child_i32("ReminderMinutesBeforeStart"),
            sequence: element.child_i32("AppointmentSequenceNumber"),
            date_time_stamp: child_datetime(element, "DateTimeStamp"),
            last_modified: child_datetime(element, "LastModifiedTime"),
            mime_content: element
                .child("MimeContent")
                .and_then(|m| decode_base64(&m.text()).ok()),
        })
    }

    /// The iCalendar text of the MIME content, if it was fetched.
    pub fn ics(&self) -> Option<String> {
        self.mime_content
            .as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    }

    /// The start time zone as an IANA zone.
    pub fn start_tz(&self) -> Option<Tz> {
        self.start_time_zone
            .as_deref()
            .or(self.time_zone_name.as_deref())
            .and_then(resolve_time_zone)
    }
}

/// Properties for calendar views, all of which `FindItem` can return.
pub fn calendar_summary_properties(version: ExchangeVersion) -> Vec<PropertyPath> {
    let mut properties: Vec<PropertyPath> = [
        "item:ItemClass",
        "item:Subject",
        "item:Categories",
        "item:Sensitivity",
        "calendar:UID",
        "calendar:Start",
        "calendar:End",
        "calendar:IsAllDayEvent",
        "calendar:Location",
        "calendar:Organizer",
        "calendar:CalendarItemType",
        "calendar:MyResponseType",
        "calendar:LegacyFreeBusyStatus",
        "calendar:IsRecurring",
        "calendar:IsMeeting",
        "calendar:IsCancelled",
    ]
    .into_iter()
    .map(PropertyPath::field)
    .collect();
    if version.has_2010_features() {
        properties.push(PropertyPath::field("calendar:RecurrenceId"));
    }
    properties
}

/// Properties fetched by [`EwsClient::get_calendar_items`].
pub fn calendar_detail_properties(version: ExchangeVersion) -> Vec<PropertyPath> {
    let mut properties = calendar_summary_properties(version);
    properties.extend(
        [
            "item:ParentFolderId",
            "item:Importance",
            "item:ReminderIsSet",
            "item:ReminderMinutesBeforeStart",
            "calendar:OriginalStart",
            "calendar:RequiredAttendees",
            "calendar:OptionalAttendees",
            "calendar:Resources",
            "calendar:Recurrence",
            "calendar:ModifiedOccurrences",
            "calendar:DeletedOccurrences",
            "calendar:AppointmentSequenceNumber",
            "calendar:DateTimeStamp",
            "calendar:TimeZone",
            "calendar:IsOnlineMeeting",
        ]
        .into_iter()
        .map(PropertyPath::field),
    );
    if version.has_2010_features() {
        properties.extend(
            [
                "calendar:StartTimeZone",
                "calendar:EndTimeZone",
                "item:LastModifiedTime",
            ]
            .into_iter()
            .map(PropertyPath::field),
        );
    }
    if version.has_2013_features() {
        properties.push(PropertyPath::field("calendar:JoinOnlineMeetingUrl"));
    }
    properties
}

/// Options for [`EwsClient::get_calendar_items`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GetCalendarItemsOptions {
    /// Fetch the iCalendar MIME content.
    pub include_mime_content: bool,
    /// Fetch the body in this format.
    pub body_type: Option<BodyType>,
}

/// When an event takes place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTiming {
    /// A timed event.
    Timed {
        /// Start.
        start: DateTime<Utc>,
        /// End.
        end: DateTime<Utc>,
        /// The zone the event is planned in (recurrences follow its DST
        /// rules); UTC when `None`.
        time_zone: Option<Tz>,
    },
    /// An all-day event.
    AllDay {
        /// First day.
        start: NaiveDate,
        /// Day after the last day (exclusive, as in iCalendar).
        end: NaiveDate,
        /// The zone whose midnights bound the days; UTC when `None`.
        time_zone: Option<Tz>,
    },
}

/// Instants and zone of an [`EventTiming`] as Exchange wants them.
struct ResolvedTiming {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    all_day: bool,
    zone_id: &'static str,
}

impl EventTiming {
    fn resolve(&self) -> Result<ResolvedTiming, EwsError> {
        match self {
            EventTiming::Timed {
                start,
                end,
                time_zone,
            } => {
                if end < start {
                    return Err(EwsError::InvalidInput("event ends before it starts".into()));
                }
                Ok(ResolvedTiming {
                    start: *start,
                    end: *end,
                    all_day: false,
                    zone_id: time_zone.map(windows_zone_id).unwrap_or("UTC"),
                })
            }
            EventTiming::AllDay {
                start,
                end,
                time_zone,
            } => {
                if end <= start {
                    return Err(EwsError::InvalidInput(
                        "an all-day event must end at least one day after it starts".into(),
                    ));
                }
                // Days are bounded by midnight in the event's zone; a zone
                // without Windows id falls back to UTC midnights.
                let tz = time_zone.filter(|tz| windows_zone_id(*tz) != "UTC" || *tz == Tz::UTC);
                let tz = tz.unwrap_or(Tz::UTC);
                Ok(ResolvedTiming {
                    start: local_midnight(tz, *start),
                    end: local_midnight(tz, *end),
                    all_day: true,
                    zone_id: windows_zone_id(tz),
                })
            }
        }
    }
}

/// Midnight of `date` in `tz` as UTC (the first valid instant when
/// midnight falls into a DST gap).
fn local_midnight(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    let midnight = date.and_time(NaiveTime::MIN);
    tz.from_local_datetime(&midnight)
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&(midnight + Duration::hours(1)))
                .earliest()
        })
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| midnight.and_utc())
}

/// A new appointment or meeting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCalendarItem {
    /// Subject.
    pub subject: String,
    /// Body.
    pub body: Option<Body>,
    /// When it takes place.
    pub timing: EventTiming,
    /// Location.
    pub location: Option<String>,
    /// Required attendees; with attendees the item is a meeting.
    pub required_attendees: Vec<Mailbox>,
    /// Optional attendees.
    pub optional_attendees: Vec<Mailbox>,
    /// Rooms and equipment.
    pub resources: Vec<Mailbox>,
    /// Free/busy status (server default: busy).
    pub free_busy: Option<FreeBusyStatus>,
    /// Reminder lead time; `None` sets no reminder.
    pub reminder_minutes: Option<u32>,
    /// Categories.
    pub categories: Vec<String>,
    /// Sensitivity.
    pub sensitivity: Option<Sensitivity>,
    /// Recurrence of a series.
    pub recurrence: Option<Recurrence>,
    /// iCalendar `UID` to use (Exchange generates one otherwise).
    pub uid: Option<String>,
}

impl NewCalendarItem {
    /// A timed appointment without attendees.
    pub fn new(subject: impl Into<String>, timing: EventTiming) -> Self {
        NewCalendarItem {
            subject: subject.into(),
            body: None,
            timing,
            location: None,
            required_attendees: Vec::new(),
            optional_attendees: Vec::new(),
            resources: Vec::new(),
            free_busy: None,
            reminder_minutes: None,
            categories: Vec::new(),
            sensitivity: None,
            recurrence: None,
            uid: None,
        }
    }
}

/// Changes applied by [`EwsClient::update_calendar_item`]; `None` leaves
/// a property alone, `Some(None)` removes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalendarItemUpdate {
    /// New subject.
    pub subject: Option<String>,
    /// New body.
    pub body: Option<Body>,
    /// New timing.
    pub timing: Option<EventTiming>,
    /// New location.
    pub location: Option<Option<String>>,
    /// New required attendees (replacing the old ones).
    pub required_attendees: Option<Vec<Mailbox>>,
    /// New optional attendees.
    pub optional_attendees: Option<Vec<Mailbox>>,
    /// New resources.
    pub resources: Option<Vec<Mailbox>>,
    /// New free/busy status.
    pub free_busy: Option<FreeBusyStatus>,
    /// New reminder lead time (`Some(None)` turns the reminder off).
    pub reminder_minutes: Option<Option<u32>>,
    /// New categories (an empty list removes all).
    pub categories: Option<Vec<String>>,
    /// New sensitivity.
    pub sensitivity: Option<Sensitivity>,
    /// New recurrence (`Some(None)` turns a series into a single event).
    pub recurrence: Option<Option<Recurrence>>,
}

/// Who is notified when a meeting is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SendInvitations {
    /// Nobody (appointments, or meetings managed elsewhere).
    SendToNone,
    /// Attendees, without a copy in sent items.
    SendOnlyToAll,
    /// Attendees, with a copy in sent items.
    SendToAllAndSaveCopy,
}

impl SendInvitations {
    fn as_str(self) -> &'static str {
        match self {
            SendInvitations::SendToNone => "SendToNone",
            SendInvitations::SendOnlyToAll => "SendOnlyToAll",
            SendInvitations::SendToAllAndSaveCopy => "SendToAllAndSaveCopy",
        }
    }
}

/// Who is notified when a meeting changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SendUpdates {
    /// Nobody.
    SendToNone,
    /// All attendees, without a copy.
    SendOnlyToAll,
    /// Only added or removed attendees, without a copy.
    SendOnlyToChanged,
    /// All attendees, with a copy in sent items.
    SendToAllAndSaveCopy,
    /// Only added or removed attendees, with a copy in sent items.
    SendToChangedAndSaveCopy,
}

impl SendUpdates {
    fn as_str(self) -> &'static str {
        match self {
            SendUpdates::SendToNone => "SendToNone",
            SendUpdates::SendOnlyToAll => "SendOnlyToAll",
            SendUpdates::SendOnlyToChanged => "SendOnlyToChanged",
            SendUpdates::SendToAllAndSaveCopy => "SendToAllAndSaveCopy",
            SendUpdates::SendToChangedAndSaveCopy => "SendToChangedAndSaveCopy",
        }
    }
}

/// An answer to a meeting request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeetingResponse {
    /// Accept.
    Accept,
    /// Tentatively accept.
    Tentative,
    /// Decline (removes the meeting from the calendar).
    Decline,
}

impl MeetingResponse {
    fn element(self) -> &'static str {
        match self {
            MeetingResponse::Accept => "t:AcceptItem",
            MeetingResponse::Tentative => "t:TentativelyAcceptItem",
            MeetingResponse::Decline => "t:DeclineItem",
        }
    }
}

fn write_attendees(w: &mut XmlWriter, tag: &str, attendees: &[Mailbox]) {
    if attendees.is_empty() {
        return;
    }
    w.start(tag, &[]);
    for mailbox in attendees {
        w.start("t:Attendee", &[]);
        mailbox.write(w);
        w.end("t:Attendee");
    }
    w.end(tag);
}

fn write_categories(w: &mut XmlWriter, categories: &[String]) {
    if categories.is_empty() {
        return;
    }
    w.start("t:Categories", &[]);
    for category in categories {
        w.text_element("t:String", category);
    }
    w.end("t:Categories");
}

/// Writes a new calendar item in schema order.
pub(crate) fn write_new_calendar_item(
    w: &mut XmlWriter,
    item: &NewCalendarItem,
    version: ExchangeVersion,
) -> Result<(), EwsError> {
    let timing = item.timing.resolve()?;
    w.start("t:CalendarItem", &[])
        .text_element("t:Subject", &item.subject);
    if let Some(sensitivity) = item.sensitivity {
        w.text_element("t:Sensitivity", sensitivity.as_str());
    }
    if let Some(body) = &item.body {
        body.write(w);
    }
    write_categories(w, &item.categories);
    match item.reminder_minutes {
        Some(minutes) => {
            w.text_element("t:ReminderIsSet", "true")
                .text_element("t:ReminderMinutesBeforeStart", &minutes.to_string());
        }
        None => {
            w.text_element("t:ReminderIsSet", "false");
        }
    }
    if let Some(uid) = &item.uid {
        w.text_element("t:UID", uid);
    }
    w.text_element("t:Start", &format_datetime(timing.start))
        .text_element("t:End", &format_datetime(timing.end))
        .text_element(
            "t:IsAllDayEvent",
            if timing.all_day { "true" } else { "false" },
        );
    if let Some(status) = item.free_busy {
        w.text_element("t:LegacyFreeBusyStatus", status.as_str());
    }
    if let Some(location) = &item.location {
        w.text_element("t:Location", location);
    }
    write_attendees(w, "t:RequiredAttendees", &item.required_attendees);
    write_attendees(w, "t:OptionalAttendees", &item.optional_attendees);
    write_attendees(w, "t:Resources", &item.resources);
    if let Some(recurrence) = &item.recurrence {
        recurrence.write(w, version.has_2013_features());
    }
    if version.has_2010_features() {
        w.empty("t:StartTimeZone", &[("Id", timing.zone_id)])
            .empty("t:EndTimeZone", &[("Id", timing.zone_id)]);
    }
    w.end("t:CalendarItem");
    Ok(())
}

pub(crate) fn create_calendar_item_request(
    folder: &FolderRef,
    item: &NewCalendarItem,
    send: SendInvitations,
    version: ExchangeVersion,
) -> Result<String, EwsError> {
    let mut w = XmlWriter::new();
    w.start("m:CreateItem", &[("SendMeetingInvitations", send.as_str())])
        .start("m:SavedItemFolderId", &[]);
    folder.write(&mut w);
    w.end("m:SavedItemFolderId").start("m:Items", &[]);
    write_new_calendar_item(&mut w, item, version)?;
    w.end("m:Items").end("m:CreateItem");
    Ok(w.into_string())
}

/// `<t:CalendarItem>inner</t:CalendarItem>` for a `SetItemField`.
fn calendar_value(inner: impl FnOnce(&mut XmlWriter)) -> String {
    let mut w = XmlWriter::new();
    w.start("t:CalendarItem", &[]);
    inner(&mut w);
    w.end("t:CalendarItem");
    w.into_string()
}

fn set(path: &str, inner: impl FnOnce(&mut XmlWriter)) -> FieldUpdate {
    FieldUpdate::set(PropertyPath::field(path), calendar_value(inner))
}

fn delete(path: &str) -> FieldUpdate {
    FieldUpdate::Delete(PropertyPath::field(path))
}

impl CalendarItemUpdate {
    fn is_empty(&self) -> bool {
        *self == CalendarItemUpdate::default()
    }

    pub(crate) fn field_updates(
        &self,
        version: ExchangeVersion,
    ) -> Result<Vec<FieldUpdate>, EwsError> {
        let mut updates = Vec::new();
        if let Some(subject) = &self.subject {
            updates.push(set("item:Subject", |w| {
                w.text_element("t:Subject", subject);
            }));
        }
        if let Some(sensitivity) = self.sensitivity {
            updates.push(set("item:Sensitivity", |w| {
                w.text_element("t:Sensitivity", sensitivity.as_str());
            }));
        }
        if let Some(body) = &self.body {
            updates.push(set("item:Body", |w| body.write(w)));
        }
        if let Some(categories) = &self.categories {
            updates.push(if categories.is_empty() {
                delete("item:Categories")
            } else {
                set("item:Categories", |w| write_categories(w, categories))
            });
        }
        if let Some(reminder) = self.reminder_minutes {
            match reminder {
                Some(minutes) => {
                    updates.push(set("item:ReminderIsSet", |w| {
                        w.text_element("t:ReminderIsSet", "true");
                    }));
                    updates.push(set("item:ReminderMinutesBeforeStart", |w| {
                        w.text_element("t:ReminderMinutesBeforeStart", &minutes.to_string());
                    }));
                }
                None => updates.push(set("item:ReminderIsSet", |w| {
                    w.text_element("t:ReminderIsSet", "false");
                })),
            }
        }
        if let Some(timing) = &self.timing {
            let timing = timing.resolve()?;
            // The zones go first so that the new times are read in them.
            if version.has_2010_features() {
                updates.push(set("calendar:StartTimeZone", |w| {
                    w.empty("t:StartTimeZone", &[("Id", timing.zone_id)]);
                }));
                updates.push(set("calendar:EndTimeZone", |w| {
                    w.empty("t:EndTimeZone", &[("Id", timing.zone_id)]);
                }));
            }
            updates.push(set("calendar:Start", |w| {
                w.text_element("t:Start", &format_datetime(timing.start));
            }));
            updates.push(set("calendar:End", |w| {
                w.text_element("t:End", &format_datetime(timing.end));
            }));
            updates.push(set("calendar:IsAllDayEvent", |w| {
                w.text_element(
                    "t:IsAllDayEvent",
                    if timing.all_day { "true" } else { "false" },
                );
            }));
        }
        if let Some(status) = self.free_busy {
            updates.push(set("calendar:LegacyFreeBusyStatus", |w| {
                w.text_element("t:LegacyFreeBusyStatus", status.as_str());
            }));
        }
        if let Some(location) = &self.location {
            updates.push(match location {
                Some(location) => set("calendar:Location", |w| {
                    w.text_element("t:Location", location);
                }),
                None => delete("calendar:Location"),
            });
        }
        for (path, tag, list) in [
            (
                "calendar:RequiredAttendees",
                "t:RequiredAttendees",
                &self.required_attendees,
            ),
            (
                "calendar:OptionalAttendees",
                "t:OptionalAttendees",
                &self.optional_attendees,
            ),
            ("calendar:Resources", "t:Resources", &self.resources),
        ] {
            if let Some(list) = list {
                updates.push(if list.is_empty() {
                    delete(path)
                } else {
                    set(path, |w| write_attendees(w, tag, list))
                });
            }
        }
        if let Some(recurrence) = &self.recurrence {
            updates.push(match recurrence {
                Some(recurrence) => set("calendar:Recurrence", |w| {
                    recurrence.write(w, version.has_2013_features());
                }),
                None => delete("calendar:Recurrence"),
            });
        }
        Ok(updates)
    }
}

pub(crate) fn respond_request(
    item: &ItemId,
    response: MeetingResponse,
    comment: Option<&str>,
    send: bool,
) -> String {
    let element = response.element();
    let mut w = XmlWriter::new();
    w.start(
        "m:CreateItem",
        &[(
            "MessageDisposition",
            if send { "SendAndSaveCopy" } else { "SaveOnly" },
        )],
    )
    .start("m:Items", &[])
    .start(element, &[]);
    if let Some(comment) = comment.filter(|c| !c.trim().is_empty()) {
        Body::text(comment).write(&mut w);
    }
    item.write(&mut w, "t:ReferenceItemId");
    w.end(element).end("m:Items").end("m:CreateItem");
    w.into_string()
}

impl EwsClient {
    /// The events between `start` and `end` with recurring series expanded
    /// into their occurrences (`CalendarView`), sorted by start. Ranges
    /// holding more than the server returns at once are split
    /// automatically.
    pub async fn calendar_view(
        &self,
        folder: impl Into<FolderRef>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<CalendarItem>, EwsError> {
        if end <= start {
            return Err(EwsError::InvalidInput(
                "calendar view range is empty".into(),
            ));
        }
        let folders = [folder.into()];
        let shape = ItemShape::with_properties(calendar_summary_properties(self.version()));
        let mut seen: HashSet<String> = HashSet::new();
        let mut items = Vec::new();
        let mut ranges = vec![(start, end)];
        while let Some((from, to)) = ranges.pop() {
            let query = FindItemQuery {
                shape: &shape,
                view: ItemView::Calendar {
                    start: from,
                    end: to,
                    max: CALENDAR_VIEW_MAX_ENTRIES,
                },
                restriction: None,
                sort: None,
                folders: &folders,
                query: None,
            };
            let splittable = to - from > MIN_SPLIT_RANGE;
            let page = match self
                .find_items_with(&query, CalendarItem::from_element)
                .await
            {
                Ok(page) => page,
                Err(err)
                    if splittable
                        && err.response_code()
                            == Some(&ResponseCode::ErrorCalendarViewRangeTooBig) =>
                {
                    let middle = from + (to - from) / 2;
                    ranges.push((middle, to));
                    ranges.push((from, middle));
                    continue;
                }
                Err(err) => return Err(err),
            };
            if !page.includes_last_item_in_range && splittable {
                let middle = from + (to - from) / 2;
                ranges.push((middle, to));
                ranges.push((from, middle));
                continue;
            }
            if !page.includes_last_item_in_range {
                tracing::warn!(%from, %to, "calendar view truncated by the server");
            }
            for item in page.items {
                if seen.insert(item.id.id.clone()) {
                    items.push(item);
                }
            }
        }
        items.sort_by_key(|item| item.start);
        Ok(items)
    }

    /// Fetches calendar items (single events, occurrences, exceptions or
    /// series masters) with [`calendar_detail_properties`] and optionally
    /// their iCalendar MIME content. One result per reference, in order.
    pub async fn get_calendar_items(
        &self,
        ids: &[ItemRef],
        options: &GetCalendarItemsOptions,
    ) -> Result<Vec<Result<CalendarItem, ResponseError>>, EwsError> {
        let shape = ItemShape {
            base: BaseShape::IdOnly,
            include_mime_content: options.include_mime_content,
            body_type: options.body_type,
            additional: calendar_detail_properties(self.version()),
        };
        let batch = if options.include_mime_content {
            GET_ITEM_MIME_BATCH_SIZE
        } else {
            GET_ITEM_BATCH_SIZE
        };
        self.get_items_with(ids, &shape, batch, CalendarItem::from_element)
            .await
    }

    /// One calendar item.
    pub async fn get_calendar_item(
        &self,
        id: impl Into<ItemRef>,
        options: &GetCalendarItemsOptions,
    ) -> Result<CalendarItem, EwsError> {
        self.get_calendar_items(&[id.into()], options)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("GetItem returned nothing".into()))?
            .map_err(EwsError::from)
    }

    /// Creates an appointment, or a meeting when it has attendees, and
    /// returns its id.
    pub async fn create_calendar_item(
        &self,
        folder: impl Into<FolderRef>,
        item: &NewCalendarItem,
        send: SendInvitations,
    ) -> Result<ItemId, EwsError> {
        let body = create_calendar_item_request(&folder.into(), item, send, self.version())?;
        let response = self.call(&SoapRequest::write("CreateItem", body)).await?;
        first_item_id(soap::single(&response)?)
            .ok_or_else(|| EwsError::InvalidResponse("CreateItem returned no item id".into()))
    }

    /// Updates a calendar item (an occurrence id changes just that
    /// occurrence) and returns its new id.
    pub async fn update_calendar_item(
        &self,
        id: impl Into<ItemRef>,
        update: &CalendarItemUpdate,
        send: SendUpdates,
    ) -> Result<ItemId, EwsError> {
        if update.is_empty() {
            return Err(EwsError::InvalidInput("empty calendar item update".into()));
        }
        let id = id.into();
        let changes = vec![(id.clone(), update.field_updates(self.version())?)];
        let result = self
            .update_items(
                &changes,
                &UpdateAttributes {
                    message_disposition: None,
                    send_invitations: Some(send.as_str()),
                    suppress_read_receipts: false,
                },
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("UpdateItem returned nothing".into()))?
            .map_err(EwsError::from)?;
        match (result, id) {
            (Some(new_id), _) => Ok(new_id),
            (None, ItemRef::Id(old)) => Ok(ItemId::new(old.id)),
            (None, _) => Err(EwsError::InvalidResponse(
                "UpdateItem returned no item id".into(),
            )),
        }
    }

    /// Deletes a calendar item; as organizer, `cancellations` decides
    /// whether attendees are notified. Attendees should decline instead.
    pub async fn delete_calendar_item(
        &self,
        id: impl Into<ItemRef>,
        delete_type: DeleteType,
        cancellations: SendCancellations,
    ) -> Result<(), EwsError> {
        let body = delete_item_request(&[id.into()], delete_type, cancellations, false);
        let response = self.call(&SoapRequest::write("DeleteItem", body)).await?;
        soap::single(&response).map(drop)
    }

    /// Answers a meeting request. `item` is the meeting request in the
    /// inbox or the calendar item. Without `send_response` the answer is
    /// recorded but not sent to the organizer.
    pub async fn respond_to_meeting(
        &self,
        item: &ItemId,
        response: MeetingResponse,
        comment: Option<&str>,
        send_response: bool,
    ) -> Result<Option<ItemId>, EwsError> {
        let body = respond_request(item, response, comment, send_response);
        let reply = self.call(&SoapRequest::write("CreateItem", body)).await?;
        Ok(first_item_id(soap::single(&reply)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recurrence::{DayOfWeek, RecurrencePattern, RecurrenceRange};
    use crate::types::DistinguishedFolder;
    use crate::xml;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn wrap(body: &str) -> Element {
        xml::parse(&format!(
            r#"<r xmlns:m="{}" xmlns:t="{}">{body}</r>"#,
            xml::NS_MESSAGES,
            xml::NS_TYPES
        ))
        .unwrap()
    }

    #[test]
    fn all_day_events_use_local_midnights() {
        let berlin: Tz = "Europe/Berlin".parse().unwrap();
        let timing = EventTiming::AllDay {
            start: date(2024, 7, 1),
            end: date(2024, 7, 3),
            time_zone: Some(berlin),
        }
        .resolve()
        .unwrap();
        assert_eq!(timing.start, utc(2024, 6, 30, 22, 0));
        assert_eq!(timing.end, utc(2024, 7, 2, 22, 0));
        assert!(timing.all_day);
        assert_eq!(timing.zone_id, "W. Europe Standard Time");
        let utc_day = EventTiming::AllDay {
            start: date(2024, 7, 1),
            end: date(2024, 7, 2),
            time_zone: None,
        }
        .resolve()
        .unwrap();
        assert_eq!(utc_day.start, utc(2024, 7, 1, 0, 0));
        assert_eq!(utc_day.zone_id, "UTC");
        assert!(
            EventTiming::AllDay {
                start: date(2024, 7, 1),
                end: date(2024, 7, 1),
                time_zone: None
            }
            .resolve()
            .is_err()
        );
        assert!(
            EventTiming::Timed {
                start: utc(2024, 7, 1, 10, 0),
                end: utc(2024, 7, 1, 9, 0),
                time_zone: None
            }
            .resolve()
            .is_err()
        );
    }

    #[test]
    fn create_request_is_schema_ordered() {
        let mut item = NewCalendarItem::new(
            "Planning <Q3>",
            EventTiming::Timed {
                start: utc(2024, 7, 1, 8, 0),
                end: utc(2024, 7, 1, 9, 0),
                time_zone: Some("Europe/Istanbul".parse().unwrap()),
            },
        );
        item.body = Some(Body::text("Agenda & notes"));
        item.location = Some("Room 1".into());
        item.required_attendees = vec![Mailbox::with_name("Alice", "alice@example.com")];
        item.optional_attendees = vec![Mailbox::new("bob@example.com")];
        item.reminder_minutes = Some(15);
        item.categories = vec!["Work".into()];
        item.free_busy = Some(FreeBusyStatus::Busy);
        item.uid = Some("uid-1@example.com".into());
        item.recurrence = Some(Recurrence {
            pattern: RecurrencePattern::Weekly {
                interval: 1,
                days: vec![DayOfWeek::Monday],
                first_day_of_week: Some(DayOfWeek::Monday),
            },
            range: RecurrenceRange::Numbered {
                start: date(2024, 7, 1),
                count: 4,
            },
        });
        let body = create_calendar_item_request(
            &DistinguishedFolder::Calendar.into(),
            &item,
            SendInvitations::SendToAllAndSaveCopy,
            ExchangeVersion::Exchange2013Sp1,
        )
        .unwrap();
        assert!(body.starts_with(r#"<m:CreateItem SendMeetingInvitations="SendToAllAndSaveCopy"><m:SavedItemFolderId><t:DistinguishedFolderId Id="calendar"/></m:SavedItemFolderId>"#));
        let root = wrap(&body);
        let event = root.path(&["CreateItem", "Items", "CalendarItem"]).unwrap();
        let names: Vec<&str> = event.elements().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Subject",
                "Body",
                "Categories",
                "ReminderIsSet",
                "ReminderMinutesBeforeStart",
                "UID",
                "Start",
                "End",
                "IsAllDayEvent",
                "LegacyFreeBusyStatus",
                "Location",
                "RequiredAttendees",
                "OptionalAttendees",
                "Recurrence",
                "StartTimeZone",
                "EndTimeZone"
            ]
        );
        assert_eq!(
            event.child_raw_text("Subject").as_deref(),
            Some("Planning <Q3>")
        );
        assert_eq!(
            event.child_text("Start").as_deref(),
            Some("2024-07-01T08:00:00Z")
        );
        assert_eq!(
            event.child("StartTimeZone").and_then(|z| z.attr("Id")),
            Some("Turkey Standard Time")
        );

        // Exchange 2007 has no StartTimeZone.
        let old = create_calendar_item_request(
            &DistinguishedFolder::Calendar.into(),
            &item,
            SendInvitations::SendToNone,
            ExchangeVersion::Exchange2007Sp1,
        )
        .unwrap();
        assert!(!old.contains("StartTimeZone"));
        assert!(!old.contains("FirstDayOfWeek"));
    }

    #[test]
    fn update_puts_time_zones_before_times() {
        let update = CalendarItemUpdate {
            subject: Some("Moved".into()),
            timing: Some(EventTiming::Timed {
                start: utc(2024, 7, 2, 8, 0),
                end: utc(2024, 7, 2, 9, 0),
                time_zone: Some("America/New_York".parse().unwrap()),
            }),
            location: Some(None),
            required_attendees: Some(vec![Mailbox::new("carol@example.com")]),
            reminder_minutes: Some(None),
            recurrence: Some(None),
            ..CalendarItemUpdate::default()
        };
        let updates = update.field_updates(ExchangeVersion::Exchange2016).unwrap();
        let paths: Vec<String> = updates
            .iter()
            .map(|u| match u {
                FieldUpdate::Set {
                    path: PropertyPath::Field(p),
                    ..
                } => format!("set {p}"),
                FieldUpdate::Delete(PropertyPath::Field(p)) => format!("delete {p}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            paths,
            [
                "set item:Subject",
                "set item:ReminderIsSet",
                "set calendar:StartTimeZone",
                "set calendar:EndTimeZone",
                "set calendar:Start",
                "set calendar:End",
                "set calendar:IsAllDayEvent",
                "delete calendar:Location",
                "set calendar:RequiredAttendees",
                "delete calendar:Recurrence"
            ]
        );
        assert!(
            matches!(&updates[2], FieldUpdate::Set { value_xml, .. } if value_xml == r#"<t:CalendarItem><t:StartTimeZone Id="Eastern Standard Time"/></t:CalendarItem>"#)
        );
        assert!(CalendarItemUpdate::default().is_empty());
    }

    #[test]
    fn meeting_responses() {
        let accept = respond_request(
            &ItemId::with_change_key("mr", "ck"),
            MeetingResponse::Accept,
            Some("See you"),
            true,
        );
        assert_eq!(
            accept,
            r#"<m:CreateItem MessageDisposition="SendAndSaveCopy"><m:Items><t:AcceptItem><t:Body BodyType="Text">See you</t:Body><t:ReferenceItemId Id="mr" ChangeKey="ck"/></t:AcceptItem></m:Items></m:CreateItem>"#
        );
        let decline = respond_request(&ItemId::new("mr"), MeetingResponse::Decline, None, false);
        assert_eq!(
            decline,
            r#"<m:CreateItem MessageDisposition="SaveOnly"><m:Items><t:DeclineItem><t:ReferenceItemId Id="mr"/></t:DeclineItem></m:Items></m:CreateItem>"#
        );
        assert!(
            respond_request(
                &ItemId::new("x"),
                MeetingResponse::Tentative,
                Some(" "),
                true
            )
            .contains("<t:TentativelyAcceptItem><t:ReferenceItemId")
        );
    }

    #[test]
    fn parses_calendar_items() {
        let root = wrap(
            r#"<t:CalendarItem>
                 <t:ItemId Id="cal-1" ChangeKey="DwAAAB"/>
                 <t:Subject>Weekly sync</t:Subject>
                 <t:Start>2024-07-01T08:00:00Z</t:Start>
                 <t:End>2024-07-01T08:30:00Z</t:End>
                 <t:IsAllDayEvent>false</t:IsAllDayEvent>
                 <t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus>
                 <t:Location>Room 1</t:Location>
                 <t:IsRecurring>true</t:IsRecurring>
                 <t:CalendarItemType>RecurringMaster</t:CalendarItemType>
                 <t:MyResponseType>Organizer</t:MyResponseType>
                 <t:Organizer><t:Mailbox><t:Name>Jane</t:Name><t:EmailAddress>jane@example.com</t:EmailAddress></t:Mailbox></t:Organizer>
                 <t:RequiredAttendees>
                   <t:Attendee><t:Mailbox><t:EmailAddress>alice@example.com</t:EmailAddress></t:Mailbox><t:ResponseType>Accept</t:ResponseType><t:LastResponseTime>2024-06-20T10:00:00Z</t:LastResponseTime></t:Attendee>
                 </t:RequiredAttendees>
                 <t:Recurrence><t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Monday</t:DaysOfWeek></t:WeeklyRecurrence><t:NoEndRecurrence><t:StartDate>2024-07-01+02:00</t:StartDate></t:NoEndRecurrence></t:Recurrence>
                 <t:ModifiedOccurrences><t:Occurrence><t:ItemId Id="occ-2"/><t:Start>2024-07-08T09:00:00Z</t:Start><t:End>2024-07-08T09:30:00Z</t:End><t:OriginalStart>2024-07-08T08:00:00Z</t:OriginalStart></t:Occurrence></t:ModifiedOccurrences>
                 <t:DeletedOccurrences><t:DeletedOccurrence><t:Start>2024-07-15T08:00:00Z</t:Start></t:DeletedOccurrence></t:DeletedOccurrences>
                 <t:StartTimeZone Id="W. Europe Standard Time" Name="(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna"/>
                 <t:UID>040000008200E00074C5B7101A82E00800000000</t:UID>
               </t:CalendarItem>"#,
        );
        let item = CalendarItem::from_element(root.child("CalendarItem").unwrap()).unwrap();
        assert_eq!(item.item_type, Some(CalendarItemType::RecurringMaster));
        assert_eq!(item.free_busy, Some(FreeBusyStatus::Busy));
        assert_eq!(
            item.organizer.as_ref().and_then(|o| o.email.as_deref()),
            Some("jane@example.com")
        );
        assert_eq!(
            item.required_attendees[0].response,
            Some(ResponseType::Accept)
        );
        assert_eq!(item.modified_occurrences[0].id.id, "occ-2");
        assert_eq!(item.deleted_occurrences, vec![utc(2024, 7, 15, 8, 0)]);
        assert_eq!(item.start_tz(), Some(Tz::Europe__Berlin));
        assert_eq!(
            item.recurrence
                .as_ref()
                .map(|r| r.to_rrule(item.start_tz(), false))
                .as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO")
        );
        assert!(
            CalendarItem::from_element(
                &wrap(r#"<t:Message><t:ItemId Id="x"/></t:Message>"#)
                    .elements()
                    .next()
                    .unwrap()
                    .clone()
            )
            .is_none()
        );
    }

    #[test]
    fn property_sets() {
        let summary = calendar_summary_properties(ExchangeVersion::Exchange2007Sp1);
        assert!(!summary.contains(&PropertyPath::field("calendar:RecurrenceId")));
        assert!(!summary.contains(&PropertyPath::field("calendar:RequiredAttendees")));
        let details = calendar_detail_properties(ExchangeVersion::Exchange2013Sp1);
        assert!(details.contains(&PropertyPath::field("calendar:JoinOnlineMeetingUrl")));
        assert!(details.contains(&PropertyPath::field("calendar:StartTimeZone")));
    }
}
