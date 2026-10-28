//! The user's calendar list and the color palette.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use envelope_ical::Alarm;
use serde::{Deserialize, Serialize};

use super::event::Reminder;
use super::mapping::map_reminder;
use super::{time, wire};
use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Result, ensure_page_progress};
use crate::serde_util::non_empty;

/// A calendar on the user's calendar list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCalendar {
    /// Calendar id (the primary calendar's id is the user's address).
    pub id: String,
    /// ETag of the calendar list entry.
    pub etag: Option<String>,
    /// The calendar's own title.
    pub summary: String,
    /// The title the user gave the calendar in their list, if any.
    pub summary_override: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Free-form geographic location.
    pub location: Option<String>,
    /// IANA time zone of the calendar.
    pub time_zone: Option<String>,
    /// Owner address of a secondary calendar.
    pub data_owner: Option<String>,
    /// Index into [`ColorPalette::calendar`].
    pub color_id: Option<String>,
    /// Main color (`#rrggbb`): `backgroundColor`, or resolved from `color_id`.
    pub background_color: Option<String>,
    /// Text color (`#rrggbb`) to use on the background color.
    pub foreground_color: Option<String>,
    /// The user's access level.
    pub access_role: AccessRole,
    /// True when the user cannot create or edit events (`freeBusyReader`,
    /// `reader` or no access).
    pub read_only: bool,
    /// Whether this is the user's primary calendar.
    pub primary: bool,
    /// Whether the calendar is shown in the UI.
    pub selected: bool,
    /// Whether the calendar is hidden from the list.
    pub hidden: bool,
    /// Whether the entry was removed from the list.
    pub deleted: bool,
    /// The user's default reminders for this calendar.
    pub default_reminders: Vec<Reminder>,
    /// Conference types events of this calendar support (e.g. `hangoutsMeet`).
    pub allowed_conference_types: Vec<String>,
}

impl GoogleCalendar {
    /// The name to show: the user's override, else the calendar's title.
    pub fn display_name(&self) -> &str {
        self.summary_override.as_deref().unwrap_or(&self.summary)
    }

    /// The default reminders as canonical alarms.
    pub fn default_alarms(&self) -> Vec<Alarm> {
        self.default_reminders
            .iter()
            .map(Reminder::to_alarm)
            .collect()
    }

    /// Whether Google Meet conferences can be added to this calendar's events.
    pub fn supports_google_meet(&self) -> bool {
        self.allowed_conference_types
            .iter()
            .any(|t| t == "hangoutsMeet")
    }
}

/// The user's access level on a calendar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessRole {
    /// No access.
    None,
    /// Can only see free/busy information.
    FreeBusyReader,
    /// Can read events (private event details hidden).
    #[default]
    Reader,
    /// Can read and write events, but not see private event details.
    WriterWithoutPrivateAccess,
    /// Can read and write events, including private details.
    Writer,
    /// Owner: writer plus sharing management.
    Owner,
}

impl AccessRole {
    /// Parses Google's value; unknown values are treated as `reader`.
    pub fn parse(value: &str) -> Self {
        match value {
            "none" => AccessRole::None,
            "freeBusyReader" => AccessRole::FreeBusyReader,
            "writerWithoutPrivateAccess" => AccessRole::WriterWithoutPrivateAccess,
            "writer" => AccessRole::Writer,
            "owner" => AccessRole::Owner,
            _ => AccessRole::Reader,
        }
    }

    /// True for roles that cannot create or change events.
    pub fn is_read_only(self) -> bool {
        matches!(
            self,
            AccessRole::None | AccessRole::FreeBusyReader | AccessRole::Reader
        )
    }
}

/// Google's color palette (`colors.get`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorPalette {
    /// Calendar colors by `colorId`.
    pub calendar: BTreeMap<String, ColorDefinition>,
    /// Event colors by `colorId`.
    pub event: BTreeMap<String, ColorDefinition>,
    /// When the palette last changed.
    pub updated: Option<DateTime<Utc>>,
}

/// A background/foreground color pair (`#rrggbb`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorDefinition {
    /// Background color.
    pub background: String,
    /// Foreground (text) color.
    pub foreground: String,
}

impl GoogleClient {
    /// Returns Google's color palette. Fetched once and cached for the
    /// lifetime of the client.
    pub async fn colors(&self) -> Result<Arc<ColorPalette>> {
        let palette = self
            .palette_cell()
            .get_or_try_init(|| async {
                let request = ApiRequest::get(Service::Calendar, &["colors"]);
                let colors: wire::Colors = self.send_json(&request, "colors.get").await?;
                Ok::<_, crate::Error>(Arc::new(map_colors(colors)))
            })
            .await?;
        Ok(Arc::clone(palette))
    }

