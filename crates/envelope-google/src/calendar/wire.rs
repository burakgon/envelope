//! Serde mirrors of the Google Calendar v3 JSON resources (decoding only).
//!
//! Every field is optional and enumerations are kept as strings, so that new
//! or unexpected values from Google never break the decoding of a page.
//! Writes build JSON directly (see `mapping`) because patch semantics need
//! explicit `null`s.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::serde_util::null_default;

/// `events.list` / `events.instances` response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventList {
    /// Kept as raw JSON so that one malformed item does not fail the page.
    #[serde(default, deserialize_with = "null_default")]
    pub items: Vec<serde_json::Value>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
    pub time_zone: Option<String>,
    pub access_role: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub default_reminders: Vec<Reminder>,
}

/// The `Events` resource.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Event {
    pub id: Option<String>,
    pub etag: Option<String>,
    pub status: Option<String>,
    pub html_link: Option<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub color_id: Option<String>,
    pub creator: Option<Person>,
    pub organizer: Option<Person>,
    pub start: Option<DateTimeValue>,
    pub end: Option<DateTimeValue>,
    pub end_time_unspecified: Option<bool>,
    #[serde(default, deserialize_with = "null_default")]
    pub recurrence: Vec<String>,
    pub recurring_event_id: Option<String>,
    pub original_start_time: Option<DateTimeValue>,
    pub transparency: Option<String>,
    pub visibility: Option<String>,
    #[serde(rename = "iCalUID")]
    pub ical_uid: Option<String>,
    pub sequence: Option<i64>,
    #[serde(default, deserialize_with = "null_default")]
    pub attendees: Vec<Attendee>,
    pub attendees_omitted: Option<bool>,
    pub extended_properties: Option<ExtendedProperties>,
    pub hangout_link: Option<String>,
    pub conference_data: Option<ConferenceData>,
    pub anyone_can_add_self: Option<bool>,
    pub guests_can_invite_others: Option<bool>,
    pub guests_can_modify: Option<bool>,
    pub guests_can_see_other_guests: Option<bool>,
    pub private_copy: Option<bool>,
    pub locked: Option<bool>,
    pub reminders: Option<Reminders>,
    pub source: Option<Source>,
    #[serde(default, deserialize_with = "null_default")]
    pub attachments: Vec<Attachment>,
    pub event_type: Option<String>,
    pub out_of_office_properties: Option<AutoDecline>,
    pub focus_time_properties: Option<AutoDecline>,
    pub working_location_properties: Option<WorkingLocation>,
    pub birthday_properties: Option<Birthday>,
}

/// `start`, `end` and `originalStartTime`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DateTimeValue {
    pub date: Option<String>,
    pub date_time: Option<String>,
    pub time_zone: Option<String>,
}

/// `creator` and `organizer`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Person {
    pub email: Option<String>,
    pub display_name: Option<String>,
    #[serde(rename = "self")]
    pub is_self: Option<bool>,
}

/// An entry of `attendees[]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Attendee {
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub organizer: Option<bool>,
    #[serde(rename = "self")]
    pub is_self: Option<bool>,
    pub resource: Option<bool>,
    pub optional: Option<bool>,
    pub response_status: Option<String>,
    pub comment: Option<String>,
    pub additional_guests: Option<i64>,
}

/// `reminders`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reminders {
    pub use_default: Option<bool>,
    #[serde(default, deserialize_with = "null_default")]
    pub overrides: Vec<Reminder>,
}

/// A reminder (`overrides[]` and `defaultReminders[]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reminder {
    pub method: Option<String>,
    pub minutes: Option<i64>,
}

/// `conferenceData`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConferenceData {
    pub create_request: Option<CreateRequest>,
    #[serde(default, deserialize_with = "null_default")]
    pub entry_points: Vec<EntryPoint>,
    pub conference_solution: Option<ConferenceSolution>,
    pub conference_id: Option<String>,
    pub notes: Option<String>,
}

/// `conferenceData.createRequest`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateRequest {
    pub status: Option<CreateStatus>,
}

/// `conferenceData.createRequest.status`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateStatus {
    pub status_code: Option<String>,
}

