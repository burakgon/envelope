//! Conversions between Exchange Web Services data and Envelope's model:
//! messages, folders, Outlook categories and calendar items.

use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use envelope_ews as ews;
use envelope_ical::{
    Alarm, AlarmAction, Attendee, AttendeeRole, CalendarUserType, Event, EventClass, EventStatus,
    EventTime, Organizer, PartStat, Transparency, TzContext,
};

use crate::db::mail::{FolderUpsert, MessageMeta};
use crate::db::now_ms;
use crate::error::{Error, Result};
use crate::mime;
use crate::model::{Address, Flags, FolderRole};
use crate::sync::graph::category_upsert;
use crate::sync::imap::folder_priority;

// ---------------------------------------------------------------------------
// Messages

/// An address from an EWS mailbox. Mailboxes without an SMTP address
/// (legacy Exchange DNs Exchange could not resolve) are skipped.
pub(super) fn address(mailbox: &ews::Mailbox) -> Option<Address> {
    let email = mailbox.smtp_address()?.trim();
    if email.is_empty() {
        return None;
    }
    Some(Address::new(
        mailbox.name.as_deref().map(|n| n.trim().to_string()),
        email,
    ))
}

fn addresses(list: &[ews::Mailbox]) -> Vec<Address> {
    list.iter().filter_map(address).collect()
}

