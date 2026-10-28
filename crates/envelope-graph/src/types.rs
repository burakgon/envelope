//! Wire types shared by several Graph resources.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::serde_util::graph_enum;

/// An e-mail address with an optional display name (Graph `emailAddress`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAddress {
    /// Display name, e.g. `Adele Vance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// SMTP address, e.g. `AdeleV@contoso.com`. Graph omits it for some
    /// senders (for example legacy X.500 addresses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// A message recipient, sender or meeting organizer (Graph `recipient`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipient {
    /// The recipient's address and display name.
    #[serde(default)]
    pub email_address: EmailAddress,
}

impl Recipient {
    /// Creates a recipient from an address and optional display name.
    pub fn new(address: impl Into<String>, name: Option<String>) -> Self {
        Recipient {
            email_address: EmailAddress {
                name,
                address: Some(address.into()),
            },
        }
    }

    /// The SMTP address, if Graph provided one.
    pub fn address(&self) -> Option<&str> {
        self.email_address.address.as_deref()
    }

    /// The display name, if any.
    pub fn name(&self) -> Option<&str> {
        self.email_address.name.as_deref()
    }
}

/// A local date and time together with a time zone (Graph `dateTimeTimeZone`).
///
/// `date_time` is kept exactly as Graph wrote it (for example
/// `2017-04-15T12:00:00.0000000`); `time_zone` is either an IANA name or a
/// Windows time zone name such as `Pacific Standard Time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateTimeTimeZone {
    /// Wall-clock time without offset, ISO 8601.
    pub date_time: String,
    /// Time zone of `date_time`; Graph assumes UTC when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

impl DateTimeTimeZone {
    /// Builds a value from a wall-clock time and a time zone name.
    pub fn new(local: NaiveDateTime, time_zone: impl Into<String>) -> Self {
        DateTimeTimeZone {
            date_time: format_local(local),
            time_zone: Some(time_zone.into()),
        }
    }

    /// Builds a UTC value.
    pub fn utc(at: DateTime<Utc>) -> Self {
        DateTimeTimeZone::new(at.naive_utc(), "UTC")
    }

    /// Parses the wall-clock part.
    pub fn local(&self) -> Option<NaiveDateTime> {
        parse_local(&self.date_time)
    }

    /// True when the time zone denotes UTC (or is absent, which Graph treats as UTC).
    pub fn is_utc(&self) -> bool {
        self.time_zone.as_deref().is_none_or(is_utc_zone)
    }

    /// The instant, when the value is in UTC.
    pub fn to_utc(&self) -> Option<DateTime<Utc>> {
        if self.is_utc() {
            self.local().map(|local| local.and_utc())
        } else {
            None
        }
    }
}

/// True for time zone identifiers that mean UTC.
pub(crate) fn is_utc_zone(tz: &str) -> bool {
    const UTC_NAMES: &[&str] = &[
        "UTC",
        "Etc/UTC",
        "Etc/GMT",
        "Etc/Universal",
        "Etc/Zulu",
        "Z",
        "Coordinated Universal Time",
        "tzone://Microsoft/Utc",
    ];
    let tz = tz.trim();
    UTC_NAMES.iter().any(|n| tz.eq_ignore_ascii_case(n))
}

/// Formats a wall-clock time the way Graph expects it.
pub(crate) fn format_local(local: NaiveDateTime) -> String {
    local.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Parses Graph wall-clock times such as `2017-04-15T12:00:00.0000000`,
/// `2017-04-15T12:00:00`, `2017-04-15T12:00:00Z` or a bare date.
pub(crate) fn parse_local(value: &str) -> Option<NaiveDateTime> {
    let trimmed = value.trim();
    let trimmed = trimmed
        .strip_suffix('Z')
        .or_else(|| trimmed.strip_suffix('z'))
        .unwrap_or(trimmed);
    NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M"))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
}

graph_enum! {
    /// Content type of an item body.
    pub enum BodyType {
        /// Plain text.
        Text = "text",
        /// HTML.
        Html = "html",
    }
    unknown = Text;
}

/// The body of a message or event (Graph `itemBody`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemBody {
    /// Whether `content` is text or HTML.
    pub content_type: BodyType,
    /// The body content.
    #[serde(default)]
    pub content: String,
}

graph_enum! {
    /// Importance of a message or event.
    pub enum Importance {
        /// Low importance.
        Low = "low",
        /// Normal importance.
        Normal = "normal",
        /// High importance.
        High = "high",
    }
    unknown = Normal;
    default = Normal;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_graph_wall_clock_times() {
        let expected = NaiveDate::from_ymd_opt(2017, 4, 15).and_then(|d| d.and_hms_opt(12, 0, 0));
        assert_eq!(parse_local("2017-04-15T12:00:00.0000000"), expected);
        assert_eq!(parse_local("2017-04-15T12:00:00"), expected);
        assert_eq!(parse_local("2017-04-15T12:00:00Z"), expected);
        assert_eq!(parse_local("2017-04-15T12:00"), expected);
        assert!(parse_local("yesterday").is_none());
    }

    #[test]
    fn utc_detection() {
        let v = DateTimeTimeZone {
            date_time: "2024-01-01T08:30:00.0000000".into(),
            time_zone: Some("UTC".into()),
        };
        assert!(v.is_utc());
        assert_eq!(
            v.to_utc().map(|d| d.to_rfc3339()),
            Some("2024-01-01T08:30:00+00:00".to_owned())
        );
        let pst = DateTimeTimeZone {
            date_time: "2024-01-01T08:30:00".into(),
            time_zone: Some("Pacific Standard Time".into()),
        };
        assert!(!pst.is_utc());
        assert!(pst.to_utc().is_none());
    }

    #[test]
    fn enums_are_case_insensitive_with_fallback() {
        let v: Importance = serde_json::from_str("\"High\"").unwrap_or_default();
        assert_eq!(v, Importance::High);
        let v: Importance = serde_json::from_str("\"surprising\"").unwrap_or(Importance::Low);
        assert_eq!(v, Importance::Normal);
        assert_eq!(
            serde_json::to_string(&BodyType::Html).ok().as_deref(),
            Some("\"html\"")
        );
    }
}
