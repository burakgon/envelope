//! Envelope's engine: local storage, synchronization, sending, search and the
//! glue between protocol crates and the user interface.

// Database rows travel as tuples and sync steps take many collaborators; both
// read better inline than wrapped in one-off types.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod ai;
pub mod calendar;
pub mod compose;
pub mod contacts;
pub mod db;
pub mod engine;
pub mod error;
pub mod mail_api;
pub mod mime;
pub mod model;
pub mod oauth;
pub mod providers;
pub mod rules;
pub mod search;
pub mod secrets;
pub mod smtp;
pub mod sync;

pub use engine::{AccountPatch, Engine, EventSink, Paths};
pub use error::{Error, Result};
pub use mail_api::UndoToken;

pub use envelope_ical;
