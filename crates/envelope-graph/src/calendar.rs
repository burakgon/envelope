//! Calendars, calendar-view sync, event CRUD, meeting responses and
//! free/busy.
//!
//! Graph v1.0 supports delta queries only on the calendar view of the
//! *primary* calendar (`/me/calendarView/delta`); other calendars are synced
//! by paging their calendar view. Calendar views contain single events,
//! occurrences and exceptions, never series masters; fetch those with
//! [`GraphClient::get_series_master`] or [`GraphClient::list_series_masters`].

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use envelope_ical::{Event, EventTime};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::batch::BatchRequest;
use crate::client::{Collection, GraphClient, GraphRequest, Page};
use crate::delta::{DeltaCursor, DeltaPage};
use crate::error::GraphError;
use crate::event::{
    EventPatch, EventWriteOptions, FreeBusyStatus, GraphEvent, OnlineMeetingProvider, WireEvent,
    create_body,
};
use crate::serde_util::{graph_enum, null_default};
use crate::types::{DateTimeTimeZone, EmailAddress};

/// Properties requested for calendars (`isDefaultCalendar` is not returned
/// unless selected).
const CALENDAR_FIELDS: &str = "id,name,color,hexColor,isDefaultCalendar,canEdit,canShare,\
canViewPrivateItems,isRemovable,owner,changeKey,defaultOnlineMeetingProvider,\
allowedOnlineMeetingProviders";

/// Asks Graph to report event times in UTC.
const PREFER_UTC: &str = "outlook.timezone=\"UTC\"";

graph_enum! {
    /// Preset color of a calendar.
    pub enum CalendarColor {
        /// Outlook picks the color.
        Auto = "auto",
        /// Light blue.
        LightBlue = "lightBlue",
        /// Light green.
        LightGreen = "lightGreen",
        /// Light orange.
        LightOrange = "lightOrange",
        /// Light gray.
        LightGray = "lightGray",
        /// Light yellow.
        LightYellow = "lightYellow",
        /// Light teal.
        LightTeal = "lightTeal",
        /// Light pink.
        LightPink = "lightPink",
        /// Light brown.
        LightBrown = "lightBrown",
        /// Light red.
        LightRed = "lightRed",
        /// Sentinel value used by Graph.
        MaxColor = "maxColor",
    }
    unknown = Auto;
    default = Auto;
}

impl CalendarColor {
    /// Approximate CSS color (`#rrggbb`) of the preset; `None` for `auto`.
    pub fn hex(self) -> Option<&'static str> {
        match self {
            CalendarColor::LightBlue => Some("#71afe5"),
            CalendarColor::LightGreen => Some("#7fba00"),
            CalendarColor::LightOrange => Some("#ffaa44"),
            CalendarColor::LightGray => Some("#a19f9d"),
            CalendarColor::LightYellow => Some("#fce100"),
            CalendarColor::LightTeal => Some("#00b7c3"),
            CalendarColor::LightPink => Some("#e43ba6"),
            CalendarColor::LightBrown => Some("#b8865b"),
            CalendarColor::LightRed => Some("#ef6950"),
            CalendarColor::Auto | CalendarColor::MaxColor => None,
        }
    }
}

/// A calendar of the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// Calendar id.
    pub id: String,
    /// Display name.
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    /// Preset color.
    #[serde(default, deserialize_with = "null_default")]
    pub color: CalendarColor,
    /// Explicit color (`#rrggbb`); empty when the user never picked one.
    #[serde(default)]
    pub hex_color: Option<String>,
    /// Whether the user can write to the calendar.
    #[serde(default, deserialize_with = "null_default")]
    pub can_edit: bool,
    /// Whether the user can share the calendar.
    #[serde(default, deserialize_with = "null_default")]
    pub can_share: bool,
    /// Whether the user can read private items.
    #[serde(default, deserialize_with = "null_default")]
    pub can_view_private_items: bool,
    /// True for the user's primary calendar.
    #[serde(default, deserialize_with = "null_default")]
    pub is_default_calendar: bool,
    /// Whether the calendar can be deleted.
    #[serde(default, deserialize_with = "null_default")]
    pub is_removable: bool,
    /// Owner (for shared calendars, the sharer).
    #[serde(default)]
    pub owner: Option<EmailAddress>,
    /// Version of the calendar object.
    #[serde(default)]
    pub change_key: Option<String>,
    /// Default online meeting provider for new events.
    #[serde(default)]
    pub default_online_meeting_provider: Option<OnlineMeetingProvider>,
    /// Online meeting providers events in this calendar may use.
    #[serde(default, deserialize_with = "null_default")]
    pub allowed_online_meeting_providers: Vec<OnlineMeetingProvider>,
}

