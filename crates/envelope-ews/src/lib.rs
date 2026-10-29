//! Exchange Web Services (EWS) client for on-premises Exchange (2013,
//! 2016, 2019, Subscription Edition), where IMAP is often disabled, and
//! for Exchange Online.
//!
//! The crate is the protocol layer only: it speaks SOAP to the EWS
//! endpoint and returns plain structs. It covers what a mail and calendar
//! client needs:
//!
//! * **Transport** — [`EwsClient`] with HTTP Basic, NTLMv2 (with message
//!   integrity code and TLS channel bindings) or OAuth bearer
//!   authentication ([`Auth`]), the `RequestServerVersion` header
//!   ([`ExchangeVersion`], default Exchange 2013 SP1), impersonation and
//!   `X-AnchorMailbox`, retries for `ErrorServerBusy` (honoring
//!   `BackOffMilliseconds`), HTTP 429/503 and connection failures
//!   ([`RetryPolicy`]), and typed errors for HTTP failures, SOAP faults and
//!   per-item response codes ([`EwsError`], [`ResponseCode`]).
//! * **Autodiscover** — [`Autodiscover`] finds the EWS URL and server
//!   version with POX Autodiscover (including redirects, the HTTP redirect
//!   method and DNS SRV records) and Autodiscover v2 (JSON).
//! * **Folders** — the folder tree with well-known roles
//!   ([`EwsClient::folder_tree`]), hierarchy sync
//!   ([`EwsClient::sync_folder_hierarchy`]) and folder management.
//! * **Mail** — item sync ([`EwsClient::sync_folder_items`]), paged
//!   newest-first listing and server-side search, batched fetching with
//!   MIME content, read state / flags / categories, move / copy / delete,
//!   sending and saving MIME messages, attachments.
//! * **Calendar** — calendar views with expanded occurrences, calendar
//!   items with their iCalendar MIME content, creating / updating /
//!   deleting events with invitations and cancellations, meeting
//!   responses, recurrence patterns ([`Recurrence`], convertible to and
//!   from `RRULE`) and Windows ↔ IANA time zones ([`timezone`]).
//! * **Contacts** — contact folder paging and name resolution for address
//!   autocompletion.
//! * **Tasks** — the Tasks folder and the task folders below it, task sync
//!   ([`EwsClient::sync_tasks`] for the changes, [`EwsClient::get_tasks`]
//!   for the fields) and creating, changing and deleting tasks, with their
//!   dates kept as days in the user's time zone.
//! * **Notifications** — streaming subscriptions
//!   ([`EwsClient::get_streaming_events`]). Polling with
//!   `SyncFolderItems` stays the source of truth; notifications only
//!   trigger it.
//!
//! # Example
//!
//! ```no_run
//! use envelope_ews::{
//!     Auth, Autodiscover, DistinguishedFolder, EwsClient, GetMessagesOptions, ItemChange,
//!     SyncOptions,
//! };
//!
//! # async fn run() -> Result<(), envelope_ews::EwsError> {
//! let auth = Auth::ntlm("CORP\\jane", "secret");
//! let found = Autodiscover::new(auth.clone())?.discover("jane@example.com").await?;
//! let client = EwsClient::builder(&found.ews_url, auth)
//!     .server_version(found.server_version.unwrap_or_default())
//!     .anchor_mailbox("jane@example.com")
//!     .build()?;
//!
//! // Mirror the inbox: the first rounds report every message as created.
//! let mut state: Option<String> = None;
//! loop {
//!     let page = client
//!         .sync_folder_items(DistinguishedFolder::Inbox, state.as_deref(), &SyncOptions::default())
//!         .await?;
//!     let created: Vec<_> = page
//!         .changes
//!         .iter()
//!         .filter_map(|change| match change {
//!             ItemChange::Created(item) => Some(item.id.clone()),
//!             _ => None,
//!         })
//!         .collect();
//!     let options = GetMessagesOptions { include_mime_content: true, ..Default::default() };
//!     for message in client.get_messages(&created, &options).await? {
//!         let message = message?;
//!         println!("{:?}: {} bytes of MIME", message.subject, message.mime_content.map_or(0, |m| m.len()));
//!     }
//!     state = Some(page.sync_state);
//!     if page.includes_last_item_in_range {
//!         break;
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! # Ids
//!
//! Item and folder ids are opaque strings with a change key
//! ([`ItemId`], [`FolderId`]). Moving an item to another folder changes
//! its id; [`EwsClient::move_items`] returns the new one.