fn clean_message_id(id: &str) -> Option<String> {
    let id = id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// The message ids of a `Message-ID`, `In-Reply-To` or `References` value.
fn message_ids(value: &str) -> Vec<String> {
    let bracketed: Vec<String> = value
        .split('<')
        .filter_map(|part| part.split_once('>'))
        .filter_map(|(id, _)| clean_message_id(id))
        .collect();
    if !bracketed.is_empty() {
        return bracketed;
    }
    value
        .split_whitespace()
        .filter_map(clean_message_id)
        .collect()
}

/// The iTIP method of a meeting message, from its item class.
pub(super) fn calendar_method(item_class: Option<&str>) -> Option<String> {
    let class = item_class?.trim().to_ascii_lowercase();
    let method = if class.starts_with("ipm.schedule.meeting.request") {
        "REQUEST"
    } else if class.starts_with("ipm.schedule.meeting.canceled")
        || class.starts_with("ipm.schedule.meeting.cancelled")
    {
        "CANCEL"
    } else if class.starts_with("ipm.schedule.meeting.resp") {
        "REPLY"
    } else {
        return None;
    };
    Some(method.to_string())
}

/// Delivery reports, read receipts and automatic replies.
fn is_automatic(item_class: Option<&str>) -> bool {
    item_class.is_some_and(|class| {
        let class = class.trim().to_ascii_lowercase();
        class.starts_with("report.")
            || class.starts_with("ipm.note.rules.oof")
            || class.starts_with("ipm.note.rules.replytemplate")
    })
}

/// Item kinds stored as mail (contacts, tasks and appointments that end up
/// in a mail folder are left out).
pub(super) fn is_mail_kind(kind: ews::ItemKind) -> bool {
    kind.is_mail() || kind == ews::ItemKind::Item
}

/// What a message fetched with [`ews::message_detail_properties`] tells
/// about itself. Mailing-list headers, the exact size of the source and
/// invitations are filled in from the MIME source once it is downloaded;
/// conversations are grouped by Exchange's conversation id.
pub(super) fn message_meta(m: &ews::Message) -> MessageMeta {
    let mut flags = 0;
    if m.is_read == Some(true) {
        flags |= Flags::SEEN;
    }
    if m.is_flagged() {
        flags |= Flags::FLAGGED;
    }
    if m.is_draft == Some(true) {
        flags |= Flags::DRAFT;
    }
    MessageMeta {
        remote_id: Some(m.id.id.clone()),
        remote_thread_id: m.conversation_id.clone().filter(|c| !c.trim().is_empty()),
        message_id: m
            .internet_message_id
            .as_deref()
            .and_then(|v| message_ids(v).into_iter().next()),
        in_reply_to: m
            .in_reply_to
            .as_deref()
            .and_then(|v| message_ids(v).into_iter().next()),
        references: m.references.as_deref().map(message_ids).unwrap_or_default(),
        subject: m.subject.as_deref().unwrap_or("").trim().to_string(),
        from: m
            .from
            .as_ref()
            .and_then(address)
            .or_else(|| m.sender.as_ref().and_then(address)),
        to: addresses(&m.to),
        cc: addresses(&m.cc),
        bcc: addresses(&m.bcc),
        reply_to: addresses(&m.reply_to),
        date: m
            .received
            .or(m.sent)
            .or(m.created)
            .map(|d| d.timestamp_millis())
            .unwrap_or_else(now_ms),
        flags,
        size: m
            .size
            .and_then(|s| i64::try_from(s).ok())
            .unwrap_or_default(),
        has_attachments: m.has_attachments.unwrap_or(false),
        snippet: m
            .preview
            .as_deref()
            .map(mime::make_snippet)
            .filter(|s| !s.is_empty()),
        is_auto: is_automatic(m.item_class.as_deref()),
        calendar_method: calendar_method(m.item_class.as_deref()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Folders and categories

/// Our role for a well-known Exchange folder. The outbox is not synchronized:
/// messages only pass through it while Exchange sends them.
pub(super) fn folder_role(role: ews::FolderRole) -> Option<FolderRole> {
    use ews::FolderRole as Ews;
    match role {
        Ews::Inbox => Some(FolderRole::Inbox),
        Ews::Drafts => Some(FolderRole::Drafts),
        Ews::SentItems => Some(FolderRole::Sent),
        Ews::DeletedItems => Some(FolderRole::Trash),
        Ews::JunkEmail => Some(FolderRole::Junk),
        Ews::Archive => Some(FolderRole::Archive),
        _ => None,
    }
}

/// Whether a folder of the mailbox tree is synchronized as a mail folder.
pub(super) fn is_synced_mail_folder(folder: &ews::Folder) -> bool {
    folder.is_mail_folder()
        && !folder.is_hidden
        && folder.kind != ews::FolderKind::Search
        && !matches!(
            folder.role,
            Some(ews::FolderRole::Outbox | ews::FolderRole::MsgFolderRoot)
        )
}

/// Folder rows for the mailbox's mail folders (parents first, as
/// [`ews::EwsClient::folder_tree`] returns them) and label rows for its
/// Outlook categories (name and CSS color).
pub(super) fn folder_upserts(
    tree: &[ews::Folder],
    categories: &[(String, Option<String>)],
) -> Vec<FolderUpsert> {
    let synced: Vec<&ews::Folder> = tree.iter().filter(|f| is_synced_mail_folder(f)).collect();
    let known: HashSet<&str> = synced.iter().map(|f| f.id.id.as_str()).collect();
    let mut paths: HashMap<&str, String> = HashMap::new();
    for f in &synced {
        let name = f.display_name.trim();
        let path = match f.parent_id.as_ref().and_then(|p| paths.get(p.id.as_str())) {
            Some(parent) => format!("{parent}/{name}"),
            None => name.to_string(),
        };
        paths.insert(f.id.id.as_str(), path);
    }
    let mut out: Vec<FolderUpsert> = synced
        .iter()
        .map(|f| {
            let role = f.role.and_then(folder_role);
            FolderUpsert {
                remote_id: f.id.id.clone(),
                name: f.display_name.trim().to_string(),
                path: paths.get(f.id.id.as_str()).cloned().unwrap_or_default(),
                parent_remote_id: f
                    .parent_id
                    .as_ref()
                    .map(|p| p.id.clone())
                    .filter(|p| known.contains(p.as_str())),
                role,
                kind: "folder".into(),
                delimiter: Some("/".into()),
                selectable: true,
                sort_order: folder_priority(role) as i64,
                color: None,
            }
        })
        .collect();
    let mut seen = HashSet::new();
    for (name, color) in categories {
        let name = name.trim();
        if name.is_empty() || !seen.insert(name.to_lowercase()) {
            continue;
        }
        out.push(category_upsert(name, color.clone()));
    }
    out
}

/// Reads the master category list (`CategoryList` user configuration of the
/// calendar folder): one request, answered with a base64 XML document.
pub(super) const CATEGORY_LIST_REQUEST: &str = concat!(
    "<m:GetUserConfiguration>",
    "<m:UserConfigurationName Name=\"CategoryList\"><t:DistinguishedFolderId Id=\"calendar\"/></m:UserConfigurationName>",
    "<m:UserConfigurationProperties>XmlData</m:UserConfigurationProperties>",
    "</m:GetUserConfiguration>"
);

/// The categories (name and CSS color) of a `GetUserConfiguration`
/// response for the master category list, or `None` when it holds none
/// (the mailbox never had categories, or the server refused).
pub(super) fn category_list(response: &str) -> Option<Vec<(String, Option<String>)>> {
    Some(parse_categories(&category_list_xml(response)?))
}

/// The master category list document of a `GetUserConfiguration` response.
pub(super) fn category_list_xml(response: &str) -> Option<String> {
    let data = element_text(response, "XmlData")?;
    let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .ok()?;
    let xml = String::from_utf8_lossy(&bytes);
    Some(xml.trim_start_matches('\u{feff}').to_string())
}

/// A change to one category of the master list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CategoryEdit {
    /// A new color preset (`-1` for none).
    Recolor(i32),
    Remove,
}

/// The master category list with one category recolored or removed, keeping
/// everything else Outlook stored there; `None` when it does not name the
/// category or cannot be read.
pub(super) fn edit_category_list(xml: &str, name: &str, edit: CategoryEdit) -> Option<String> {
    use quick_xml::events::{BytesStart, Event as Xml};
    use quick_xml::{Reader, Writer};
    let is_target = |e: &BytesStart<'_>| {
        e.local_name().as_ref() == "category"
            && e.attributes().flatten().any(|a| {
                a.key.local_name().as_ref() == "name"
                    && a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .is_ok_and(|v| v.trim().eq_ignore_ascii_case(name.trim()))
            })
    };
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::new());
    let mut found = false;
    // Depth inside a removed element.
    let mut skipping = 0usize;
    loop {
        let event = match reader.read_event() {
            Ok(Xml::Eof) => break,
            Ok(event) => event,
            Err(_) => return None,
        };
        if skipping > 0 {
            match event {
                Xml::Start(_) => skipping += 1,
                Xml::End(_) => skipping -= 1,
                _ => {}
            }
            continue;
        }
        let (target, open) = match &event {
            Xml::Start(e) => (is_target(e), true),
            Xml::Empty(e) => (is_target(e), false),
            _ => (false, false),
        };
        if !target {
            writer.write_event(event).ok()?;
            continue;
        }
        found = true;
        let (Xml::Start(e) | Xml::Empty(e)) = &event else {
            continue;
        };
        match edit {
            CategoryEdit::Remove => {
                if open {
                    skipping = 1;
                }
            }
            CategoryEdit::Recolor(color) => {
                let color = color.to_string();
                let mut element = BytesStart::new(e.name().as_ref().to_string());
                let mut colored = false;
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == "color" {
                        element.push_attribute(("color", color.as_str()));
                        colored = true;
                    } else {
                        element.push_attribute(a);
                    }
                }
                if !colored {
                    element.push_attribute(("color", color.as_str()));
                }
                let out = if open {
                    Xml::Start(element)
                } else {
                    Xml::Empty(element)
                };
                writer.write_event(out).ok()?;
            }
        }
    }
    found.then(|| String::from_utf8(writer.into_inner()).ok())?
}

