//! Page-by-page event synchronisation with sync tokens.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::calendars::AccessRole;
use super::event::{
    CancelledOccurrence, EventChange, EventType, GoogleEvent, Reminder, SkippedItem,
};
use super::mapping::{MappingContext, classify, map_reminder};
use super::{ColorPalette, time, wire};
use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Error, Result, ensure_page_progress};

/// Largest page size Google allows for `events.list`.
pub(crate) const MAX_PAGE_SIZE: u32 = 2500;

/// Parameters of an event synchronisation run.
///
/// A run is either a **full sync** (no `sync_token`; optionally limited to a
/// time window) or an **incremental sync** that returns only what changed
/// since the run that produced `sync_token`. Both list series masters and
/// their exceptions rather than expanded instances (`singleEvents=false`).
///
/// Google requires all other parameters to stay the same between the full
/// sync and the incremental syncs that follow it, so keep `event_types` and
/// `page_size` stable for a calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSyncOptions {
    /// `nextSyncToken` of the previous run; `None` for a full sync.
    pub sync_token: Option<String>,
    /// Full sync only: skip events that end before this instant.
    pub time_min: Option<DateTime<Utc>>,
    /// Full sync only: skip events that start after this instant.
    pub time_max: Option<DateTime<Utc>>,
    /// Full sync only: also return deleted events. Incremental syncs always
    /// report deletions (they are sent with `showDeleted=true`).
    pub show_deleted: bool,
    /// Events per page (1–2500, default 2500).
    pub page_size: u32,
    /// Restrict to these event types; empty means all types.
    pub event_types: Vec<EventType>,
}

impl Default for EventSyncOptions {
    fn default() -> Self {
        EventSyncOptions {
            sync_token: None,
            time_min: None,
            time_max: None,
            show_deleted: false,
            page_size: MAX_PAGE_SIZE,
            event_types: Vec::new(),
        }
    }
}

impl EventSyncOptions {
    /// A full sync of all events.
    pub fn full() -> Self {
        EventSyncOptions::default()
    }

    /// An incremental sync continuing from a stored `nextSyncToken`.
    pub fn incremental(sync_token: impl Into<String>) -> Self {
        EventSyncOptions {
            sync_token: Some(sync_token.into()),
            ..EventSyncOptions::default()
        }
    }

    /// Limits a full sync to events ending after `time_min`.
    pub fn time_min(mut self, time_min: DateTime<Utc>) -> Self {
        self.time_min = Some(time_min);
        self
    }

    /// Limits a full sync to events starting before `time_max`.
    pub fn time_max(mut self, time_max: DateTime<Utc>) -> Self {
        self.time_max = Some(time_max);
        self
    }

    /// True for an incremental sync.
    pub fn is_incremental(&self) -> bool {
        self.sync_token.is_some()
    }
}

/// One page of an event synchronisation run.
///
/// Persist each page as it arrives: apply `events` (upserts),
/// `cancelled_occurrences` (exclusions from a series) and
/// `deleted_event_ids`. `next_sync_token` is only set on the last page;
/// store it once that page is persisted, it starts the next incremental sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventPage {
    /// New or changed events: single events, series masters and modified
    /// occurrences (with `event.recurrence_id`).
    pub events: Vec<GoogleEvent>,
    /// Cancelled occurrences of recurring series (`EXDATE` information).
    pub cancelled_occurrences: Vec<CancelledOccurrence>,
    /// Google ids of deleted events. A deleted series master removes the
    /// whole series, including overrides stored under its id.
    pub deleted_event_ids: Vec<String>,
    /// Items that could not be mapped (logged and skipped).
    pub skipped: Vec<SkippedItem>,
    /// Token of the next page, if any.
    pub next_page_token: Option<String>,
    /// Token for the next incremental sync (last page only).
    pub next_sync_token: Option<String>,
    /// The calendar's time zone.
    pub time_zone: Option<String>,
    /// The user's access role on the calendar.
    pub access_role: Option<AccessRole>,
    /// The user's default reminders for the calendar.
    pub default_reminders: Vec<Reminder>,
}

