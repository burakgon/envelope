//! Mapping between Google event JSON and [`GoogleEvent`] / [`Event`].

use std::collections::HashMap;
use std::sync::Arc;

use envelope_ical::{
    Alarm, Attendee, AttendeeRole, CalendarUserType, Event, EventStatus, Organizer, PartStat,
    Transparency,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::event::{
    Attachment, AttendeeDetails, AutoDeclineSettings, BirthdayProperties, CancelledOccurrence,
    Conference, EntryPoint, EntryPointKind, EventChange, EventType, EventTypeProperties,
    ExtendedProperties, GoogleEvent, OfficeLocation, Reminder, ReminderMethod, Visibility,
    WorkingLocation,
};
use super::{ColorPalette, recurrence, time, wire};
use crate::error::{Error, Result};
use crate::html;
use crate::serde_util::non_empty;

/// Google keeps at most five reminder overrides per event.
const MAX_REMINDER_OVERRIDES: usize = 5;

/// Calendar-level data needed to map events: Google only sends
/// `reminders.useDefault` and `colorId` on the event itself.
#[derive(Debug, Clone, Default)]
pub struct MappingContext {
    /// The user's default reminders for the calendar (`defaultReminders` of
    /// the calendar list entry or of an `events.list` response); they become
    /// the alarms of events that use the calendar defaults.
    pub default_reminders: Vec<Reminder>,
    /// The color palette from `colors.get`, to turn `colorId` into a hex color.
    pub palette: Option<Arc<ColorPalette>>,
}

impl EventChange {
    /// Maps one Google `Event` resource, classifying cancelled items into
    /// [`EventChange::CancelledOccurrence`] (a cancelled occurrence of a
    /// series) and [`EventChange::Deleted`] (everything else that is cancelled).
    pub fn from_json(resource: &Value, context: &MappingContext) -> Result<Self> {
        let wire = wire::Event::deserialize(resource).map_err(|source| Error::Decode {
            context: "event",
            source,
        })?;
        classify(wire, context).map_err(Error::InvalidResponse)
    }
}

impl GoogleEvent {
    /// Maps one Google `Event` resource without classifying cancellations:
    /// a cancelled event is returned with `event.status` set to `Cancelled`
    /// as long as it still carries its times.
    pub fn from_json(resource: &Value, context: &MappingContext) -> Result<Self> {
        let wire = wire::Event::deserialize(resource).map_err(|source| Error::Decode {
            context: "event",
            source,
        })?;
        map_event(wire, context).map_err(Error::InvalidResponse)
    }

    /// Renders the event as a Google `Event` resource for `insert`/`update`.
    ///
    /// Read-only fields (organizer, creator, html link, conference data, …)
    /// are not included. Floating times are anchored in `floating_time_zone`
    /// and fail with [`Error::InvalidInput`] without one.
    pub fn to_json(&self, floating_time_zone: Option<&str>) -> Result<Value> {
        event_to_json(self, floating_time_zone).map(Value::Object)
    }
}

/// Classifies a list item (see [`EventChange`]).
pub(crate) fn classify(wire: wire::Event, context: &MappingContext) -> Result<EventChange, String> {
    if wire.status.as_deref() == Some("cancelled") {
        let id = non_empty(wire.id.clone()).ok_or("cancelled event without id")?;
        if let (Some(series), Some(original)) = (
            non_empty(wire.recurring_event_id.clone()),
            wire.original_start_time.as_ref(),
        ) {
            let fallback_tz = wire.start.as_ref().and_then(|s| s.time_zone.as_deref());
            let original_start = time::parse(original, fallback_tz)
                .map_err(|e| format!("cancelled occurrence {id}: {e}"))?;
            return Ok(EventChange::CancelledOccurrence(CancelledOccurrence {
                id,
                recurring_event_id: series,
                original_start,
            }));
        }
        return Ok(EventChange::Deleted(id));
    }
    map_event(wire, context).map(|event| EventChange::Upsert(Box::new(event)))
}

/// Maps a Google event into a [`GoogleEvent`].
pub(crate) fn map_event(
    wire: wire::Event,
    context: &MappingContext,
) -> Result<GoogleEvent, String> {
    let id = non_empty(wire.id).ok_or("event without id")?;
    let start_value = wire
        .start
        .as_ref()
        .ok_or_else(|| format!("event {id} has no start time"))?;
    let start = time::parse(start_value, None).map_err(|e| format!("event {id} start: {e}"))?;
    let end = match &wire.end {
        Some(end) => time::parse(end, None).map_err(|e| format!("event {id} end: {e}"))?,
        None => time::default_end(&start),
    };
    let recurring_event_id = non_empty(wire.recurring_event_id);
    let uid = non_empty(wire.ical_uid).unwrap_or_else(|| {
        format!(
            "{}@google.com",
            recurring_event_id.as_deref().unwrap_or(&id)
        )
    });

    let mut event = Event::new(uid, start, end);
    if recurring_event_id.is_some()
        && let Some(original) = &wire.original_start_time
    {
        let recurrence_id = time::parse(original, start_value.time_zone.as_deref())
            .map_err(|e| format!("event {id} originalStartTime: {e}"))?;
        event.recurrence_id = Some(recurrence_id);
    }
    event.summary = wire.summary.unwrap_or_default();
    (event.description, event.description_html) = split_description(wire.description);
    event.location = non_empty(wire.location);

    let recurrence = recurrence::parse(&wire.recurrence, &event.start);
    event.rrule = recurrence.rrule;
    event.rdates = recurrence.rdates;
    event.exdates = recurrence.exdates;

    event.status = match wire.status.as_deref() {
        Some("tentative") => EventStatus::Tentative,
        Some("cancelled") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    };
    event.transparency = match wire.transparency.as_deref() {
        Some("transparent") => Transparency::Transparent,
        _ => Transparency::Opaque,
    };
    let visibility = Visibility::parse(wire.visibility.as_deref().unwrap_or_default());
    event.class = visibility.class();

    let organizer_is_self = wire
        .organizer
        .as_ref()
        .and_then(|o| o.is_self)
        .unwrap_or(false);
    event.organizer = wire.organizer.and_then(map_person);
    let creator = wire.creator.and_then(map_person);
    let (attendees, attendee_details) = map_attendees(wire.attendees);
    event.attendees = attendees;

    let use_default_reminders = wire
        .reminders
        .as_ref()
        .and_then(|r| r.use_default)
        .unwrap_or(false);
    event.alarms = if use_default_reminders {
        context
            .default_reminders
            .iter()
            .map(Reminder::to_alarm)
            .collect()
    } else {
        wire.reminders
            .map(|r| r.overrides)
            .unwrap_or_default()
            .iter()
            .filter_map(map_reminder)
            .map(|r| r.to_alarm())
            .collect()
    };

    let color_id = non_empty(wire.color_id);
    event.color = color_id.as_ref().and_then(|id| {
        context
            .palette
            .as_ref()
            .and_then(|p| p.event.get(id))
            .map(|c| c.background.clone())
    });

    let conference = wire.conference_data.map(map_conference);
    let hangout_link = non_empty(wire.hangout_link);
    event.conference_url = conference
        .as_ref()
        .and_then(Conference::video_uri)
        .map(str::to_owned)
        .or_else(|| hangout_link.clone());

    event.sequence = wire.sequence.unwrap_or(0);
    event.created = wire.created.as_deref().and_then(time::parse_timestamp);
    event.last_modified = wire.updated.as_deref().and_then(time::parse_timestamp);
    let (source_url, source_title) = wire
        .source
        .map(|s| (non_empty(s.url), non_empty(s.title)))
        .unwrap_or_default();
    event.url = source_url;

    let event_type = EventType::from(wire.event_type.as_deref().unwrap_or_default());
    let type_properties = match &event_type {
        EventType::OutOfOffice => wire
            .out_of_office_properties
            .map(|p| EventTypeProperties::OutOfOffice(map_auto_decline(p))),
        EventType::FocusTime => wire
            .focus_time_properties
            .map(|p| EventTypeProperties::FocusTime(map_auto_decline(p))),
        EventType::WorkingLocation => wire
            .working_location_properties
            .and_then(map_working_location)
            .map(EventTypeProperties::WorkingLocation),
        EventType::Birthday => wire.birthday_properties.map(|p| {
            EventTypeProperties::Birthday(BirthdayProperties {
                kind: non_empty(p.kind).unwrap_or_else(|| "birthday".to_owned()),
                contact: non_empty(p.contact),
                custom_type_name: non_empty(p.custom_type_name),
            })
        }),
        _ => None,
    };

    Ok(GoogleEvent {
        id,
        etag: non_empty(wire.etag),
        html_link: non_empty(wire.html_link),
        recurring_event_id,
        color_id,
        event_type,
        type_properties,
        visibility,
        hangout_link,
        conference,
        guests_can_modify: wire.guests_can_modify.unwrap_or(false),
        guests_can_invite_others: wire.guests_can_invite_others.unwrap_or(true),
        guests_can_see_other_guests: wire.guests_can_see_other_guests.unwrap_or(true),
        anyone_can_add_self: wire.anyone_can_add_self.unwrap_or(false),
        attendees_omitted: wire.attendees_omitted.unwrap_or(false),
        private_copy: wire.private_copy.unwrap_or(false),
        locked: wire.locked.unwrap_or(false),
        end_time_unspecified: wire.end_time_unspecified.unwrap_or(false),
        use_default_reminders,
        organizer_is_self,
        creator,
        attendee_details,
        source_title,
        attachments: wire
            .attachments
            .into_iter()
            .filter_map(|a| {
                Some(Attachment {
                    file_url: non_empty(a.file_url)?,
                    title: non_empty(a.title),
                    mime_type: non_empty(a.mime_type),
                    icon_link: non_empty(a.icon_link),
                    file_id: non_empty(a.file_id),
                })
            })
            .collect(),
        extended_properties: wire
            .extended_properties
            .map(|p| ExtendedProperties {
                private: p.private,
                shared: p.shared,
            })
            .unwrap_or_default(),
        extra_recurrence: recurrence.unsupported,
        event,
    })
}

/// Splits a Google description into plain text and, if it is HTML, the
/// original markup.
fn split_description(raw: Option<String>) -> (Option<String>, Option<String>) {
    let Some(raw) = non_empty(raw) else {
        return (None, None);
    };
    if html::looks_like_html(&raw) {
        let text = html::html_to_text(&raw);
        (non_empty(Some(text)), Some(raw))
    } else {
        (Some(raw), None)
    }
}

/// The description to write: the HTML version as long as the plain text
/// still matches it (i.e. the caller did not edit the plain text), else the
/// plain text.
pub(crate) fn description_for_write(event: &Event) -> Option<String> {
    if let Some(markup) = &event.description_html
        && html::html_to_text(markup) == event.description.as_deref().unwrap_or_default()
    {
        return Some(markup.clone());
    }
    event.description.clone()
}

fn map_person(person: wire::Person) -> Option<Organizer> {
    let email = non_empty(person.email)?.trim().to_lowercase();
    Some(Organizer {
        email,
        name: non_empty(person.display_name),
    })
}

fn map_attendees(wire: Vec<wire::Attendee>) -> (Vec<Attendee>, Vec<AttendeeDetails>) {
    let mut attendees = Vec::with_capacity(wire.len());
    let mut details = Vec::with_capacity(wire.len());
    for attendee in wire {
        let Some(email) = non_empty(attendee.email).map(|e| e.trim().to_lowercase()) else {
            continue;
        };
        let is_organizer = attendee.organizer.unwrap_or(false);
        let is_resource = attendee.resource.unwrap_or(false);
        attendees.push(Attendee {
            email: email.clone(),
            name: non_empty(attendee.display_name),
            partstat: parse_partstat(attendee.response_status.as_deref()),
            role: if is_organizer {
                AttendeeRole::Chair
            } else if attendee.optional.unwrap_or(false) {
                AttendeeRole::Optional
            } else {
                AttendeeRole::Required
            },
            cutype: if is_resource {
                CalendarUserType::Resource
            } else {
                CalendarUserType::Individual
            },
            rsvp: !is_organizer && !is_resource,
        });
        details.push(AttendeeDetails {
            email,
            is_self: attendee.is_self.unwrap_or(false),
            comment: non_empty(attendee.comment),
            additional_guests: attendee
                .additional_guests
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
        });
    }
    (attendees, details)
}

pub(crate) fn parse_partstat(status: Option<&str>) -> PartStat {
    match status {
        Some("accepted") => PartStat::Accepted,
        Some("declined") => PartStat::Declined,
        Some("tentative") => PartStat::Tentative,
        _ => PartStat::NeedsAction,
    }
}

/// Google's `responseStatus` for a participation state (Google has no
/// "delegated"; it is written as `needsAction`).
pub(crate) fn partstat_str(partstat: PartStat) -> &'static str {
    match partstat {
        PartStat::Accepted => "accepted",
        PartStat::Declined => "declined",
        PartStat::Tentative => "tentative",
        PartStat::NeedsAction | PartStat::Delegated => "needsAction",
    }
}