/// Stores a master category list (`UpdateUserConfiguration`).
pub(super) fn category_update_request(xml: &str) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(xml.as_bytes());
    format!(
        "<m:UpdateUserConfiguration><m:UserConfiguration>\
         <t:UserConfigurationName Name=\"CategoryList\"><t:DistinguishedFolderId Id=\"calendar\"/></t:UserConfigurationName>\
         <t:XmlData>{data}</t:XmlData>\
         </m:UserConfiguration></m:UpdateUserConfiguration>"
    )
}

/// The message of a response that reports an error, if it does.
pub(super) fn response_error(response: &str) -> Option<String> {
    response.contains("ResponseClass=\"Error\"").then(|| {
        element_text(response, "MessageText")
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "Exchange refused the change".into())
    })
}

/// Text of the first element with the given local name.
fn element_text(xml: &str, local_name: &str) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event as Xml;
    let mut reader = Reader::from_str(xml);
    let mut inside = false;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Xml::Start(e)) if e.local_name().as_ref() == local_name => inside = true,
            Ok(Xml::Text(t)) if inside => text.push_str(&t.xml10_content()),
            Ok(Xml::End(e)) if inside && e.local_name().as_ref() == local_name => {
                return Some(text);
            }
            Ok(Xml::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

fn parse_categories(xml: &str) -> Vec<(String, Option<String>)> {
    use quick_xml::Reader;
    use quick_xml::events::Event as Xml;
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Xml::Start(e) | Xml::Empty(e)) if e.local_name().as_ref() == "category" => {
                let mut name = None;
                let mut color = None;
                for attribute in e.attributes().flatten() {
                    let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    else {
                        continue;
                    };
                    match attribute.key.local_name().as_ref() {
                        "name" => name = Some(value.trim().to_string()),
                        "color" => color = value.trim().parse::<i32>().ok(),
                        _ => {}
                    }
                }
                if let Some(name) = name.filter(|n| !n.is_empty()) {
                    out.push((name, color.and_then(category_color)));
                }
            }
            Ok(Xml::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// CSS color of an Outlook category color preset (`-1` is none).
pub(super) fn category_color(preset: i32) -> Option<String> {
    let index = usize::try_from(preset).ok()?;
    envelope_graph::CategoryColor::ALL
        .get(index + 1)
        .and_then(|c| c.hex())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Calendar items

/// What [`calendar_event`] needs to know beyond the item itself.
pub(super) struct EventContext<'a> {
    /// The mailbox owner's address, lower-case.
    pub(super) me: &'a str,
    /// Zone for all-day items whose zone Exchange does not report.
    pub(super) default_tz: Tz,
    /// `RRULE` of the series an occurrence belongs to.
    pub(super) series_rule: Option<&'a str>,
}

/// The calendar day an all-day boundary falls on: boundaries are midnights
/// of the event's zone, rounded to the nearest one should a zone shift them.
fn all_day_date(at: DateTime<Utc>, tz: Tz) -> NaiveDate {
    let local = at.with_timezone(&tz).naive_local();
    if local.hour() >= 12 {
        local.date().succ_opt().unwrap_or(local.date())
    } else {
        local.date()
    }
}

fn timed(at: DateTime<Utc>, tz: Option<Tz>) -> EventTime {
    match tz {
        Some(tz) if tz != Tz::UTC => EventTime::Zoned {
            local: at.with_timezone(&tz).naive_local(),
            tzid: tz.name().to_string(),
        },
        _ => EventTime::Utc { at },
    }
}

fn partstat(response: Option<ews::ResponseType>) -> PartStat {
    match response {
        Some(ews::ResponseType::Accept | ews::ResponseType::Organizer) => PartStat::Accepted,
        Some(ews::ResponseType::Tentative) => PartStat::Tentative,
        Some(ews::ResponseType::Decline) => PartStat::Declined,
        _ => PartStat::NeedsAction,
    }
}

/// Whether the item is part of a recurring series (an occurrence or a
/// modified occurrence).
pub(super) fn is_series_instance(item: &ews::CalendarItem) -> bool {
    matches!(
        item.item_type,
        Some(ews::CalendarItemType::Occurrence | ews::CalendarItemType::Exception)
    )
}

/// An expanded calendar item (single event, occurrence or modified
/// occurrence) as an event. Occurrences carry the recurrence id of the
/// series and, informationally, its rule.
pub(super) fn calendar_event(item: &ews::CalendarItem, cx: &EventContext<'_>) -> Event {
    let zone = item.start_tz();
    let all_day = item.is_all_day == Some(true);
    let start_at = item.start.unwrap_or_else(Utc::now);
    let end_at = item.end.unwrap_or(start_at).max(start_at);
    let day_zone = zone.unwrap_or(cx.default_tz);
    let (start, end) = if all_day {
        let first = all_day_date(start_at, day_zone);
        let last = all_day_date(end_at, day_zone).max(first.succ_opt().unwrap_or(first));
        (
            EventTime::Date { date: first },
            EventTime::Date { date: last },
        )
    } else {
        let end_zone = item
            .end_time_zone
            .as_deref()
            .and_then(ews::timezone::resolve_time_zone)
            .or(zone);
        (timed(start_at, zone), timed(end_at, end_zone))
    };
    let uid = item
        .uid
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map_or_else(|| item.id.id.clone(), str::to_string);
    let instance = is_series_instance(item);
    let recurrence_id = instance
        .then(|| item.recurrence_id.or(item.original_start).or(item.start))
        .flatten()
        .map(|at| {
            if all_day {
                EventTime::Date {
                    date: all_day_date(at, day_zone),
                }
            } else {
                EventTime::Utc { at }
            }
        });

    let (description, description_html) = match &item.body {
        Some(body) if !body.content.trim().is_empty() => match body.body_type {
            ews::BodyType::Html => {
                let text = mime::html_to_text(&body.content);
                (
                    Some(text).filter(|t| !t.trim().is_empty()),
                    Some(body.content.clone()),
                )
            }
            _ => (Some(body.content.trim().to_string()), None),
        },
        _ => (None, None),
    };
    let location = item
        .location
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string);

    let mut attendees: Vec<Attendee> = Vec::new();
    for (list, role, cutype) in [
        (
            &item.required_attendees,
            AttendeeRole::Required,
            CalendarUserType::Individual,
        ),
        (
            &item.optional_attendees,
            AttendeeRole::Optional,
            CalendarUserType::Individual,
        ),
        (
            &item.resources,
            AttendeeRole::NonParticipant,
            CalendarUserType::Resource,
        ),
    ] {
        for a in list {
            let Some(addr) = address(&a.mailbox) else {
                continue;
            };
            let email = addr.email.to_lowercase();
            if attendees.iter().any(|x| x.email == email) {
                continue;
            }
            attendees.push(Attendee {
                email,
                name: addr.name,
                partstat: partstat(a.response),
                role,
                cutype,
                rsvp: cutype == CalendarUserType::Individual,
            });
        }
    }
    // Only the organizer's copy tracks answers; the mailbox owner's own
    // answer is in `MyResponseType`.
    let mine = match item.my_response_type {
        Some(ews::ResponseType::Accept) => Some(PartStat::Accepted),
        Some(ews::ResponseType::Tentative) => Some(PartStat::Tentative),
        Some(ews::ResponseType::Decline) => Some(PartStat::Declined),
        Some(ews::ResponseType::NoResponseReceived) => Some(PartStat::NeedsAction),
        _ => None,
    };
    if let Some(status) = mine
        && item.is_meeting != Some(false)
        && !cx.me.is_empty()
    {
        match attendees.iter_mut().find(|a| a.email == cx.me) {
            Some(a) => a.partstat = status,
            None => attendees.push(Attendee {
                email: cx.me.to_string(),
                name: None,
                partstat: status,
                role: AttendeeRole::Required,
                cutype: CalendarUserType::Individual,
                rsvp: true,
            }),
        }
    }
    let organizer = item
        .organizer
        .as_ref()
        .and_then(address)
        .map(|a| Organizer {
            email: a.email.to_lowercase(),
            name: a.name,
        });

    let mut event = Event::new(uid, start, end);
    event.recurrence_id = recurrence_id;
    event.summary = item.subject.clone().unwrap_or_default();
    event.description = description;
    event.description_html = description_html;
    event.conference_url = item
        .join_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .or_else(|| {
            location
                .as_deref()
                .and_then(envelope_ical::conference::find_meeting_url)
        })
        .or_else(|| {
            item.body
                .as_ref()
                .and_then(|b| envelope_ical::conference::find_meeting_url(&b.content))
        });
    event.location = location;
    event.rrule = if instance {
        cx.series_rule.map(str::to_string)
    } else {
        None
    };
    event.status = if item.is_cancelled == Some(true) {
        EventStatus::Cancelled
    } else {
        EventStatus::Confirmed
    };
    event.transparency = if item.free_busy == Some(ews::FreeBusyStatus::Free) {
        Transparency::Transparent
    } else {
        Transparency::Opaque
    };
    event.class = match item.sensitivity {
        Some(ews::Sensitivity::Personal | ews::Sensitivity::Private) => EventClass::Private,
        Some(ews::Sensitivity::Confidential) => EventClass::Confidential,
        _ => EventClass::Public,
    };
    event.organizer = organizer;
    event.attendees = attendees;
    event.alarms = if item.reminder_is_set == Some(true) {
        vec![Alarm {
            action: AlarmAction::Display,
            minutes_before: i64::from(item.reminder_minutes_before_start.unwrap_or(15)),
            description: None,
        }]
    } else {
        Vec::new()
    };
    event.categories = item.categories.clone();
    event.sequence = i64::from(item.sequence.unwrap_or_default());
    event.last_modified = item.last_modified;
    event
}

/// The series rule of a recurring master, as an `RRULE` value.
pub(super) fn series_rule(master: &ews::CalendarItem) -> Option<String> {
    let recurrence = master.recurrence.as_ref()?;
    Some(recurrence.to_rrule(master.start_tz(), master.is_all_day == Some(true)))
}

fn utc(time: &EventTime, default_tz: Tz) -> DateTime<Utc> {
    envelope_ical::to_utc(time, &TzContext::new(), default_tz)
}

fn zone_of(time: &EventTime, default_tz: Tz) -> Tz {
    match time {
        EventTime::Zoned { tzid, .. } => envelope_ical::resolve_tzid(tzid).unwrap_or(default_tz),
        _ => default_tz,
    }
}

/// The local date an event starts on.
pub(super) fn start_date(event: &Event, default_tz: Tz) -> NaiveDate {
    match &event.start {
        EventTime::Date { date } => *date,
        other => utc(other, default_tz)
            .with_timezone(&zone_of(other, default_tz))
            .date_naive(),
    }
}

/// The local date of an event time in `zone` (dates stay as they are).
pub(super) fn local_date(time: &EventTime, zone: Tz) -> NaiveDate {
    match time {
        EventTime::Date { date } => *date,
        other => utc(other, zone).with_timezone(&zone).date_naive(),
    }
}

/// Midnight of `date` in `tz` (the first valid instant should midnight fall
/// into a daylight saving gap).
fn midnight(date: NaiveDate, tz: Tz) -> DateTime<Utc> {
    use chrono::TimeZone;
    let local = date.and_time(chrono::NaiveTime::MIN);
    tz.from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&(local + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| local.and_utc())
}

/// Start and end instants of a timing.
pub(super) fn instants(timing: &ews::EventTiming) -> (DateTime<Utc>, DateTime<Utc>) {
    match timing {
        ews::EventTiming::Timed { start, end, .. } => (*start, *end),
        ews::EventTiming::AllDay {
            start,
            end,
            time_zone,
        } => {
            let tz = time_zone.unwrap_or(Tz::UTC);
            (midnight(*start, tz), midnight(*end, tz))
        }
    }
}

/// The timing of a series master after one of its occurrences moved from
/// `old` to `new`: the series moves by as much, with the new length.
pub(super) fn shifted_timing(
    master: &ews::CalendarItem,
    old: &Event,
    new: &Event,
    default_tz: Tz,
) -> ews::EventTiming {
    let first = master.start.unwrap_or_else(|| utc(&old.start, default_tz));
    match (&old.start, &new.start, &new.end) {
        (
            EventTime::Date { date: old_day },
            EventTime::Date { date: new_day },
            EventTime::Date { date: new_end },
        ) => {
            let zone = master.start_tz().unwrap_or(default_tz);
            let start = all_day_date(first, zone) + (*new_day - *old_day);
            ews::EventTiming::AllDay {
                start,
                end: start + (*new_end - *new_day).max(chrono::Duration::days(1)),
                time_zone: Some(default_tz),
            }
        }
        _ if new.is_all_day() => event_timing(&new.start, &new.end, default_tz),
        _ => {
            let shift = utc(&new.start, default_tz) - utc(&old.start, default_tz);
            let length = (utc(&new.end, default_tz) - utc(&new.start, default_tz))
                .max(chrono::Duration::zero());
            let start = first + shift;
            ews::EventTiming::Timed {
                start,
                end: start + length,
                time_zone: Some(zone_of(&new.start, default_tz)),
            }
        }
    }
}

/// When an event takes place, as Exchange wants it: instants plus the zone
/// the event is planned in (all-day events: days in the user's zone).
pub(super) fn event_timing(start: &EventTime, end: &EventTime, default_tz: Tz) -> ews::EventTiming {
    match start {
        EventTime::Date { date } => {
            let last = match end {
                EventTime::Date { date } => *date,
                other => utc(other, default_tz)
                    .with_timezone(&default_tz)
                    .date_naive(),
            };
            ews::EventTiming::AllDay {
                start: *date,
                end: if last > *date {
                    last
                } else {
                    date.succ_opt().unwrap_or(*date)
                },
                time_zone: Some(default_tz),
            }
        }
        _ => {
            let from = utc(start, default_tz);
            ews::EventTiming::Timed {
                start: from,
                end: utc(end, default_tz).max(from),
                time_zone: Some(zone_of(start, default_tz)),
            }
        }
    }
}

fn mailbox_of(attendee: &Attendee) -> ews::Mailbox {
    match attendee.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => ews::Mailbox::with_name(name, attendee.email.trim()),
        _ => ews::Mailbox::new(attendee.email.trim()),
    }
}

