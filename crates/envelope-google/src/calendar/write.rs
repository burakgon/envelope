//! Event writes: insert, update, patch, delete, move, RSVP, quick add and
//! occurrence lookup.

use chrono::{DateTime, Utc};
use envelope_ical::{
    Alarm, Attendee, Event, EventClass, EventStatus, EventTime, PartStat, Transparency,
};
use reqwest::Method;
use serde_json::{Map, Value, json};

use super::event::{EventChange, GoogleEvent, Visibility};
use super::mapping::{
    MappingContext, attendees_json, classify, description_for_write, event_to_json, map_event,
    partstat_str, reminders_json, status_str, transparency_str,
};
use super::sync::{EventPage, MAX_PAGE_SIZE};
use super::{recurrence, time, wire};
use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Error, Result, ensure_page_progress};

/// Who Google notifies about a change (`sendUpdates`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendUpdates {
    /// Notify all guests.
    All,
    /// Notify only guests who do not use Google Calendar.
    ExternalOnly,
    /// Notify nobody.
    None,
}

impl SendUpdates {
    /// Google's wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            SendUpdates::All => "all",
            SendUpdates::ExternalOnly => "externalOnly",
            SendUpdates::None => "none",
        }
    }
}

/// Options shared by the event write operations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteOptions {
    /// Guest notifications; `None` leaves Google's default.
    pub send_updates: Option<SendUpdates>,
    /// Only apply the change if the event's etag still matches (update,
    /// patch, RSVP and delete). A mismatch fails with
    /// [`Error::is_precondition_failed`].
    pub if_match: Option<String>,
    /// Create a Google Meet conference for the event (insert, update and
    /// patch). Creation may complete asynchronously: check
    /// [`Conference::create_status`](super::Conference::create_status).
    pub add_google_meet: bool,
}

impl WriteOptions {
    /// Sets who is notified.
    pub fn send_updates(mut self, send_updates: SendUpdates) -> Self {
        self.send_updates = Some(send_updates);
        self
    }

    /// Makes the write conditional on the given etag.
    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.if_match = Some(etag.into());
        self
    }

    /// Requests a Google Meet conference.
    pub fn with_google_meet(mut self) -> Self {
        self.add_google_meet = true;
        self
    }
}

/// A partial update of an event (`events.patch`).
///
/// Only the fields that were set are sent. Arrays (attendees, recurrence,
/// reminder overrides) replace the stored value as a whole, and clearing a
/// field sends an explicit `null`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventPatch {
    fields: Map<String, Value>,
    start: Option<EventTime>,
    end: Option<EventTime>,
    remove_conference: bool,
}

impl EventPatch {
    /// An empty patch.
    pub fn new() -> Self {
        EventPatch::default()
    }

    /// A patch carrying the user-editable canonical fields of `event`:
    /// summary, description, location, times, recurrence (for series
    /// masters), status, transparency, visibility and attendees. Reminders,
    /// color and conference are left untouched.
    pub fn from_event(event: &Event) -> Self {
        let mut patch = EventPatch::new()
            .summary(event.summary.clone())
            .description(description_for_write(event).as_deref())
            .location(event.location.as_deref())
            .start(event.start.clone())
            .end(event.end.clone())
            .status(event.status)
            .transparency(event.transparency)
            .class(event.class)
            .attendees(&event.attendees);
        if event.recurrence_id.is_none() {
            patch = patch.recurrence(event);
        }
        patch
    }

    /// Sets the title.
    pub fn summary(self, summary: impl Into<String>) -> Self {
        self.set("summary", summary.into())
    }

    /// Sets (or with `None` clears) the description; HTML is allowed.
    pub fn description(self, description: Option<&str>) -> Self {
        self.set("description", description)
    }

    /// Sets (or with `None` clears) the location.
    pub fn location(self, location: Option<&str>) -> Self {
        self.set("location", location)
    }