    /// Lists every calendar on the user's calendar list, hidden ones included.
    pub async fn list_calendars(&self) -> Result<Vec<GoogleCalendar>> {
        let palette = self.colors().await?;
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let request = ApiRequest::get(Service::Calendar, &["users", "me", "calendarList"])
                .query("maxResults", "250")
                .query("showHidden", "true")
                .query_opt("pageToken", page_token.clone());
            let page: wire::CalendarList = self.send_json(&request, "calendarList.list").await?;
            ensure_page_progress(
                page_token.as_deref(),
                page.next_page_token.as_deref(),
                "calendarList.list",
            )?;
            for item in page.items {
                match wire::CalendarListEntry::deserialize(&item) {
                    Ok(entry) => {
                        if let Some(calendar) = map_calendar(entry, &palette) {
                            calendars.push(calendar);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "skipping malformed calendar list entry")
                    }
                }
            }
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => break,
            }
        }
        for calendar in &calendars {
            self.remember_default_reminders(&calendar.id, &calendar.default_reminders);
            if calendar.primary {
                self.remember_default_reminders("primary", &calendar.default_reminders);
            }
        }
        Ok(calendars)
    }

    /// Fetches one calendar list entry (`primary` is accepted as id).
    pub async fn get_calendar(&self, calendar_id: &str) -> Result<GoogleCalendar> {
        let palette = self.colors().await?;
        let request = ApiRequest::get(
            Service::Calendar,
            &["users", "me", "calendarList", calendar_id],
        );
        let entry: wire::CalendarListEntry = self.send_json(&request, "calendarList.get").await?;
        let calendar = map_calendar(entry, &palette).ok_or_else(|| {
            crate::Error::InvalidResponse("calendar list entry without id".to_owned())
        })?;
        self.remember_default_reminders(calendar_id, &calendar.default_reminders);
        Ok(calendar)
    }
}

fn map_colors(colors: wire::Colors) -> ColorPalette {
    let convert = |map: BTreeMap<String, wire::ColorPair>| {
        map.into_iter()
            .filter_map(|(id, pair)| {
                Some((
                    id,
                    ColorDefinition {
                        background: non_empty(pair.background)?,
                        foreground: non_empty(pair.foreground)
                            .unwrap_or_else(|| "#1d1d1d".to_owned()),
                    },
                ))
            })
            .collect()
    };
    ColorPalette {
        calendar: convert(colors.calendar),
        event: convert(colors.event),
        updated: colors.updated.as_deref().and_then(time::parse_timestamp),
    }
}

pub(crate) fn map_calendar(
    entry: wire::CalendarListEntry,
    palette: &ColorPalette,
) -> Option<GoogleCalendar> {
    let id = non_empty(entry.id)?;
    let color_id = non_empty(entry.color_id);
    let palette_color = color_id.as_ref().and_then(|c| palette.calendar.get(c));
    let access_role = AccessRole::parse(entry.access_role.as_deref().unwrap_or_default());
    Some(GoogleCalendar {
        summary: entry.summary.unwrap_or_else(|| id.clone()),
        id,
        etag: non_empty(entry.etag),
        summary_override: non_empty(entry.summary_override),
        description: non_empty(entry.description),
        location: non_empty(entry.location),
        time_zone: non_empty(entry.time_zone),
        data_owner: non_empty(entry.data_owner),
        background_color: non_empty(entry.background_color)
            .or_else(|| palette_color.map(|c| c.background.clone())),
        foreground_color: non_empty(entry.foreground_color)
            .or_else(|| palette_color.map(|c| c.foreground.clone())),
        color_id,
        access_role,
        read_only: access_role.is_read_only(),
        primary: entry.primary.unwrap_or(false),
        selected: entry.selected.unwrap_or(false),
        hidden: entry.hidden.unwrap_or(false),
        deleted: entry.deleted.unwrap_or(false),
        default_reminders: entry
            .default_reminders
            .iter()
            .filter_map(map_reminder)
            .collect(),
        allowed_conference_types: entry
            .conference_properties
            .map(|p| p.allowed_conference_solution_types)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_roles_map_to_read_only() {
        for (raw, read_only) in [
            ("owner", false),
            ("writer", false),
            ("writerWithoutPrivateAccess", false),
            ("reader", true),
            ("freeBusyReader", true),
            ("none", true),
            ("somethingElse", true),
        ] {
            assert_eq!(AccessRole::parse(raw).is_read_only(), read_only, "{raw}");
        }
    }

    #[test]
    fn color_id_is_resolved_when_hex_is_missing() {
        let palette = ColorPalette {
            calendar: BTreeMap::from([(
                "7".to_owned(),
                ColorDefinition {
                    background: "#42d692".to_owned(),
                    foreground: "#1d1d1d".to_owned(),
                },
            )]),
            ..ColorPalette::default()
        };
        let entry = wire::CalendarListEntry {
            id: Some("team@group.calendar.google.com".to_owned()),
            color_id: Some("7".to_owned()),
            access_role: Some("writer".to_owned()),
            ..Default::default()
        };
        let calendar = map_calendar(entry, &palette).expect("maps");
        assert_eq!(calendar.background_color.as_deref(), Some("#42d692"));
        assert_eq!(calendar.foreground_color.as_deref(), Some("#1d1d1d"));
        assert_eq!(calendar.display_name(), "team@group.calendar.google.com");
        assert!(!calendar.read_only);
    }
}
