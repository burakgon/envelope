//! Envelope's engine: local storage, synchronization, sending, search and the
//! glue between protocol crates and the user interface.

pub mod ai;
pub mod compose;
pub mod db;
pub mod engine;
pub mod error;
pub mod mail_api;
pub mod mime;
pub mod model;
pub mod oauth;
pub mod providers;
pub mod search;
pub mod secrets;
pub mod smtp;
pub mod sync;

pub use engine::{AccountPatch, Engine, EventSink, Paths};
pub use error::{Error, Result};
pub use mail_api::UndoToken;
