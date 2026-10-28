//! Error type shared by the whole crate.
//!
//! Parsing is deliberately tolerant and never fails on malformed input; errors
//! are only reported when an operation cannot be carried out at all (no
//! calendar data, the event to edit does not exist, …).

/// Errors returned by envelope-ical.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The input did not contain any iCalendar data (`VCALENDAR` or a bare `VEVENT`).
    #[error("no iCalendar data found")]
    NoCalendar,
    /// The input did not contain any vCard.
    #[error("no vCard found")]
    NoVCard,
    /// No `VEVENT` with the given UID (and recurrence id, when one was requested) exists.
    #[error("no event with UID `{uid}`{}", recurrence_suffix(.recurrence_id))]
    EventNotFound {
        /// The UID that was looked up.
        uid: String,
        /// The recurrence id that was looked up, in iCalendar notation.
        recurrence_id: Option<String>,
    },
    /// The operation needs a recurring series, but the event does not recur.
    #[error("event `{uid}` is not a recurring series")]
    NotRecurring {
        /// UID of the event.
        uid: String,
    },
    /// "This and following" was requested at (or before) the first occurrence,
    /// which would leave the series empty. Edit or delete the whole series instead.
    #[error("cannot truncate series `{uid}` at or before its first occurrence")]
    TruncateAtSeriesStart {
        /// UID of the series.
        uid: String,
    },
    /// The event cannot be written because it has no UID.
    #[error("event has no UID")]
    MissingUid,
    /// An iTIP message was requested for an empty list of events.
    #[error("no events given")]
    NoEvents,
}

fn recurrence_suffix(recurrence_id: &Option<String>) -> String {
    match recurrence_id {
        Some(rid) => format!(" and RECURRENCE-ID {rid}"),
        None => String::new(),
    }
}

/// Convenience alias for results carrying an [`enum@Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_readable() {
        let e = Error::EventNotFound {
            uid: "abc".into(),
            recurrence_id: Some("20240101T090000Z".into()),
        };
        assert_eq!(
            e.to_string(),
            "no event with UID `abc` and RECURRENCE-ID 20240101T090000Z"
        );
        let e = Error::EventNotFound {
            uid: "abc".into(),
            recurrence_id: None,
        };
        assert_eq!(e.to_string(), "no event with UID `abc`");
    }
}