impl Calendar {
    /// The calendar color as CSS `#rrggbb`: the explicit `hexColor` when set,
    /// otherwise an approximation of the preset.
    pub fn css_color(&self) -> Option<String> {
        self.hex_color
            .as_deref()
            .and_then(normalize_hex)
            .or_else(|| self.color.hex().map(str::to_owned))
    }
}

fn normalize_hex(value: &str) -> Option<String> {
    let digits = value.trim().trim_start_matches('#');
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        6 => Some(format!("#{}", digits.to_ascii_lowercase())),
        3 => Some(
            std::iter::once('#')
                .chain(digits.chars().flat_map(|c| [c, c]))
                .collect::<String>()
                .to_ascii_lowercase(),
        ),
        _ => None,
    }
}

/// A time range for calendar views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeWindow {
    /// Inclusive start.
    pub start: DateTime<Utc>,
    /// Exclusive end.
    pub end: DateTime<Utc>,
}

impl TimeWindow {
    /// A window from `start` to `end`; fails when `end` is not after `start`.
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, GraphError> {
        if end <= start {
            return Err(GraphError::InvalidInput(
                "time window must end after it starts".into(),
            ));
        }
        Ok(TimeWindow { start, end })
    }

    fn start_param(&self) -> String {
        self.start.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    fn end_param(&self) -> String {
        self.end.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    fn apply(&self, request: GraphRequest) -> GraphRequest {
        request
            .query("startDateTime", self.start_param())
            .query("endDateTime", self.end_param())
    }
}

graph_enum! {
    /// A meeting response.
    pub enum EventResponse {
        /// Accept.
        Accept = "accept",
        /// Tentatively accept.
        TentativelyAccept = "tentativelyAccept",
        /// Decline.
        Decline = "decline",
    }
}

/// Free/busy information of one mailbox (`getSchedule`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInformation {
    /// The address that was queried.
    pub schedule_id: String,
    /// One digit per interval: `0` free (or working elsewhere), `1`
    /// tentative, `2` busy, `3` out of office.
    #[serde(default, deserialize_with = "null_default")]
    pub availability_view: String,
    /// Busy periods.
    #[serde(default, deserialize_with = "null_default")]
    pub schedule_items: Vec<ScheduleItem>,
    /// Set when the mailbox could not be queried.
    #[serde(default)]
    pub error: Option<FreeBusyError>,
}

/// One busy period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleItem {
    /// Free/busy status.
    #[serde(default, deserialize_with = "null_default")]
    pub status: FreeBusyStatus,
    /// Start (UTC as requested).
    pub start: DateTimeTimeZone,
    /// End (UTC as requested).
    pub end: DateTimeTimeZone,
    /// Subject, when visible.
    #[serde(default)]
    pub subject: Option<String>,
    /// Location, when visible.
    #[serde(default)]
    pub location: Option<String>,
    /// Whether the item is private.
    #[serde(default, deserialize_with = "null_default")]
    pub is_private: bool,
}

/// Why free/busy information is missing for a mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeBusyError {
    /// Error description.
    #[serde(default, deserialize_with = "null_default")]
    pub message: String,
    /// Error code.
    #[serde(default, deserialize_with = "null_default")]
    pub response_code: String,
}