#![warn(missing_docs)]

mod auth;
pub mod autodiscover;
mod availability;
mod calendar;
mod client;
mod contacts;
mod error;
mod folders;
mod items;
mod messages;
mod notifications;
mod ntlm;
mod oof;
pub mod recurrence;
mod soap;
mod tasks;
pub mod timezone;
mod transport;
mod types;
mod version;
mod xml;

pub use auth::{Auth, StaticToken, TokenSource};
pub use autodiscover::{
    Autodiscover, AutodiscoverBuilder, AutodiscoverEndpoint, AutodiscoverOptions,
    AutodiscoverResult,
};
pub use availability::{BusyKind, BusyPeriod};
pub use calendar::{
    Attendee, CalendarItem, CalendarItemType, CalendarItemUpdate, EventTiming, FreeBusyStatus,
    GetCalendarItemsOptions, MeetingResponse, NewCalendarItem, OccurrenceInfo, ResponseType,
    SendInvitations, SendUpdates, calendar_detail_properties, calendar_summary_properties,
};
pub use client::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_NTLM_CONNECTIONS, DEFAULT_TIMEOUT, DEFAULT_USER_AGENT,
    EWS_PATH, EwsClient, EwsClientBuilder, RetryPolicy,
};
pub use contacts::{
    Contact, ContactChange, ContactEmail, ContactPhone, ContactText, NewContact, ResolveScope,
    ResolvedName,
};
pub use error::{EwsError, ResponseCode, ResponseError, Result, SoapFault};
pub use folders::{
    DeleteType, FOLDER_CLASS_CALENDAR, FOLDER_CLASS_CONTACTS, FOLDER_CLASS_MAIL,
    FOLDER_CLASS_TASKS, Folder, FolderChange, FolderKind, FolderRole, FolderSyncPage,
    FolderTraversal,
};
pub use items::{
    GET_ITEM_BATCH_SIZE, GET_ITEM_MIME_BATCH_SIZE, ItemChange, ItemPage, ItemSyncPage,
    MAX_SYNC_CHANGES, SendCancellations, SyncOptions, WRITE_BATCH_SIZE,
};
pub use messages::{
    Attachment, AttachmentInfo, AttachmentKind, FindMessagesOptions, Flag, FlagStatus,
    GetMessagesOptions, MAX_FIND_PAGE_SIZE, Message, MessageUpdate, MimeDisposition,
    message_detail_properties, message_summary_properties,
};
pub use notifications::{
    Notification, NotificationEvent, NotificationEventType, StreamingEvents, StreamingMessage,
};
pub use oof::{ExternalAudience, OofSettings, OofState};
pub use recurrence::{DayOfWeek, DayOfWeekIndex, Recurrence, RecurrencePattern, RecurrenceRange};
pub use tasks::{NewTask, Task, TaskStatus, TaskSyncPage, TaskUpdate, task_properties};
pub use types::{
    BaseShape, Body, BodyType, DistinguishedFolder, FolderId, FolderRef, Importance, ItemId,
    ItemKind, ItemRef, ItemShape, Mailbox, PropertyPath, Sensitivity, format_datetime, parse_date,
    parse_datetime,
};
pub use url::Url;
pub use version::{ExchangeVersion, ServerVersionInfo};

#[cfg(test)]
mod send_bounds {
    //! Compile-time check that every public future is `Send`, so callers can
    //! spawn them on a multi-threaded runtime.

    use chrono::Utc;

    use super::*;

    fn assert_send<T: Send>(_: T) {}