/// Required, optional and resource attendees to invite: everyone but the
/// organizer and the mailbox owner.
pub(super) fn attendee_lists(
    event: &Event,
    me: &str,
) -> (Vec<ews::Mailbox>, Vec<ews::Mailbox>, Vec<ews::Mailbox>) {
    let organizer = event.organizer.as_ref().map(|o| o.email.to_lowercase());
    let (mut required, mut optional, mut resources) = (Vec::new(), Vec::new(), Vec::new());
    for a in &event.attendees {
        let email = a.email.trim().to_lowercase();
        if email.is_empty() || email == me || organizer.as_deref() == Some(email.as_str()) {
            continue;
        }
        let mailbox = mailbox_of(a);
        match (a.role, a.cutype) {
            (_, CalendarUserType::Resource | CalendarUserType::Room)
            | (AttendeeRole::NonParticipant, _) => resources.push(mailbox),
            (AttendeeRole::Optional, _) => optional.push(mailbox),
            _ => required.push(mailbox),
        }
    }
    (required, optional, resources)
}

fn reminder_minutes(event: &Event) -> Option<u32> {
    event
        .alarms
        .iter()
        .filter(|a| a.action != AlarmAction::Email)
        .find_map(|a| u32::try_from(a.minutes_before).ok())
}

