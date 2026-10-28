//! Event extraction: iCalendar text → [`Event`]s.

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::color::css_color_to_hex;
use crate::component::{Component, Property};
use crate::conference::detect_conference_url;
use crate::error::{Error, Result};
use crate::model::{
    Alarm, AlarmAction, Attendee, AttendeeRole, CalendarUserType, Event, EventClass, EventStatus,
    EventTime, Organizer, PartStat, Transparency,
};
use crate::parser::{decode_bytes, parse_components};
use crate::tz::{TzContext, wall_clock, with_wall_clock};
use crate::value::{parse_duration, property_time, property_times};

/// Options for event extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Zone used to interpret floating and all-day times where an instant is
    /// needed (absolute alarm triggers, `CREATED` without `Z`).
    pub default_tz: Tz,
    /// Rewrite TZIDs to IANA names where they can be resolved (Windows and
    /// Outlook names, vendor prefixes, `VTIMEZONE`s matching an IANA zone)
    /// and TZIDs meaning UTC to UTC values. On by default.
    pub normalize_tzids: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            default_tz: Tz::UTC,
            normalize_tzids: true,
        }
    }
}

/// A parsed iCalendar stream.
#[derive(Debug, Clone, Default)]
pub struct CalendarData {
    /// Top-level components as parsed (usually one `VCALENDAR`).
    pub components: Vec<Component>,
    /// Every `VEVENT`, in source order. Series masters and their overrides
    /// are separate events sharing a UID.
    pub events: Vec<Event>,
    /// Resolver carrying every `VTIMEZONE` of the stream. Needed to convert
    /// times whose TZID could not be mapped to an IANA zone.
    pub tz: TzContext,
    /// The iTIP `METHOD` of the first calendar, upper-cased.
    pub method: Option<String>,
}

/// Parses iCalendar text (one or several `VCALENDAR`s, or bare `VEVENT`s).
///
/// Fails only with [`Error::NoCalendar`] when the text contains no
/// calendar data at all; malformed content is tolerated.
///
/// ```
/// let data = envelope_ical::parse_calendar(
///     "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=W. Europe Standard Time:20240105T090000\r\n\
///      DURATION:PT30M\r\nSUMMARY:Stand-up\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
/// )
/// .unwrap();
/// let event = &data.events[0];
/// assert_eq!(event.summary, "Stand-up");
/// assert!(matches!(&event.start, envelope_ical::EventTime::Zoned { tzid, .. } if tzid == "Europe/Berlin"));
/// ```
pub fn parse_calendar(text: &str) -> Result<CalendarData> {
    parse_calendar_with(text, &ExtractOptions::default())
}

/// Like [`parse_calendar`] but from raw bytes (byte-level unfolding and
/// legacy charset fallback, see [`crate::parse_components_bytes`]).
pub fn parse_calendar_bytes(bytes: &[u8]) -> Result<CalendarData> {
    parse_calendar_with(&decode_bytes(bytes), &ExtractOptions::default())
}

/// [`parse_calendar`] with explicit options.
pub fn parse_calendar_with(text: &str, options: &ExtractOptions) -> Result<CalendarData> {
    let components = parse_components(text);
    let has_calendar = components.iter().any(|c| {
        c.is("VCALENDAR")
            || c.is("VEVENT")
            || c.is("VTODO")
            || c.is("VJOURNAL")
            || c.is("VTIMEZONE")
    });
    if !has_calendar {
        return Err(Error::NoCalendar);
    }
    let mut tz = TzContext::new();
    for component in &components {
        if component.is("VCALENDAR") {
            tz.add_calendar(component);
        } else if component.is("VTIMEZONE") {
            tz.add_vtimezone(component);
        }
    }
    let mut events = Vec::new();
    for component in &components {
        if component.is("VCALENDAR") {
            events.extend(events_from_calendar(component, &tz, options));
        } else if component.is("VEVENT") {
            events.extend(event_from_vevent(component, &tz, options));
        }
    }
    let method = components
        .iter()
        .find(|c| c.is("VCALENDAR"))
        .and_then(|c| c.value("METHOD"))
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty());
    Ok(CalendarData {
        components,
        events,
        tz,
        method,
    })
}