/// `conferenceData.entryPoints[]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntryPoint {
    pub entry_point_type: Option<String>,
    pub uri: Option<String>,
    pub label: Option<String>,
    pub pin: Option<String>,
    pub access_code: Option<String>,
    pub meeting_code: Option<String>,
    pub passcode: Option<String>,
    pub password: Option<String>,
    pub region_code: Option<String>,
}

/// `conferenceData.conferenceSolution`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConferenceSolution {
    pub key: Option<SolutionKey>,
    pub name: Option<String>,
    pub icon_uri: Option<String>,
}

/// `conferenceSolution.key`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SolutionKey {
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

/// `source`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Source {
    pub url: Option<String>,
    pub title: Option<String>,
}

/// `attachments[]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Attachment {
    pub file_url: Option<String>,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub icon_link: Option<String>,
    pub file_id: Option<String>,
}

/// `extendedProperties`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ExtendedProperties {
    #[serde(default, deserialize_with = "null_default")]
    pub private: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "null_default")]
    pub shared: BTreeMap<String, String>,
}

/// `outOfOfficeProperties` and `focusTimeProperties`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutoDecline {
    pub auto_decline_mode: Option<String>,
    pub decline_message: Option<String>,
    pub chat_status: Option<String>,
}

/// `workingLocationProperties`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkingLocation {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub home_office: Option<serde_json::Value>,
    pub custom_location: Option<CustomLocation>,
    pub office_location: Option<OfficeLocation>,
}

/// `workingLocationProperties.customLocation`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct CustomLocation {
    pub label: Option<String>,
}

/// `workingLocationProperties.officeLocation`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficeLocation {
    pub building_id: Option<String>,
    pub floor_id: Option<String>,
    pub floor_section_id: Option<String>,
    pub desk_id: Option<String>,
    pub label: Option<String>,
}

/// `birthdayProperties`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Birthday {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub contact: Option<String>,
    pub custom_type_name: Option<String>,
}

/// `calendarList.list` response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CalendarList {
    #[serde(default, deserialize_with = "null_default")]
    pub items: Vec<serde_json::Value>,
    pub next_page_token: Option<String>,
}

/// The `CalendarList` resource.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CalendarListEntry {
    pub id: Option<String>,
    pub etag: Option<String>,
    pub summary: Option<String>,
    pub summary_override: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub time_zone: Option<String>,
    pub data_owner: Option<String>,
    pub color_id: Option<String>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
    pub hidden: Option<bool>,
    pub selected: Option<bool>,
    pub access_role: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub default_reminders: Vec<Reminder>,
    pub primary: Option<bool>,
    pub deleted: Option<bool>,
    pub conference_properties: Option<ConferenceProperties>,
}

/// `conferenceProperties`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConferenceProperties {
    #[serde(default, deserialize_with = "null_default")]
    pub allowed_conference_solution_types: Vec<String>,
}

/// The `Colors` resource.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Colors {
    #[serde(default, deserialize_with = "null_default")]
    pub calendar: BTreeMap<String, ColorPair>,
    #[serde(default, deserialize_with = "null_default")]
    pub event: BTreeMap<String, ColorPair>,
    pub updated: Option<String>,
}

/// A color definition.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ColorPair {
    pub background: Option<String>,
    pub foreground: Option<String>,
}

/// `freebusy.query` response.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FreeBusyResponse {
    #[serde(default, deserialize_with = "null_default")]
    pub calendars: BTreeMap<String, FreeBusyCalendar>,
}

/// `calendars.(key)` of a free/busy response.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FreeBusyCalendar {
    #[serde(default, deserialize_with = "null_default")]
    pub busy: Vec<FreeBusyPeriod>,
    #[serde(default, deserialize_with = "null_default")]
    pub errors: Vec<FreeBusyError>,
}

/// `calendars.(key).busy[]`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FreeBusyPeriod {
    pub start: Option<String>,
    pub end: Option<String>,
}

/// `calendars.(key).errors[]`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FreeBusyError {
    pub reason: Option<String>,
}