fn sensitivity(class: EventClass) -> ews::Sensitivity {
    match class {
        EventClass::Public => ews::Sensitivity::Normal,
        EventClass::Private => ews::Sensitivity::Private,
        EventClass::Confidential => ews::Sensitivity::Confidential,
    }
}

fn free_busy(transparency: Transparency) -> ews::FreeBusyStatus {
    match transparency {
        Transparency::Transparent => ews::FreeBusyStatus::Free,
        Transparency::Opaque => ews::FreeBusyStatus::Busy,
    }
}

fn description(event: &Event) -> Option<&str> {
    event
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
}

fn location(event: &Event) -> Option<String> {
    event
        .location
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

/// A repeat rule as an Exchange recurrence for a series starting on `start`.
pub(super) fn recurrence(rrule: &str, start: NaiveDate) -> Result<ews::Recurrence> {
    ews::Recurrence::from_rrule(rrule, start)
        .map_err(|e| Error::Invalid(format!("Exchange cannot repeat an event this way ({e})")))
}

/// Whether two repeat rules describe the same series starting on `start`.
/// Rewriting a series' recurrence makes Exchange drop its modified
/// occurrences, so only real changes count.
pub(super) fn same_rule(a: Option<&str>, b: Option<&str>, start: NaiveDate) -> bool {
    let parse = |rule: &str| {
        ews::Recurrence::from_rrule(rule, start).ok().map(|mut r| {
            // The week start does not move occurrences of a weekly rule.
            if let ews::RecurrencePattern::Weekly {
                first_day_of_week, ..
            } = &mut r.pattern
            {
                *first_day_of_week = None;
            }
            r
        })
    };
    let a = a.map(str::trim).filter(|r| !r.is_empty());
    let b = b.map(str::trim).filter(|r| !r.is_empty());
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.eq_ignore_ascii_case(b)
                || matches!((parse(a), parse(b)), (Some(x), Some(y)) if x == y)
        }
        _ => false,
    }
}

