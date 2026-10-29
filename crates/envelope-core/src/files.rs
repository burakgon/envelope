//! Opening saved messages (`.eml`) and calendar files (`.ics`) handed to the
//! app by the file manager.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use envelope_ical::{Event, EventTime};
use serde::Serialize;

use crate::calendar::local_tz;
use crate::db::calendar as cdb;
use crate::db::mail;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mime;
use crate::model::{Address, EngineEvent};

/// Files larger than this are not opened (a message this size would not
/// be accepted by any mail server either).
const MAX_FILE: u64 = 200 * 1024 * 1024;

/// What a file turned out to contain.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OpenedFile {
    Message(FileMessage),
    Calendar(FileCalendar),
    Contacts(FileContacts),
}

/// A message read from a file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMessage {
    pub path: String,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub date: i64,
    /// Inline pictures are embedded as `data:` URLs.
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<FileAttachment>,
    /// The message was encrypted and is shown decrypted.
    pub encrypted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachment {
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
}

/// The events of a calendar file, ready to be added to a calendar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCalendar {
    pub path: String,
    /// `X-WR-CALNAME`, when the file names its calendar.
    pub name: Option<String>,
    pub events: Vec<FileEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEvent {
    pub uid: String,
    pub summary: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub location: Option<String>,
    pub recurring: bool,
    /// Already on one of the user's calendars.
    pub exists: bool,
}

/// The people in a vCard file, ready to be added to an address book.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContacts {
    pub path: String,
    pub contacts: Vec<FileContact>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContact {
    /// Position in the file, for choosing which to add.
    pub index: usize,
    pub name: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    /// Someone with this address is already in an address book.
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub added: usize,
    /// Events that were already on a calendar.
    pub skipped: usize,
}

enum Kind {
    Message,
    Calendar,
    Contacts,
}

fn kind_of(path: &Path, data: &[u8]) -> Option<Kind> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "eml" | "mht" | "mhtml" => return Some(Kind::Message),
        "ics" | "ical" | "icalendar" | "ifb" | "vcs" => return Some(Kind::Calendar),
        // Address book exports (Gmail, Outlook, Thunderbird).
        "vcf" | "vcard" | "csv" => return Some(Kind::Contacts),
        _ => {}
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(4096)]);
    let head = head.trim_start_matches('\u{feff}').trim_start();
    let upper = head.to_ascii_uppercase();
    if upper.starts_with("BEGIN:VCALENDAR") {
        return Some(Kind::Calendar);
    }
    if upper.starts_with("BEGIN:VCARD") {
        return Some(Kind::Contacts);
    }
    // A message starts with header lines.
    let first = head.lines().next().unwrap_or("");
    let looks_like_header = first
        .split_once(':')
        .is_some_and(|(name, _)| !name.is_empty() && !name.contains(' '));
    looks_like_header.then_some(Kind::Message)
}

async fn read(path: &Path) -> Result<Vec<u8>> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| Error::NotFound(format!("{} could not be opened.", path.display())))?;
    if meta.len() > MAX_FILE {
        return Err(Error::Invalid("This file is too large to open.".into()));
    }
    Ok(tokio::fs::read(path).await?)
}

/// Replaces `cid:` references to the message's own parts with `data:` URLs.
fn embed_inline_parts(html: &str, raw: &[u8], parts: &[mail::AttachmentMeta]) -> String {
    let mut html = html.to_string();
    for part in parts {
        let Some(cid) = &part.content_id else {
            continue;
        };
        if !part.mime_type.starts_with("image/") {
            continue;
        }
        let needle = format!("cid:{cid}");
        if !html.contains(&needle) {
            continue;
        }
        if let Some(bytes) = mime::extract_part(raw, &part.part_id) {
            let url = format!(
                "data:{};base64,{}",
                part.mime_type,
                base64::engine::general_purpose::STANDARD.encode(bytes)
            );
            html = replace_reference(&html, &needle, &url);
        }
    }
    html
}

