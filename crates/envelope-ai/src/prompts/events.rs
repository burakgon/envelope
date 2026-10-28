//! Extracting calendar events from an email.

use std::fmt;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::clean::{CleanOptions, TrimStyle, clean_body, truncate};
use super::format::{
    describe_datetime, first_name, parse_datetime, render_email, security_rules, timezone_label,
    user_label,
};
use super::{Budget, MailMessageCtx, UserCtx, json_list, json_str};
use crate::error::AiError;
use crate::json::extract_json;
use crate::types::ChatMessage;

/// A point in time as given by the model, validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventTime {
    /// A calendar date, for all-day events.
    Date(NaiveDate),
    /// A precise time with its UTC offset.
    DateTime(DateTime<FixedOffset>),
    /// A wall-clock time without zone information ("floating" in iCalendar
    /// terms); interpret it in the user's time zone.
    Floating(NaiveDateTime),
}

impl EventTime {
    /// Parses ISO 8601 / RFC 3339 dates and date-times, leniently (missing
    /// seconds, `Z`, space instead of `T`).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let zoned = match text.strip_suffix('Z').or_else(|| text.strip_suffix('z')) {
            Some(utc) => format!("{utc}+00:00"),
            None => text.to_owned(),
        };
        if let Ok(parsed) = DateTime::parse_from_rfc3339(&zoned) {
            return Some(Self::DateTime(parsed));
        }
        for format in [
            "%Y-%m-%dT%H:%M%:z",
            "%Y-%m-%d %H:%M:%S%.f%:z",
            "%Y-%m-%d %H:%M%:z",
        ] {
            if let Ok(parsed) = DateTime::parse_from_str(&zoned, format) {
                return Some(Self::DateTime(parsed));
            }
        }
        for format in [
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%dT%H:%M",
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%d %H:%M",
        ] {
            if let Ok(parsed) = NaiveDateTime::parse_from_str(text, format) {
                return Some(Self::Floating(parsed));
            }
        }
        NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .ok()
            .map(Self::Date)
    }

    /// The calendar date (local to the time's own offset).
    pub fn date(&self) -> NaiveDate {
        match self {
            Self::Date(date) => *date,
            Self::DateTime(time) => time.date_naive(),
            Self::Floating(time) => time.date(),
        }
    }

    /// Wall-clock time in the value's own offset (midnight for dates).
    fn local(&self) -> NaiveDateTime {
        match self {
            Self::Date(date) => date.and_time(chrono::NaiveTime::MIN),
            Self::DateTime(time) => time.naive_local(),
            Self::Floating(time) => *time,
        }
    }

    fn is_before(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::DateTime(a), Self::DateTime(b)) => a < b,
            _ => self.local() < other.local(),
        }
    }
}

impl fmt::Display for EventTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Date(date) => write!(f, "{}", date.format("%Y-%m-%d")),
            Self::DateTime(time) => write!(f, "{}", time.format("%Y-%m-%dT%H:%M:%S%:z")),
            Self::Floating(time) => write!(f, "{}", time.format("%Y-%m-%dT%H:%M:%S")),
        }
    }
}

impl Serialize for EventTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EventTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid date or time: {text}")))
    }
}

/// A calendar event found in an email.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedEvent {
    /// Short, specific title.
    pub title: String,
    /// Start: a date for all-day events, otherwise a date-time.
    pub start: EventTime,
    /// End, if stated. For all-day events this is the last day (inclusive).
    pub end: Option<EventTime>,
    /// Whether this is an all-day (date-only) event.
    pub all_day: bool,
    /// Address or meeting link.
    pub location: Option<String>,
    /// Useful context: agenda, organizer, dial-in or booking details.
    pub description: Option<String>,
    /// 0–1: how sure the model is that this is a real event with correct times.
    pub confidence: f32,
}

