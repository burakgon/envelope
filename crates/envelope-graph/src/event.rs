//! Graph events and their mapping onto [`envelope_ical::Event`].

use std::collections::HashSet;

use chrono::{DateTime, Days, FixedOffset, NaiveDate, NaiveDateTime, Timelike, Utc};
use chrono_tz::Tz;
use envelope_ical::{
    Alarm, AlarmAction, Attendee as IcalAttendee, AttendeeRole, CalendarUserType, Event,
    EventClass, EventStatus, EventTime, Organizer, PartStat, Transparency,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use crate::client::new_request_id;
use crate::error::GraphError;
use crate::html::{find_meeting_url, html_to_text};
use crate::ical_uid::series_uid;
use crate::recurrence::{PatternedRecurrence, SeriesZone, from_rrule, to_rrule_in};
use crate::serde_util::{graph_enum, null_default};
use crate::types::{
    BodyType, DateTimeTimeZone, EmailAddress, Importance, ItemBody, Recipient, format_local,
};

graph_enum! {
    /// Kind of a Graph event.
    pub enum EventType {
        /// A non-recurring event.
        SingleInstance = "singleInstance",
        /// An unmodified occurrence of a recurring series.
        Occurrence = "occurrence",
        /// A modified occurrence of a recurring series.
        Exception = "exception",
        /// The master of a recurring series.
        SeriesMaster = "seriesMaster",
    }
    unknown = SingleInstance;
    default = SingleInstance;
}

graph_enum! {
    /// Free/busy status (`showAs`).
    pub enum FreeBusyStatus {
        /// Unknown.
        Unknown = "unknown",
        /// Free.
        Free = "free",
        /// Tentative.
        Tentative = "tentative",
        /// Busy.
        Busy = "busy",
        /// Out of office.
        Oof = "oof",
        /// Working elsewhere.
        WorkingElsewhere = "workingElsewhere",
    }
    unknown = Unknown;
    default = Busy;
}

graph_enum! {
    /// Privacy level of an event.
    pub enum Sensitivity {
        /// Normal.
        Normal = "normal",
        /// Personal.
        Personal = "personal",
        /// Private.
        Private = "private",
        /// Confidential.
        Confidential = "confidential",
    }
    unknown = Normal;
    default = Normal;
}

graph_enum! {
    /// An attendee's (or the user's) response to a meeting.
    pub enum ResponseType {
        /// No response yet.
        None = "none",
        /// The user organizes the meeting.
        Organizer = "organizer",
        /// Tentatively accepted.
        TentativelyAccepted = "tentativelyAccepted",
        /// Accepted.
        Accepted = "accepted",
        /// Declined.
        Declined = "declined",
        /// Not responded (same as `None`).
        NotResponded = "notResponded",
    }
    unknown = None;
    default = None;
}

graph_enum! {
    /// Kind of attendee.
    pub enum AttendeeType {
        /// Required attendee.
        Required = "required",
        /// Optional attendee.
        Optional = "optional",
        /// A resource such as a room or equipment.
        Resource = "resource",
    }
    unknown = Required;
    default = Required;
}

graph_enum! {
    /// Online meeting service of an event.
    pub enum OnlineMeetingProvider {
        /// Unknown or none.
        Unknown = "unknown",
        /// Microsoft Teams (work or school).
        TeamsForBusiness = "teamsForBusiness",
        /// Skype for Business.
        SkypeForBusiness = "skypeForBusiness",
        /// Skype (consumer).
        SkypeForConsumer = "skypeForConsumer",
    }
    unknown = Unknown;
    default = Unknown;
}

graph_enum! {
    /// Kind of a location.
    pub enum LocationType {
        /// Unspecified (locations created through the API).
        Default = "default",
        /// A bookable meeting room.
        ConferenceRoom = "conferenceRoom",
        /// A home address.
        HomeAddress = "homeAddress",
        /// A business address.
        BusinessAddress = "businessAddress",
        /// Geographic coordinates.
        GeoCoordinates = "geoCoordinates",
        /// A street address.
        StreetAddress = "streetAddress",
        /// A hotel.
        Hotel = "hotel",
        /// A restaurant.
        Restaurant = "restaurant",
        /// A local business.
        LocalBusiness = "localBusiness",
        /// A postal address.
        PostalAddress = "postalAddress",
    }
    unknown = Default;
    default = Default;
}

/// A response and when it was given (Graph `responseStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseStatus {
    /// The response.
    #[serde(default, deserialize_with = "null_default")]
    pub response: ResponseType,
    /// When it was given (Graph reports `0001-01-01T00:00:00Z` when never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<DateTime<Utc>>,
}

/// A meeting attendee as Graph reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    /// Required, optional or resource.
    #[serde(rename = "type", default, deserialize_with = "null_default")]
    pub kind: AttendeeType,
    /// The attendee's response.
    #[serde(default, deserialize_with = "null_default")]
    pub status: ResponseStatus,
    /// Address and name.
    #[serde(default, deserialize_with = "null_default")]
    pub email_address: EmailAddress,
}

/// An event location.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// E-mail address of a room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_email_address: Option<String>,
    /// URI of the location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_uri: Option<String>,
    /// Kind of location.
    #[serde(default, deserialize_with = "null_default")]
    pub location_type: LocationType,
}

/// Join information of an online meeting.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineMeetingInfo {
    /// The link to join the meeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_url: Option<String>,
    /// Conference id for dial-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_id: Option<String>,
    /// Toll number for dial-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toll_number: Option<String>,
}