/// Replaces `needle` where it is a whole reference: not where it only
/// begins a longer one (`cid:part1` inside `cid:part10`).
fn replace_reference(html: &str, needle: &str, with: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(needle) {
        let after = rest[i + needle.len()..].chars().next();
        let whole = !after.is_some_and(|c| c.is_alphanumeric() || "._@-$%+=".contains(c));
        out.push_str(&rest[..i]);
        out.push_str(if whole { with } else { needle });
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// A copy of an imported event that cannot send anything: the guests and
/// organizer are dropped (a calendar server would otherwise mail them
/// invitations).
fn importable(mut event: Event, tz: &envelope_ical::TzContext) -> Event {
    // A changed occurrence without its series is an event of its own.
    event.uid = import_uid(&event, tz);
    event.recurrence_id = None;
    event.attendees.clear();
    event.organizer = None;
    utc_where_unknown(&mut event, tz);
    event
}

/// The UID an event is added under: its own, or for a lone changed
/// occurrence one of its own (several can share their series' UID).
fn import_uid(event: &Event, tz: &envelope_ical::TzContext) -> String {
    match &event.recurrence_id {
        Some(rid) => format!(
            "{}-{}",
            event.uid,
            envelope_ical::to_utc(rid, tz, local_tz()).timestamp()
        ),
        None => event.uid.clone(),
    }
}

/// Times in zones without an IANA name become UTC, so that they keep their
/// instant wherever the event is stored.
pub(crate) fn utc_where_unknown(event: &mut Event, tz: &envelope_ical::TzContext) {
    let fix = |time: &mut EventTime| {
        if let EventTime::Zoned { tzid, .. } = time
            && tzid.parse::<chrono_tz::Tz>().is_err()
        {
            // A known zone under another name keeps its clock changes.
            if let Some(zone) = envelope_ical::resolve_tzid(tzid) {
                *tzid = zone.name().to_owned();
            } else {
                *time = EventTime::Utc {
                    at: envelope_ical::to_utc(time, tz, local_tz()),
                };
            }
        }
    };
    fix(&mut event.start);
    fix(&mut event.end);
}

/// The events of a calendar file worth adding: series and single events,
/// plus changed occurrences whose series is not in the file.
fn file_events(data: &envelope_ical::CalendarData) -> Vec<&Event> {
    data.events
        .iter()
        .filter(|e| {
            e.recurrence_id.is_none()
                || !data
                    .events
                    .iter()
                    .any(|m| m.uid == e.uid && m.recurrence_id.is_none())
        })
        .collect()
}

impl Engine {
    /// Whether an event from a file is already on one of the user's own
    /// calendars (subscribed feeds do not count).
    fn already_on_calendar(&self, event: &Event, tz: &envelope_ical::TzContext) -> Result<bool> {
        let subscribed: std::collections::HashSet<i64> = self
            .db
            .read(cdb::list_calendars)?
            .into_iter()
            .filter(|c| c.kind == "subscription")
            .map(|c| c.id)
            .collect();
        let own = |uid: &str| -> Result<Vec<cdb::EventRow>> {
            Ok(self
                .db
                .read(|c| cdb::rows_by_uid(c, uid))?
                .into_iter()
                .filter(|r| !subscribed.contains(&r.calendar_id))
                .collect())
        };
        if !own(&import_uid(event, tz))?.is_empty() {
            return Ok(true);
        }
        // A lone occurrence whose whole series is already there.
        Ok(event.recurrence_id.is_some()
            && own(&event.uid)?.iter().any(|r| r.recurrence_id.is_none()))
    }

    /// Reads a message or calendar file.
    pub async fn open_file(&self, path: &Path) -> Result<OpenedFile> {
        let data = read(path).await?;
        let display = path.to_string_lossy().into_owned();
        match kind_of(path, &data) {
            Some(Kind::Message) => {
                let unreadable = || Error::Invalid("This file is not a readable message.".into());
                let (meta, outer_body) = mime::parse_full(&data).ok_or_else(unreadable)?;
                // Encrypted mail is shown from its decrypted content, whose
                // protected headers carry the real subject.
                let decrypted = self.decrypt_file_message(&data).await?;
                let content = decrypted.as_deref().unwrap_or(&data);
                let (inner, body) = match &decrypted {
                    Some(d) => mime::parse_full(d).ok_or_else(unreadable)?,
                    None => (meta.clone(), outer_body),
                };
                let html = body
                    .html
                    .as_deref()
                    .map(|h| embed_inline_parts(h, content, &body.attachments));
                let subject = if decrypted.is_some() && !inner.subject.is_empty() {
                    inner.subject
                } else {
                    meta.subject
                };
                Ok(OpenedFile::Message(FileMessage {
                    path: display,
                    subject,
                    from: meta.from,
                    to: meta.to,
                    cc: meta.cc,
                    reply_to: meta.reply_to,
                    date: meta.date,
                    html,
                    text: body.text,
                    attachments: body
                        .attachments
                        .into_iter()
                        .filter(|a| !a.inline && !envelope_smime::mime::is_smime_type(&a.mime_type))
                        .map(|a| FileAttachment {
                            part_id: a.part_id,
                            filename: a.filename,
                            mime_type: a.mime_type,
                            size: a.size,
                        })
                        .collect(),
                    // Opaque S/MIME signed data is unwrapped too, but not
                    // encrypted.
                    encrypted: decrypted.is_some()
                        && crate::crypto::Protection::detect(&data)
                            .is_some_and(crate::crypto::Protection::is_encrypted),
                }))
            }
            Some(Kind::Calendar) => {
                let cal = envelope_ical::parse_calendar_bytes(&data)
                    .map_err(|_| Error::Invalid("This file has no calendar events.".into()))?;
                let name = cal
                    .components
                    .iter()
                    .find(|c| c.is("VCALENDAR"))
                    .and_then(|c| c.value("X-WR-CALNAME"))
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty());
                let mut events = Vec::new();
                for event in file_events(&cal) {
                    let exists = self.already_on_calendar(event, &cal.tz)?;
                    events.push(FileEvent {
                        uid: import_uid(event, &cal.tz),
                        summary: event.summary.clone(),
                        start: envelope_ical::to_utc(&event.start, &cal.tz, local_tz())
                            .timestamp_millis(),
                        end: envelope_ical::to_utc(&event.end, &cal.tz, local_tz())
                            .timestamp_millis(),
                        all_day: event.is_all_day(),
                        location: event.location.clone(),
                        recurring: event.is_recurring(),
                        exists,
                    });
                }
                if events.is_empty() {
                    return Err(Error::Invalid("This file has no calendar events.".into()));
                }
                Ok(OpenedFile::Calendar(FileCalendar {
                    path: display,
                    name,
                    events,
                }))
            }
            Some(Kind::Contacts) => {
                let contacts = file_contacts(&data)?;
                let known = self.contacts_known(&contacts)?;
                Ok(OpenedFile::Contacts(FileContacts {
                    path: display,
                    contacts: contacts
                        .iter()
                        .zip(known)
                        .enumerate()
                        .map(|(index, (c, exists))| FileContact {
                            index,
                            name: crate::contacts::display_name(c),
                            email: c.emails.first().map(|e| e.value.clone()),
                            organization: c.organization.clone(),
                            exists,
                        })
                        .collect(),
                }))
            }
            None => Err(Error::Invalid(
                "Envelope opens saved messages (.eml), calendar files (.ics), contact cards (.vcf) and contact lists (.csv).".into(),
            )),
        }
    }

    /// Adds the events of a calendar file to a calendar, skipping those
    /// already on one. With `uids`, only those events are added.
    pub async fn import_calendar_file(
        &self,
        path: &Path,
        calendar_id: i64,
        uids: Option<Vec<String>>,
    ) -> Result<ImportResult> {
        let data = read(path).await?;
        let cal = envelope_ical::parse_calendar_bytes(&data)
            .map_err(|_| Error::Invalid("This file has no calendar events.".into()))?;
        let target = self.db.read(|c| cdb::get_calendar(c, calendar_id))?;
        if target.read_only {
            return Err(Error::Invalid("This calendar is read-only.".into()));
        }
        let mut result = ImportResult {
            added: 0,
            skipped: 0,
        };
        for event in file_events(&cal) {
            if uids
                .as_ref()
                .is_some_and(|u| !u.contains(&import_uid(event, &cal.tz)))
            {
                continue;
            }
            if self.already_on_calendar(event, &cal.tz)? {
                result.skipped += 1;
                continue;
            }
            let event = importable(event.clone(), &cal.tz);
            crate::sync::calendar::write_create(self, &target, &event, false).await?;
            result.added += 1;
        }
        if result.added > 0 {
            self.emit(EngineEvent::CalendarChanged {
                calendar_ids: vec![calendar_id],
            });
        }
        Ok(result)
    }

    /// Adds the people of a vCard file (or only those at `indices`) to an
    /// address book, leaving out the ones already known.
    pub async fn import_contacts_file(
        &self,
        path: &Path,
        book_id: i64,
        indices: Option<Vec<usize>>,
    ) -> Result<ImportResult> {
        let data = read(path).await?;
        let chosen: Vec<_> = file_contacts(&data)?
            .into_iter()
            .enumerate()
            .filter(|(i, _)| indices.as_ref().is_none_or(|wanted| wanted.contains(i)))
            .map(|(_, c)| c)
            .collect();
        let (added, skipped) = self.import_contacts(book_id, chosen).await?;
        Ok(ImportResult { added, skipped })
    }

    /// Writes an attachment of a message file to the cache and returns its
    /// path, for opening or saving it.
    pub async fn file_part(&self, path: &Path, part_id: &str) -> Result<PathBuf> {
        let mut data = read(path).await?;
        if let Some(decrypted) = self.decrypt_file_message(&data).await? {
            data = decrypted;
        }
        let (_, body) = mime::parse_full(&data)
            .ok_or_else(|| Error::Invalid("This file is not a readable message.".into()))?;
        let part = body
            .attachments
            .iter()
            .find(|a| a.part_id == part_id)
            .ok_or_else(|| Error::NotFound("attachment".into()))?;
        let bytes = mime::extract_part(&data, part_id)
            .ok_or_else(|| Error::NotFound("attachment".into()))?;
        let name: String = part
            .filename
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | '\0') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let dir = self
            .paths
            .cache
            .join("files")
            .join(format!(
                "{:016x}",
                fxhash(path.to_string_lossy().as_bytes())
            ))
            .join(part_id.replace(':', "-"));
        tokio::fs::create_dir_all(&dir).await?;
        let out = dir.join(if name.trim().is_empty() {
            "attachment".to_string()
        } else {
            name
        });
        tokio::fs::write(&out, bytes).await?;
        Ok(out)
    }
}

