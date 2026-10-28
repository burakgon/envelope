//! CalDAV calendars (RFC 4791): listing, creation, property updates and
//! time-range queries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use crate::client::{DavClient, DavRequest, Depth, dav_method};
use crate::collection::{collection_url_in, expect_created};
use crate::error::{DavError, Result};
use crate::href;
use crate::href::ObjectKind;
use crate::multistatus::Response;
use crate::object::{DavObject, data_prop, objects_from_reply};
use crate::props::{self, color_for_server, normalize_color, parse_order, timezone_id};
use crate::xml::{self, Ns, PropName, REQUEST_NAMESPACES, XML_DECLARATION, text_element};

/// Calendar component types a calendar accepts
/// (`supported-calendar-component-set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSet {
    /// Events (`VEVENT`).
    pub vevent: bool,
    /// Tasks (`VTODO`).
    pub vtodo: bool,
    /// Journal entries (`VJOURNAL`).
    pub vjournal: bool,
}

impl ComponentSet {
    /// Every component type; what a server without
    /// `supported-calendar-component-set` accepts.
    pub const ALL: ComponentSet = ComponentSet {
        vevent: true,
        vtodo: true,
        vjournal: true,
    };
    /// Events only.
    pub const EVENTS: ComponentSet = ComponentSet {
        vevent: true,
        vtodo: false,
        vjournal: false,
    };
    /// Tasks only (for example iCloud reminder lists).
    pub const TASKS: ComponentSet = ComponentSet {
        vevent: false,
        vtodo: true,
        vjournal: false,
    };

    fn is_empty(self) -> bool {
        !(self.vevent || self.vtodo || self.vjournal)
    }

    fn names(self) -> impl Iterator<Item = &'static str> {
        [
            (self.vevent, "VEVENT"),
            (self.vtodo, "VTODO"),
            (self.vjournal, "VJOURNAL"),
        ]
        .into_iter()
        .filter_map(|(enabled, name)| enabled.then_some(name))
    }
}

/// A calendar collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    /// Absolute, normalised URL of the collection.
    pub href: String,
    /// `DAV:displayname`.
    pub display_name: Option<String>,
    /// `calendar-description`.
    pub description: Option<String>,
    /// Apple `calendar-color`, normalised to `#rrggbb`.
    pub color: Option<String>,
    /// Apple `calendar-order` (lower sorts first).
    pub order: Option<i32>,
    /// CalendarServer `getctag`: changes whenever anything in the calendar
    /// changes.
    pub ctag: Option<String>,
    /// RFC 6578 sync token of the current state.
    pub sync_token: Option<String>,
    /// Component types the calendar accepts.
    pub components: ComponentSet,
    /// True when the user may not add, change or delete objects.
    pub read_only: bool,
    /// `calendar-timezone`: a `VCALENDAR` holding the calendar's default
    /// `VTIMEZONE`.
    pub timezone: Option<String>,
    /// TZID of the default time zone (from `calendar-timezone-id` or the
    /// `VTIMEZONE` above).
    pub timezone_id: Option<String>,
    /// True when the calendar supports `sync-collection`; otherwise use
    /// [`DavClient::list_etags`] to detect changes.
    pub supports_sync: bool,
}

/// Properties of a calendar to create with [`DavClient::create_calendar`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCalendar {
    /// Name shown to the user.
    pub display_name: String,
    /// `#rrggbb` colour.
    pub color: Option<String>,
    /// Free-form description.
    pub description: Option<String>,
    /// Component types to accept; [`ComponentSet::EVENTS`] by default. An
    /// empty set leaves the choice to the server.
    pub components: ComponentSet,
    /// `VCALENDAR` with the default `VTIMEZONE`.
    pub timezone: Option<String>,
}

impl NewCalendar {
    /// An event calendar with the given name and no other properties.
    pub fn new(display_name: impl Into<String>) -> Self {
        NewCalendar {
            display_name: display_name.into(),
            color: None,
            description: None,
            components: ComponentSet::EVENTS,
            timezone: None,
        }
    }

    /// Sets the colour (`#rrggbb`).
    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Sets the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the accepted component types.
    pub fn with_components(mut self, components: ComponentSet) -> Self {
        self.components = components;
        self
    }
}

/// Changes applied by [`DavClient::update_calendar`]; `None` fields are left
/// untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarPatch {
    /// New display name.
    pub display_name: Option<String>,
    /// New `#rrggbb` colour.
    pub color: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// New sort order.
    pub order: Option<i32>,
}