/// Parses iCalendar text and returns its events; see [`parse_calendar`].
pub fn parse_events(text: &str) -> Result<Vec<Event>> {
    parse_calendar(text).map(|data| data.events)
}

/// Extracts the events of one `VCALENDAR` component.
pub fn events_from_calendar(
    calendar: &Component,
    ctx: &TzContext,
    options: &ExtractOptions,
) -> Vec<Event> {
    calendar
        .children("VEVENT")
        .filter_map(|vevent| event_from_vevent(vevent, ctx, options))
        .collect()
}

/// Extracts one `VEVENT`. Returns `None` if no start can be determined: a
/// `DTSTART` that cannot be parsed (and no `RECURRENCE-ID`), or neither
/// `DTSTART`, `RECURRENCE-ID` nor `DTSTAMP` (the last two are fallbacks for
/// iTIP messages that legitimately omit `DTSTART`).
pub fn event_from_vevent(
    vevent: &Component,
    ctx: &TzContext,
    options: &ExtractOptions,
) -> Option<Event> {
    let normalize = |time: EventTime| {
        if options.normalize_tzids {
            ctx.normalize(time)
        } else {
            time
        }
    };
    let time_of = |name: &str| vevent.property(name).and_then(property_time).map(normalize);
    let tz = options.default_tz;

    let recurrence_id = time_of("RECURRENCE-ID");
    // A missing DTSTART is legal in some iTIP messages (CANCEL, REPLY); a
    // present but unparseable one means the event cannot be placed.
    let mut start = match vevent.property("DTSTART") {
        Some(_) => time_of("DTSTART").or_else(|| recurrence_id.clone())?,
        None => recurrence_id.clone().or_else(|| time_of("DTSTAMP"))?,
    };
    let mut end = event_end(vevent, &start, ctx, tz, normalize);
    if is_outlook_all_day(vevent)
        && let Some((s, e)) = as_all_day(&start, &end)
    {
        start = s;
        end = e;
    }

    let uid = vevent
        .value("UID")
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map_or_else(|| synthesized_uid(vevent), str::to_owned);

    let mut event = Event::new(uid, start, end);
    event.recurrence_id = recurrence_id;
    event.summary = vevent.text("SUMMARY").unwrap_or_default().trim().to_owned();
    event.description = non_empty(vevent.text("DESCRIPTION"));
    event.description_html = vevent
        .properties_named("X-ALT-DESC")
        .find(|p| {
            p.param("FMTTYPE")
                .is_none_or(|f| f.eq_ignore_ascii_case("text/html"))
        })
        .map(Property::text_value)
        .filter(|html| !html.trim().is_empty());
    event.location = non_empty(vevent.text("LOCATION"));
    event.url = non_empty(vevent.text("URL"));
    event.rrule = vevent
        .value("RRULE")
        .map(|r| {
            let r = r.trim();
            match r.get(..6) {
                Some(prefix) if prefix.eq_ignore_ascii_case("RRULE:") => r[6..].trim().to_owned(),
                _ => r.to_owned(),
            }
        })
        .filter(|r| !r.is_empty());
    event.rdates = vevent
        .properties_named("RDATE")
        .flat_map(property_times)
        .map(normalize)
        .collect();
    event.exdates = vevent
        .properties_named("EXDATE")
        .flat_map(property_times)
        .map(normalize)
        .collect();
    event.status = parse_status(vevent.value("STATUS"));
    event.transparency = parse_transparency(vevent);
    event.class = parse_class(vevent.value("CLASS"));
    event.organizer = vevent.property("ORGANIZER").and_then(|p| {
        Some(Organizer {
            email: cal_address(p)?,
            name: common_name(p),
        })
    });
    event.attendees = vevent
        .properties_named("ATTENDEE")
        .filter_map(parse_attendee)
        .collect();
    event.alarms = vevent
        .children("VALARM")
        .filter_map(|valarm| parse_alarm(valarm, &event.start, &event.end, &event.summary, ctx, tz))
        .collect();
    let mut categories: Vec<String> = Vec::new();
    for property in vevent.properties_named("CATEGORIES") {
        for category in property.text_values() {
            if !categories.contains(&category) {
                categories.push(category);
            }
        }
    }
    event.categories = categories;
    event.color = vevent.text("COLOR").and_then(|c| css_color_to_hex(&c));
    event.conference_url = detect_conference_url(vevent);
    event.sequence = vevent
        .value("SEQUENCE")
        .and_then(|s| s.trim().trim_start_matches('+').parse::<i64>().ok())
        .unwrap_or(0);
    event.created = instant_of(vevent.property("CREATED"), ctx, tz);
    event.last_modified = instant_of(vevent.property("LAST-MODIFIED"), ctx, tz);
    Some(event)
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// A timestamp property (`CREATED`, `LAST-MODIFIED`) as an instant; values
/// that are (incorrectly) not in UTC are interpreted like any other time.
fn instant_of(property: Option<&Property>, ctx: &TzContext, tz: Tz) -> Option<DateTime<Utc>> {
    property
        .and_then(property_time)
        .map(|time| ctx.to_utc(&time, tz))
}

/// Computes the exclusive end: `DTEND`, else `DTSTART` + `DURATION`, else
/// the RFC 5545 defaults (one day for dates, zero length otherwise).
/// Ends before the start (including negative durations) are clamped.
fn event_end(
    vevent: &Component,
    start: &EventTime,
    ctx: &TzContext,
    tz: Tz,
    normalize: impl Fn(EventTime) -> EventTime,
) -> EventTime {
    let one_day_after = |time: &EventTime| -> EventTime {
        let date = wall_clock(time).date();
        EventTime::Date {
            date: date.succ_opt().unwrap_or(date),
        }
    };
    if let Some(end) = vevent
        .property("DTEND")
        .and_then(property_time)
        .map(normalize)
    {
        return match (start, end) {
            (EventTime::Date { date: s }, end) => {
                let e = wall_clock(&end).date();
                if e <= *s {
                    one_day_after(start)
                } else {
                    EventTime::Date { date: e }
                }
            }
            (_, EventTime::Date { date }) => {
                let end = with_wall_clock(date.and_time(NaiveTime::MIN), start);
                clamp_end(start, end, ctx, tz)
            }
            (_, end) => clamp_end(start, end, ctx, tz),
        };
    }
    if let Some(duration) = vevent.value("DURATION").and_then(parse_duration) {
        let duration = duration.max(TimeDelta::zero());
        return match start {
            EventTime::Date { date } => {
                let days = duration.num_days().max(1);
                EventTime::Date {
                    date: date
                        .checked_add_signed(TimeDelta::try_days(days).unwrap_or(TimeDelta::zero()))
                        .unwrap_or(*date),
                }
            }
            EventTime::Zoned { local, tzid } => EventTime::Zoned {
                local: local.checked_add_signed(duration).unwrap_or(*local),
                tzid: tzid.clone(),
            },
            EventTime::Floating { local } => EventTime::Floating {
                local: local.checked_add_signed(duration).unwrap_or(*local),
            },
            EventTime::Utc { at } => EventTime::Utc {
                at: at.checked_add_signed(duration).unwrap_or(*at),
            },
        };
    }
    match start {
        EventTime::Date { .. } => one_day_after(start),
        other => other.clone(),
    }
}

fn clamp_end(start: &EventTime, end: EventTime, ctx: &TzContext, tz: Tz) -> EventTime {
    if ctx.to_utc(&end, tz) < ctx.to_utc(start, tz) {
        start.clone()
    } else {
        end
    }
}

fn is_outlook_all_day(vevent: &Component) -> bool {
    [
        "X-MICROSOFT-CDO-ALLDAYEVENT",
        "X-MICROSOFT-MSNCALENDAR-ALLDAYEVENT",
    ]
    .iter()
    .any(|name| {
        vevent
            .value(name)
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("TRUE"))
    })
}