    /// Sets the start. Use zoned times for recurring events.
    pub fn start(mut self, start: EventTime) -> Self {
        self.start = Some(start);
        self
    }

    /// Sets the (exclusive) end.
    pub fn end(mut self, end: EventTime) -> Self {
        self.end = Some(end);
        self
    }

    /// Replaces the recurrence with the `rrule`, `exdates` and `rdates` of
    /// `event` (an empty recurrence turns a series into a single event).
    pub fn recurrence(self, event: &Event) -> Self {
        self.set("recurrence", recurrence::format(event, &[]))
    }

    /// Sets the status (`cancelled` deletes the event).
    pub fn status(self, status: EventStatus) -> Self {
        self.set("status", status_str(status))
    }

    /// Sets whether the event blocks time.
    pub fn transparency(self, transparency: Transparency) -> Self {
        self.set("transparency", transparency_str(transparency))
    }

    /// Sets the visibility from an iCalendar class (`Public` becomes the
    /// calendar default; use [`EventPatch::visibility`] for an explicit
    /// `public`).
    pub fn class(self, class: EventClass) -> Self {
        self.visibility(Visibility::Default.for_class(class))
    }

    /// Sets Google's visibility.
    pub fn visibility(self, visibility: Visibility) -> Self {
        self.set("visibility", visibility.as_str())
    }

    /// Replaces the attendee list (organizer-only on Google's side). Google
    /// replaces the whole array, so attendee comments and additional guests
    /// are cleared; use [`EventPatch::attendees_of`] to keep them.
    pub fn attendees(self, attendees: &[Attendee]) -> Self {
        self.set("attendees", attendees_json(attendees, &[]))
    }

    /// Replaces the attendee list with `event.event.attendees`, keeping each
    /// attendee's comment and additional guests from
    /// [`GoogleEvent::attendee_details`].
    pub fn attendees_of(self, event: &GoogleEvent) -> Self {
        self.set(
            "attendees",
            attendees_json(&event.event.attendees, &event.attendee_details),
        )
    }

    /// Replaces the reminders with custom alarms (at most five are kept).
    pub fn alarms(self, alarms: &[Alarm]) -> Self {
        self.set("reminders", reminders_json(false, alarms))
    }

    /// Switches the event to the calendar's default reminders.
    pub fn use_default_reminders(self) -> Self {
        self.set("reminders", json!({ "useDefault": true }))
    }

    /// Sets (or with `None` resets) the event color id.
    pub fn color_id(self, color_id: Option<&str>) -> Self {
        self.set("colorId", color_id)
    }

    /// Sets whether guests may modify the event (organizer only).
    pub fn guests_can_modify(self, allowed: bool) -> Self {
        self.set("guestsCanModify", allowed)
    }

    /// Sets whether guests may invite others (organizer only).
    pub fn guests_can_invite_others(self, allowed: bool) -> Self {
        self.set("guestsCanInviteOthers", allowed)
    }

    /// Sets whether guests may see the guest list (organizer only).
    pub fn guests_can_see_other_guests(self, allowed: bool) -> Self {
        self.set("guestsCanSeeOtherGuests", allowed)
    }

    /// Removes the event's conference.
    pub fn remove_conference(mut self) -> Self {
        self.remove_conference = true;
        self
    }

    /// Sets any other field of the Google `Event` resource verbatim.
    pub fn raw(self, field: impl Into<String>, value: Value) -> Self {
        let mut patch = self;
        patch.fields.insert(field.into(), value);
        patch
    }