/// Minimal fields read before fetching a series master in its own zone.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesProbe {
    #[serde(default)]
    id: String,
    /// The start in UTC (probes are requested without a time zone).
    #[serde(default)]
    start: Option<DateTimeTimeZone>,
    #[serde(default)]
    original_start_time_zone: Option<String>,
    #[serde(default)]
    recurrence: Option<Value>,
    #[serde(default, deserialize_with = "null_default")]
    cancelled_occurrences: Vec<String>,
}

impl SeriesProbe {
    /// The zone the series is defined in, if Graph can render times in it.
    fn time_zone(&self) -> String {
        let usable = |tz: &&str| !tz.trim().is_empty() && !tz.starts_with("tzone://");
        self.recurrence
            .as_ref()
            .and_then(|r| r.pointer("/range/recurrenceTimeZone"))
            .and_then(Value::as_str)
            .filter(usable)
            .or_else(|| self.original_start_time_zone.as_deref().filter(usable))
            .unwrap_or("UTC")
            .to_owned()
    }
}

fn prefer_time_zone(time_zone: &str) -> String {
    format!("outlook.timezone=\"{}\"", time_zone.replace('"', ""))
}

fn events_collection(calendar_id: Option<&str>) -> Vec<String> {
    match calendar_id {
        Some(id) => vec!["me".into(), "calendars".into(), id.into(), "events".into()],
        None => vec!["me".into(), "events".into()],
    }
}

impl GraphClient {
    fn map_event(&self, wire: WireEvent) -> Result<GraphEvent, GraphError> {
        GraphEvent::from_wire(wire, &|tz| self.resolve_time_zone(tz))
    }

    /// The zone of an event's start, for recurrence conversion.
    fn event_zone(&self, event: &Event) -> Option<Tz> {
        match &event.start {
            EventTime::Zoned { tzid, .. } => self.resolve_time_zone(tzid),
            EventTime::Utc { .. } => Some(Tz::UTC),
            EventTime::Date { .. } | EventTime::Floating { .. } => None,
        }
    }

    /// All calendars of the user, including calendars shared with them.
    pub async fn list_calendars(&self) -> Result<Vec<Calendar>, GraphError> {
        let request = GraphRequest::get(["me", "calendars"])
            .query("$select", CALENDAR_FIELDS)
            .query("$top", "100");
        self.get_all(&request, None).await
    }

    /// One page of the delta of the primary calendar's view of `window`.
    ///
    /// Start a round with `cursor = None`; continue with the returned
    /// [`DeltaCursor`] exactly as for [`GraphClient::message_delta`] (the
    /// window is encoded in the links). Removed ids include events that left
    /// the window. Times are reported in UTC; all event properties are
    /// returned (`$select` is not supported here).
    pub async fn calendar_view_delta(
        &self,
        window: &TimeWindow,
        cursor: Option<&DeltaCursor>,
        page_size: Option<u32>,
    ) -> Result<DeltaPage<GraphEvent>, GraphError> {
        let mut request = match cursor {
            Some(cursor) => GraphRequest::link(Method::GET, cursor.url()),
            None => window.apply(GraphRequest::get(["me", "calendarView", "delta"])),
        }
        .prefer(PREFER_UTC);
        if let Some(size) = page_size {
            request = request.prefer(format!("odata.maxpagesize={size}"));
        }
        let page: DeltaPage<WireEvent> = self.delta_page_raw(&request).await?;
        page.try_map(|wire| self.map_event(wire))
    }