/// Outlook sometimes writes all-day events as midnight-to-midnight times.
fn as_all_day(start: &EventTime, end: &EventTime) -> Option<(EventTime, EventTime)> {
    if start.is_date() {
        return None;
    }
    let (s, e) = (wall_clock(start), wall_clock(end));
    if s.time() != NaiveTime::MIN
        || e.time() != NaiveTime::MIN
        || matches!(start, EventTime::Utc { .. })
    {
        return None;
    }
    let end_date = if e.date() > s.date() {
        e.date()
    } else {
        s.date().succ_opt()?
    };
    Some((
        EventTime::Date { date: s.date() },
        EventTime::Date { date: end_date },
    ))
}

fn parse_status(value: Option<&str>) -> EventStatus {
    match value.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
        Some("TENTATIVE") => EventStatus::Tentative,
        Some("CANCELLED" | "CANCELED") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    }
}

fn parse_transparency(vevent: &Component) -> Transparency {
    match vevent
        .value("TRANSP")
        .map(|v| v.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("TRANSPARENT") => Transparency::Transparent,
        Some(_) => Transparency::Opaque,
        None => {
            let free = vevent
                .value("X-MICROSOFT-CDO-BUSYSTATUS")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("FREE"));
            if free {
                Transparency::Transparent
            } else {
                Transparency::Opaque
            }
        }
    }
}

