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
}

fn kind_of(path: &Path, data: &[u8]) -> Option<Kind> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "eml" | "mht" | "mhtml" => return Some(Kind::Message),
        "ics" | "ical" | "icalendar" | "ifb" | "vcs" => return Some(Kind::Calendar),
        _ => {}
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(4096)]);
    let head = head.trim_start_matches('\u{feff}').trim_start();
    if head.to_ascii_uppercase().starts_with("BEGIN:VCALENDAR") {
        return Some(Kind::Calendar);
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
            html = html.replace(&needle, &url);
        }
    }
    html
}

/// A copy of an imported event that cannot send anything: the guests and
/// organizer are dropped (a calendar server would otherwise mail them
/// invitations).
fn importable(mut event: Event, tz: &envelope_ical::TzContext) -> Event {
    event.attendees.clear();
    event.organizer = None;
    utc_where_unknown(&mut event, tz);
    event
}

/// Times in zones without an IANA name become UTC, so that they keep their
/// instant wherever the event is stored.
pub(crate) fn utc_where_unknown(event: &mut Event, tz: &envelope_ical::TzContext) {
    let fix = |time: &mut EventTime| {
        if let EventTime::Zoned { tzid, .. } = time
            && tzid.parse::<chrono_tz::Tz>().is_err()
        {
            *time = EventTime::Utc {
                at: envelope_ical::to_utc(time, tz, local_tz()),
            };
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
    /// Reads a message or calendar file.
    pub async fn open_file(&self, path: &Path) -> Result<OpenedFile> {
        let data = read(path).await?;
        let display = path.to_string_lossy().into_owned();
        match kind_of(path, &data) {
            Some(Kind::Message) => {
                let (meta, body) = mime::parse_full(&data)
                    .ok_or_else(|| Error::Invalid("This file is not a readable message.".into()))?;
                let html = body
                    .html
                    .as_deref()
                    .map(|h| embed_inline_parts(h, &data, &body.attachments));
                Ok(OpenedFile::Message(FileMessage {
                    path: display,
                    subject: meta.subject,
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
                        .filter(|a| !a.inline)
                        .map(|a| FileAttachment {
                            part_id: a.part_id,
                            filename: a.filename,
                            mime_type: a.mime_type,
                            size: a.size,
                        })
                        .collect(),
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
                    let exists = !self
                        .db
                        .read(|c| cdb::rows_by_uid(c, &event.uid))?
                        .is_empty();
                    events.push(FileEvent {
                        uid: event.uid.clone(),
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
            None => Err(Error::Invalid(
                "Envelope opens saved messages (.eml) and calendar files (.ics).".into(),
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
            if uids.as_ref().is_some_and(|u| !u.contains(&event.uid)) {
                continue;
            }
            if !self
                .db
                .read(|c| cdb::rows_by_uid(c, &event.uid))?
                .is_empty()
            {
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

    /// Writes an attachment of a message file to the cache and returns its
    /// path, for opening or saving it.
    pub async fn file_part(&self, path: &Path, part_id: &str) -> Result<PathBuf> {
        let data = read(path).await?;
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
        let event = importable(events[2].clone(), &data.tz);
        assert!(matches!(&event.start, EventTime::Zoned { tzid, .. } if tzid == "Europe/Istanbul"));
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