/// Builds a prompt that extracts calendar events from `message`, resolving
/// relative dates against the message date and times to absolute ISO 8601 with
/// UTC offsets. The answer is `{"events": [...]}` JSON.
///
/// Use with [`super::Task::ExtractEvents`] and parse with [`parse_events`].
pub fn extract_events(
    user: &UserCtx,
    message: &MailMessageCtx,
    budget: Budget,
) -> Vec<ChatMessage> {
    let who = user_label(user);
    let first = first_name(user);
    let security = security_rules(user);
    let tz = timezone_label(user);
    let now = if user.now.trim().is_empty() {
        "unknown".to_owned()
    } else {
        describe_datetime(&user.now)
    };
    let sent = if message.date.trim().is_empty() {
        "an unknown date (assume it was sent recently)".to_owned()
    } else {
        describe_datetime(&message.date)
    };
    let zone_rule = default_zone_rule(user, message);
    let system = format!(
        "You extract calendar events from an email for {who}, in Envelope, a desktop email client.

{security}

Context:
- {first}'s time zone: {tz}. Current date and time: {now}.
- The email was sent on {sent}. Resolve relative dates (\"tomorrow\", \"next Tuesday\", \"in two weeks\") relative to the date the email was sent, not to today.
- {zone_rule} When the email states a time zone (\"3 pm EST\", \"15:00 CET\"), use that zone instead, with its correct UTC offset on that date, including daylight saving time.

What to extract:
- Meetings, calls, appointments, reservations, flights and other travel, deadlines, events and invitations that {first} might want in their calendar.
- Include an event only if its date can be determined. Skip vague mentions (\"let's catch up sometime\"), events that had already ended when the email was sent, and marketing such as sales or offers that expire.
- If several alternative times are proposed and none is confirmed, include each proposal with a confidence of at most 0.5.
- If the email moves or cancels an event, output the event as it now stands; start the title with \"Cancelled:\" if it was cancelled.
- The email may quote earlier messages; prefer the newest information.

Fields of each event:
- \"title\": short and specific, e.g. \"Dentist appointment\" or \"Call with Alice about the Q3 budget\", in the language of the email.
- \"start\": ISO 8601. Timed events: date, time and UTC offset, e.g. \"2026-11-03T14:00:00+01:00\". All-day events: the date only, e.g. \"2026-11-03\".
- \"end\": same format as \"start\", or null when no end is stated. For multi-day all-day events, the last day (inclusive).
- \"all_day\": true for date-only events (birthdays, holidays, deadlines without a time, multi-day conferences), otherwise false.
- \"location\": the address or video-call link exactly as written in the email, or null.
- \"description\": one or two sentences of useful context (agenda, organizer, booking or dial-in details), or null.
- \"confidence\": a number from 0 to 1 for how sure you are that this is a real event with a correctly resolved date and time.

Output only this JSON object, with no code fences or other text:
{{\"events\": [{{\"title\": \"\u{2026}\", \"start\": \"\u{2026}\", \"end\": null, \"all_day\": false, \"location\": null, \"description\": null, \"confidence\": 0.9}}]}}
If there are no events, output {{\"events\": []}}."
    );
    let options = CleanOptions {
        strip_quotes: false,
        strip_signature: true,
    };
    let body = clean_body(&message.body_text, options);
    let body = truncate(
        &body,
        budget.max_chars.max(Budget::MIN_MAX_CHARS),
        TrimStyle::Head,
    );
    let email = render_email(1, message, &body, user);
    let user_message = format!("{email}\n\nExtract the calendar events from this email, as JSON.");
    vec![ChatMessage::system(system), ChatMessage::user(user_message)]
}

/// Which zone applies to times the email gives without a zone.
fn default_zone_rule(user: &UserCtx, message: &MailMessageCtx) -> String {
    let tz = timezone_label(user);
    let first = first_name(user);
    let sent = parse_datetime(&message.date);
    let now = parse_datetime(&user.now);
    match (sent, now) {
        (Some(sent), Some(now)) if sent.offset() == now.offset() => format!(
            "Times given without a time zone are in {tz}, {first}'s time zone (the sender uses the same UTC offset)."
        ),
        (Some(sent), _) if sent.offset().local_minus_utc() != 0 => format!(
            "Times given without a time zone are in the sender's local time, UTC{} (the offset of the email's Date header).",
            sent.format("%:z")
        ),
        _ => format!("Times given without a time zone are in {tz}, {first}'s time zone."),
    }
}

/// Parses the answer to [`extract_events`]. Events with a missing title or an
/// unparsable start are skipped; inconsistent fields are normalized (all-day
/// events use dates, ends before starts are dropped, confidence is clamped).
///
/// # Errors
///
/// [`AiError::MalformedOutput`] when the answer contains no event list.
pub fn parse_events(answer: &str) -> Result<Vec<ExtractedEvent>, AiError> {
    let value: Value = extract_json(answer)?;
    let items = json_list(&value, &["events", "calendar_events", "items"])
        .ok_or_else(|| AiError::malformed_output("expected a list of events"))?;
    let mut events: Vec<ExtractedEvent> = Vec::new();
    for item in items {
        let Some(event) = parse_event(item) else {
            continue;
        };
        if !events
            .iter()
            .any(|e| e.title == event.title && e.start == event.start)
        {
            events.push(event);
        }
    }
    Ok(events)
}

fn parse_event(item: &Value) -> Option<ExtractedEvent> {
    let title = json_str(item, &["title", "summary", "name"])?;
    let mut start = item
        .get("start")
        .and_then(Value::as_str)
        .and_then(EventTime::parse)?;
    let mut end = item
        .get("end")
        .and_then(Value::as_str)
        .and_then(EventTime::parse);
    let stated_all_day = match item.get("all_day").or_else(|| item.get("allDay")) {
        Some(Value::Bool(flag)) => Some(*flag),
        Some(Value::String(flag)) => Some(flag.eq_ignore_ascii_case("true")),
        _ => None,
    };
    let all_day = stated_all_day.unwrap_or(false) || matches!(start, EventTime::Date(_));
    if all_day {
        start = EventTime::Date(start.date());
        end = end.map(|e| EventTime::Date(e.date()));
    }
    if end.is_some_and(|e| e.is_before(&start) || (all_day && e == start)) {
        end = None;
    }
    let confidence = match item.get("confidence") {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|c| c.is_finite())
    .map_or(0.5, |c| c.clamp(0.0, 1.0)) as f32;
    Some(ExtractedEvent {
        title,
        start,
        end,
        all_day,
        location: json_str(item, &["location", "place"]),
        description: json_str(item, &["description", "details", "notes"]),
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_time_formats() {
        let cases = [
            ("2026-11-03T14:00:00+01:00", "2026-11-03T14:00:00+01:00"),
            ("2026-11-03T14:00+01:00", "2026-11-03T14:00:00+01:00"),
            ("2026-11-03T14:00:00Z", "2026-11-03T14:00:00+00:00"),
            ("2026-11-03T14:00Z", "2026-11-03T14:00:00+00:00"),
            ("2026-11-03 14:00", "2026-11-03T14:00:00"),
            ("2026-11-03T14:00:00.000", "2026-11-03T14:00:00"),
            ("2026-11-03", "2026-11-03"),
        ];
        for (input, expected) in cases {
            let parsed = EventTime::parse(input).map(|t| t.to_string());
            assert_eq!(parsed.as_deref(), Some(expected), "{input}");
        }
        assert_eq!(EventTime::parse("next Tuesday"), None);
        assert_eq!(EventTime::parse("2026-13-40"), None);
    }

    #[test]
    fn zone_rule_prefers_sender_offset() {
        let user = UserCtx {
            name: "Ada".into(),
            timezone: "Europe/Istanbul".into(),
            now: "2026-10-28T10:00:00+03:00".into(),
            ..UserCtx::default()
        };
        let mut message = MailMessageCtx {
            date: "2026-10-27T09:00:00+03:00".into(),
            ..MailMessageCtx::default()
        };
        assert!(default_zone_rule(&user, &message).contains("Europe/Istanbul"));
        message.date = "2026-10-27T09:00:00-04:00".into();
        assert!(default_zone_rule(&user, &message).contains("UTC-04:00"));
        message.date = "2026-10-27T09:00:00+00:00".into();
        assert!(default_zone_rule(&user, &message).contains("Europe/Istanbul"));
    }
}