fn parse_class(value: Option<&str>) -> EventClass {
    match value.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
        None | Some("PUBLIC") | Some("") => EventClass::Public,
        Some("CONFIDENTIAL") => EventClass::Confidential,
        // RFC 5545: unknown classes must be treated as PRIVATE.
        Some(_) => EventClass::Private,
    }
}

/// The lower-cased e-mail address of an `ORGANIZER` / `ATTENDEE` property:
/// the `mailto:` value, else the `EMAIL` parameter (iCloud writes
/// `urn:uuid:` values), else the raw value.
pub fn cal_address(property: &Property) -> Option<String> {
    let value = property.value.trim();
    let lower = value.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("mailto:") {
        let email = percent_decode(rest.trim()).trim().to_owned();
        if !email.is_empty() {
            return Some(email);
        }
    }
    if let Some(email) = property
        .param("EMAIL")
        .map(|e| e.trim().trim_start_matches("mailto:").to_ascii_lowercase())
        .filter(|e| !e.is_empty())
    {
        return Some(email);
    }
    (!lower.is_empty()).then_some(lower)
}

fn percent_decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = text
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn common_name(property: &Property) -> Option<String> {
    property
        .param("CN")
        .map(|cn| cn.trim().trim_matches('"').trim().to_owned())
        .filter(|cn| !cn.is_empty())
}

/// Maps a `PARTSTAT` value.
pub(crate) fn parse_partstat(value: Option<&str>) -> PartStat {
    match value.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
        Some("ACCEPTED") => PartStat::Accepted,
        Some("DECLINED") => PartStat::Declined,
        Some("TENTATIVE") => PartStat::Tentative,
        Some("DELEGATED") => PartStat::Delegated,
        _ => PartStat::NeedsAction,
    }
}

