//! OAuth 2.0 scopes used by this crate.
//!
//! Request [`ALL`] (plus [`MAIL`] for IMAP/SMTP access over XOAUTH2) when
//! authorising a Google account.

/// `https://www.googleapis.com/auth/calendar` — see, edit, share and delete
/// all calendars the user can access.
///
/// Needed for the calendar list, colors, event sync and every event write
/// (insert, patch, update, delete, move, RSVP, quick add), instance listing
/// and free/busy queries. The narrower `calendar.events` scope cannot read
/// the calendar list, so the full scope is requested.
pub const CALENDAR: &str = "https://www.googleapis.com/auth/calendar";

/// `https://www.googleapis.com/auth/contacts.readonly` — read the user's
/// contacts.
///
/// Needed for `people/me/connections` (contact sync) and
/// `people:searchContacts` (address autocomplete).
pub const CONTACTS_READONLY: &str = "https://www.googleapis.com/auth/contacts.readonly";

/// `https://www.googleapis.com/auth/contacts.other.readonly` — read the
/// user's "Other contacts", the addresses Gmail collects automatically from
/// people the user interacted with.
///
/// Needed for `otherContacts` (sync) and `otherContacts:search`
/// (autocomplete of people who are not saved contacts).
pub const CONTACTS_OTHER_READONLY: &str = "https://www.googleapis.com/auth/contacts.other.readonly";

/// `https://www.googleapis.com/auth/gmail.settings.basic` — manage basic
/// Gmail settings.
///
/// Needed to list send-as aliases (`users/me/settings/sendAs`) with their
/// display names, reply-to addresses and HTML signatures.
pub const GMAIL_SETTINGS_BASIC: &str = "https://www.googleapis.com/auth/gmail.settings.basic";

/// `https://mail.google.com/` — full mailbox access.
///
/// Not used by this crate's own requests except `users/me/profile`, which
/// Google does not allow with [`GMAIL_SETTINGS_BASIC`] alone. Envelope
/// requests it anyway for IMAP and SMTP over XOAUTH2, so the profile endpoint
/// works with the same token.
pub const MAIL: &str = "https://mail.google.com/";

/// Every scope this crate's Calendar, People and send-as requests need.
pub const ALL: [&str; 4] = [
    CALENDAR,
    CONTACTS_READONLY,
    CONTACTS_OTHER_READONLY,
    GMAIL_SETTINGS_BASIC,
];