    /// One page of the calendar view of any calendar (`None` = primary).
    /// Pass the previous page's `next_link` to continue.
    pub async fn calendar_view_page(
        &self,
        calendar_id: Option<&str>,
        window: &TimeWindow,
        next_link: Option<&str>,
        page_size: u32,
    ) -> Result<Page<GraphEvent>, GraphError> {
        let request = match next_link {
            Some(link) => GraphRequest::link(Method::GET, link),
            None => window
                .apply(calendar_view_request(calendar_id))
                .query("$top", page_size.clamp(1, 1000).to_string()),
        }
        .prefer(PREFER_UTC);
        let page: Page<WireEvent> = self.get_page(&request).await?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(|wire| self.map_event(wire))
                .collect::<Result<_, _>>()?,
            next_link: page.next_link,
        })
    }

    /// The complete calendar view of a calendar (`None` = primary) for
    /// `window`, following all pages.
    pub async fn calendar_view(
        &self,
        calendar_id: Option<&str>,
        window: &TimeWindow,
    ) -> Result<Vec<GraphEvent>, GraphError> {
        let request = window
            .apply(calendar_view_request(calendar_id))
            .query("$top", "250")
            .prefer(PREFER_UTC);
        let wires: Vec<WireEvent> = self.get_all(&request, None).await?;
        wires.into_iter().map(|wire| self.map_event(wire)).collect()
    }

    /// One event, with times in `time_zone` (default UTC; Windows or IANA
    /// name). Series masters always come in their own time zone, so that
    /// DTSTART and RRULE agree (this may take a second request).
    pub async fn get_event(
        &self,
        event_id: &str,
        time_zone: Option<&str>,
    ) -> Result<GraphEvent, GraphError> {
        let wire = self
            .fetch_event(event_id, time_zone.unwrap_or("UTC"))
            .await?;
        let wire = self.in_series_zone(wire).await?;
        self.map_event(wire)
    }

    async fn fetch_event(&self, event_id: &str, time_zone: &str) -> Result<WireEvent, GraphError> {
        let request =
            GraphRequest::get(["me", "events", event_id]).prefer(prefer_time_zone(time_zone));
        self.send_json(&request).await
    }

    /// Series masters only make sense in their own zone (the RRULE days are
    /// wall-clock days there). A master received in another zone is fetched
    /// again in its series zone; its UTC start is kept as an offset hint.
    async fn in_series_zone(&self, wire: WireEvent) -> Result<WireEvent, GraphError> {
        let Some(zone) = wire.series_zone().map(str::to_owned) else {
            return Ok(wire);
        };
        // All-day series are dates; their zone does not matter.
        if wire.is_all_day() || wire.start_in_zone(&zone) {
            return Ok(wire);
        }
        let mut fetched = self.fetch_event(&wire.id, &zone).await?;
        fetched.utc_start_hint = wire.utc_start().or(wire.utc_start_hint);
        if fetched.cancelled_occurrences.is_empty() {
            fetched.cancelled_occurrences = wire.cancelled_occurrences;
        }
        Ok(fetched)
    }

    /// A series master with its times in the series' own time zone (so the
    /// RRULE and the wall-clock start line up across DST) and cancelled
    /// occurrences as EXDATEs.
    pub async fn get_series_master(&self, event_id: &str) -> Result<GraphEvent, GraphError> {
        let probe_request = |select: &str| {
            GraphRequest::get(["me", "events", event_id]).query("$select", select.to_owned())
        };
        let probe: SeriesProbe = match self
            .send_json(&probe_request(
                "start,originalStartTimeZone,recurrence,cancelledOccurrences",
            ))
            .await
        {
            Ok(probe) => probe,
            Err(err) if err.status() == Some(400) => {
                self.send_json(&probe_request("start,originalStartTimeZone,recurrence"))
                    .await?
            }
            Err(err) => return Err(err),
        };
        let mut wire = self.fetch_event(event_id, &probe.time_zone()).await?;
        wire.cancelled_occurrences = probe.cancelled_occurrences;
        wire.utc_start_hint = probe.start.as_ref().and_then(DateTimeTimeZone::to_utc);
        self.map_event(wire)
    }

    /// All series masters of a calendar (`None` = primary), each in its own
    /// time zone (fetched with JSON batches). Masters deleted meanwhile are
    /// skipped. Cancelled occurrences are only reported by
    /// [`GraphClient::get_series_master`].
    pub async fn list_series_masters(
        &self,
        calendar_id: Option<&str>,
    ) -> Result<Vec<GraphEvent>, GraphError> {
        let request = GraphRequest::get(events_collection(calendar_id))
            .query("$filter", "type eq 'seriesMaster'")
            .query("$select", "id,start,originalStartTimeZone,recurrence")
            .query("$top", "100");
        let probes: Vec<SeriesProbe> = self.get_all(&request, None).await?;
        let probes: Vec<SeriesProbe> = probes.into_iter().filter(|p| !p.id.is_empty()).collect();
        // Batch ids are compared case-insensitively while Exchange ids are
        // not, so the position serves as the id.
        let requests = probes
            .iter()
            .enumerate()
            .map(|(index, probe)| {
                BatchRequest::new(
                    index.to_string(),
                    GraphRequest::get(["me", "events", probe.id.as_str()])
                        .prefer(prefer_time_zone(&probe.time_zone())),
                )
            })
            .collect();
        let mut masters = Vec::new();
        for (response, probe) in self.batch(requests).await?.into_iter().zip(&probes) {
            match response.into_result() {
                Ok(body) => {
                    let mut wire: WireEvent = serde_json::from_value(body.unwrap_or(Value::Null))
                        .map_err(|e| GraphError::decode("event", e))?;
                    wire.utc_start_hint = probe.start.as_ref().and_then(DateTimeTimeZone::to_utc);
                    masters.push(self.map_event(wire)?);
                }
                Err(err) if err.is_not_found() => {}
                Err(err) => return Err(err),
            }
        }
        Ok(masters)
    }

    /// Occurrences and exceptions of a series within `window` (times in UTC).
    pub async fn event_instances(
        &self,
        series_master_id: &str,
        window: &TimeWindow,
    ) -> Result<Vec<GraphEvent>, GraphError> {
        let request = window
            .apply(GraphRequest::get([
                "me",
                "events",
                series_master_id,
                "instances",
            ]))
            .query("$top", "250")
            .prefer(PREFER_UTC);
        let wires: Vec<WireEvent> = self.get_all(&request, None).await?;
        wires.into_iter().map(|wire| self.map_event(wire)).collect()
    }

    /// Creates an event in a calendar (`None` = primary) and returns it as
    /// stored by Graph. Graph assigns the `iCalUId`, so the returned
    /// [`Event::uid`] replaces the one of `event`. Attendees are invited
    /// immediately. Recurring events are returned in their series zone.
    pub async fn create_event(
        &self,
        calendar_id: Option<&str>,
        event: &Event,
        options: &EventWriteOptions,
    ) -> Result<GraphEvent, GraphError> {
        let body = create_body(event, options, self.event_zone(event))?;
        let request = GraphRequest::post(events_collection(calendar_id))
            .prefer(PREFER_UTC)
            .json(&body)?;
        let wire: WireEvent = self.send_json(&request).await?;
        let wire = self.in_series_zone(wire).await?;
        self.map_event(wire)
    }

    /// Applies a patch to an event and returns the updated event (series
    /// masters in their own zone). Attendees receive an update when meeting
    /// properties change.
    pub async fn update_event(
        &self,
        event_id: &str,
        patch: &EventPatch,
    ) -> Result<GraphEvent, GraphError> {
        if patch.is_empty() {
            return self.get_event(event_id, None).await;
        }
        let request = GraphRequest::patch(["me", "events", event_id])
            .prefer(PREFER_UTC)
            .json(patch.fields())?;
        let wire: WireEvent = self.send_json(&request).await?;
        let wire = self.in_series_zone(wire).await?;
        self.map_event(wire)
    }

    /// Sends only what changed between `old` and `new` (see
    /// [`EventPatch::diff`]) and returns the updated event.
    pub async fn update_event_diff(
        &self,
        event_id: &str,
        old: &Event,
        new: &Event,
        options: &EventWriteOptions,
    ) -> Result<GraphEvent, GraphError> {
        let patch = EventPatch::diff(old, new, options, self.event_zone(new))?;
        self.update_event(event_id, &patch).await
    }

    /// Deletes an event. For meetings organized by the user, Graph sends
    /// cancellations to the attendees.
    pub async fn delete_event(&self, event_id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::delete(["me", "events", event_id]))
            .await
    }

    /// Cancels a meeting the user organizes, sending `comment` to attendees.
    /// Works on single occurrences, too; attendees get a `400` error.
    pub async fn cancel_event(
        &self,
        event_id: &str,
        comment: Option<&str>,
    ) -> Result<(), GraphError> {
        let request = GraphRequest::post(["me", "events", event_id, "cancel"])
            .json(&json!({ "comment": comment.unwrap_or_default() }))?;
        self.send_empty(&request).await
    }

    /// Accepts, tentatively accepts or declines a meeting invitation.
    /// With `send_response = false` the organizer is not notified.
    pub async fn respond_to_event(
        &self,
        event_id: &str,
        response: EventResponse,
        comment: Option<&str>,
        send_response: bool,
    ) -> Result<(), GraphError> {
        let request =
            GraphRequest::post(["me", "events", event_id, response.as_str()]).json(&json!({
                "comment": comment.unwrap_or_default(),
                "sendResponse": send_response,
            }))?;
        self.send_empty(&request).await
    }

    /// Free/busy information for up to 20 addresses over `window` (less than
    /// 62 days), with `interval_minutes` (5–1440) per availability digit.
    /// Not available for personal Microsoft accounts.
    pub async fn get_schedule(
        &self,
        schedules: &[String],
        window: &TimeWindow,
        interval_minutes: u32,
    ) -> Result<Vec<ScheduleInformation>, GraphError> {
        if schedules.is_empty() {
            return Ok(Vec::new());
        }
        if schedules.len() > 20 {
            return Err(GraphError::InvalidInput(
                "getSchedule accepts at most 20 addresses".into(),
            ));
        }
        let request = GraphRequest::post(["me", "calendar", "getSchedule"])
            .prefer(PREFER_UTC)
            .json(&json!({
                "schedules": schedules,
                "startTime": DateTimeTimeZone::utc(window.start),
                "endTime": DateTimeTimeZone::utc(window.end),
                "availabilityViewInterval": interval_minutes.clamp(5, 1440),
            }))?;
        let collection: Collection<ScheduleInformation> = self.send_json(&request).await?;
        Ok(collection.value)
    }
}