const CALENDAR_PROPS: &[PropName] = &[
    (Ns::Dav, "resourcetype"),
    (Ns::Dav, "displayname"),
    (Ns::AppleIcal, "calendar-color"),
    (Ns::AppleIcal, "calendar-order"),
    (Ns::CalDav, "calendar-description"),
    (Ns::CalendarServer, "getctag"),
    (Ns::Dav, "sync-token"),
    (Ns::CalDav, "supported-calendar-component-set"),
    (Ns::Dav, "current-user-privilege-set"),
    (Ns::CalDav, "calendar-timezone"),
    (Ns::CalDav, "calendar-timezone-id"),
    (Ns::Dav, "supported-report-set"),
];

impl DavClient {
    /// Lists the calendars in a calendar home set (from
    /// [`Discovery::calendar_home_set`](crate::Discovery::calendar_home_set)).
    ///
    /// Only calendar collections are returned: scheduling inbox / outbox,
    /// notification and subscription collections are skipped. The order is
    /// the server's.
    pub async fn list_calendars(&self, home_set: &str) -> Result<Vec<Calendar>> {
        let home = self.resolve_input(home_set)?;
        let reply = self
            .propfind(&home, Depth::One, xml::propfind_body(CALENDAR_PROPS))
            .await?;
        let calendars: Vec<Calendar> = reply
            .entries()
            .into_iter()
            .filter(|(url, _)| !href::same_resource(url, &home))
            .filter_map(|(url, response)| calendar_from_response(url, response))
            .collect();
        debug!(home = %home, count = calendars.len(), "listed calendars");
        Ok(calendars)
    }

    /// Reads the properties of a single calendar.
    pub async fn get_calendar(&self, href: &str) -> Result<Calendar> {
        let url = self.resolve_input(href)?;
        let reply = self
            .propfind(&url, Depth::Zero, xml::propfind_body(CALENDAR_PROPS))
            .await?;
        reply
            .own_entry()
            .and_then(|(url, response)| calendar_from_response(url, response))
            .ok_or_else(|| DavError::InvalidResponse(format!("{url} is not a calendar")))
    }

    /// Creates a calendar with `MKCALENDAR` inside `home_set` and returns
    /// its href. The collection gets a random name; the display name is a
    /// property.
    pub async fn create_calendar(&self, home_set: &str, calendar: &NewCalendar) -> Result<String> {
        let home = self.resolve_input(home_set)?;
        let url = collection_url_in(&home)?;
        let body = mkcalendar_body(calendar)?;
        let response = self
            .send(DavRequest::new(dav_method("MKCALENDAR")?, url.clone()).xml(body))
            .await?;
        expect_created(response)?;
        Ok(url.into())
    }

    /// Renames, recolours or reorders a calendar with `PROPPATCH`.
    pub async fn update_calendar(&self, href: &str, patch: &CalendarPatch) -> Result<()> {
        let url = self.resolve_input(href)?;
        let mut props = String::new();
        if let Some(name) = &patch.display_name {
            props.push_str(&text_element(Ns::Dav, "displayname", name));
        }
        if let Some(color) = &patch.color {
            props.push_str(&text_element(
                Ns::AppleIcal,
                "calendar-color",
                &color_for_server(color)?,
            ));
        }
        if let Some(description) = &patch.description {
            props.push_str(&text_element(
                Ns::CalDav,
                "calendar-description",
                description,
            ));
        }
        if let Some(order) = patch.order {
            props.push_str(&text_element(
                Ns::AppleIcal,
                "calendar-order",
                &order.to_string(),
            ));
        }
        if props.is_empty() {
            return Ok(());
        }
        self.proppatch(&url, &props).await
    }

