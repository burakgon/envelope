//! Public event types: the [`GoogleEvent`] wrapper and Google-specific data.

use std::collections::BTreeMap;
use std::fmt;

use envelope_ical::{
    Alarm, AlarmAction, Attendee, Event, EventClass, EventTime, Organizer, PartStat,
};
use serde::{Deserialize, Serialize};

use super::time;

/// A Google Calendar event: the provider-neutral [`Event`] plus everything
/// Google-specific that the canonical model has no place for.
///
/// The same type is used for reads and writes. When writing, the canonical
/// fields in [`GoogleEvent::event`] are authoritative except where noted on
/// the individual fields below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleEvent {
    /// Google's event id: unique per calendar and different from the
    /// iCalendar UID (`event.uid`). Occurrences of a recurring series have
    /// ids of their own. Empty for events not yet created on the server
    /// (Google then assigns one; a client-chosen id must use the characters
    /// `a-v` and `0-9` and be 5–1024 characters long).
    pub id: String,
    /// ETag of the resource; pass it as [`WriteOptions::if_match`](super::WriteOptions::if_match)
    /// for optimistic concurrency.
    pub etag: Option<String>,
    /// Link to the event in the Google Calendar web UI.
    pub html_link: Option<String>,
    /// For an occurrence of a recurring series, the Google id of the series
    /// master (its original start is in `event.recurrence_id`).
    pub recurring_event_id: Option<String>,
    /// Event color (key into [`ColorPalette::event`](super::ColorPalette::event)).
    /// `event.color` is derived from it; on writes this field is authoritative.
    pub color_id: Option<String>,
    /// The kind of event (`eventType`). Cannot be changed after creation.
    pub event_type: EventType,
    /// Properties specific to [`GoogleEvent::event_type`].
    pub type_properties: Option<EventTypeProperties>,
    /// Google's visibility. `event.class` is derived from it; on writes
    /// `default` is kept as long as `event.class` is still `Public`.
    pub visibility: Visibility,
    /// The legacy `hangoutLink` (also set for Google Meet conferences).
    pub hangout_link: Option<String>,
    /// Conference details (Google Meet or an add-on such as Zoom). Only
    /// written when requested through [`WriteOptions::add_google_meet`](super::WriteOptions::add_google_meet)
    /// or [`EventPatch`](super::EventPatch).
    pub conference: Option<Conference>,
    /// Whether attendees other than the organizer may modify the event.
    pub guests_can_modify: bool,
    /// Whether attendees may invite others.
    pub guests_can_invite_others: bool,
    /// Whether attendees may see who else is invited.
    pub guests_can_see_other_guests: bool,
    /// Whether anyone can invite themselves (deprecated by Google).
    pub anyone_can_add_self: bool,
    /// Whether attendees may have been left out of this representation.
    pub attendees_omitted: bool,
    /// Whether this is a private copy not propagated to other attendees.
    pub private_copy: bool,
    /// Whether this is a locked copy (summary, time, location and
    /// recurrence cannot be changed).
    pub locked: bool,
    /// Whether the end time is actually unspecified (Google still reports one).
    pub end_time_unspecified: bool,
    /// Whether the calendar's default reminders apply. When `true`,
    /// `event.alarms` mirror the calendar defaults and are not written;
    /// use [`GoogleEvent::set_alarms`] to switch to custom reminders.
    pub use_default_reminders: bool,
    /// Whether the authenticated user organizes the event (`organizer.self`).
    pub organizer_is_self: bool,
    /// Who created the event.
    pub creator: Option<Organizer>,
    /// Google-only attendee data, one entry per attendee in `event.attendees`.
    pub attendee_details: Vec<AttendeeDetails>,
    /// Title of the event's source (`source.title`); the URL is `event.url`.
    pub source_title: Option<String>,
    /// File attachments (usually Google Drive files).
    pub attachments: Vec<Attachment>,
    /// Extended properties set by applications.
    pub extended_properties: ExtendedProperties,
    /// Recurrence lines the canonical model cannot express (such as
    /// `EXRULE`), kept verbatim so that writes preserve them.
    pub extra_recurrence: Vec<String>,
    /// The provider-neutral event.
    pub event: Event,
}