pub(crate) fn map_reminder(reminder: &wire::Reminder) -> Option<Reminder> {
    Some(Reminder {
        method: match reminder.method.as_deref() {
            Some("email") => ReminderMethod::Email,
            _ => ReminderMethod::Popup,
        },
        minutes: reminder.minutes?,
    })
}

fn map_conference(data: wire::ConferenceData) -> Conference {
    let (solution_name, solution_type, icon_uri) = data
        .conference_solution
        .map(|s| {
            (
                non_empty(s.name),
                s.key.and_then(|k| non_empty(k.kind)),
                non_empty(s.icon_uri),
            )
        })
        .unwrap_or_default();
    Conference {
        conference_id: non_empty(data.conference_id),
        solution_name,
        solution_type,
        icon_uri,
        entry_points: data
            .entry_points
            .into_iter()
            .filter_map(|e| {
                Some(EntryPoint {
                    kind: match e.entry_point_type.as_deref() {
                        Some("video") => EntryPointKind::Video,
                        Some("phone") => EntryPointKind::Phone,
                        Some("sip") => EntryPointKind::Sip,
                        _ => EntryPointKind::More,
                    },
                    uri: non_empty(e.uri)?,
                    label: non_empty(e.label),
                    pin: non_empty(e.pin),
                    access_code: non_empty(e.access_code),
                    meeting_code: non_empty(e.meeting_code),
                    passcode: non_empty(e.passcode),
                    password: non_empty(e.password),
                    region_code: non_empty(e.region_code),
                })
            })
            .collect(),
        notes: non_empty(data.notes),
        create_status: data
            .create_request
            .and_then(|r| r.status)
            .and_then(|s| non_empty(s.status_code)),
    }
}

