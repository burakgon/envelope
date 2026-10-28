//! Google Calendar API v3: calendar list, colors, event synchronisation,
//! event writes and free/busy.
//!
//! Events are mapped into the provider-neutral [`envelope_ical::Event`] and
//! wrapped in a [`GoogleEvent`] that keeps the Google-only data. Recurring
//! events are synchronised as series masters plus exceptions
//! (`singleEvents=false`); a cancelled occurrence of a series is reported as
//! a [`CancelledOccurrence`] — the equivalent of an `EXDATE`.

mod calendars;
mod event;
mod freebusy;
mod mapping;
mod recurrence;
mod sync;
mod time;
mod wire;
mod write;

pub use calendars::{AccessRole, ColorDefinition, ColorPalette, GoogleCalendar};
pub use event::{
    Attachment, AttendeeDetails, AutoDeclineSettings, BirthdayProperties, CancelledOccurrence,
    Conference, EntryPoint, EntryPointKind, EventChange, EventType, EventTypeProperties,
    ExtendedProperties, GoogleEvent, OfficeLocation, Reminder, ReminderMethod, SkippedItem,
    Visibility, WorkingLocation,
};
pub use freebusy::{BusyPeriod, CalendarBusy, FreeBusy, FreeBusyRequest};
pub use mapping::MappingContext;
pub use sync::{EventPage, EventSync, EventSyncOptions};
pub use write::{EventPatch, InstancesQuery, SendUpdates, WriteOptions};