impl GoogleEvent {
    /// Wraps a canonical event for insertion with Google's defaults: no id
    /// (Google assigns one), a `default` event, guests may invite others and
    /// see each other, and the calendar's default reminders apply when the
    /// event has no alarms of its own.
    pub fn new(event: Event) -> Self {
        GoogleEvent {
            id: String::new(),
            etag: None,
            html_link: None,
            recurring_event_id: None,
            color_id: None,
            event_type: EventType::Default,
            type_properties: None,
            visibility: match event.class {
                EventClass::Public => Visibility::Default,
                EventClass::Private => Visibility::Private,
                EventClass::Confidential => Visibility::Confidential,
            },
            hangout_link: None,
            conference: None,
            guests_can_modify: false,
            guests_can_invite_others: true,
            guests_can_see_other_guests: true,
            anyone_can_add_self: false,
            attendees_omitted: false,
            private_copy: false,
            locked: false,
            end_time_unspecified: false,
            use_default_reminders: event.alarms.is_empty(),
            organizer_is_self: true,
            creator: None,
            attendee_details: Vec::new(),
            source_title: None,
            attachments: Vec::new(),
            extended_properties: ExtendedProperties::default(),
            extra_recurrence: Vec::new(),
            event,
        }
    }

    /// The authenticated user's own attendee entry, if they are invited.
    pub fn self_attendee(&self) -> Option<&Attendee> {
        let email = &self.attendee_details.iter().find(|d| d.is_self)?.email;
        self.event
            .attendees
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(email))
    }

    /// The authenticated user's RSVP state, if they are invited.
    pub fn self_response(&self) -> Option<PartStat> {
        self.self_attendee().map(|a| a.partstat)
    }

    /// Google-only data of the attendee with the given address.
    pub fn attendee_details(&self, email: &str) -> Option<&AttendeeDetails> {
        self.attendee_details
            .iter()
            .find(|d| d.email.eq_ignore_ascii_case(email))
    }

    /// Replaces the reminders with custom ones (Google keeps at most five,
    /// between 0 and 40 320 minutes before the start).
    pub fn set_alarms(&mut self, alarms: Vec<Alarm>) {
        self.event.alarms = alarms;
        self.use_default_reminders = false;
    }

    /// Adds `EXDATE`s for the cancelled occurrences that belong to this series
    /// (matching [`CancelledOccurrence::recurring_event_id`] against
    /// [`GoogleEvent::id`]). Values are converted to the form of the series'
    /// start so that recurrence expansion can match them. Returns how many
    /// new exclusions were added.
    pub fn apply_cancelled_occurrences(&mut self, cancelled: &[CancelledOccurrence]) -> usize {
        let mut added = 0;
        for occurrence in cancelled.iter().filter(|c| c.recurring_event_id == self.id) {
            let exdate = time::conform(occurrence.original_start.clone(), &self.event.start);
            if !self.event.exdates.contains(&exdate) {
                self.event.exdates.push(exdate);
                added += 1;
            }
        }
        added
    }
}

/// Google-only data about an attendee.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttendeeDetails {
    /// Lower-cased address, matching [`Attendee::email`].
    pub email: String,
    /// Whether this entry is the authenticated user (`self`).
    pub is_self: bool,
    /// The attendee's response comment.
    pub comment: Option<String>,
    /// Number of additional guests the attendee brings.
    pub additional_guests: u32,
}

/// A cancelled occurrence of a recurring series.
///
/// Google reports a deleted single occurrence as a cancelled exception
/// instead of adding an `EXDATE` to the series. Treat it as an exclusion of
/// `original_start` from the series whose Google id is `recurring_event_id`
/// (see [`GoogleEvent::apply_cancelled_occurrences`]), and drop any stored
/// override with the same `id`. Google asks clients to keep these for the
/// lifetime of the series: a later update of the master does not repeat them,
/// so store them keyed by `id` and re-apply them whenever the master changes.
/// If an upsert with the same `id` arrives later (the occurrence was
/// restored), it replaces the cancellation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelledOccurrence {
    /// Google id of the cancelled occurrence.
    pub id: String,
    /// Google id of the series master.
    pub recurring_event_id: String,
    /// The occurrence's original start (the `EXDATE` value).
    pub original_start: EventTime,
}