/// The people of a vCard file (contact groups left out) or of a CSV export.
fn file_contacts(data: &[u8]) -> Result<Vec<envelope_ical::Contact>> {
    // Unfolded, with older cards' legacy character sets decoded.
    let text = envelope_ical::decode_bytes(data);
    let mut people = Vec::new();
    if !text.to_ascii_uppercase().contains("BEGIN:VCARD") {
        let text = crate::contacts::csv::decode(data);
        if crate::contacts::csv::looks_like_contacts(&text) {
            people = crate::contacts::csv::parse(&text);
        }
    }
    for card in split_vcards(&text) {
        if let Ok((contact, kind)) = envelope_ical::vcard::parse_vcard_with_kind(card)
            && !kind.is_some_and(|k| k.eq_ignore_ascii_case("group"))
        {
            people.push(contact);
        }
    }
    if people.is_empty() {
        return Err(Error::Invalid("This file has no contacts.".into()));
    }
    Ok(people)
}

/// The `BEGIN:VCARD` … `END:VCARD` blocks of a file.
fn split_vcards(text: &str) -> Vec<&str> {
    let upper = text.to_ascii_uppercase();
    let mut cards = Vec::new();
    let mut from = 0;
    while let Some(start) = upper[from..].find("BEGIN:VCARD").map(|i| from + i) {
        let Some(end) = upper[start..]
            .find("END:VCARD")
            .map(|i| start + i + "END:VCARD".len())
        else {
            break;
        };
        cards.push(&text[start..end]);
        from = end;
    }
    cards
}