    /// Fetches the events of a calendar overlapping `[start, end)` with a
    /// `calendar-query` report (RFC 4791 §7.8). Recurring events are
    /// returned once, unexpanded, if any occurrence overlaps.
    pub async fn query_time_range(
        &self,
        calendar: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<DavObject>> {
        if end <= start {
            return Err(DavError::InvalidInput(
                "the time range ends before it starts".into(),
            ));
        }
        let url = self.resolve_input(calendar)?;
        let body = time_range_query_body("VEVENT", start, end);
        let reply = self.report(&url, Depth::One, body).await?;
        let (objects, _) = objects_from_reply(&reply, &url, data_prop(ObjectKind::Calendar));
        Ok(objects)
    }
}

/// Builds a [`Calendar`] from a PROPFIND response, or `None` if the resource
/// is not a calendar collection.
fn calendar_from_response(url: Url, response: &Response) -> Option<Calendar> {
    if response.is_not_found() || !response.has_resource_type(Ns::CalDav, "calendar") {
        return None;
    }
    let timezone = response.prop_text(Ns::CalDav, "calendar-timezone");
    let timezone_id = response
        .prop_text(Ns::CalDav, "calendar-timezone-id")
        .or_else(|| timezone.as_deref().and_then(timezone_id));
    Some(Calendar {
        href: url.into(),
        display_name: response.prop_text(Ns::Dav, "displayname"),
        description: response.prop_text(Ns::CalDav, "calendar-description"),
        color: response
            .prop_text(Ns::AppleIcal, "calendar-color")
            .and_then(|color| normalize_color(&color)),
        order: response
            .prop_text(Ns::AppleIcal, "calendar-order")
            .and_then(|order| parse_order(&order)),
        ctag: response.prop_text(Ns::CalendarServer, "getctag"),
        sync_token: response.prop_text(Ns::Dav, "sync-token"),
        components: components(response),
        read_only: props::is_read_only(response),
        timezone,
        timezone_id,
        supports_sync: props::supports_sync(response),
    })
}

/// Interprets `supported-calendar-component-set`; absence means everything
/// is accepted (RFC 4791 §5.2.3).
fn components(response: &Response) -> ComponentSet {
    let Some(set) = response.prop(Ns::CalDav, "supported-calendar-component-set") else {
        return ComponentSet::ALL;
    };
    let names: Vec<String> = set
        .children_named(Ns::CalDav, "comp")
        .filter_map(|comp| comp.attr("name"))
        .map(|name| name.trim().to_ascii_uppercase())
        .collect();
    if names.is_empty() {
        return ComponentSet::ALL;
    }
    let has = |wanted: &str| names.iter().any(|name| name == wanted);
    ComponentSet {
        vevent: has("VEVENT"),
        vtodo: has("VTODO"),
        vjournal: has("VJOURNAL"),
    }
}

fn mkcalendar_body(calendar: &NewCalendar) -> Result<String> {
    let mut props = text_element(Ns::Dav, "displayname", &calendar.display_name);
    if let Some(color) = &calendar.color {
        props.push_str(&text_element(
            Ns::AppleIcal,
            "calendar-color",
            &color_for_server(color)?,
        ));
    }
    if let Some(description) = &calendar.description {
        props.push_str(&text_element(
            Ns::CalDav,
            "calendar-description",
            description,
        ));
    }
    if !calendar.components.is_empty() {
        props.push_str("<C:supported-calendar-component-set>");
        for name in calendar.components.names() {
            props.push_str(&format!("<C:comp name=\"{name}\"/>"));
        }
        props.push_str("</C:supported-calendar-component-set>");
    }
    if let Some(timezone) = &calendar.timezone {
        props.push_str(&text_element(Ns::CalDav, "calendar-timezone", timezone));
    }
    Ok(format!(
        "{XML_DECLARATION}<C:mkcalendar {REQUEST_NAMESPACES}><D:set><D:prop>{props}</D:prop></D:set></C:mkcalendar>"
    ))
}

/// Formats a UTC instant as an iCalendar DATE-TIME (`20240101T000000Z`).
fn ical_utc(instant: DateTime<Utc>) -> String {
    instant.format("%Y%m%dT%H%M%SZ").to_string()
}

fn time_range_query_body(component: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    format!(
        "{XML_DECLARATION}<C:calendar-query {REQUEST_NAMESPACES}>\
         <D:prop><D:getetag/><C:calendar-data/></D:prop>\
         <C:filter><C:comp-filter name=\"VCALENDAR\"><C:comp-filter name=\"{component}\">\
         <C:time-range start=\"{}\" end=\"{}\"/>\
         </C:comp-filter></C:comp-filter></C:filter></C:calendar-query>",
        ical_utc(start),
        ical_utc(end)
    )
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::client::MultiStatusReply;
    use crate::multistatus::parse_multistatus;

    pub(crate) fn calendars_from_fixture(xml: &str, home: &str) -> Vec<Calendar> {
        let home = Url::parse(home).unwrap();
        let reply = MultiStatusReply {
            url: home.clone(),
            multistatus: parse_multistatus(xml).unwrap(),
        };
        reply
            .entries()
            .into_iter()
            .filter(|(url, _)| !href::same_resource(url, &home))
            .filter_map(|(url, response)| calendar_from_response(url, response))
            .collect()
    }

    fn find<'a>(calendars: &'a [Calendar], suffix: &str) -> &'a Calendar {
        calendars
            .iter()
            .find(|c| c.href.ends_with(suffix))
            .unwrap_or_else(|| panic!("no calendar ending with {suffix}"))
    }

    #[test]
    fn icloud_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/icloud_calendars.xml"),
            "https://p07-caldav.icloud.com/1234567890/calendars/",
        );
        let names: Vec<_> = calendars
            .iter()
            .map(|c| c.display_name.clone().unwrap_or_default())
            .collect();
        assert_eq!(names, ["Home", "Work", "Reminders", "Shared with me"]);
        let home = find(&calendars, "/home/");
        assert_eq!(
            home.href,
            "https://p07-caldav.icloud.com/1234567890/calendars/home/"
        );
        assert_eq!(home.color.as_deref(), Some("#ff2968"));
        assert_eq!(home.order, Some(1));
        assert_eq!(
            home.ctag.as_deref(),
            Some("HwoQEgwAAAh4j8A2AAAAAAAAAAAAAAAAGAAgCB==")
        );
        assert_eq!(
            home.sync_token.as_deref(),
            Some("HwoQEgwAAAh4j8A2AAAAAAAAAAAAAAAAGAAgCB==")
        );
        assert_eq!(home.components, ComponentSet::EVENTS);
        assert!(!home.read_only);
        assert!(home.supports_sync);
        assert_eq!(home.timezone_id.as_deref(), Some("Europe/Istanbul"));
        assert!(
            home.timezone
                .as_deref()
                .unwrap()
                .starts_with("BEGIN:VCALENDAR")
        );
        let reminders = find(&calendars, "/tasks/");
        assert_eq!(reminders.components, ComponentSet::TASKS);
        let shared = find(&calendars, "/shared-1/");
        assert!(shared.read_only);
        assert_eq!(shared.color.as_deref(), Some("#1badf8"));
    }

    #[test]
    fn google_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/google_calendars.xml"),
            "https://apidata.googleusercontent.com/caldav/v2/jane.doe%40gmail.com/",
        );
        assert_eq!(calendars.len(), 3);
        let primary = &calendars[0];
        assert_eq!(
            primary.href,
            "https://apidata.googleusercontent.com/caldav/v2/jane.doe@gmail.com/events/"
        );
        assert_eq!(primary.display_name.as_deref(), Some("Jane Doe"));
        assert_eq!(primary.color.as_deref(), Some("#9fe1e7"));
        assert_eq!(primary.timezone_id.as_deref(), Some("Europe/Istanbul"));
        assert!(!primary.read_only);
        let holidays = find(
            &calendars,
            "/en.turkish%23holiday@group.v.calendar.google.com/events/",
        );
        assert!(holidays.read_only);
        assert_eq!(
            holidays.display_name.as_deref(),
            Some("Holidays in Türkiye")
        );
        let team = find(
            &calendars,
            "/c_0123456789abcdef@group.calendar.google.com/events/",
        );
        assert_eq!(team.description.as_deref(), Some("Team rota & on-call"));
    }

    #[test]
    fn nextcloud_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/nextcloud_calendars.xml"),
            "https://cloud.example.com/remote.php/dav/calendars/jane/",
        );
        let names: Vec<_> = calendars
            .iter()
            .map(|c| c.display_name.clone().unwrap_or_default())
            .collect();
        assert_eq!(names, ["Personal", "Contact birthdays", "Tasks"]);
        let personal = &calendars[0];
        assert_eq!(
            personal.href,
            "https://cloud.example.com/remote.php/dav/calendars/jane/personal/"
        );
        assert_eq!(personal.color.as_deref(), Some("#0082c9"));
        assert_eq!(personal.order, Some(0));
        assert_eq!(personal.ctag.as_deref(), Some("http://sabre.io/ns/sync/42"));
        assert_eq!(
            personal.sync_token.as_deref(),
            Some("http://sabre.io/ns/sync/42")
        );
        assert_eq!(
            personal.components,
            ComponentSet {
                vevent: true,
                vtodo: true,
                vjournal: false
            }
        );
        assert!(!personal.read_only);
        assert!(calendars[1].read_only);
        assert_eq!(calendars[2].components, ComponentSet::TASKS);
    }

    #[test]
    fn radicale_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/radicale_calendars.xml"),
            "http://127.0.0.1:5232/jane/",
        );
        assert_eq!(calendars.len(), 1);
        let work = &calendars[0];
        assert_eq!(work.href, "http://127.0.0.1:5232/jane/work/");
        assert_eq!(work.display_name.as_deref(), Some("Work & Life"));
        assert_eq!(work.color.as_deref(), Some("#ff2968"));
        assert!(
            work.sync_token
                .as_deref()
                .unwrap()
                .starts_with("http://radicale.org/ns/sync/")
        );
        assert_eq!(work.components, ComponentSet::EVENTS);
        assert!(!work.read_only);
        assert!(work.supports_sync);
    }

    #[test]
    fn fastmail_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/fastmail_calendars.xml"),
            "https://caldav.fastmail.com/dav/calendars/user/jane@fastmail.com/",
        );
        assert_eq!(calendars.len(), 2);
        assert_eq!(
            calendars[0].href,
            "https://caldav.fastmail.com/dav/calendars/user/jane@fastmail.com/Default/"
        );
        assert_eq!(calendars[0].color.as_deref(), Some("#3a429c"));
        assert_eq!(
            calendars[0].components,
            ComponentSet {
                vevent: true,
                vtodo: true,
                vjournal: false
            }
        );
        assert!(calendars[1].read_only);
        assert_eq!(calendars[1].display_name.as_deref(), Some("Team (shared)"));
    }

    #[test]
    fn baikal_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/baikal_calendars.xml"),
            "https://dav.example.org/dav.php/calendars/jane/",
        );
        assert_eq!(calendars.len(), 1);
        let default = &calendars[0];
        assert_eq!(
            default.href,
            "https://dav.example.org/dav.php/calendars/jane/default/"
        );
        assert_eq!(default.display_name.as_deref(), Some("Default calendar"));
        // No supported-calendar-component-set: everything is accepted.
        assert_eq!(default.components, ComponentSet::ALL);
        assert_eq!(default.color, None);
        assert_eq!(
            default.sync_token.as_deref(),
            Some("http://sabre.io/ns/sync/7")
        );
    }

    #[test]
    fn yahoo_calendars() {
        let calendars = calendars_from_fixture(
            include_str!("../tests/fixtures/yahoo_calendars.xml"),
            "https://caldav.calendar.yahoo.com/dav/jdoe/Calendar/",
        );
        assert_eq!(calendars.len(), 1);
        assert_eq!(
            calendars[0].href,
            "https://caldav.calendar.yahoo.com/dav/jdoe/Calendar/Jane_Doe/"
        );
        assert_eq!(calendars[0].display_name.as_deref(), Some("Jane Doe"));
        assert_eq!(calendars[0].ctag.as_deref(), Some("1708444215133"));
        assert_eq!(calendars[0].color.as_deref(), Some("#8f45bf"));
        assert_eq!(calendars[0].sync_token, None);
        assert!(!calendars[0].supports_sync);
    }

    #[test]
    fn mkcalendar_body_is_well_formed() {
        let body = mkcalendar_body(
            &NewCalendar::new("Work & Life")
                .with_color("#ff2968")
                .with_description("<desc>")
                .with_components(ComponentSet {
                    vevent: true,
                    vtodo: true,
                    vjournal: false,
                }),
        )
        .unwrap();
        let root = xml::parse(&body).unwrap();
        assert!(root.is(Ns::CalDav, "mkcalendar"));
        let prop = root.descendant(Ns::Dav, "prop").unwrap();
        assert_eq!(
            prop.child(Ns::Dav, "displayname").unwrap().text(),
            "Work & Life"
        );
        assert_eq!(
            prop.child(Ns::AppleIcal, "calendar-color").unwrap().text(),
            "#FF2968FF"
        );
        assert_eq!(
            prop.child(Ns::CalDav, "calendar-description")
                .unwrap()
                .text(),
            "<desc>"
        );
        let comps: Vec<&str> = prop
            .child(Ns::CalDav, "supported-calendar-component-set")
            .unwrap()
            .children_named(Ns::CalDav, "comp")
            .filter_map(|c| c.attr("name"))
            .collect();
        assert_eq!(comps, ["VEVENT", "VTODO"]);
        assert!(matches!(
            mkcalendar_body(&NewCalendar::new("x").with_color("nope")),
            Err(DavError::InvalidInput(_))
        ));
    }

    #[test]
    fn time_range_body() {
        let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 2, 1, 12, 30, 5).unwrap();
        let root = xml::parse(&time_range_query_body("VEVENT", start, end)).unwrap();
        let range = root.descendant(Ns::CalDav, "time-range").unwrap();
        assert_eq!(range.attr("start"), Some("20240101T000000Z"));
        assert_eq!(range.attr("end"), Some("20240201T123005Z"));
        let filter = root.descendant(Ns::CalDav, "comp-filter").unwrap();
        assert_eq!(filter.attr("name"), Some("VCALENDAR"));
        assert_eq!(
            filter
                .child(Ns::CalDav, "comp-filter")
                .unwrap()
                .attr("name"),
            Some("VEVENT")
        );
    }
}