fn calendar_view_request(calendar_id: Option<&str>) -> GraphRequest {
    match calendar_id {
        Some(id) => GraphRequest::get(["me", "calendars", id, "calendarView"]),
        None => GraphRequest::get(["me", "calendarView"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_colors() {
        let mut calendar: Calendar = serde_json::from_value(json!({
            "id": "c1", "name": "Calendar", "color": "lightBlue", "hexColor": "",
            "isDefaultCalendar": true, "canEdit": true,
            "owner": {"name": "Megan Bowen", "address": "MeganB@contoso.com"}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(calendar.css_color().as_deref(), Some("#71afe5"));
        calendar.hex_color = Some("#E3008C".into());
        assert_eq!(calendar.css_color().as_deref(), Some("#e3008c"));
        calendar.hex_color = Some("#abc".into());
        assert_eq!(calendar.css_color().as_deref(), Some("#aabbcc"));
        calendar.hex_color = None;
        calendar.color = CalendarColor::Auto;
        assert_eq!(calendar.css_color(), None);
    }

    #[test]
    fn windows_must_be_ordered() {
        let now = Utc::now();
        assert!(TimeWindow::new(now, now).is_err());
        let window = TimeWindow::new(now, now + chrono::Duration::days(7));
        assert!(window.is_ok());
    }

    #[test]
    fn series_probe_prefers_recurrence_zone() {
        let probe: SeriesProbe = serde_json::from_value(json!({
            "originalStartTimeZone": "tzone://Microsoft/Custom",
            "recurrence": {"range": {"recurrenceTimeZone": "Pacific Standard Time"}}
        }))
        .unwrap_or_default();
        assert_eq!(probe.time_zone(), "Pacific Standard Time");
        let custom: SeriesProbe =
            serde_json::from_value(json!({"originalStartTimeZone": "tzone://Microsoft/Custom"}))
                .unwrap_or_default();
        assert_eq!(custom.time_zone(), "UTC");
    }
}