pub(crate) fn parse_attendee(property: &Property) -> Option<Attendee> {
    let role = match property
        .param("ROLE")
        .map(str::to_ascii_uppercase)
        .as_deref()
    {
        Some("CHAIR") => AttendeeRole::Chair,
        Some("OPT-PARTICIPANT") => AttendeeRole::Optional,
        Some("NON-PARTICIPANT") => AttendeeRole::NonParticipant,
        _ => AttendeeRole::Required,
    };
    let cutype = match property
        .param("CUTYPE")
        .map(str::to_ascii_uppercase)
        .as_deref()
    {
        None | Some("INDIVIDUAL") => CalendarUserType::Individual,
        Some("GROUP") => CalendarUserType::Group,
        Some("RESOURCE") => CalendarUserType::Resource,
        Some("ROOM") => CalendarUserType::Room,
        Some(_) => CalendarUserType::Unknown,
    };
    Some(Attendee {
        email: cal_address(property)?,
        name: common_name(property),
        partstat: parse_partstat(property.param("PARTSTAT")),
        role,
        cutype,
        rsvp: property
            .param("RSVP")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("TRUE")),
    })
}

/// Alarm descriptions that only repeat the event title or a client's
/// placeholder carry no information and are reported as `None`.
fn meaningful_alarm_description(description: Option<String>, summary: &str) -> Option<String> {
    const PLACEHOLDERS: [&str; 5] = [
        "reminder",
        "this is an event reminder",
        "default mozilla description",
        "event reminder",
        "alarm notification",
    ];
    description.filter(|d| {
        let lower = d.to_lowercase();
        d != summary && !PLACEHOLDERS.contains(&lower.as_str())
    })
}

pub(crate) fn parse_alarm(
    valarm: &Component,
    start: &EventTime,
    end: &EventTime,
    summary: &str,
    ctx: &TzContext,
    tz: Tz,
) -> Option<Alarm> {
    let action = match valarm
        .value("ACTION")
        .map(|a| a.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("AUDIO") => AlarmAction::Audio,
        Some("EMAIL") => AlarmAction::Email,
        Some("NONE") => return None,
        _ => AlarmAction::Display,
    };
    let trigger = valarm.property("TRIGGER")?;
    let start_utc = ctx.to_utc(start, tz);
    let absolute = trigger
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
        || parse_duration(&trigger.value).is_none();
    let minutes_before = if absolute {
        let at = property_time(trigger).map(|t| ctx.to_utc(&t, tz))?;
        (start_utc - at).num_minutes()
    } else {
        let offset = parse_duration(&trigger.value)?;
        let related_end = trigger
            .param("RELATED")
            .is_some_and(|r| r.eq_ignore_ascii_case("END"));
        let from_start = if related_end {
            (ctx.to_utc(end, tz) - start_utc).checked_add(&offset)?
        } else {
            offset
        };
        -from_start.num_minutes()
    };
    Some(Alarm {
        action,
        minutes_before,
        description: meaningful_alarm_description(non_empty(valarm.text("DESCRIPTION")), summary),
    })
}

