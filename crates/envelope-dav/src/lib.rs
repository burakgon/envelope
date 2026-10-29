//! CalDAV ([RFC 4791]), CardDAV ([RFC 6352]) and WebDAV collection
//! synchronisation ([RFC 6578]) client for Envelope.
//!
//! The crate is a transport layer. It discovers accounts, lists calendars and
//! address books, keeps them in sync and reads or writes individual
//! resources, but it never interprets iCalendar or vCard data: objects travel
//! as raw `text/calendar` / `text/vcard` strings, to be parsed by
//! `envelope-ical`.
//!
//! # Typical flow
//!
//! ```no_run
//! use envelope_dav::{Auth, DavClient, Precondition, ServiceType};
//!
//! # async fn run() -> Result<(), envelope_dav::DavError> {
//! let client = DavClient::new("https://cloud.example.com", Auth::basic("jane", "app-password"))?;
//!
//! // 1. Discovery: principal, home sets and the user's e-mail addresses.
//! let account = client.discover(ServiceType::CalDav).await?;
//!
//! // 2. Collections.
//! let calendars = client.list_calendars(&account.calendar_home_set[0]).await?;
//! let calendar = &calendars[0];
//!
//! // 3. Initial sync: every member is reported as changed...
//! let delta = client.sync_collection(&calendar.href, None).await?;
//! let hrefs: Vec<&str> = delta.changed.iter().map(|o| o.href.as_str()).collect();
//! // ...and fetched in batches.
//! let fetched = client.multiget(&calendar.href, &hrefs).await?;
//! for object in &fetched.objects {
//!     println!("{} {:?} {} bytes", object.href, object.etag, object.data.len());
//! }
//!
//! // 4. Later syncs only report what changed since the stored token.
//! match client.sync_collection(&calendar.href, delta.sync_token.as_deref()).await {
//!     Ok(changes) => { /* apply changes.changed / changes.removed */ }
//!     Err(err) if err.requires_full_resync() => { /* sync again with `None` */ }
//!     Err(err) => return Err(err),
//! }
//!
//! // 5. Writes are conditional on entity tags.
//! let href = envelope_dav::object_href(&calendar.href, "event-uid", envelope_dav::ObjectKind::Calendar)?;
//! let etag = client.put(&href, "BEGIN:VCALENDAR\r\n...", Precondition::IfNoneMatch).await?;
//! client.delete(&href, etag.as_deref()).await?;
//! # Ok(()) }
//! ```
//!
//! # Hrefs
//!
//! Every href returned by the client is an absolute URL, normalised so that
//! the different spellings servers use for one resource (`%40` or `@`,
//! explicit default ports, relative paths, ...) compare equal as strings; see
//! [`normalize_href`]. Methods accept absolute URLs on any host (iCloud keeps
//! calendars on `pNN-caldav.icloud.com`) or paths, which are resolved against
//! the client's base URL.
//!
//! # Authentication
//!
//! [`Auth::Basic`] sends username and password as HTTP Basic credentials and
//! switches to HTTP Digest automatically when a server asks for it.
//! [`Auth::Bearer`] takes OAuth 2 tokens from a [`TokenSource`], which is told
//! to [`invalidate`](TokenSource::invalidate) a rejected token before the
//! request is retried once. See [`KNOWN_PROVIDERS`] for the endpoints and
//! credential types of hosted services.
//!
//! # Testing against a real server
//!
//! Besides unit tests, `tests/radicale.rs` exercises the whole API against a
//! live [Radicale](https://radicale.org) 3 server. The tests are skipped
//! unless `ENVELOPE_TEST_RADICALE_URL` points at a server started with
//! `--auth-type none`; `scripts/radicale-tests.sh` (in this crate) starts a
//! throwaway instance, runs them and stops it again:
//!
//! ```text
//! crates/envelope-dav/scripts/radicale-tests.sh
//! # or, against a server you started yourself:
//! ENVELOPE_TEST_RADICALE_URL=http://127.0.0.1:15232/ cargo test -p envelope-dav --test radicale
//! ```
//!
//! [RFC 4791]: https://www.rfc-editor.org/rfc/rfc4791
//! [RFC 6352]: https://www.rfc-editor.org/rfc/rfc6352
//! [RFC 6578]: https://www.rfc-editor.org/rfc/rfc6578

#![warn(missing_docs)]

mod auth;
mod caldav;
mod carddav;
mod client;
mod collection;
mod discovery;
mod error;
mod href;
mod multistatus;
mod object;
mod props;
mod providers;
mod sync;
mod util;
mod xml;

pub use auth::{Auth, TokenSource};
pub use caldav::{Calendar, CalendarPatch, ComponentSet, NewCalendar};
pub use carddav::{AddressBook, AddressBookPatch, NewAddressBook};
pub use client::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_TIMEOUT, DEFAULT_USER_AGENT, DavClient, DavClientBuilder,
};
pub use discovery::{Discovery, ServiceType};
pub use error::{DavError, Result};
pub use href::{ObjectKind, normalize_href, object_href, resource_name};
pub use object::{DavObject, MULTIGET_BATCH_SIZE, MultigetResult, ObjectRef, Precondition};
pub use providers::{
    KNOWN_PROVIDERS, Provider, ProviderAuth, SELF_HOSTED_SERVERS, ServerPaths, provider_for_email,
    provider_for_host,
};
pub use sync::SyncDelta;
pub use url::Url;

#[cfg(test)]
mod send_bounds {
    //! Compile-time check that every public future is `Send`, so callers can
    //! spawn them on a multi-threaded runtime.

    use chrono::Utc;

    use super::*;

    fn assert_send<T: Send>(_: T) {}

    #[allow(dead_code)]
    fn public_futures_are_send(client: &DavClient, hrefs: &[String], strs: &[&str]) {
        assert_send(client.discover(ServiceType::CalDav));
        assert_send(client.discover_from("https://example.com", ServiceType::CardDav));
        assert_send(client.list_calendars("h"));
        assert_send(client.get_calendar("h"));
        assert_send(client.create_calendar("h", &NewCalendar::new("x")));
        assert_send(client.update_calendar("h", &CalendarPatch::default()));
        assert_send(client.delete_collection("h"));
        assert_send(client.list_addressbooks("h"));
        assert_send(client.get_addressbook("h"));
        assert_send(client.create_addressbook("h", &NewAddressBook::new("x")));
        assert_send(client.update_addressbook("h", &AddressBookPatch::default()));
        assert_send(client.sync_collection("h", None));
        assert_send(client.list_etags("h"));
        assert_send(client.list_todo_etags("h"));
        assert_send(client.multiget("h", hrefs));
        assert_send(client.multiget("h", strs));
        assert_send(client.addressbook_multiget("h", hrefs));
        assert_send(client.query_time_range("h", Utc::now(), Utc::now()));
        assert_send(client.get("h"));
        assert_send(client.get_etag("h"));
        assert_send(client.put("h", "b", Precondition::None));
        assert_send(client.delete("h", None));
    }

    #[test]
    fn public_types_are_thread_safe() {
        fn shareable<T: Send + Sync + Clone + 'static>() {}
        fn sendable<T: Send + Sync + 'static>() {}
        shareable::<DavClient>();
        shareable::<Auth>();
        shareable::<Calendar>();
        shareable::<SyncDelta>();
        sendable::<DavError>();
    }
}