fn map_auto_decline(props: wire::AutoDecline) -> AutoDeclineSettings {
    AutoDeclineSettings {
        auto_decline_mode: non_empty(props.auto_decline_mode),
        decline_message: non_empty(props.decline_message),
        chat_status: non_empty(props.chat_status),
    }
}

fn map_working_location(props: wire::WorkingLocation) -> Option<WorkingLocation> {
    match props.kind.as_deref() {
        Some("homeOffice") => Some(WorkingLocation::HomeOffice),
        Some("officeLocation") => {
            let office = props.office_location.unwrap_or_default();
            Some(WorkingLocation::Office(OfficeLocation {
                label: non_empty(office.label),
                building_id: non_empty(office.building_id),
                floor_id: non_empty(office.floor_id),
                floor_section_id: non_empty(office.floor_section_id),
                desk_id: non_empty(office.desk_id),
            }))
        }
        Some("customLocation") => Some(WorkingLocation::Custom {
            label: props.custom_location.and_then(|c| non_empty(c.label)),
        }),
        _ if props.home_office.is_some() => Some(WorkingLocation::HomeOffice),
        _ => None,
    }
}

/// Renders a [`GoogleEvent`] as a Google `Event` resource.
pub(crate) fn event_to_json(
    google: &GoogleEvent,
    floating_tz: Option<&str>,
) -> Result<Map<String, Value>> {
    let event = &google.event;
    let recurrence = if event.recurrence_id.is_none() {
        recurrence::format(event, &google.extra_recurrence)
    } else {
        Vec::new()
    };
    let recurring = !recurrence.is_empty();

    let mut body = Map::new();
    if !google.id.is_empty() {
        body.insert("id".into(), google.id.clone().into());
    }
    if !event.uid.is_empty() {
        body.insert("iCalUID".into(), event.uid.clone().into());
    }
    body.insert("summary".into(), event.summary.clone().into());
    body.insert("description".into(), description_for_write(event).into());
    body.insert("location".into(), event.location.clone().into());
    body.insert(
        "start".into(),
        time::to_json(&event.start, recurring, floating_tz)?,
    );
    body.insert(
        "end".into(),
        time::to_json(&event.end, recurring, floating_tz)?,
    );
    if google.end_time_unspecified {
        body.insert("endTimeUnspecified".into(), true.into());
    }
    if recurring {
        body.insert("recurrence".into(), recurrence.into());
    }
    body.insert("status".into(), status_str(event.status).into());
    body.insert(
        "transparency".into(),
        transparency_str(event.transparency).into(),
    );
    body.insert(
        "visibility".into(),
        google.visibility.for_class(event.class).as_str().into(),
    );
    body.insert(
        "attendees".into(),
        attendees_json(&event.attendees, &google.attendee_details),
    );
    if google.attendees_omitted {
        body.insert("attendeesOmitted".into(), true.into());
    }
    body.insert("guestsCanModify".into(), google.guests_can_modify.into());
    body.insert(
        "guestsCanInviteOthers".into(),
        google.guests_can_invite_others.into(),
    );
    body.insert(
        "guestsCanSeeOtherGuests".into(),
        google.guests_can_see_other_guests.into(),
    );
    if google.anyone_can_add_self {
        body.insert("anyoneCanAddSelf".into(), true.into());
    }
    body.insert(
        "reminders".into(),
        reminders_json(google.use_default_reminders, &event.alarms),
    );
    if let Some(color_id) = &google.color_id {
        body.insert("colorId".into(), color_id.clone().into());
    }
    if google.event_type != EventType::Default {
        body.insert("eventType".into(), google.event_type.as_str().into());
    }
    if let Some((key, value)) = google.type_properties.as_ref().map(type_properties_json) {
        body.insert(key.into(), value);
    }
    if let Some(url) = event.url.as_deref().filter(|u| is_http_url(u)) {
        let title = google
            .source_title
            .clone()
            .unwrap_or_else(|| event.summary.clone());
        body.insert("source".into(), json!({ "url": url, "title": title }));
    }
    if !google.extended_properties.is_empty() {
        body.insert(
            "extendedProperties".into(),
            json!({
                "private": google.extended_properties.private,
                "shared": google.extended_properties.shared,
            }),
        );
    }
    if !google.attachments.is_empty() {
        body.insert(
            "attachments".into(),
            google
                .attachments
                .iter()
                .map(|a| {
                    let mut item = Map::new();
                    item.insert("fileUrl".into(), a.file_url.clone().into());
                    insert_opt(&mut item, "title", &a.title);
                    insert_opt(&mut item, "mimeType", &a.mime_type);
                    insert_opt(&mut item, "iconLink", &a.icon_link);
                    Value::Object(item)
                })
                .collect::<Vec<_>>()
                .into(),
        );
    }
    Ok(body)
}