/// One item of an event list, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventChange {
    /// A new or changed event: a single event, a series master or a modified
    /// occurrence of a series (`event.recurrence_id` set).
    Upsert(Box<GoogleEvent>),
    /// A cancelled occurrence of a series (an exclusion).
    CancelledOccurrence(CancelledOccurrence),
    /// The event with this Google id was deleted (or the user was removed
    /// from it). For a series master this removes the whole series,
    /// including its stored overrides.
    Deleted(String),
}

/// An event list item that could not be mapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedItem {
    /// The Google id, if the item had one.
    pub id: Option<String>,
    /// Why the item was skipped.
    pub reason: String,
}

/// Google's `eventType`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum EventType {
    /// A regular event.
    #[default]
    Default,
    /// A birthday or other special date from Google Contacts.
    Birthday,
    /// A focus-time block.
    FocusTime,
    /// An event Gmail created from an e-mail (cannot be created via the API).
    FromGmail,
    /// An out-of-office block.
    OutOfOffice,
    /// A working-location marker.
    WorkingLocation,
    /// A type this crate does not know yet.
    Other(String),
}

impl EventType {
    /// Google's wire value.
    pub fn as_str(&self) -> &str {
        match self {
            EventType::Default => "default",
            EventType::Birthday => "birthday",
            EventType::FocusTime => "focusTime",
            EventType::FromGmail => "fromGmail",
            EventType::OutOfOffice => "outOfOffice",
            EventType::WorkingLocation => "workingLocation",
            EventType::Other(other) => other,
        }
    }
}

impl From<&str> for EventType {
    fn from(value: &str) -> Self {
        match value {
            "" | "default" => EventType::Default,
            "birthday" => EventType::Birthday,
            "focusTime" => EventType::FocusTime,
            "fromGmail" => EventType::FromGmail,
            "outOfOffice" => EventType::OutOfOffice,
            "workingLocation" => EventType::WorkingLocation,
            other => EventType::Other(other.to_owned()),
        }
    }
}

impl From<String> for EventType {
    fn from(value: String) -> Self {
        EventType::from(value.as_str())
    }
}

impl From<EventType> for String {
    fn from(value: EventType) -> Self {
        value.as_str().to_owned()
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Properties that only exist for particular [`EventType`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventTypeProperties {
    /// `outOfOfficeProperties`.
    OutOfOffice(AutoDeclineSettings),
    /// `focusTimeProperties`.
    FocusTime(AutoDeclineSettings),
    /// `workingLocationProperties`.
    WorkingLocation(WorkingLocation),
    /// `birthdayProperties`.
    Birthday(BirthdayProperties),
}

/// Auto-decline settings of out-of-office and focus-time events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoDeclineSettings {
    /// `declineNone`, `declineAllConflictingInvitations` or
    /// `declineOnlyNewConflictingInvitations`.
    pub auto_decline_mode: Option<String>,
    /// Response message sent with automatic declines.
    pub decline_message: Option<String>,
    /// Chat status during focus time (`available` or `doNotDisturb`).
    pub chat_status: Option<String>,
}

/// Where the user works, for `workingLocation` events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkingLocation {
    /// Working from home.
    HomeOffice,
    /// Working from an office.
    Office(OfficeLocation),
    /// Working from somewhere else.
    Custom {
        /// Free-form label of the location.
        label: Option<String>,
    },
}

/// An office working location.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeLocation {
    /// Office name shown in Google Calendar.
    pub label: Option<String>,
    /// Building id from the organization's resources database.
    pub building_id: Option<String>,
    /// Floor id.
    pub floor_id: Option<String>,
    /// Floor section id.
    pub floor_section_id: Option<String>,
    /// Desk id.
    pub desk_id: Option<String>,
}