/// The Graph `event` resource as received from the API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireEvent {
    pub(crate) id: String,
    #[serde(default)]
    change_key: Option<String>,
    #[serde(default, rename = "iCalUId")]
    ical_uid: Option<String>,
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<ItemBody>,
    #[serde(default)]
    start: Option<DateTimeTimeZone>,
    #[serde(default)]
    end: Option<DateTimeTimeZone>,
    #[serde(default)]
    original_start: Option<DateTime<Utc>>,
    #[serde(default)]
    original_start_time_zone: Option<String>,
    #[serde(default)]
    original_end_time_zone: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    is_all_day: bool,
    #[serde(default, deserialize_with = "null_default")]
    is_cancelled: bool,
    #[serde(default, deserialize_with = "null_default")]
    is_organizer: bool,
    #[serde(default, deserialize_with = "null_default")]
    is_online_meeting: bool,
    #[serde(default)]
    online_meeting_provider: Option<OnlineMeetingProvider>,
    #[serde(default)]
    online_meeting: Option<OnlineMeetingInfo>,
    #[serde(default)]
    online_meeting_url: Option<String>,
    #[serde(default, rename = "type")]
    event_type: Option<EventType>,
    #[serde(default)]
    series_master_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_recurrence")]
    recurrence: Option<PatternedRecurrence>,
    #[serde(default)]
    location: Option<Location>,
    #[serde(default, deserialize_with = "null_default")]
    locations: Vec<Location>,
    #[serde(default, deserialize_with = "null_default")]
    attendees: Vec<Attendee>,
    #[serde(default)]
    organizer: Option<Recipient>,
    #[serde(default)]
    show_as: Option<FreeBusyStatus>,
    #[serde(default)]
    sensitivity: Option<Sensitivity>,
    #[serde(default)]
    importance: Option<Importance>,
    #[serde(default, deserialize_with = "null_default")]
    is_reminder_on: bool,
    #[serde(default)]
    reminder_minutes_before_start: Option<i64>,
    #[serde(default, deserialize_with = "null_default")]
    categories: Vec<String>,
    #[serde(default)]
    web_link: Option<String>,
    #[serde(default)]
    response_requested: Option<bool>,
    #[serde(default)]
    response_status: Option<ResponseStatus>,
    #[serde(default)]
    allow_new_time_proposals: Option<bool>,
    #[serde(default, deserialize_with = "null_default")]
    hide_attendees: bool,
    #[serde(default, deserialize_with = "null_default")]
    is_draft: bool,
    #[serde(default)]
    created_date_time: Option<DateTime<Utc>>,
    #[serde(default)]
    last_modified_date_time: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "null_default")]
    pub(crate) cancelled_occurrences: Vec<String>,
    /// The series start as a UTC instant, when known from another response;
    /// gives the zone offset when the zone name cannot be resolved.
    #[serde(skip)]
    pub(crate) utc_start_hint: Option<DateTime<Utc>>,
}

/// True for zone names Graph can render times in.
fn usable_zone(tz: &&str) -> bool {
    !tz.trim().is_empty() && !tz.starts_with("tzone://")
}

impl WireEvent {
    /// For events with a recurrence: the zone the series is defined in.
    pub(crate) fn series_zone(&self) -> Option<&str> {
        let recurrence = self.recurrence.as_ref()?;
        recurrence
            .range
            .recurrence_time_zone
            .as_deref()
            .filter(usable_zone)
            .or_else(|| self.original_start_time_zone.as_deref().filter(usable_zone))
    }

    /// True when the start is expressed in `zone`.
    pub(crate) fn start_in_zone(&self, zone: &str) -> bool {
        self.start
            .as_ref()
            .and_then(|s| s.time_zone.as_deref())
            .is_some_and(|tz| tz.trim().eq_ignore_ascii_case(zone.trim()))
    }

    /// True for all-day events.
    pub(crate) fn is_all_day(&self) -> bool {
        self.is_all_day
    }

    /// The start instant, when the start is expressed in UTC.
    pub(crate) fn utc_start(&self) -> Option<DateTime<Utc>> {
        self.start.as_ref().and_then(DateTimeTimeZone::to_utc)
    }
}

/// Keeps an event usable when its recurrence has an unexpected shape.
fn lenient_recurrence<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PatternedRecurrence>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| match serde_json::from_value(v) {
        Ok(recurrence) => Some(recurrence),
        Err(err) => {
            tracing::warn!(error = %err, "ignoring unreadable event recurrence");
            None
        }
    }))
}

/// A Graph event: the provider-neutral [`Event`] plus the Graph-specific
/// properties a client needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEvent {
    /// Graph event id (use it for updates, responses and deletion).
    pub id: String,
    /// Version of the item; changes with every modification.
    pub change_key: Option<String>,
    /// The raw `iCalUId`; unlike [`Event::uid`] it differs per occurrence.
    pub ical_uid: Option<String>,
    /// Single instance, occurrence, exception or series master.
    pub event_type: EventType,
    /// Id of the series master for occurrences and exceptions.
    pub series_master_id: Option<String>,
    /// Original start of an occurrence or exception (UTC).
    pub original_start: Option<DateTime<Utc>>,
    /// Time zone the start was created in (Windows or IANA name;
    /// `tzone://Microsoft/Custom` for legacy custom zones).
    pub original_start_time_zone: Option<String>,
    /// Time zone the end was created in.
    pub original_end_time_zone: Option<String>,
    /// True if the signed-in user (or a delegate) organizes the event.
    pub is_organizer: bool,
    /// The signed-in user's response.
    pub response_status: Option<ResponseStatus>,
    /// Whether the organizer asked for responses.
    pub response_requested: bool,
    /// Whether attendees may propose new times.
    pub allow_new_time_proposals: bool,
    /// Whether the event is an online meeting.
    pub is_online_meeting: bool,
    /// The online meeting service.
    pub online_meeting_provider: Option<OnlineMeetingProvider>,
    /// Online meeting join details.
    pub online_meeting: Option<OnlineMeetingInfo>,
    /// Free/busy status as Graph reports it.
    pub show_as: FreeBusyStatus,
    /// Privacy level as Graph reports it.
    pub sensitivity: Sensitivity,
    /// Importance.
    pub importance: Importance,
    /// True for unsent meeting drafts.
    pub is_draft: bool,
    /// Whether attendees only see themselves.
    pub hide_attendees: bool,
    /// All locations of the event.
    pub locations: Vec<Location>,
    /// The recurrence of a series master, as Graph reports it.
    pub recurrence: Option<PatternedRecurrence>,
    /// The mapped, provider-neutral event.
    pub event: Event,
}

impl GraphEvent {
    /// Maps a Graph `event` JSON object.
    ///
    /// `resolve_tz` resolves time zone names (see
    /// [`TimeZoneResolver`](crate::TimeZoneResolver)); it is only used to
    /// compute the exact `UNTIL` of recurrence rules.
    pub fn from_json(
        value: Value,
        resolve_tz: &dyn Fn(&str) -> Option<Tz>,
    ) -> Result<Self, GraphError> {
        let wire: WireEvent =
            serde_json::from_value(value).map_err(|e| GraphError::decode("event", e))?;
        GraphEvent::from_wire(wire, resolve_tz)
    }

