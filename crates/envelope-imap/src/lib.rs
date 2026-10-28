//! A fast, async IMAP client tailored for desktop mail synchronization.
//!
//! Built on the battle-tested `imap-proto` parser with our own framing layer,
//! it supports TLS/STARTTLS, `AUTHENTICATE` (PLAIN, XOAUTH2, OAUTHBEARER),
//! CONDSTORE/QRESYNC, IDLE, MOVE, UIDPLUS, LIST-STATUS, SPECIAL-USE, UTF8=ACCEPT
//! and the Gmail extensions (X-GM-MSGID, X-GM-THRID, X-GM-LABELS, X-GM-RAW).

pub mod client;
pub mod error;
pub mod stream;
pub mod types;
pub mod utf7;

pub use client::{Auth, Client, Config, Done, IdleEnd, Part, QresyncParams, Selected};
pub use error::{Error, Result};
pub use stream::Security;
pub use types::{
    BodyPart, Code, FetchItem, ListEntry, MailboxStatus, Status, Untagged, uid_set_string,
};