/// A new calendar item for an event. With `invite`, the attendees are
/// invited (the mailbox owner organizes the meeting); otherwise the item is
/// a plain appointment.
pub(super) fn new_calendar_item(
    event: &Event,
    me: &str,
    invite: bool,
    default_tz: Tz,
) -> Result<ews::NewCalendarItem> {
    let mut item = ews::NewCalendarItem::new(
        event.summary.clone(),
        event_timing(&event.start, &event.end, default_tz),
    );
    item.body = description(event).map(ews::Body::text);
    item.location = location(event);
    if invite {
        let (required, optional, resources) = attendee_lists(event, me);
        item.required_attendees = required;
        item.optional_attendees = optional;
        item.resources = resources;
    }
    item.free_busy = Some(free_busy(event.transparency));
    item.reminder_minutes = reminder_minutes(event);
    item.categories = event.categories.clone();
    item.sensitivity = Some(sensitivity(event.class));
    item.recurrence = event
        .rrule
        .as_deref()
        .map(|rule| recurrence(rule, start_date(event, default_tz)))
        .transpose()?;
    item.uid = Some(event.uid.clone()).filter(|u| !u.trim().is_empty());
    Ok(item)
}

/// Whether two events take place at different times.
pub(super) fn times_differ(old: &Event, new: &Event, default_tz: Tz) -> bool {
    if old.is_all_day() != new.is_all_day() {
        return true;
    }
    if old.is_all_day() {
        return old.start != new.start || old.end != new.end;
    }
    utc(&old.start, default_tz) != utc(&new.start, default_tz)
        || utc(&old.end, default_tz) != utc(&new.end, default_tz)
}