/// A stable 64-bit FNV-1a hash, to give each opened file its own folder.
fn fxhash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_files_by_name_and_content() {
        assert!(matches!(
            kind_of(Path::new("a.EML"), b""),
            Some(Kind::Message)
        ));
        assert!(matches!(
            kind_of(Path::new("invite"), b"\xef\xbb\xbfBEGIN:VCALENDAR\r\n"),
            Some(Kind::Calendar)
        ));
        assert!(matches!(
            kind_of(
                Path::new("saved"),
                b"Return-Path: <a@b.c>\r\nSubject: x\r\n\r\nhi"
            ),
            Some(Kind::Message)
        ));
        assert!(kind_of(Path::new("notes.txt"), b"just some words").is_none());
    }

    #[test]
    fn imported_events_send_no_invitations() {
        let data = envelope_ical::parse_calendar(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m1\r\n\
             DTSTART;TZID=Custom Zone:20261102T090000\r\nDTEND;TZID=Custom Zone:20261102T100000\r\n\
             SUMMARY:Planning\r\nORGANIZER:mailto:boss@example.com\r\n\
             ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r\n\
             RRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:m1\r\nRECURRENCE-ID:20261109T060000Z\r\n\
             DTSTART:20261109T070000Z\r\nDTEND:20261109T080000Z\r\nSUMMARY:Planning (moved)\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:o1\r\nRECURRENCE-ID:20261110T060000Z\r\n\
             DTSTART:20261110T070000Z\r\nDTEND:20261110T080000Z\r\nSUMMARY:One of a series\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:z1\r\nDTSTART;TZID=Europe/Istanbul:20261103T090000\r\n\
             DTEND;TZID=Europe/Istanbul:20261103T100000\r\nSUMMARY:Review\r\nEND:VEVENT\r\n\
             END:VCALENDAR\r\n",
        )
        .unwrap();
        let events = file_events(&data);
        // A changed occurrence of a series in the file is left out (the series is
        // added as planned); a lone occurrence is added on its own.
        assert_eq!(
            events.iter().map(|e| e.uid.as_str()).collect::<Vec<_>>(),
            ["m1", "o1", "z1"]
        );
        let event = importable(events[0].clone(), &data.tz);
        assert!(event.attendees.is_empty() && event.organizer.is_none());
        assert!(matches!(event.start, EventTime::Utc { .. }));
        assert!(matches!(event.end, EventTime::Utc { .. }));
        assert_eq!(event.rrule.as_deref(), Some("FREQ=WEEKLY"));
        // The lone occurrence becomes an event of its own.
        let lone = importable(events[1].clone(), &data.tz);
        assert!(lone.uid.starts_with("o1-") && lone.recurrence_id.is_none());
        let event = importable(events[2].clone(), &data.tz);
        assert!(matches!(&event.start, EventTime::Zoned { tzid, .. } if tzid == "Europe/Istanbul"));
    }

    #[test]
    fn replaces_whole_references_only() {
        assert_eq!(
            replace_reference(
                r#"<img src="cid:part1"><img src="cid:part10">"#,
                "cid:part1",
                "data:x"
            ),
            r#"<img src="data:x"><img src="cid:part10">"#
        );
        assert_eq!(
            replace_reference("url(cid:a@b)", "cid:a@b", "data:y"),
            "url(data:y)"
        );
    }

    #[test]
    fn inline_pictures_become_data_urls() {
        let raw = b"From: a@example.com\r\nSubject: Pic\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n\
<p><img src=\"cid:logo@x\"></p>\r\n--b\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\n\
Content-Transfer-Encoding: base64\r\nContent-Disposition: inline\r\n\r\niVBORw0KGgo=\r\n--b--\r\n";
        let (_, body) = mime::parse_full(raw).unwrap();
        let html = embed_inline_parts(body.html.as_deref().unwrap(), raw, &body.attachments);
        assert!(
            html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
            "{html}"
        );
    }
}