impl EventPage {
    /// Decodes an `events.list` (or `events.instances`) response body.
    pub fn from_json(body: &[u8], palette: Option<Arc<ColorPalette>>) -> Result<Self> {
        let list: wire::EventList =
            serde_json::from_slice(body).map_err(|source| Error::Decode {
                context: "events.list",
                source,
            })?;
        let default_reminders: Vec<Reminder> = list
            .default_reminders
            .iter()
            .filter_map(map_reminder)
            .collect();
        let context = MappingContext {
            default_reminders: default_reminders.clone(),
            palette,
        };
        let mut page = EventPage {
            next_page_token: list.next_page_token.filter(|t| !t.is_empty()),
            next_sync_token: list.next_sync_token.filter(|t| !t.is_empty()),
            time_zone: list.time_zone,
            access_role: list.access_role.as_deref().map(AccessRole::parse),
            default_reminders,
            ..EventPage::default()
        };
        for item in list.items {
            let id = item.get("id").and_then(|v| v.as_str()).map(str::to_owned);
            let change = wire::Event::deserialize(&item)
                .map_err(|e| e.to_string())
                .and_then(|event| classify(event, &context));
            match change {
                Ok(EventChange::Upsert(event)) => page.events.push(*event),
                Ok(EventChange::CancelledOccurrence(occurrence)) => {
                    page.cancelled_occurrences.push(occurrence);
                }
                Ok(EventChange::Deleted(id)) => page.deleted_event_ids.push(id),
                Err(reason) => {
                    tracing::warn!(id = id.as_deref().unwrap_or_default(), %reason, "skipping event");
                    page.skipped.push(SkippedItem { id, reason });
                }
            }
        }
        Ok(page)
    }
}

/// Iterates over the pages of an event synchronisation run.
///
/// Created by [`GoogleClient::event_sync`]. On [`Error::SyncTokenExpired`],
/// wipe the calendar's local events and start a new full sync.
#[derive(Debug, Clone)]
pub struct EventSync {
    client: GoogleClient,
    calendar_id: String,
    options: EventSyncOptions,
    next_page: Option<Option<String>>,
}

impl EventSync {
    /// Resumes an interrupted run at a stored `next_page_token` (the options
    /// must be the same as for the interrupted run).
    pub fn resume_at(mut self, page_token: impl Into<String>) -> Self {
        self.next_page = Some(Some(page_token.into()));
        self
    }

    /// The options of this run.
    pub fn options(&self) -> &EventSyncOptions {
        &self.options
    }

    /// Fetches the next page; `Ok(None)` once the last page was returned.
    pub async fn next_page(&mut self) -> Result<Option<EventPage>> {
        let Some(page_token) = self.next_page.clone() else {
            return Ok(None);
        };
        let page = self
            .client
            .events_page(&self.calendar_id, &self.options, page_token.as_deref())
            .await?;
        ensure_page_progress(
            page_token.as_deref(),
            page.next_page_token.as_deref(),
            "events.list",
        )?;
        self.next_page = page.next_page_token.clone().map(Some);
        Ok(Some(page))
    }
}

impl GoogleClient {
    /// Starts a synchronisation run over a calendar's events.
    pub fn event_sync(
        &self,
        calendar_id: impl Into<String>,
        options: EventSyncOptions,
    ) -> EventSync {
        EventSync {
            client: self.clone(),
            calendar_id: calendar_id.into(),
            options,
            next_page: Some(None),
        }
    }

    /// Fetches a single page of `events.list` for a synchronisation run.
    ///
    /// Use [`GoogleClient::event_sync`] to iterate; this lower-level call is
    /// useful when the caller drives paging itself.
    pub async fn events_page(
        &self,
        calendar_id: &str,
        options: &EventSyncOptions,
        page_token: Option<&str>,
    ) -> Result<EventPage> {
        let palette = self.colors().await?;
        let mut request = ApiRequest::get(Service::Calendar, &["calendars", calendar_id, "events"])
            .query("singleEvents", "false")
            .query(
                "maxResults",
                options.page_size.clamp(1, MAX_PAGE_SIZE).to_string(),
            )
            .sync_request();
        request = match &options.sync_token {
            Some(token) => request
                .query("syncToken", token.as_str())
                .query("showDeleted", "true"),
            None => request
                .query_opt("timeMin", options.time_min.as_ref().map(time::rfc3339))
                .query_opt("timeMax", options.time_max.as_ref().map(time::rfc3339))
                .query_if(options.show_deleted, "showDeleted", "true"),
        };
        for event_type in &options.event_types {
            request = request.query("eventTypes", event_type.as_str());
        }
        request = request.query_opt("pageToken", page_token);
        let body = self.execute(&request).await?;
        let page = EventPage::from_json(&body, Some(palette))?;
        self.remember_default_reminders(calendar_id, &page.default_reminders);
        Ok(page)
    }
}