fn emails(list: &[ews::Mailbox]) -> Vec<String> {
    let mut out: Vec<String> = list
        .iter()
        .filter_map(|m| m.email.as_deref())
        .map(|e| e.trim().to_lowercase())
        .collect();
    out.sort();
    out
}

/// The property changes from `old` to `new` (timing and recurrence are left
/// to the caller, which knows whether a series or one occurrence changes).
pub(super) fn calendar_update(old: &Event, new: &Event, me: &str) -> ews::CalendarItemUpdate {
    let mut update = ews::CalendarItemUpdate::default();
    if new.summary.trim() != old.summary.trim() {
        update.subject = Some(new.summary.clone());
    }
    if description(new) != description(old) {
        update.body = Some(ews::Body::text(description(new).unwrap_or_default()));
    }
    if location(new) != location(old) {
        update.location = Some(location(new));
    }
    let (required, optional, resources) = attendee_lists(new, me);
    let (old_required, old_optional, old_resources) = attendee_lists(old, me);
    if emails(&required) != emails(&old_required) {
        update.required_attendees = Some(required);
    }
    if emails(&optional) != emails(&old_optional) {
        update.optional_attendees = Some(optional);
    }
    if emails(&resources) != emails(&old_resources) {
        update.resources = Some(resources);
    }
    if new.transparency != old.transparency {
        update.free_busy = Some(free_busy(new.transparency));
    }
    if reminder_minutes(new) != reminder_minutes(old) {
        update.reminder_minutes = Some(reminder_minutes(new));
    }
    if new.categories != old.categories {
        update.categories = Some(new.categories.clone());
    }
    if new.class != old.class {
        update.sensitivity = Some(sensitivity(new.class));
    }
    update
}