fn insert_opt(map: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), value.clone().into());
    }
}

fn is_http_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

pub(crate) fn status_str(status: EventStatus) -> &'static str {
    match status {
        EventStatus::Confirmed => "confirmed",
        EventStatus::Tentative => "tentative",
        EventStatus::Cancelled => "cancelled",
    }
}

pub(crate) fn transparency_str(transparency: Transparency) -> &'static str {
    match transparency {
        Transparency::Opaque => "opaque",
        Transparency::Transparent => "transparent",
    }
}

/// Google `attendees[]` for the canonical attendees, carrying over comments
/// and additional guests from `details`.
pub(crate) fn attendees_json(attendees: &[Attendee], details: &[AttendeeDetails]) -> Value {
    let details: HashMap<String, &AttendeeDetails> = details
        .iter()
        .map(|d| (d.email.to_lowercase(), d))
        .collect();
    attendees
        .iter()
        .filter(|a| !a.email.trim().is_empty())
        .map(|attendee| {
            let mut item = Map::new();
            item.insert("email".into(), attendee.email.trim().into());
            insert_opt(&mut item, "displayName", &attendee.name);
            if attendee.role == AttendeeRole::Optional {
                item.insert("optional".into(), true.into());
            }
            if matches!(
                attendee.cutype,
                CalendarUserType::Resource | CalendarUserType::Room
            ) {
                item.insert("resource".into(), true.into());
            }
            item.insert(
                "responseStatus".into(),
                partstat_str(attendee.partstat).into(),
            );
            if let Some(extra) = details.get(&attendee.email.trim().to_lowercase()) {
                insert_opt(&mut item, "comment", &extra.comment);
                if extra.additional_guests > 0 {
                    item.insert("additionalGuests".into(), extra.additional_guests.into());
                }
            }
            Value::Object(item)
        })
        .collect::<Vec<_>>()
        .into()
}

