//! Microsoft Graph v1.0 client for Outlook.com and Microsoft 365 accounts.
//!
//! The crate covers what a mail and calendar client needs from Graph:
//!
//! * **Core HTTP** — [`GraphClient`] with token refresh on `401`, retries with
//!   backoff for `429`/`503`/`504` honoring `Retry-After`, typed
//!   [`GraphError`]s, paging helpers and JSON [`batch`](GraphClient::batch)ing.
//! * **Profile & settings** — [`GraphClient::me`], mailbox time zone and
//!   automatic replies.
//! * **Mail** — the folder tree with well-known roles, per-folder message
//!   delta sync ([`GraphClient::message_delta`]), listing, search, raw MIME,
//!   flags/read state/categories (single and batched), move/copy/delete,
//!   sending MIME and MIME drafts, and master categories.
//! * **Calendar** — calendars, calendar-view sync (delta for the primary
//!   calendar, paging for the others), event CRUD, meeting responses,
//!   instances and free/busy. Events are mapped onto the provider-neutral
//!   [`envelope_ical::Event`], with Graph specifics kept in [`GraphEvent`].
//! * **Contacts** — contact paging and delta plus people search, mapped onto
//!   [`envelope_ical::Contact`].
//!
//! All requests go to `/me`, the signed-in user.

#![warn(missing_docs)]

mod auth;
mod batch;
mod calendar;
mod categories;
mod client;
mod contacts;
mod delta;
mod error;
mod event;
mod folders;
pub mod html;
pub mod ical_uid;
mod messages;
mod profile;
pub mod recurrence;
mod serde_util;
mod types;

pub use auth::{StaticToken, TokenSource};
pub use batch::{BatchRequest, BatchResponse, MAX_BATCH_SIZE};
pub use calendar::{
    Calendar, CalendarColor, EventResponse, FreeBusyError, ScheduleInformation, ScheduleItem,
    TimeWindow,
};
pub use categories::{CategoryColor, OutlookCategory};
pub use client::{
    GRAPH_V1_BASE_URL, GraphClient, GraphClientBuilder, GraphRequest, GraphResponse, NationalCloud,
    Page, RetryPolicy, TimeZoneResolver, resolve_iana_time_zone,
};
pub use contacts::ContactFolder;
pub use delta::{DeltaCursor, DeltaPage};
pub use error::{ApiError, GraphError, RecurrenceError};
pub use event::{
    Attendee as GraphAttendee, AttendeeType, EventPatch, EventType, EventWriteOptions,
    FreeBusyStatus, GraphEvent, Location, LocationType, OnlineMeetingInfo, OnlineMeetingProvider,
    ResponseStatus, ResponseType, Sensitivity,
};
pub use folders::{FolderRole, MailFolder};
pub use messages::{
    FlagStatus, FollowupFlag, InferenceClassification, InternetMessageHeader, MESSAGE_LIST_FIELDS,
    Message, MessageDeltaOptions, MessageListOptions, MessageUpdate,
};
pub use profile::{
    AutomaticRepliesSetting, AutomaticRepliesStatus, ExternalAudienceScope, LocaleInfo,
    MailboxSettings, UserProfile,
};
pub use recurrence::{
    DayOfWeek, PatternedRecurrence, RecurrencePattern, RecurrencePatternType, RecurrenceRange,
    RecurrenceRangeType, WeekIndex,
};
pub use types::{BodyType, DateTimeTimeZone, EmailAddress, Importance, ItemBody, Recipient};