    pub(crate) fn from_wire(
        wire: WireEvent,
        resolve_tz: &dyn Fn(&str) -> Option<Tz>,
    ) -> Result<Self, GraphError> {
        let event_type = wire.event_type.unwrap_or_default();
        let start_raw = wire
            .start
            .as_ref()
            .ok_or_else(|| GraphError::Decode(format!("event {} has no start", wire.id)))?;
        let mut start = event_time(start_raw, wire.is_all_day, &wire.id)?;
        let mut end = match &wire.end {
            Some(end) => event_time(end, wire.is_all_day, &wire.id)?,
            None => start.clone(),
        };
        let mut utc_start_hint = wire.utc_start_hint;
        // A series master reported in UTC: its RRULE days are wall-clock days
        // of the series' zone, so express the times in that zone.
        if let (Some(series_tz), EventTime::Utc { at }) = (wire.series_zone(), &start)
            && !crate::types::is_utc_zone(series_tz)
            && let Some(tz) = resolve_tz(series_tz)
        {
            let at = *at;
            start = EventTime::Zoned {
                local: at.with_timezone(&tz).naive_local(),
                tzid: series_tz.to_owned(),
            };
            if let EventTime::Utc { at: end_at } = end {
                end = EventTime::Zoned {
                    local: end_at.with_timezone(&tz).naive_local(),
                    tzid: series_tz.to_owned(),
                };
            }
            utc_start_hint = Some(at);
        } else if let (Some(zone), EventTime::Utc { at }) = (
            wire.original_start_time_zone
                .as_deref()
                .filter(usable_zone)
                .filter(|zone| !crate::types::is_utc_zone(zone)),
            &start,
        ) && let Some(tz) = resolve_tz(zone)
        {
            // Single events and occurrences reported in UTC: in the zone
            // they were planned in, so that editing them keeps it.
            start = EventTime::Zoned {
                local: at.with_timezone(&tz).naive_local(),
                tzid: zone.to_owned(),
            };
            if let EventTime::Utc { at: end_at } = end {
                let (end_zone, end_tz) = wire
                    .original_end_time_zone
                    .as_deref()
                    .filter(usable_zone)
                    .filter(|zone| !crate::types::is_utc_zone(zone))
                    .and_then(|zone| Some((zone, resolve_tz(zone)?)))
                    .unwrap_or((zone, tz));
                end = EventTime::Zoned {
                    local: end_at.with_timezone(&end_tz).naive_local(),
                    tzid: end_zone.to_owned(),
                };
            }
        }

        let uid = wire
            .ical_uid
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(series_uid)
            .or_else(|| wire.uid.clone().filter(|u| !u.trim().is_empty()))
            .unwrap_or_else(|| wire.id.clone());

        let recurrence_id = match event_type {
            EventType::Occurrence if wire.is_all_day => Some(start.clone()),
            EventType::Exception if wire.is_all_day => wire.original_start.map(|at| {
                // `originalStart` is local midnight of the series zone in UTC.
                let date = wire
                    .original_start_time_zone
                    .as_deref()
                    .filter(usable_zone)
                    .and_then(resolve_tz)
                    .map_or_else(
                        || all_day_date(at.naive_utc()),
                        |tz| at.with_timezone(&tz).date_naive(),
                    );
                EventTime::Date { date }
            }),
            EventType::Occurrence | EventType::Exception => wire
                .original_start
                .map(|at| EventTime::Utc { at })
                .or_else(|| (event_type == EventType::Occurrence).then(|| start.clone())),
            EventType::SingleInstance | EventType::SeriesMaster => None,
        };

        let (description, description_html) = match &wire.body {
            Some(body) if !body.content.trim().is_empty() => match body.content_type {
                BodyType::Html => {
                    let text = html_to_text(&body.content);
                    (
                        (!text.is_empty()).then_some(text),
                        Some(body.content.clone()),
                    )
                }
                BodyType::Text => (Some(body.content.clone()), None),
            },
            _ => (None, None),
        };

        let location = wire
            .location
            .as_ref()
            .and_then(|l| l.display_name.clone())
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                let names: Vec<&str> = wire
                    .locations
                    .iter()
                    .filter_map(|l| l.display_name.as_deref())
                    .filter(|name| !name.trim().is_empty())
                    .collect();
                (!names.is_empty()).then(|| names.join("; "))
            });

        let rooms: HashSet<String> = wire
            .location
            .iter()
            .chain(&wire.locations)
            .filter(|l| l.location_type == LocationType::ConferenceRoom)
            .filter_map(|l| l.location_email_address.as_deref())
            .map(|address| address.trim().to_lowercase())
            .collect();
        let response_requested = wire.response_requested.unwrap_or(true);
        let attendees = wire
            .attendees
            .iter()
            .filter_map(|a| map_attendee(a, &rooms, response_requested))
            .collect();

        let organizer = wire.organizer.as_ref().and_then(|o| {
            let email = o.address()?.trim().to_lowercase();
            (!email.is_empty()).then(|| Organizer {
                email,
                name: o.name().filter(|n| !n.trim().is_empty()).map(str::to_owned),
            })
        });

        let alarms = if wire.is_reminder_on {
            vec![Alarm {
                action: AlarmAction::Display,
                minutes_before: wire.reminder_minutes_before_start.unwrap_or(15),
                description: None,
            }]
        } else {
            Vec::new()
        };

        let conference_url = wire
            .online_meeting
            .as_ref()
            .and_then(|m| m.join_url.clone())
            .or_else(|| wire.online_meeting_url.clone())
            .filter(|url| !url.trim().is_empty())
            .or_else(|| location.as_deref().and_then(find_meeting_url))
            .or_else(|| {
                wire.body
                    .as_ref()
                    .and_then(|b| find_meeting_url(&b.content))
            });

        let rrule = match (&wire.recurrence, event_type) {
            (Some(recurrence), EventType::SeriesMaster | EventType::SingleInstance) => {
                let zone = recurrence
                    .range
                    .recurrence_time_zone
                    .as_deref()
                    .and_then(resolve_tz)
                    .or_else(|| match &start {
                        EventTime::Zoned { tzid, .. } => resolve_tz(tzid),
                        _ => None,
                    })
                    .or_else(|| {
                        wire.original_start_time_zone
                            .as_deref()
                            .and_then(resolve_tz)
                    })
                    .map(SeriesZone::Named)
                    .or_else(|| fixed_offset(&start, utc_start_hint).map(SeriesZone::Fixed));
                match to_rrule_in(recurrence, &start, zone) {
                    Ok(rule) => Some(rule),
                    Err(err) => {
                        tracing::warn!(id = %wire.id, error = %err, "cannot express recurrence as RRULE");
                        None
                    }
                }
            }
            _ => None,
        };
        let exdates = if rrule.is_some() {
            wire.cancelled_occurrences
                .iter()
                .filter_map(|occurrence| occurrence.rsplit('.').next())
                .filter_map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                .map(|date| at_date(date, &start))
                .collect()
        } else {
            Vec::new()
        };

        let show_as = wire.show_as.unwrap_or_default();
        let sensitivity = wire.sensitivity.unwrap_or_default();
        let mut event = Event::new(uid, start, end);
        event.recurrence_id = recurrence_id;
        event.summary = wire.subject.clone().unwrap_or_default();
        event.description = description;
        event.description_html = description_html;
        event.location = location;
        event.url = wire.web_link.clone();
        event.rrule = rrule;
        event.exdates = exdates;
        event.status = if wire.is_cancelled {
            EventStatus::Cancelled
        } else {
            EventStatus::Confirmed
        };
        event.transparency = if show_as == FreeBusyStatus::Free {
            Transparency::Transparent
        } else {
            Transparency::Opaque
        };
        event.class = match sensitivity {
            Sensitivity::Normal => EventClass::Public,
            Sensitivity::Personal | Sensitivity::Private => EventClass::Private,
            Sensitivity::Confidential => EventClass::Confidential,
        };
        event.organizer = organizer;
        event.attendees = attendees;
        event.alarms = alarms;
        event.categories = wire.categories.clone();
        event.conference_url = conference_url;
        event.created = wire.created_date_time;
        event.last_modified = wire.last_modified_date_time;

        Ok(GraphEvent {
            id: wire.id,
            change_key: wire.change_key,
            ical_uid: wire.ical_uid,
            event_type,
            series_master_id: wire.series_master_id,
            original_start: wire.original_start,
            original_start_time_zone: wire.original_start_time_zone,
            original_end_time_zone: wire.original_end_time_zone,
            is_organizer: wire.is_organizer,
            response_status: wire.response_status,
            response_requested,
            allow_new_time_proposals: wire.allow_new_time_proposals.unwrap_or(true),
            is_online_meeting: wire.is_online_meeting,
            online_meeting_provider: wire.online_meeting_provider,
            online_meeting: wire.online_meeting,
            show_as,
            sensitivity,
            importance: wire.importance.unwrap_or_default(),
            is_draft: wire.is_draft,
            hide_attendees: wire.hide_attendees,
            locations: wire.locations,
            recurrence: wire.recurrence,
            event,
        })
    }
}