/// Google `reminders` for the given alarms: duplicates are dropped, offsets
/// clamped to Google's range and at most five overrides kept.
pub(crate) fn reminders_json(use_default: bool, alarms: &[Alarm]) -> Value {
    if use_default {
        return json!({ "useDefault": true });
    }
    let mut overrides: Vec<Reminder> = Vec::new();
    for reminder in alarms.iter().map(Reminder::from_alarm) {
        if !overrides.contains(&reminder) {
            overrides.push(reminder);
        }
    }
    if overrides.len() > MAX_REMINDER_OVERRIDES {
        tracing::debug!(
            dropped = overrides.len() - MAX_REMINDER_OVERRIDES,
            "Google keeps at most five reminders; dropping the rest"
        );
        overrides.truncate(MAX_REMINDER_OVERRIDES);
    }
    json!({
        "useDefault": false,
        "overrides": overrides
            .iter()
            .map(|r| json!({
                "method": match r.method {
                    ReminderMethod::Popup => "popup",
                    ReminderMethod::Email => "email",
                },
                "minutes": r.minutes,
            }))
            .collect::<Vec<_>>(),
    })
}

/// The `…Properties` field for type-specific properties.
pub(crate) fn type_properties_json(props: &EventTypeProperties) -> (&'static str, Value) {
    fn auto_decline(settings: &AutoDeclineSettings, with_chat: bool) -> Value {
        let mut map = Map::new();
        insert_opt(&mut map, "autoDeclineMode", &settings.auto_decline_mode);
        insert_opt(&mut map, "declineMessage", &settings.decline_message);
        if with_chat {
            insert_opt(&mut map, "chatStatus", &settings.chat_status);
        }
        Value::Object(map)
    }
    match props {
        EventTypeProperties::OutOfOffice(settings) => {
            ("outOfOfficeProperties", auto_decline(settings, false))
        }
        EventTypeProperties::FocusTime(settings) => {
            ("focusTimeProperties", auto_decline(settings, true))
        }
        EventTypeProperties::WorkingLocation(location) => (
            "workingLocationProperties",
            match location {
                WorkingLocation::HomeOffice => json!({ "type": "homeOffice", "homeOffice": {} }),
                WorkingLocation::Office(office) => {
                    let mut map = Map::new();
                    insert_opt(&mut map, "label", &office.label);
                    insert_opt(&mut map, "buildingId", &office.building_id);
                    insert_opt(&mut map, "floorId", &office.floor_id);
                    insert_opt(&mut map, "floorSectionId", &office.floor_section_id);
                    insert_opt(&mut map, "deskId", &office.desk_id);
                    json!({ "type": "officeLocation", "officeLocation": map })
                }
                WorkingLocation::Custom { label } => {
                    let mut map = Map::new();
                    insert_opt(&mut map, "label", label);
                    json!({ "type": "customLocation", "customLocation": map })
                }
            },
        ),
        // `contact` and `customTypeName` are read-only.
        EventTypeProperties::Birthday(birthday) => {
            ("birthdayProperties", json!({ "type": birthday.kind }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envelope_ical::{AlarmAction, EventClass, EventTime};

    fn minimal(extra: Value) -> Value {
        let mut base = json!({
            "id": "evt1",
            "iCalUID": "evt1@google.com",
            "start": {"dateTime": "2024-02-01T10:00:00Z"},
            "end": {"dateTime": "2024-02-01T11:00:00Z"},
        });
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        base
    }

    #[test]
    fn missing_start_is_reported_not_panicking() {
        let value = json!({"id": "x", "status": "confirmed"});
        let err = EventChange::from_json(&value, &MappingContext::default()).expect_err("no start");
        assert!(matches!(err, Error::InvalidResponse(msg) if msg.contains("no start")));
    }

    #[test]
    fn default_reminders_come_from_the_context() {
        let context = MappingContext {
            default_reminders: vec![Reminder {
                method: ReminderMethod::Popup,
                minutes: 10,
            }],
            palette: None,
        };
        let event = GoogleEvent::from_json(
            &minimal(json!({"reminders": {"useDefault": true}})),
            &context,
        )
        .expect("maps");
        assert!(event.use_default_reminders);
        assert_eq!(event.event.alarms.len(), 1);
        assert_eq!(event.event.alarms[0].action, AlarmAction::Display);
        let json = event.to_json(None).expect("json");
        assert_eq!(json["reminders"], json!({"useDefault": true}));
    }

    #[test]
    fn reminder_overrides_are_deduplicated_and_capped() {
        let alarm = |minutes| Alarm {
            action: AlarmAction::Display,
            minutes_before: minutes,
            description: None,
        };
        let alarms: Vec<Alarm> = [5, 5, 10, 15, 30, 60, 90, 99_999]
            .into_iter()
            .map(alarm)
            .collect();
        let json = reminders_json(false, &alarms);
        let overrides = json["overrides"].as_array().expect("array");
        assert_eq!(overrides.len(), 5);
        assert_eq!(overrides[0], json!({"method": "popup", "minutes": 5}));
        assert_eq!(overrides[1], json!({"method": "popup", "minutes": 10}));
    }

    #[test]
    fn html_description_survives_unless_text_was_edited() {
        let mut event = GoogleEvent::from_json(
            &minimal(json!({"description": "Agenda<br><b>Demo</b> &amp; Q&amp;A"})),
            &MappingContext::default(),
        )
        .expect("maps");
        assert_eq!(
            event.event.description.as_deref(),
            Some("Agenda\nDemo & Q&A")
        );
        let original = event.event.description_html.clone();
        assert_eq!(
            event.to_json(None).expect("json")["description"],
            json!(original)
        );
        event.event.description = Some("Edited".to_owned());
        assert_eq!(
            event.to_json(None).expect("json")["description"],
            json!("Edited")
        );
        event.event.description = None;
        assert_eq!(
            event.to_json(None).expect("json")["description"],
            Value::Null
        );
    }

    #[test]
    fn plain_text_description_has_no_html() {
        let event = GoogleEvent::from_json(
            &minimal(json!({"description": "Line 1\nLine 2 <with angle>"})),
            &MappingContext::default(),
        )
        .expect("maps");
        assert_eq!(
            event.event.description.as_deref(),
            Some("Line 1\nLine 2 <with angle>")
        );
        assert_eq!(event.event.description_html, None);
    }

    #[test]
    fn visibility_default_round_trips() {
        let mut event = GoogleEvent::from_json(
            &minimal(json!({"visibility": "default"})),
            &MappingContext::default(),
        )
        .expect("maps");
        assert_eq!(event.event.class, EventClass::Public);
        assert_eq!(
            event.to_json(None).expect("json")["visibility"],
            json!("default")
        );
        event.event.class = EventClass::Private;
        assert_eq!(
            event.to_json(None).expect("json")["visibility"],
            json!("private")
        );
    }

    #[test]
    fn missing_ical_uid_falls_back_to_google_id() {
        let value = json!({
            "id": "inst_20240101T090000Z",
            "recurringEventId": "inst",
            "originalStartTime": {"dateTime": "2024-01-01T09:00:00Z", "timeZone": "UTC"},
            "start": {"dateTime": "2024-01-01T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2024-01-01T11:00:00Z", "timeZone": "UTC"},
        });
        let event = GoogleEvent::from_json(&value, &MappingContext::default()).expect("maps");
        assert_eq!(event.event.uid, "inst@google.com");
        assert!(matches!(
            event.event.recurrence_id,
            Some(EventTime::Zoned { .. })
        ));
        // Occurrences never carry recurrence lines.
        assert!(
            event
                .to_json(None)
                .expect("json")
                .get("recurrence")
                .is_none()
        );
    }

    #[test]
    fn editor_rules_and_zones_round_trip() {
        // 10:00 in New York on 10 November 2026, the month's second Tuesday.
        let local = chrono::NaiveDate::from_ymd_opt(2026, 11, 10)
            .and_then(|d| d.and_hms_opt(10, 0, 0))
            .expect("date");
        for rule in [
            "FREQ=MONTHLY;BYDAY=2TU;COUNT=5",
            "FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR",
            "FREQ=MONTHLY;BYMONTHDAY=31",
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20270101T045959Z",
            "FREQ=DAILY;INTERVAL=3;COUNT=10",
            "FREQ=YEARLY;INTERVAL=2",
        ] {
            let mut event = Event::new(
                "uid",
                EventTime::Zoned {
                    local,
                    tzid: "America/New_York".into(),
                },
                EventTime::Zoned {
                    local: local + chrono::Duration::hours(1),
                    tzid: "America/New_York".into(),
                },
            );
            event.rrule = Some(rule.to_owned());
            let body = GoogleEvent::new(event).to_json(None).expect("json");
            assert_eq!(body["recurrence"], json!([format!("RRULE:{rule}")]));
            assert_eq!(
                body["start"],
                json!({"date": null, "dateTime": "2026-11-10T10:00:00-05:00", "timeZone": "America/New_York"})
            );
            // Google answers with the same fields; they read back the same.
            let answer = minimal(json!({
                "start": {"dateTime": "2026-11-10T10:00:00-05:00", "timeZone": "America/New_York"},
                "end": {"dateTime": "2026-11-10T11:00:00-05:00", "timeZone": "America/New_York"},
                "recurrence": [format!("RRULE:{rule}")],
            }));
            let back = GoogleEvent::from_json(&answer, &MappingContext::default()).expect("maps");
            assert_eq!(back.event.rrule.as_deref(), Some(rule));
            assert_eq!(
                back.event.start,
                EventTime::Zoned {
                    local,
                    tzid: "America/New_York".into()
                }
            );
            assert!(back.extra_recurrence.is_empty());
        }
    }

    #[test]
    fn type_properties_round_trip() {
        let value = minimal(json!({
            "eventType": "workingLocation",
            "workingLocationProperties": {
                "type": "officeLocation",
                "officeLocation": {"label": "HQ", "buildingId": "b1", "floorId": "3"}
            }
        }));
        let event = GoogleEvent::from_json(&value, &MappingContext::default()).expect("maps");
        assert_eq!(event.event_type, EventType::WorkingLocation);
        let json = event.to_json(None).expect("json");
        assert_eq!(json["eventType"], json!("workingLocation"));
        assert_eq!(
            json["workingLocationProperties"],
            json!({"type": "officeLocation", "officeLocation": {"label": "HQ", "buildingId": "b1", "floorId": "3"}})
        );
    }
}