/// Details of a `birthday` event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthdayProperties {
    /// `birthday`, `anniversary`, `custom`, `other` or `self`.
    pub kind: String,
    /// People API resource name of the contact (`people/c…`).
    pub contact: Option<String>,
    /// Label of a `custom` special date.
    pub custom_type_name: Option<String>,
}

/// Google's event visibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Visibility {
    /// The calendar's default visibility.
    #[default]
    Default,
    /// Details visible to everyone who can see the calendar.
    Public,
    /// Details visible to attendees only.
    Private,
    /// Same as `Private` (kept by Google for compatibility).
    Confidential,
}

impl Visibility {
    /// Google's wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Default => "default",
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Confidential => "confidential",
        }
    }

    /// Parses Google's value; unknown values are treated as `default`.
    pub fn parse(value: &str) -> Self {
        match value {
            "public" => Visibility::Public,
            "private" => Visibility::Private,
            "confidential" => Visibility::Confidential,
            _ => Visibility::Default,
        }
    }

    /// The iCalendar `CLASS` this visibility corresponds to.
    pub fn class(self) -> EventClass {
        match self {
            Visibility::Default | Visibility::Public => EventClass::Public,
            Visibility::Private => EventClass::Private,
            Visibility::Confidential => EventClass::Confidential,
        }
    }

    /// The visibility to write for `class`, keeping `self` when it already
    /// corresponds to `class` (so `default` survives a round trip).
    pub fn for_class(self, class: EventClass) -> Self {
        if self.class() == class {
            return self;
        }
        match class {
            EventClass::Public => Visibility::Default,
            EventClass::Private => Visibility::Private,
            EventClass::Confidential => Visibility::Confidential,
        }
    }
}

/// A reminder as Google represents it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    /// How the user is reminded.
    pub method: ReminderMethod,
    /// Minutes before the start (Google allows 0–40 320).
    pub minutes: i64,
}

/// How a [`Reminder`] is delivered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReminderMethod {
    /// A notification on the user's devices.
    #[default]
    Popup,
    /// An e-mail.
    Email,
}

/// The largest reminder offset Google accepts (four weeks).
pub(crate) const MAX_REMINDER_MINUTES: i64 = 40_320;

impl Reminder {
    /// The canonical alarm for this reminder.
    pub fn to_alarm(&self) -> Alarm {
        Alarm {
            action: match self.method {
                ReminderMethod::Popup => AlarmAction::Display,
                ReminderMethod::Email => AlarmAction::Email,
            },
            minutes_before: self.minutes,
            description: None,
        }
    }

    /// The Google reminder closest to an alarm: display and audio alarms
    /// become pop-ups, and the offset is clamped to 0–40 320 minutes.
    pub fn from_alarm(alarm: &Alarm) -> Self {
        Reminder {
            method: match alarm.action {
                AlarmAction::Email => ReminderMethod::Email,
                AlarmAction::Display | AlarmAction::Audio => ReminderMethod::Popup,
            },
            minutes: alarm.minutes_before.clamp(0, MAX_REMINDER_MINUTES),
        }
    }
}

/// Conference details of an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conference {
    /// Conference id (for Meet, the meeting code such as `abc-defg-hij`).
    pub conference_id: Option<String>,
    /// User-visible solution name, e.g. `Google Meet`.
    pub solution_name: Option<String>,
    /// Solution type: `hangoutsMeet`, `addOn`, …
    pub solution_type: Option<String>,
    /// Icon of the solution.
    pub icon_uri: Option<String>,
    /// Ways to join.
    pub entry_points: Vec<EntryPoint>,
    /// Additional notes (may contain HTML).
    pub notes: Option<String>,
    /// Status of a pending conference creation: `pending`, `success` or
    /// `failure`. While `pending`, re-fetch the event later for the links.
    pub create_status: Option<String>,
}