    #[allow(dead_code)]
    fn public_futures_are_send(client: &EwsClient, ids: &[ItemId], autodiscover: &Autodiscover) {
        let folder = FolderId::new("f");
        assert_send(client.get_folder(DistinguishedFolder::Inbox));
        assert_send(client.get_folders(&[]));
        assert_send(client.find_folders(DistinguishedFolder::Root, FolderTraversal::Deep));
        assert_send(client.folder_tree());
        assert_send(client.well_known_folder_ids());
        assert_send(client.sync_folder_hierarchy(None));
        assert_send(client.create_folder(DistinguishedFolder::Inbox, "x", FOLDER_CLASS_MAIL));
        assert_send(client.rename_folder(&folder, "y"));
        assert_send(client.delete_folder(folder.clone(), DeleteType::HardDelete));
        assert_send(client.move_folder(&folder, DistinguishedFolder::Inbox));
        assert_send(client.empty_folder(folder.clone(), DeleteType::HardDelete, false));
        assert_send(client.sync_folder_items(folder.clone(), None, &SyncOptions::default()));
        assert_send(client.find_messages(folder.clone(), &FindMessagesOptions::default()));
        assert_send(client.search_messages(&[], "x", 0, 10));
        assert_send(client.get_messages(ids, &GetMessagesOptions::default()));
        assert_send(client.get_mime_content(&ids[0]));
        assert_send(client.update_messages(&[]));
        assert_send(client.set_read(ids, true));
        assert_send(client.move_items(ids, DistinguishedFolder::Inbox));
        assert_send(client.copy_items(ids, DistinguishedFolder::Inbox));
        assert_send(client.delete_items(ids, DeleteType::SoftDelete));
        assert_send(client.send_mime(b"x"));
        assert_send(client.save_draft_mime(b"x"));
        assert_send(client.send_draft(&ids[0]));
        assert_send(client.get_attachments(&["a"]));
        assert_send(client.mark_all_read(DistinguishedFolder::Inbox, true));
        assert_send(client.calendar_view(DistinguishedFolder::Calendar, Utc::now(), Utc::now()));
        assert_send(client.get_calendar_items(&[], &GetCalendarItemsOptions::default()));
        assert_send(client.delete_calendar_item(
            ids[0].clone(),
            DeleteType::MoveToDeletedItems,
            SendCancellations::SendToNone,
        ));
        assert_send(client.respond_to_meeting(&ids[0], MeetingResponse::Accept, None, true));
        assert_send(client.find_contacts(DistinguishedFolder::Contacts, 0, 10));
        assert_send(client.task_folders());
        assert_send(client.sync_tasks(DistinguishedFolder::Tasks, None, 512));
        assert_send(client.get_tasks(ids, chrono_tz::Tz::UTC));
        assert_send(client.get_task(&ids[0], chrono_tz::Tz::UTC));
        assert_send(client.create_task(
            DistinguishedFolder::Tasks,
            &NewTask::default(),
            chrono_tz::Tz::UTC,
        ));
        assert_send(client.update_task(&ids[0], &TaskUpdate::default(), chrono_tz::Tz::UTC));
        assert_send(client.delete_task(&ids[0], DeleteType::MoveToDeletedItems));
        assert_send(client.resolve_names("x", ResolveScope::ActiveDirectory, false));
        assert_send(client.subscribe_streaming(&[], &[]));
        assert_send(client.get_streaming_events(&[], 1));
        assert_send(client.unsubscribe("s"));
        assert_send(client.execute_raw("<m:x/>", true));
        assert_send(autodiscover.discover("a@example.com"));
    }

    #[test]
    fn public_types_are_thread_safe() {
        fn shareable<T: Send + Sync + Clone + 'static>() {}
        fn sendable<T: Send + Sync + 'static>() {}
        shareable::<EwsClient>();
        shareable::<Auth>();
        shareable::<Message>();
        shareable::<CalendarItem>();
        shareable::<Task>();
        shareable::<Folder>();
        sendable::<EwsError>();
        sendable::<Autodiscover>();
        sendable::<StreamingEvents>();
    }
}