fn event_time(value: &DateTimeTimeZone, all_day: bool, id: &str) -> Result<EventTime, GraphError> {
    let local = value.local().ok_or_else(|| {
        GraphError::Decode(format!(
            "event {id}: invalid dateTime `{}`",
            value.date_time
        ))
    })?;
    Ok(if all_day {
        EventTime::Date {
            date: all_day_date(local),
        }
    } else if value.is_utc() {
        EventTime::Utc {
            at: local.and_utc(),
        }
    } else {
        EventTime::Zoned {
            local,
            tzid: value.time_zone.clone().unwrap_or_default(),
        }
    })
}

/// The calendar date of an all-day boundary. Graph reports all-day events at
/// midnight; should a boundary arrive shifted by a time zone offset, it is
/// rounded to the nearest midnight.
fn all_day_date(local: NaiveDateTime) -> NaiveDate {
    if local.hour() >= 12 {
        local.date().succ_opt().unwrap_or(local.date())
    } else {
        local.date()
    }
}

/// The UTC offset of a zoned start whose instant is known.
fn fixed_offset(start: &EventTime, utc: Option<DateTime<Utc>>) -> Option<FixedOffset> {
    let (EventTime::Zoned { local, .. }, Some(utc)) = (start, utc) else {
        return None;
    };
    let seconds = local.signed_duration_since(utc.naive_utc()).num_seconds();
    i32::try_from(seconds).ok().and_then(FixedOffset::east_opt)
}

/// The occurrence of a series on `date`, at the series' start time.
fn at_date(date: NaiveDate, start: &EventTime) -> EventTime {
    match start {
        EventTime::Date { .. } => EventTime::Date { date },
        EventTime::Zoned { local, tzid } => EventTime::Zoned {
            local: date.and_time(local.time()),
            tzid: tzid.clone(),
        },
        EventTime::Floating { local } => EventTime::Floating {
            local: date.and_time(local.time()),
        },
        EventTime::Utc { at } => EventTime::Utc {
            at: date.and_time(at.time()).and_utc(),
        },
    }
}

fn map_attendee(attendee: &Attendee, rooms: &HashSet<String>, rsvp: bool) -> Option<IcalAttendee> {
    let email = attendee
        .email_address
        .address
        .as_deref()?
        .trim()
        .to_lowercase();
    if email.is_empty() {
        return None;
    }
    let is_room = rooms.contains(&email);
    let (role, cutype) = match attendee.kind {
        AttendeeType::Required => (AttendeeRole::Required, CalendarUserType::Individual),
        AttendeeType::Optional => (AttendeeRole::Optional, CalendarUserType::Individual),
        AttendeeType::Resource => (AttendeeRole::NonParticipant, CalendarUserType::Resource),
    };
    let cutype = if is_room {
        CalendarUserType::Room
    } else {
        cutype
    };
    let partstat = match attendee.status.response {
        ResponseType::Accepted | ResponseType::Organizer => PartStat::Accepted,
        ResponseType::TentativelyAccepted => PartStat::Tentative,
        ResponseType::Declined => PartStat::Declined,
        ResponseType::None | ResponseType::NotResponded => PartStat::NeedsAction,
    };
    Some(IcalAttendee {
        email,
        name: attendee
            .email_address
            .name
            .clone()
            .filter(|n| !n.trim().is_empty()),
        partstat,
        role,
        cutype,
        rsvp: rsvp && attendee.kind != AttendeeType::Resource && !is_room,
    })
}

/// How events are written to Graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventWriteOptions {
    /// Create the event as an online meeting with this provider (for example
    /// [`OnlineMeetingProvider::TeamsForBusiness`]). The provider must be one
    /// of the calendar's allowed providers.
    pub online_meeting: Option<OnlineMeetingProvider>,
    /// Time zone written for all-day and floating times (Windows or IANA
    /// name). Defaults to `UTC`; the mailbox time zone is a good choice.
    pub time_zone: Option<String>,
    /// `allowNewTimeProposals`.
    pub allow_new_time_proposals: Option<bool>,
    /// `responseRequested`.
    pub response_requested: Option<bool>,
    /// Idempotency key (`transactionId`) for creation; a random one is used
    /// when absent, so retried creations cannot duplicate the event.
    pub transaction_id: Option<String>,
}

/// The name to send for a zone: its Windows id when it is an IANA zone.
/// Graph accepts every Windows zone but only a list of IANA names, and
/// Outlook shows its attendees the Windows zone's own name.
fn graph_zone(tzid: &str) -> &str {
    tzid.trim()
        .parse::<Tz>()
        .ok()
        .and_then(envelope_ical::tz::iana_to_windows)
        .unwrap_or(tzid)
}