/// Exchange's answer for an RSVP status.
pub(super) fn meeting_response(status: PartStat) -> ews::MeetingResponse {
    match status {
        PartStat::Accepted => ews::MeetingResponse::Accept,
        PartStat::Declined => ews::MeetingResponse::Decline,
        _ => ews::MeetingResponse::Tentative,
    }
}

// ---------------------------------------------------------------------------
// Raw requests the client library does not wrap

/// Escapes text for an XML attribute or element.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn extended_property(tag: u16, kind: &str, value: &str) -> String {
    format!(
        "<t:SetItemField><t:ExtendedFieldURI PropertyTag=\"0x{tag:04x}\" PropertyType=\"{kind}\"/>\
         <t:Message><t:ExtendedProperty><t:ExtendedFieldURI PropertyTag=\"0x{tag:04x}\" PropertyType=\"{kind}\"/>\
         <t:Value>{}</t:Value></t:ExtendedProperty></t:Message></t:SetItemField>",
        escape(value)
    )
}

/// Records a reply or forward the way Outlook does (last verb executed, its
/// time and the matching icon), so other Outlook clients show it.
pub(super) fn last_verb_request(
    item_ids: &[String],
    forwarded: bool,
    now: DateTime<Utc>,
) -> String {
    let (verb, icon) = if forwarded { (104, 262) } else { (102, 261) };
    let when = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut xml = String::from(
        "<m:UpdateItem MessageDisposition=\"SaveOnly\" ConflictResolution=\"AlwaysOverwrite\"><m:ItemChanges>",
    );
    for id in item_ids {
        xml.push_str(&format!(
            "<t:ItemChange><t:ItemId Id=\"{}\"/><t:Updates>{}{}{}</t:Updates></t:ItemChange>",
            escape(id),
            extended_property(0x1081, "Integer", &verb.to_string()),
            extended_property(0x1080, "Integer", &icon.to_string()),
            extended_property(0x1082, "SystemTime", &when),
        ));
    }
    xml.push_str("</m:ItemChanges></m:UpdateItem>");
    xml
}

/// Moves one occurrence of a series to new times. Only the instants change;
/// the series keeps its time zone.
pub(super) fn occurrence_times_request(
    item_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    notify: bool,
) -> String {
    let send = if notify {
        "SendToAllAndSaveCopy"
    } else {
        "SendToNone"
    };
    let field = |name: &str, at: DateTime<Utc>| {
        format!(
            "<t:SetItemField><t:FieldURI FieldURI=\"calendar:{name}\"/><t:CalendarItem><t:{name}>{}</t:{name}></t:CalendarItem></t:SetItemField>",
            ews::format_datetime(at)
        )
    };
    format!(
        "<m:UpdateItem ConflictResolution=\"AlwaysOverwrite\" SendMeetingInvitationsOrCancellations=\"{send}\">\
         <m:ItemChanges><t:ItemChange><t:ItemId Id=\"{}\"/><t:Updates>{}{}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>",
        escape(item_id),
        field("Start", start),
        field("End", end)
    )
}

/// The response class and code of the first response message of a raw
/// response, when it reports an error.
pub(super) fn raw_error(response: &str) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event as Xml;
    let mut reader = Reader::from_str(response);
    let mut error = false;
    loop {
        match reader.read_event() {
            Ok(Xml::Start(e)) if e.local_name().as_ref().ends_with("ResponseMessage") => {
                error = e.attributes().flatten().any(|a| {
                    a.key.local_name().as_ref() == "ResponseClass"
                        && a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .is_ok_and(|v| v == "Error")
                });
            }
            Ok(Xml::Start(e)) if error && e.local_name().as_ref() == "ResponseCode" => {
                return element_text_from(&mut reader, "ResponseCode")
                    .or_else(|| Some("Error".into()));
            }
            Ok(Xml::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

fn element_text_from(reader: &mut quick_xml::Reader<&[u8]>, local_name: &str) -> Option<String> {
    use quick_xml::events::Event as Xml;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Xml::Text(t)) => text.push_str(&t.xml10_content()),
            Ok(Xml::End(e)) if e.local_name().as_ref() == local_name => {
                return Some(text.trim().to_string());
            }
            Ok(Xml::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}
