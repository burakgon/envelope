//! Google Calendar, People, Gmail-settings and Tasks client for Envelope.
//!
//! Mail itself is synchronised over IMAP elsewhere; this crate covers the
//! Google-specific REST APIs a mail and calendar client needs on top of that:
//!
//! * **Calendar API v3** — the calendar list (with colors and default
//!   reminders), page-by-page event synchronisation with sync tokens, event
//!   writes (insert / patch / update / delete / move / RSVP / quick add /
//!   instances, optionally creating a Google Meet conference) and free/busy
//!   queries. Events are mapped into the provider-neutral
//!   [`envelope_ical::Event`] and wrapped in a [`GoogleEvent`] that keeps the
//!   Google-only data (event id, etag, event type, conference details, …).
//! * **People API** — the user's contacts and "other contacts" (people they
//!   e-mailed) mapped into [`envelope_ical::Contact`], with incremental sync,
//!   the search endpoints used for address autocomplete, and contact writes
//!   (create, field-level update keeping what the model does not cover,
//!   delete, photos).
//! * **Gmail API settings** — send-as aliases (display names, reply-to,
//!   signatures) and the account profile.
//! * **Tasks API v1** — task lists and tasks, pulled page by page in full
//!   or incrementally ([`GoogleClient::task_changes`], with a JSON-storable
//!   [`TaskCursor`]), and task writes (insert / patch / delete / move).
//!
//! Authentication is delegated to a [`TokenSource`]; the OAuth scopes needed
//! are listed in [`scopes`]. Every request goes through one HTTP layer that
//! refreshes the token once on `401`, retries rate-limit and server errors
//! with exponential backoff, and turns Google's error payloads into typed
//! [`Error`]s (with a dedicated [`Error::SyncTokenExpired`]).
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use envelope_google::{EventSyncOptions, GoogleClient, StaticToken};
//!
//! # async fn demo() -> envelope_google::Result<()> {
//! let client = GoogleClient::new(Arc::new(StaticToken::new("access-token")))?;
//! for calendar in client.list_calendars().await? {
//!     let mut sync = client.event_sync(&calendar.id, EventSyncOptions::full());
//!     while let Some(page) = sync.next_page().await? {
//!         // Persist `page.events`, `page.cancelled_occurrences` and
//!         // `page.deleted_event_ids`, then remember the sync token (it is
//!         // only present on the last page) for the next incremental run.
//!         if let Some(token) = page.next_sync_token {
//!             println!("{}: next sync token {token}", calendar.display_name());
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod auth;
pub mod calendar;
mod client;
mod error;
pub mod gmail;
mod html;
pub mod people;
pub mod scopes;
mod serde_util;
pub mod tasks;

pub use auth::{StaticToken, TokenSource};
pub use calendar::{
    AccessRole, Attachment, AttendeeDetails, AutoDeclineSettings, BirthdayProperties, BusyPeriod,
    CalendarBusy, CancelledOccurrence, ColorDefinition, ColorPalette, Conference, EntryPoint,
    EntryPointKind, EventChange, EventPage, EventPatch, EventSync, EventSyncOptions, EventType,
    EventTypeProperties, ExtendedProperties, FreeBusy, FreeBusyRequest, GoogleCalendar,
    GoogleEvent, InstancesQuery, MappingContext, OfficeLocation, Reminder, ReminderMethod,
    SendUpdates, SkippedItem, Visibility, WorkingLocation, WriteOptions,
};
pub use client::{ClientConfig, GoogleClient, RetryPolicy};
pub use error::{ApiError, Error, Result};
pub use gmail::{GmailProfile, SendAsAlias, SmtpRelay, VacationSettings, VerificationStatus};
pub use people::{ContactPage, ContactSource, ContactSync, GoogleContact, GoogleContactGroup};
pub use tasks::{
    GoogleTask, GoogleTaskList, NewTask, TaskChanges, TaskCursor, TaskLink, TaskPage, TaskPatch,
    TaskPosition, TaskQuery, TaskStatus,
};