fn time_json(time: &EventTime, fallback_tz: &str) -> Value {
    match time {
        EventTime::Date { date } => json!({
            "dateTime": format_local(date.and_time(chrono::NaiveTime::MIN)),
            "timeZone": fallback_tz,
        }),
        EventTime::Zoned { local, tzid } => {
            json!({ "dateTime": format_local(*local), "timeZone": graph_zone(tzid) })
        }
        EventTime::Utc { at } => {
            json!({ "dateTime": format_local(at.naive_utc()), "timeZone": "UTC" })
        }
        EventTime::Floating { local } => {
            json!({ "dateTime": format_local(*local), "timeZone": fallback_tz })
        }
    }
}

fn times_json(event: &Event, fallback_tz: &str) -> Result<(Value, Value), GraphError> {
    match (&event.start, &event.end) {
        (EventTime::Date { date: start }, end) => {
            let end = match end {
                EventTime::Date { date } if date > start => *date,
                EventTime::Date { .. } => start.checked_add_days(Days::new(1)).unwrap_or(*start),
                _ => {
                    return Err(GraphError::InvalidInput(
                        "an all-day event needs an all-day end".into(),
                    ));
                }
            };
            Ok((
                time_json(&EventTime::Date { date: *start }, fallback_tz),
                time_json(&EventTime::Date { date: end }, fallback_tz),
            ))
        }
        (_, EventTime::Date { .. }) => Err(GraphError::InvalidInput(
            "a timed event cannot have an all-day end".into(),
        )),
        (start, end) => Ok((time_json(start, fallback_tz), time_json(end, fallback_tz))),
    }
}

fn body_json(html: Option<&str>, text: Option<&str>) -> Value {
    match (html, text) {
        (Some(html), _) => json!({ "contentType": "html", "content": html }),
        (None, Some(text)) => json!({ "contentType": "text", "content": text }),
        (None, None) => json!({ "contentType": "text", "content": "" }),
    }
}

fn attendee_json(attendee: &IcalAttendee) -> Value {
    let kind = match (attendee.cutype, attendee.role) {
        (CalendarUserType::Resource | CalendarUserType::Room, _) => AttendeeType::Resource,
        (_, AttendeeRole::Optional | AttendeeRole::NonParticipant) => AttendeeType::Optional,
        _ => AttendeeType::Required,
    };
    let mut address = Map::new();
    address.insert("address".into(), Value::String(attendee.email.clone()));
    if let Some(name) = &attendee.name {
        address.insert("name".into(), Value::String(name.clone()));
    }
    json!({ "emailAddress": address, "type": kind.as_str() })
}

/// Placeholder that differs from every real recurrence (used when an old
/// rule cannot be converted, so a diff always rewrites the recurrence).
const UNCONVERTIBLE: &str = "unconvertible recurrence";

/// All writable properties of an event, as Graph JSON.
fn writable_fields(
    event: &Event,
    options: &EventWriteOptions,
    tz: Option<Tz>,
    strict: bool,
) -> Result<Map<String, Value>, GraphError> {
    let mut fields = Map::new();
    fields.insert("subject".into(), Value::String(event.summary.clone()));
    fields.insert(
        "body".into(),
        body_json(
            event.description_html.as_deref(),
            event.description.as_deref(),
        ),
    );
    let fallback_tz = options.time_zone.as_deref().unwrap_or("UTC");
    let (start, end) = times_json(event, fallback_tz)?;
    fields.insert("start".into(), start);
    fields.insert("end".into(), end);
    fields.insert("isAllDay".into(), Value::Bool(event.is_all_day()));
    fields.insert(
        "location".into(),
        json!({ "displayName": event.location.clone().unwrap_or_default() }),
    );
    fields.insert(
        "attendees".into(),
        Value::Array(event.attendees.iter().map(attendee_json).collect()),
    );
    let recurrence = match &event.rrule {
        None => Value::Null,
        Some(rule) => match from_rrule(rule, &event.start, tz) {
            Ok(recurrence) => serde_json::to_value(recurrence)
                .map_err(|e| GraphError::InvalidInput(format!("recurrence: {e}")))?,
            Err(err) if strict => return Err(err.into()),
            Err(_) => Value::String(UNCONVERTIBLE.into()),
        },
    };
    fields.insert("recurrence".into(), recurrence);
    let show_as = match event.transparency {
        Transparency::Transparent => FreeBusyStatus::Free,
        Transparency::Opaque => FreeBusyStatus::Busy,
    };
    fields.insert("showAs".into(), Value::String(show_as.as_str().into()));
    let sensitivity = match event.class {
        EventClass::Public => Sensitivity::Normal,
        EventClass::Private => Sensitivity::Private,
        EventClass::Confidential => Sensitivity::Confidential,
    };
    fields.insert(
        "sensitivity".into(),
        Value::String(sensitivity.as_str().into()),
    );
    let reminder = event
        .alarms
        .iter()
        .map(|a| a.minutes_before)
        .find(|m| *m >= 0);
    fields.insert("isReminderOn".into(), Value::Bool(reminder.is_some()));
    if let Some(minutes) = reminder {
        fields.insert("reminderMinutesBeforeStart".into(), json!(minutes));
    }
    fields.insert("categories".into(), json!(event.categories));
    Ok(fields)
}

/// The JSON body for creating `event`.
pub(crate) fn create_body(
    event: &Event,
    options: &EventWriteOptions,
    tz: Option<Tz>,
) -> Result<Value, GraphError> {
    let mut fields = writable_fields(event, options, tz, true)?;
    if fields.get("recurrence").is_some_and(Value::is_null) {
        fields.remove("recurrence");
    }
    if event
        .location
        .as_deref()
        .is_none_or(|l| l.trim().is_empty())
    {
        fields.remove("location");
    }
    if let Some(provider) = options.online_meeting {
        fields.insert("isOnlineMeeting".into(), Value::Bool(true));
        fields.insert(
            "onlineMeetingProvider".into(),
            Value::String(provider.as_str().into()),
        );
    }
    if let Some(allow) = options.allow_new_time_proposals {
        fields.insert("allowNewTimeProposals".into(), Value::Bool(allow));
    }
    if let Some(requested) = options.response_requested {
        fields.insert("responseRequested".into(), Value::Bool(requested));
    }
    let transaction_id = options
        .transaction_id
        .clone()
        .unwrap_or_else(new_request_id);
    fields.insert("transactionId".into(), Value::String(transaction_id));
    Ok(Value::Object(fields))
}