    /// True if the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
            && self.start.is_none()
            && self.end.is_none()
            && !self.remove_conference
    }

    fn set(mut self, field: &str, value: impl Into<Value>) -> Self {
        self.fields.insert(field.to_owned(), value.into());
        self
    }

    /// The JSON body; returns whether conference data is involved.
    fn to_body(
        &self,
        floating_tz: Option<&str>,
        add_google_meet: bool,
    ) -> Result<(Map<String, Value>, bool)> {
        let mut body = self.fields.clone();
        let recurring = body
            .get("recurrence")
            .and_then(Value::as_array)
            .is_some_and(|lines| !lines.is_empty());
        if let Some(start) = &self.start {
            body.insert(
                "start".into(),
                time::to_json(start, recurring, floating_tz)?,
            );
        }
        if let Some(end) = &self.end {
            body.insert("end".into(), time::to_json(end, recurring, floating_tz)?);
        }
        let conference = if add_google_meet {
            body.insert("conferenceData".into(), meet_request());
            true
        } else if self.remove_conference {
            body.insert("conferenceData".into(), Value::Null);
            true
        } else {
            false
        };
        Ok((body, conference))
    }
}

/// Options for listing the occurrences of a recurring event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstancesQuery {
    /// Only occurrences ending after this instant.
    pub time_min: Option<DateTime<Utc>>,
    /// Only occurrences starting before this instant.
    pub time_max: Option<DateTime<Utc>>,
    /// Only the occurrence originally starting at this time.
    pub original_start: Option<EventTime>,
    /// Maximum number of occurrences to return (default 250). Endless
    /// series need a limit or `time_max`.
    pub max_results: Option<usize>,
}

const DEFAULT_MAX_INSTANCES: usize = 250;

fn meet_request() -> Value {
    json!({
        "createRequest": {
            "requestId": uuid::Uuid::new_v4().to_string(),
            "conferenceSolutionKey": { "type": "hangoutsMeet" },
        }
    })
}

impl GoogleClient {
    /// Fetches one event by Google id. Cancelled events are classified like
    /// in a sync (see [`EventChange`]).
    pub async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<EventChange> {
        let request = ApiRequest::get(
            Service::Calendar,
            &["calendars", calendar_id, "events", event_id],
        );
        let event: wire::Event = self.send_json(&request, "events.get").await?;
        let context = self.write_context(calendar_id).await;
        classify(event, &context).map_err(Error::InvalidResponse)
    }