impl Conference {
    /// The video link, if any.
    pub fn video_uri(&self) -> Option<&str> {
        self.entry_points
            .iter()
            .find(|e| e.kind == EntryPointKind::Video)
            .map(|e| e.uri.as_str())
    }
}

/// A way to join a conference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPoint {
    /// The kind of entry point.
    pub kind: EntryPointKind,
    /// `https:` for video, `tel:` for phone, `sip:` for SIP.
    pub uri: String,
    /// User-visible label (the URL or phone number).
    pub label: Option<String>,
    /// PIN to enter.
    pub pin: Option<String>,
    /// Access code.
    pub access_code: Option<String>,
    /// Meeting code.
    pub meeting_code: Option<String>,
    /// Passcode.
    pub passcode: Option<String>,
    /// Password.
    pub password: Option<String>,
    /// Region of a phone number (CLDR region code such as `SE`).
    pub region_code: Option<String>,
}

/// Kind of a conference [`EntryPoint`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryPointKind {
    /// A video link.
    Video,
    /// A dial-in number.
    Phone,
    /// A SIP address.
    Sip,
    /// Further joining information (also used for unknown kinds).
    #[default]
    More,
}

/// A file attached to an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// URL of the file (for Drive files, the `alternateLink`).
    pub file_url: String,
    /// Title of the attachment.
    pub title: Option<String>,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Icon URL.
    pub icon_link: Option<String>,
    /// Drive file id.
    pub file_id: Option<String>,
}

/// Application-defined key/value properties of an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedProperties {
    /// Properties private to this copy of the event.
    pub private: BTreeMap<String, String>,
    /// Properties shared with the other attendees' copies.
    pub shared: BTreeMap<String, String>,
}

impl ExtendedProperties {
    /// True if no properties are set.
    pub fn is_empty(&self) -> bool {
        self.private.is_empty() && self.shared.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn event_type_round_trips_unknown_values() {
        assert_eq!(EventType::from("focusTime"), EventType::FocusTime);
        let other = EventType::from("somethingNew");
        assert_eq!(other.as_str(), "somethingNew");
        let json = serde_json::to_string(&other).expect("serialize");
        assert_eq!(json, "\"somethingNew\"");
        let back: EventType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, other);
    }

    #[test]
    fn visibility_keeps_default_while_class_is_unchanged() {
        assert_eq!(
            Visibility::Default.for_class(EventClass::Public),
            Visibility::Default
        );
        assert_eq!(
            Visibility::Public.for_class(EventClass::Public),
            Visibility::Public
        );
        assert_eq!(
            Visibility::Default.for_class(EventClass::Private),
            Visibility::Private
        );
        assert_eq!(
            Visibility::Private.for_class(EventClass::Public),
            Visibility::Default
        );
    }

    #[test]
    fn reminders_convert_to_alarms_and_back() {
        let email = Reminder {
            method: ReminderMethod::Email,
            minutes: 1440,
        };
        let alarm = email.to_alarm();
        assert_eq!(alarm.action, AlarmAction::Email);
        assert_eq!(alarm.minutes_before, 1440);
        assert_eq!(Reminder::from_alarm(&alarm), email);
        let after_start = Alarm {
            action: AlarmAction::Audio,
            minutes_before: -5,
            description: None,
        };
        assert_eq!(
            Reminder::from_alarm(&after_start),
            Reminder {
                method: ReminderMethod::Popup,
                minutes: 0
            }
        );
    }

    #[test]
    fn new_event_uses_google_defaults() {
        let date = NaiveDate::from_ymd_opt(2024, 3, 1).expect("date");
        let event = Event::new(
            "uid-1",
            EventTime::Date { date },
            EventTime::Date {
                date: date.succ_opt().expect("date"),
            },
        );
        let google = GoogleEvent::new(event);
        assert!(google.id.is_empty());
        assert!(google.use_default_reminders);
        assert_eq!(google.visibility, Visibility::Default);
        assert!(google.guests_can_invite_others && google.guests_can_see_other_guests);
        assert_eq!(google.self_response(), None);
    }
}