/// The properties sent with an event update (`PATCH`).
///
/// Build one with [`EventPatch::diff`] to send only what changed, with
/// [`EventPatch::from_event`] to rewrite every writable property, or set raw
/// Graph properties with [`EventPatch::set`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventPatch {
    fields: Map<String, Value>,
}

impl EventPatch {
    /// An empty patch.
    pub fn new() -> Self {
        EventPatch::default()
    }

    /// Every writable property of `event` (removing the recurrence when
    /// `event.rrule` is `None`).
    pub fn from_event(
        event: &Event,
        options: &EventWriteOptions,
        tz: Option<Tz>,
    ) -> Result<Self, GraphError> {
        Ok(EventPatch {
            fields: writable_fields(event, options, tz, true)?,
        })
    }

    /// Only the properties that differ between `old` and `new`.
    ///
    /// Start, end and `isAllDay` are always sent together, as are the
    /// reminder fields. The body is sent as HTML when `description_html`
    /// changed and as text when only `description` changed; replacing the
    /// body of an online meeting must keep its join block (edit
    /// `description_html`), otherwise Outlook disables the online meeting.
    /// `tz` resolves the series zone for recurrence conversion.
    pub fn diff(
        old: &Event,
        new: &Event,
        options: &EventWriteOptions,
        tz: Option<Tz>,
    ) -> Result<Self, GraphError> {
        let before = writable_fields(old, options, tz, false)?;
        let after = writable_fields(new, options, tz, true)?;
        let mut fields = Map::new();
        for (key, value) in &after {
            if key != "body" && before.get(key) != Some(value) {
                fields.insert(key.clone(), value.clone());
            }
        }
        for group in [
            &["start", "end", "isAllDay"][..],
            &["isReminderOn", "reminderMinutesBeforeStart"][..],
        ] {
            if group.iter().any(|k| fields.contains_key(*k)) {
                for key in group {
                    if let Some(value) = after.get(*key) {
                        fields.insert((*key).to_owned(), value.clone());
                    }
                }
            }
        }
        if new.description_html != old.description_html {
            fields.insert(
                "body".into(),
                body_json(new.description_html.as_deref(), new.description.as_deref()),
            );
        } else if new.description != old.description {
            fields.insert("body".into(), body_json(None, new.description.as_deref()));
        }
        Ok(EventPatch { fields })
    }

    /// Sets a raw Graph property.
    pub fn set(mut self, property: impl Into<String>, value: Value) -> Self {
        self.fields.insert(property.into(), value);
        self
    }

    /// True when the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The Graph properties that will be sent.
    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn no_tz(_: &str) -> Option<Tz> {
        None
    }