    /// Creates an event (`events.insert`). `event.uid` is sent as the
    /// `iCalUID` and a non-empty [`GoogleEvent::id`] as the Google id.
    pub async fn insert_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        options: &WriteOptions,
    ) -> Result<GoogleEvent> {
        let mut body = event_to_json(event, self.floating_time_zone())?;
        if options.add_google_meet {
            body.insert("conferenceData".into(), meet_request());
        }
        let request = ApiRequest::new(
            Method::POST,
            Service::Calendar,
            &["calendars", calendar_id, "events"],
        )
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str))
        .query_if(options.add_google_meet, "conferenceDataVersion", "1")
        .query_if(!event.attachments.is_empty(), "supportsAttachments", "true")
        .json(&body)?;
        self.event_response(calendar_id, &request, "events.insert")
            .await
    }

    /// Replaces an event (`events.update`, a full `PUT`). Fields not present
    /// in the [`GoogleEvent`] are cleared on the server, except conference
    /// data, which is only touched with [`WriteOptions::add_google_meet`].
    ///
    /// A full update also rewrites shared properties (guest permissions,
    /// attendees), which Google only allows the organizer to change; as an
    /// attendee, change your own copy with [`GoogleClient::patch_event`]
    /// (reminders, color) or [`GoogleClient::respond_to_event`].
    pub async fn update_event(
        &self,
        calendar_id: &str,
        event: &GoogleEvent,
        options: &WriteOptions,
    ) -> Result<GoogleEvent> {
        if event.id.is_empty() {
            return Err(Error::InvalidInput(
                "update_event needs the Google event id".to_owned(),
            ));
        }
        let mut body = event_to_json(event, self.floating_time_zone())?;
        if options.add_google_meet {
            body.insert("conferenceData".into(), meet_request());
        }
        let request = ApiRequest::new(
            Method::PUT,
            Service::Calendar,
            &["calendars", calendar_id, "events", &event.id],
        )
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str))
        .query_if(options.add_google_meet, "conferenceDataVersion", "1")
        .query_if(!event.attachments.is_empty(), "supportsAttachments", "true")
        .if_match(options.if_match.as_deref())
        .json(&body)?;
        self.event_response(calendar_id, &request, "events.update")
            .await
    }

    /// Partially updates an event (`events.patch`).
    pub async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        patch: &EventPatch,
        options: &WriteOptions,
    ) -> Result<GoogleEvent> {
        let (body, conference) =
            patch.to_body(self.floating_time_zone(), options.add_google_meet)?;
        let request = ApiRequest::new(
            Method::PATCH,
            Service::Calendar,
            &["calendars", calendar_id, "events", event_id],
        )
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str))
        .query_if(conference, "conferenceDataVersion", "1")
        .query_if(
            body.contains_key("attachments"),
            "supportsAttachments",
            "true",
        )
        .if_match(options.if_match.as_deref())
        .json(&body)?;
        self.event_response(calendar_id, &request, "events.patch")
            .await
    }

    /// Deletes an event. Deleting an event that is already gone (`410`)
    /// succeeds.
    pub async fn delete_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        options: &WriteOptions,
    ) -> Result<()> {
        let request = ApiRequest::new(
            Method::DELETE,
            Service::Calendar,
            &["calendars", calendar_id, "events", event_id],
        )
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str))
        .if_match(options.if_match.as_deref());
        match self.execute(&request).await {
            Ok(_) => Ok(()),
            Err(err) if err.is_gone() => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Moves an event to another calendar, which changes its organizer.
    /// Only `default` events can be moved.
    pub async fn move_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        destination_calendar_id: &str,
        options: &WriteOptions,
    ) -> Result<GoogleEvent> {
        let request = ApiRequest::new(
            Method::POST,
            Service::Calendar,
            &["calendars", calendar_id, "events", event_id, "move"],
        )
        .query("destination", destination_calendar_id)
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str));
        self.event_response(destination_calendar_id, &request, "events.move")
            .await
    }

    /// Answers an invitation: sets the authenticated user's `responseStatus`
    /// (and optionally their comment; `None` keeps the current one).
    ///
    /// Only the user's own attendee entry is sent (with
    /// `attendeesOmitted=true`), so the guest list itself is never rewritten.
    /// To answer for a single occurrence of a series, pass the occurrence's
    /// id (see [`GoogleClient::find_instance`]).
    pub async fn respond_to_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        response: PartStat,
        comment: Option<&str>,
        options: &WriteOptions,
    ) -> Result<GoogleEvent> {
        let request = ApiRequest::get(
            Service::Calendar,
            &["calendars", calendar_id, "events", event_id],
        );
        let current: wire::Event = self.send_json(&request, "events.get").await?;
        let me = current
            .attendees
            .iter()
            .find(|a| a.is_self == Some(true))
            .ok_or_else(|| {
                Error::InvalidInput(
                    "the authenticated user is not an attendee of this event".to_owned(),
                )
            })?;
        let email = me.email.as_deref().ok_or_else(|| {
            Error::InvalidResponse("own attendee entry without e-mail".to_owned())
        })?;
        let mut attendee = Map::new();
        attendee.insert("email".into(), email.into());
        if let Some(name) = &me.display_name {
            attendee.insert("displayName".into(), name.clone().into());
        }
        if me.optional == Some(true) {
            attendee.insert("optional".into(), true.into());
        }
        if let Some(guests) = me.additional_guests.filter(|n| *n > 0) {
            attendee.insert("additionalGuests".into(), guests.into());
        }
        if let Some(comment) = comment.map(str::to_owned).or_else(|| me.comment.clone()) {
            attendee.insert("comment".into(), comment.into());
        }
        attendee.insert("responseStatus".into(), partstat_str(response).into());
        let body = json!({ "attendeesOmitted": true, "attendees": [attendee] });
        let request = ApiRequest::new(
            Method::PATCH,
            Service::Calendar,
            &["calendars", calendar_id, "events", event_id],
        )
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str))
        .if_match(options.if_match.as_deref())
        .json(&body)?;
        self.event_response(calendar_id, &request, "events.patch")
            .await
    }

    /// Creates an event from natural-language text such as
    /// `"Lunch with Ana tomorrow 12:30"` (`events.quickAdd`).
    pub async fn quick_add(
        &self,
        calendar_id: &str,
        text: &str,
        options: &WriteOptions,
    ) -> Result<GoogleEvent> {
        let request = ApiRequest::new(
            Method::POST,
            Service::Calendar,
            &["calendars", calendar_id, "events", "quickAdd"],
        )
        .query("text", text)
        .query_opt("sendUpdates", options.send_updates.map(SendUpdates::as_str));
        self.event_response(calendar_id, &request, "events.quickAdd")
            .await
    }

    /// Lists the occurrences of a recurring event, e.g. to obtain the Google
    /// id of a single occurrence before changing, answering or deleting just
    /// that one. Cancelled occurrences are not included.
    pub async fn list_instances(
        &self,
        calendar_id: &str,
        event_id: &str,
        query: &InstancesQuery,
    ) -> Result<Vec<GoogleEvent>> {
        let limit = query.max_results.unwrap_or(DEFAULT_MAX_INSTANCES);
        let page_size =
            u32::try_from(limit.clamp(1, MAX_PAGE_SIZE as usize)).unwrap_or(MAX_PAGE_SIZE);
        let original_start = match &query.original_start {
            Some(time) => Some(time::to_query_value(time).ok_or_else(|| {
                Error::InvalidInput("original start has an unknown time zone".to_owned())
            })?),
            None => None,
        };
        let palette = self.colors().await?;
        let mut instances = Vec::new();
        let mut page_token: Option<String> = None;
        while instances.len() < limit {
            let request = ApiRequest::get(
                Service::Calendar,
                &["calendars", calendar_id, "events", event_id, "instances"],
            )
            .query("maxResults", page_size.to_string())
            .query_opt("timeMin", query.time_min.as_ref().map(time::rfc3339))
            .query_opt("timeMax", query.time_max.as_ref().map(time::rfc3339))
            .query_opt("originalStart", original_start.clone())
            .query_opt("pageToken", page_token.clone());
            let body = self.execute(&request).await?;
            let page = EventPage::from_json(&body, Some(palette.clone()))?;
            ensure_page_progress(
                page_token.as_deref(),
                page.next_page_token.as_deref(),
                "events.instances",
            )?;
            self.remember_default_reminders(calendar_id, &page.default_reminders);
            instances.extend(page.events);
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        instances.truncate(limit);
        Ok(instances)
    }

    /// Finds the occurrence of a recurring event that originally started at
    /// `original_start` (as found in the master's expansion or in
    /// `event.recurrence_id`). Returns `None` if it does not exist or was
    /// cancelled.
    pub async fn find_instance(
        &self,
        calendar_id: &str,
        event_id: &str,
        original_start: &EventTime,
    ) -> Result<Option<GoogleEvent>> {
        let query = InstancesQuery {
            original_start: Some(original_start.clone()),
            max_results: Some(DEFAULT_MAX_INSTANCES),
            ..InstancesQuery::default()
        };
        let instances = self.list_instances(calendar_id, event_id, &query).await?;
        Ok(instances.into_iter().find(|instance| {
            instance
                .event
                .recurrence_id
                .as_ref()
                .is_some_and(|id| time::same_occurrence(id, original_start))
        }))
    }

    /// Sends a write and maps the returned event.
    async fn event_response(
        &self,
        calendar_id: &str,
        request: &ApiRequest<'_>,
        context: &'static str,
    ) -> Result<GoogleEvent> {
        let event: wire::Event = self.send_json(request, context).await?;
        let mapping = self.write_context(calendar_id).await;
        map_event(event, &mapping).map_err(Error::InvalidResponse)
    }

    /// Calendar data for mapping single events. Best effort: the write has
    /// already happened, so lookup failures only cost reminder/color detail.
    async fn write_context(&self, calendar_id: &str) -> MappingContext {
        let default_reminders = match self.cached_default_reminders(calendar_id) {
            Some(reminders) => reminders,
            None => match self.get_calendar(calendar_id).await {
                Ok(calendar) => calendar.default_reminders,
                Err(err) => {
                    tracing::warn!(error = %err, calendar_id, "could not load default reminders");
                    Vec::new()
                }
            },
        };
        let palette = match self.colors().await {
            Ok(palette) => Some(palette),
            Err(err) => {
                tracing::warn!(error = %err, "could not load the color palette");
                None
            }
        };
        MappingContext {
            default_reminders,
            palette,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn patch_from_event_carries_editable_fields() {
        let date = NaiveDate::from_ymd_opt(2024, 6, 1).expect("date");
        let mut event = Event::new(
            "uid",
            EventTime::Date { date },
            EventTime::Date {
                date: date.succ_opt().expect("date"),
            },
        );
        event.summary = "Offsite".to_owned();
        event.rrule = Some("FREQ=YEARLY".to_owned());
        let (body, conference) = EventPatch::from_event(&event)
            .to_body(None, false)
            .expect("body");
        assert!(!conference);
        assert_eq!(body["summary"], json!("Offsite"));
        assert_eq!(body["description"], Value::Null);
        assert_eq!(body["visibility"], json!("default"));
        assert_eq!(body["recurrence"], json!(["RRULE:FREQ=YEARLY"]));
        assert_eq!(
            body["start"],
            json!({"date": "2024-06-01", "dateTime": null, "timeZone": null})
        );
        assert_eq!(body["attendees"], json!([]));
        assert!(!body.contains_key("reminders"));
    }

    #[test]
    fn attendees_of_keeps_google_details() {
        let date = NaiveDate::from_ymd_opt(2024, 6, 1).expect("date");
        let mut event = Event::new("uid", EventTime::Date { date }, EventTime::Date { date });
        event.attendees = vec![Attendee {
            email: "grace@example.com".to_owned(),
            partstat: PartStat::Accepted,
            ..Attendee::default()
        }];
        let mut google = GoogleEvent::new(event);
        google.attendee_details.push(crate::AttendeeDetails {
            email: "grace@example.com".to_owned(),
            is_self: false,
            comment: Some("On my way".to_owned()),
            additional_guests: 2,
        });
        let (body, _) = EventPatch::new()
            .attendees_of(&google)
            .to_body(None, false)
            .expect("body");
        assert_eq!(
            body["attendees"],
            json!([{
                "email": "grace@example.com",
                "responseStatus": "accepted",
                "comment": "On my way",
                "additionalGuests": 2
            }])
        );
        let (plain, _) = EventPatch::new()
            .attendees(&google.event.attendees)
            .to_body(None, false)
            .expect("body");
        assert!(plain["attendees"][0].get("comment").is_none());
    }

    #[test]
    fn patch_conference_flags() {
        let (body, conference) = EventPatch::new()
            .remove_conference()
            .to_body(None, false)
            .expect("body");
        assert!(conference);
        assert_eq!(body["conferenceData"], Value::Null);
        let (body, conference) = EventPatch::new().to_body(None, true).expect("body");
        assert!(conference);
        assert_eq!(
            body["conferenceData"]["createRequest"]["conferenceSolutionKey"]["type"],
            json!("hangoutsMeet")
        );
        assert!(EventPatch::new().is_empty());
        assert!(!EventPatch::new().color_id(None).is_empty());
    }

    #[test]
    fn send_updates_wire_values() {
        assert_eq!(SendUpdates::All.as_str(), "all");
        assert_eq!(SendUpdates::ExternalOnly.as_str(), "externalOnly");
        assert_eq!(SendUpdates::None.as_str(), "none");
    }
}