/// A stable UID for events that lack one, derived from their content.
fn synthesized_uid(vevent: &Component) -> String {
    // FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for name in [
        "DTSTART",
        "DTEND",
        "SUMMARY",
        "LOCATION",
        "RRULE",
        "RECURRENCE-ID",
    ] {
        for byte in name.bytes().chain(vevent.value(name).unwrap_or("").bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("envelope-generated-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime};

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").expect("valid test datetime")
    }

    fn one(body: &str) -> Event {
        let text =
            format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n{body}END:VEVENT\r\nEND:VCALENDAR\r\n");
        let mut events = parse_events(&text).expect("calendar");
        assert_eq!(events.len(), 1);
        events.remove(0)
    }

    #[test]
    fn end_rules() {
        let e = one("UID:1\r\nDTSTART;VALUE=DATE:20240301\r\n");
        assert_eq!(
            e.end,
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 3, 2).expect("date")
            }
        );
        let e = one("UID:1\r\nDTSTART:20240301T090000Z\r\n");
        assert_eq!(e.end, e.start);
        let e = one("UID:1\r\nDTSTART;TZID=Europe/Berlin:20240301T090000\r\nDURATION:PT1H30M\r\n");
        assert_eq!(
            e.end,
            EventTime::Zoned {
                local: dt("2024-03-01 10:30"),
                tzid: "Europe/Berlin".into()
            }
        );
        let e = one("UID:1\r\nDTSTART;VALUE=DATE:20240301\r\nDURATION:P3D\r\n");
        assert_eq!(
            e.end,
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 3, 4).expect("date")
            }
        );
        let e = one("UID:1\r\nDTSTART:20240301T090000Z\r\nDURATION:-PT1H\r\n");
        assert_eq!(e.end, e.start);
        let e = one("UID:1\r\nDTSTART:20240301T090000Z\r\nDTEND:20240301T080000Z\r\n");
        assert_eq!(e.end, e.start);
        let e = one("UID:1\r\nDTSTART;VALUE=DATE:20240301\r\nDTEND;VALUE=DATE:20240301\r\n");
        assert_eq!(
            e.end,
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 3, 2).expect("date")
            }
        );
        let e = one(
            "UID:1\r\nDTSTART;TZID=America/New_York:20240301T220000\r\nDTEND;VALUE=DATE:20240302\r\n",
        );
        assert_eq!(
            e.end,
            EventTime::Zoned {
                local: dt("2024-03-02 00:00"),
                tzid: "America/New_York".into()
            }
        );
    }

    #[test]
    fn outlook_midnight_all_day() {
        let e = one(
            "UID:1\r\nDTSTART;TZID=W. Europe Standard Time:20240301T000000\r\nDTEND;TZID=W. Europe Standard Time:20240302T000000\r\nX-MICROSOFT-CDO-ALLDAYEVENT:TRUE\r\n",
        );
        assert!(e.is_all_day());
        assert_eq!(
            e.end,
            EventTime::Date {
                date: NaiveDate::from_ymd_opt(2024, 3, 2).expect("date")
            }
        );
    }

    #[test]
    fn people() {
        let e = one(
            "UID:1\r\nDTSTART:20240301T090000Z\r\nORGANIZER;CN=\"Boss, The\";SENT-BY=\"mailto:assistant@x.com\":MAILTO:Boss@Example.COM\r\nATTENDEE;CN=Ali;PARTSTAT=ACCEPTED;ROLE=CHAIR;RSVP=TRUE:mailto:ali@example.com\r\nATTENDEE;CUTYPE=ROOM;PARTSTAT=TENTATIVE;ROLE=NON-PARTICIPANT:mailto:room%40example.com\r\nATTENDEE;CN=Ay\u{15f}e;EMAIL=ayse@icloud.com;PARTSTAT=DECLINED;ROLE=OPT-PARTICIPANT:urn:uuid:1234\r\nATTENDEE;CUTYPE=X-BOT;PARTSTAT=X-WEIRD:mailto:bot@example.com\r\n",
        );
        let org = e.organizer.expect("organizer");
        assert_eq!(org.email, "boss@example.com");
        assert_eq!(org.name.as_deref(), Some("Boss, The"));
        assert_eq!(e.attendees.len(), 4);
        assert_eq!(e.attendees[0].partstat, PartStat::Accepted);
        assert_eq!(e.attendees[0].role, AttendeeRole::Chair);
        assert!(e.attendees[0].rsvp);
        assert_eq!(e.attendees[1].email, "room@example.com");
        assert_eq!(e.attendees[1].cutype, CalendarUserType::Room);
        assert_eq!(e.attendees[1].role, AttendeeRole::NonParticipant);
        assert_eq!(e.attendees[2].email, "ayse@icloud.com");
        assert_eq!(e.attendees[2].name.as_deref(), Some("Ay\u{15f}e"));
        assert_eq!(e.attendees[2].role, AttendeeRole::Optional);
        assert_eq!(e.attendees[3].cutype, CalendarUserType::Unknown);
        assert_eq!(e.attendees[3].partstat, PartStat::NeedsAction);
    }

    #[test]
    fn alarms() {
        let e = one(
            "UID:1\r\nDTSTART:20240301T090000Z\r\nDTEND:20240301T100000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:Soon\r\nEND:VALARM\r\nBEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER;RELATED=END:-PT10M\r\nEND:VALARM\r\nBEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER;VALUE=DATE-TIME:20240229T090000Z\r\nEND:VALARM\r\nBEGIN:VALARM\r\nACTION:NONE\r\nTRIGGER;VALUE=DATE-TIME:19760401T005545Z\r\nEND:VALARM\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:PT5M\r\nEND:VALARM\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\n",
        );
        let got: Vec<(AlarmAction, i64)> = e
            .alarms
            .iter()
            .map(|a| (a.action, a.minutes_before))
            .collect();
        assert_eq!(
            got,
            vec![
                (AlarmAction::Display, 15),
                (AlarmAction::Audio, -50),
                (AlarmAction::Email, 1440),
                (AlarmAction::Display, -5),
            ]
        );
        assert_eq!(e.alarms[0].description.as_deref(), Some("Soon"));
    }

    #[test]
    fn misc_properties() {
        let e = one(
            "DTSTART:20240301T090000Z\r\nSUMMARY:Plan\\, review\\; ship\r\nDESCRIPTION:Line1\\nLine2\r\nX-ALT-DESC;FMTTYPE=text/html:<p>Line1</p>\r\nCATEGORIES:Work,Project X\r\nCATEGORIES:Work\r\nCOLOR:turquoise\r\nSTATUS:CANCELED\r\nCLASS:X-SECRET\r\nSEQUENCE:3\r\nCREATED:20240101T120000Z\r\nLAST-MODIFIED:20240102T120000Z\r\nX-MICROSOFT-CDO-BUSYSTATUS:FREE\r\nURL:https://example.com/e?a=1\\,2\r\n",
        );
        assert!(e.uid.starts_with("envelope-generated-"));
        assert_eq!(e.summary, "Plan, review; ship");
        assert_eq!(e.description.as_deref(), Some("Line1\nLine2"));
        assert_eq!(e.description_html.as_deref(), Some("<p>Line1</p>"));
        assert_eq!(
            e.categories,
            vec!["Work".to_string(), "Project X".to_string()]
        );
        assert_eq!(e.color.as_deref(), Some("#40e0d0"));
        assert_eq!(e.status, EventStatus::Cancelled);
        assert_eq!(e.class, EventClass::Private);
        assert_eq!(e.transparency, Transparency::Transparent);
        assert_eq!(e.sequence, 3);
        assert_eq!(e.created, Some(dt("2024-01-01 12:00").and_utc()));
        assert_eq!(e.last_modified, Some(dt("2024-01-02 12:00").and_utc()));
        assert_eq!(e.url.as_deref(), Some("https://example.com/e?a=1,2"));
        // Synthesized UIDs are stable.
        let again = one("DTSTART:20240301T090000Z\r\nSUMMARY:Plan\\, review\\; ship\r\n");
        assert_eq!(again.uid, e.uid);
    }

    #[test]
    fn invalid_dtstart_skips_but_missing_dtstart_falls_back() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:bad\r\nDTSTAMP:20240101T000000Z\r\nDTSTART;VALUE=DATE:20230229\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:cancel\r\nDTSTAMP:20240101T000000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let events = parse_events(text).expect("calendar");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid, "cancel");
        assert_eq!(
            events[0].start,
            EventTime::Utc {
                at: dt("2024-01-01 00:00").and_utc()
            }
        );
    }

    #[test]
    fn no_calendar_is_an_error() {
        assert_eq!(parse_events("hello world").unwrap_err(), Error::NoCalendar);
        assert_eq!(
            parse_events("BEGIN:VCARD\nEND:VCARD\n").unwrap_err(),
            Error::NoCalendar
        );
        // Bare VEVENTs are accepted.
        let events = parse_events("BEGIN:VEVENT\nUID:x\nDTSTART:20240101T000000Z\nEND:VEVENT\n")
            .expect("events");
        assert_eq!(events.len(), 1);
    }
}