    fn map(value: Value) -> GraphEvent {
        GraphEvent::from_json(value, &no_tz).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn maps_a_meeting() {
        let event = map(json!({
            "id": "AAMkAGI1AAAt9AHjAAA=",
            "changeKey": "DwAAABYAAAA",
            "iCalUId": "040000008200E00074C5B7101A82E00800000000D3D70B8A1A5AD301000000000000000010000000E5DA5F0F18E5AB4AA6E6E6B79C1C2A4E",
            "subject": "Plan summer company picnic",
            "body": {"contentType": "html", "content": "<html><body><p>Let's kick-start this event planning!</p></body></html>"},
            "start": {"dateTime": "2017-08-30T11:00:00.0000000", "timeZone": "Pacific Standard Time"},
            "end": {"dateTime": "2017-08-30T12:00:00.0000000", "timeZone": "Pacific Standard Time"},
            "isAllDay": false,
            "showAs": "tentative",
            "sensitivity": "personal",
            "isReminderOn": true,
            "reminderMinutesBeforeStart": 30,
            "type": "singleInstance",
            "location": {"displayName": "Conf Room 3; Fourth Coffee; Home Office", "locationType": "default"},
            "locations": [
                {"displayName": "Conf Room 3", "locationType": "conferenceRoom", "locationEmailAddress": "ConfRoom3@contoso.com"}
            ],
            "attendees": [
                {"type": "required", "status": {"response": "tentativelyAccepted", "time": "2017-08-29T10:00:00Z"},
                 "emailAddress": {"name": "Dana Swope", "address": "DanaS@contoso.com"}},
                {"type": "optional", "status": {"response": "none", "time": "0001-01-01T00:00:00Z"},
                 "emailAddress": {"name": "Alex Wilber", "address": "AlexW@contoso.com"}},
                {"type": "resource", "status": {"response": "accepted"},
                 "emailAddress": {"name": "Conf Room 3", "address": "ConfRoom3@contoso.com"}}
            ],
            "organizer": {"emailAddress": {"name": "Megan Bowen", "address": "MeganB@contoso.com"}},
            "isOnlineMeeting": true,
            "onlineMeetingProvider": "teamsForBusiness",
            "onlineMeeting": {"joinUrl": "https://teams.microsoft.com/l/meetup-join/19%3ameeting_x"},
            "webLink": "https://outlook.office365.com/owa/?itemid=AAMkAGI1AAAt9AHjAAA%3D&exvsurl=1&path=/calendar/item",
            "categories": ["Red category"],
            "isCancelled": false,
            "createdDateTime": "2017-08-29T18:12:52.2512474Z",
            "lastModifiedDateTime": "2017-08-29T18:14:00.4398012Z"
        }));
        let e = &event.event;
        assert_eq!(e.summary, "Plan summer company picnic");
        assert_eq!(
            e.description.as_deref(),
            Some("Let's kick-start this event planning!")
        );
        assert!(e.description_html.is_some());
        assert_eq!(
            e.start,
            EventTime::Zoned {
                local: NaiveDate::from_ymd_opt(2017, 8, 30)
                    .and_then(|d| d.and_hms_opt(11, 0, 0))
                    .unwrap_or_default(),
                tzid: "Pacific Standard Time".into(),
            }
        );
        assert_eq!(e.transparency, Transparency::Opaque);
        assert_eq!(e.class, EventClass::Private);
        assert_eq!(e.alarms.first().map(|a| a.minutes_before), Some(30));
        assert_eq!(e.attendees.len(), 3);
        assert_eq!(e.attendees[0].email, "danas@contoso.com");
        assert_eq!(e.attendees[0].partstat, PartStat::Tentative);
        assert_eq!(e.attendees[1].role, AttendeeRole::Optional);
        assert_eq!(e.attendees[2].cutype, CalendarUserType::Room);
        assert_eq!(e.attendees[2].role, AttendeeRole::NonParticipant);
        assert!(!e.attendees[2].rsvp);
        assert_eq!(
            e.organizer.as_ref().map(|o| o.email.as_str()),
            Some("meganb@contoso.com")
        );
        assert_eq!(
            e.conference_url.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_x")
        );
        assert_eq!(e.categories, vec!["Red category".to_owned()]);
        assert!(
            e.url
                .as_deref()
                .is_some_and(|u| u.starts_with("https://outlook.office365.com/"))
        );
        assert!(e.created.is_some() && e.last_modified.is_some());
        assert_eq!(event.show_as, FreeBusyStatus::Tentative);
        assert_eq!(
            event.online_meeting_provider,
            Some(OnlineMeetingProvider::TeamsForBusiness)
        );
    }

    #[test]
    fn maps_occurrences_onto_the_series_uid() {
        let event = map(json!({
            "id": "occ1",
            "iCalUId": "040000008200E00074C5B7101A82E00807E1050ED3D70B8A1A5AD301000000000000000010000000E5DA5F0F18E5AB4AA6E6E6B79C1C2A4E",
            "type": "exception",
            "seriesMasterId": "master1",
            "originalStart": "2017-05-14T16:00:00Z",
            "start": {"dateTime": "2017-05-14T17:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2017-05-14T18:00:00.0000000", "timeZone": "UTC"},
            "showAs": "free",
            "isCancelled": true
        }));
        assert_eq!(
            event.event.uid,
            "040000008200E00074C5B7101A82E00800000000D3D70B8A1A5AD301000000000000000010000000E5DA5F0F18E5AB4AA6E6E6B79C1C2A4E"
        );
        let original = "2017-05-14T16:00:00Z".parse::<DateTime<Utc>>().ok();
        assert_eq!(
            event.event.recurrence_id,
            original.map(|at| EventTime::Utc { at })
        );
        assert!(matches!(event.event.start, EventTime::Utc { .. }));
        assert_eq!(event.event.status, EventStatus::Cancelled);
        assert_eq!(event.event.transparency, Transparency::Transparent);
        assert_eq!(event.series_master_id.as_deref(), Some("master1"));
    }

    #[test]
    fn maps_all_day_series_master_with_cancellations() {
        let event = map(json!({
            "id": "m",
            "type": "seriesMaster",
            "isAllDay": true,
            "start": {"dateTime": "2024-03-04T00:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2024-03-05T00:00:00.0000000", "timeZone": "UTC"},
            "recurrence": {
                "pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"], "firstDayOfWeek": "sunday"},
                "range": {"type": "endDate", "startDate": "2024-03-04", "endDate": "2024-06-24"}
            },
            "cancelledOccurrences": ["OID.m.2024-03-11", "OID.m.2024-04-01"]
        }));
        let e = &event.event;
        let d = |m, d| NaiveDate::from_ymd_opt(2024, m, d).unwrap_or_default();
        assert_eq!(e.start, EventTime::Date { date: d(3, 4) });
        assert_eq!(e.end, EventTime::Date { date: d(3, 5) });
        assert_eq!(
            e.rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20240624")
        );
        assert_eq!(
            e.exdates,
            vec![
                EventTime::Date { date: d(3, 11) },
                EventTime::Date { date: d(4, 1) }
            ]
        );
        assert!(e.is_recurring());
    }

    fn iana(tz: &str) -> Option<Tz> {
        crate::client::resolve_iana_time_zone(tz)
    }

    #[test]
    fn utc_masters_move_into_their_series_zone() {
        let event = GraphEvent::from_json(
            json!({
                "id": "m",
                "type": "seriesMaster",
                "start": {"dateTime": "2024-03-05T02:00:00.0000000", "timeZone": "UTC"},
                "end": {"dateTime": "2024-03-05T03:00:00.0000000", "timeZone": "UTC"},
                "originalStartTimeZone": "America/Los_Angeles",
                "recurrence": {
                    "pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"], "firstDayOfWeek": "sunday"},
                    "range": {"type": "endDate", "startDate": "2024-03-04", "endDate": "2024-03-25",
                              "recurrenceTimeZone": "America/Los_Angeles"}
                }
            }),
            &iana,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Monday 18:00 in Los Angeles, not Tuesday 02:00 UTC.
        let local = NaiveDate::from_ymd_opt(2024, 3, 4)
            .and_then(|d| d.and_hms_opt(18, 0, 0))
            .unwrap_or_default();
        assert_eq!(
            event.event.start,
            EventTime::Zoned {
                local,
                tzid: "America/Los_Angeles".into()
            }
        );
        // March 25 18:00 PDT is 01:00Z on the 26th; UNTIL keeps it.
        assert_eq!(
            event.event.rrule.as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO;WKST=SU;UNTIL=20240326T065959Z")
        );
    }

    #[test]
    fn all_day_exceptions_use_the_series_zone() {
        let event = GraphEvent::from_json(
            json!({
                "id": "x",
                "type": "exception",
                "isAllDay": true,
                "originalStart": "2024-03-03T11:00:00Z",
                "originalStartTimeZone": "Pacific/Auckland",
                "start": {"dateTime": "2024-03-05T00:00:00.0000000", "timeZone": "UTC"},
                "end": {"dateTime": "2024-03-06T00:00:00.0000000", "timeZone": "UTC"}
            }),
            &iana,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            event.event.recurrence_id,
            NaiveDate::from_ymd_opt(2024, 3, 4).map(|date| EventTime::Date { date })
        );
    }

    fn sample_event() -> Event {
        let local = NaiveDate::from_ymd_opt(2024, 3, 4)
            .and_then(|d| d.and_hms_opt(9, 0, 0))
            .unwrap_or_default();
        let mut event = Event::new(
            "uid-1",
            EventTime::Zoned {
                local,
                tzid: "Europe/Istanbul".into(),
            },
            EventTime::Zoned {
                local: local + chrono::Duration::hours(1),
                tzid: "Europe/Istanbul".into(),
            },
        );
        event.summary = "Weekly sync".into();
        event.description = Some("Notes".into());
        event.location = Some("Room 1".into());
        event.rrule = Some("FREQ=WEEKLY;BYDAY=MO;COUNT=5".into());
        event.attendees = vec![IcalAttendee {
            email: "a@example.com".into(),
            name: Some("A".into()),
            role: AttendeeRole::Optional,
            ..IcalAttendee::default()
        }];
        event.alarms = vec![Alarm {
            action: AlarmAction::Display,
            minutes_before: 10,
            description: None,
        }];
        event
    }

    #[test]
    fn builds_create_body() {
        let options = EventWriteOptions {
            online_meeting: Some(OnlineMeetingProvider::TeamsForBusiness),
            transaction_id: Some("tx-1".into()),
            ..EventWriteOptions::default()
        };
        let body = create_body(&sample_event(), &options, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body["subject"], "Weekly sync");
        assert_eq!(
            body["start"],
            json!({"dateTime": "2024-03-04T09:00:00", "timeZone": "Turkey Standard Time"})
        );
        assert_eq!(body["isAllDay"], false);
        assert_eq!(
            body["body"],
            json!({"contentType": "text", "content": "Notes"})
        );
        assert_eq!(body["attendees"][0]["type"], "optional");
        assert_eq!(
            body["recurrence"]["pattern"]["daysOfWeek"],
            json!(["monday"])
        );
        assert_eq!(body["recurrence"]["range"]["numberOfOccurrences"], 5);
        assert_eq!(body["reminderMinutesBeforeStart"], 10);
        assert_eq!(body["isOnlineMeeting"], true);
        assert_eq!(body["onlineMeetingProvider"], "teamsForBusiness");
        assert_eq!(body["transactionId"], "tx-1");
    }

    #[test]
    fn zones_are_written_as_windows_zones() {
        let at = |h| {
            NaiveDate::from_ymd_opt(2026, 11, 10)
                .and_then(|d| d.and_hms_opt(h, 0, 0))
                .unwrap_or_default()
        };
        let mut event = sample_event();
        for (iana, windows) in [
            ("America/New_York", "Eastern Standard Time"),
            ("Europe/Rome", "W. Europe Standard Time"),
            ("Asia/Kolkata", "India Standard Time"),
            ("Asia/Calcutta", "India Standard Time"),
            // Already a Windows name.
            ("Pacific Standard Time", "Pacific Standard Time"),
        ] {
            event.start = EventTime::Zoned {
                local: at(10),
                tzid: iana.into(),
            };
            event.end = EventTime::Zoned {
                local: at(11),
                tzid: iana.into(),
            };
            let body = create_body(&event, &EventWriteOptions::default(), None)
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                body["start"],
                json!({"dateTime": "2026-11-10T10:00:00", "timeZone": windows}),
                "{iana}"
            );
            assert_eq!(body["end"]["timeZone"], windows, "{iana}");
        }
        // The same zone under its two names is no change.
        let mut renamed = event.clone();
        renamed.start = EventTime::Zoned {
            local: at(10),
            tzid: "America/Los_Angeles".into(),
        };
        renamed.end = EventTime::Zoned {
            local: at(11),
            tzid: "America/Los_Angeles".into(),
        };
        let patch = EventPatch::diff(&event, &renamed, &EventWriteOptions::default(), None)
            .unwrap_or_default();
        assert!(patch.is_empty(), "{:?}", sorted_keys(&patch));
    }

    #[test]
    fn single_events_keep_the_zone_they_were_planned_in() {
        let windows = |tz: &str| envelope_ical::resolve_tzid(tz);
        let event = GraphEvent::from_json(
            json!({
                "id": "e",
                "type": "singleInstance",
                "start": {"dateTime": "2026-11-10T15:00:00.0000000", "timeZone": "UTC"},
                "end": {"dateTime": "2026-11-10T16:30:00.0000000", "timeZone": "UTC"},
                "originalStartTimeZone": "Eastern Standard Time",
                "originalEndTimeZone": "Eastern Standard Time"
            }),
            &windows,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let local = |h, m| {
            NaiveDate::from_ymd_opt(2026, 11, 10)
                .and_then(|d| d.and_hms_opt(h, m, 0))
                .unwrap_or_default()
        };
        assert_eq!(
            event.event.start,
            EventTime::Zoned {
                local: local(10, 0),
                tzid: "Eastern Standard Time".into()
            }
        );
        assert_eq!(
            event.event.end,
            EventTime::Zoned {
                local: local(11, 30),
                tzid: "Eastern Standard Time".into()
            }
        );
        // A zone that cannot be resolved, or UTC itself, leaves the times in UTC.
        for zone in ["tzone://Microsoft/Custom", "UTC", "Nowhere Standard Time"] {
            let event = GraphEvent::from_json(
                json!({
                    "id": "e",
                    "start": {"dateTime": "2026-11-10T15:00:00.0000000", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-11-10T16:00:00.0000000", "timeZone": "UTC"},
                    "originalStartTimeZone": zone
                }),
                &windows,
            )
            .unwrap_or_else(|e| panic!("{e}"));
            assert!(matches!(event.event.start, EventTime::Utc { .. }), "{zone}");
        }
    }

    fn sorted_keys(patch: &EventPatch) -> Vec<&str> {
        let mut keys: Vec<&str> = patch.fields().keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn diff_sends_only_changes() {
        let old = sample_event();
        let mut new = old.clone();
        let options = EventWriteOptions::default();
        assert!(
            EventPatch::diff(&old, &new, &options, None)
                .map(|p| p.is_empty())
                .unwrap_or(false)
        );

        new.summary = "Weekly sync (moved)".into();
        new.alarms.clear();
        let patch = EventPatch::diff(&old, &new, &options, None).unwrap_or_default();
        assert_eq!(sorted_keys(&patch), vec!["isReminderOn", "subject"]);

        let mut moved = old.clone();
        moved.end = EventTime::Zoned {
            local: NaiveDate::from_ymd_opt(2024, 3, 4)
                .and_then(|d| d.and_hms_opt(11, 0, 0))
                .unwrap_or_default(),
            tzid: "Europe/Istanbul".into(),
        };
        moved.description = Some("New notes".into());
        let patch = EventPatch::diff(&old, &moved, &options, None).unwrap_or_default();
        assert_eq!(
            sorted_keys(&patch),
            vec!["body", "end", "isAllDay", "start"]
        );
        assert_eq!(
            patch.fields()["body"],
            json!({"contentType": "text", "content": "New notes"})
        );
    }

    #[test]
    fn all_day_writes_use_dates() {
        let mut event = Event::new(
            "x",
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 5, 1).unwrap_or_default(),
            },
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 5, 1).unwrap_or_default(),
            },
        );
        event.summary = "Holiday".into();
        let options = EventWriteOptions {
            time_zone: Some("Turkey Standard Time".into()),
            ..EventWriteOptions::default()
        };
        let body = create_body(&event, &options, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(body["isAllDay"], true);
        assert_eq!(
            body["start"],
            json!({"dateTime": "2024-05-01T00:00:00", "timeZone": "Turkey Standard Time"})
        );
        assert_eq!(
            body["end"],
            json!({"dateTime": "2024-05-02T00:00:00", "timeZone": "Turkey Standard Time"})
        );
        assert!(body.get("location").is_none());
        assert!(body.get("recurrence").is_none());
        assert_eq!(body["isReminderOn"], false);
        assert!(
            body["transactionId"]
                .as_str()
                .is_some_and(|t| t.len() == 36)
        );
    }
}
