//! Address books and contacts for the UI: listing, searching, reading and
//! editing.
//!
//! Edits are stored locally first and written to the server in the
//! background by [`crate::sync::contacts`], so they never wait on the network
//! and work offline.

use std::collections::HashSet;

use base64::Engine as _;
use chrono::{Datelike, NaiveDate};
use envelope_ical::vcard::NO_YEAR;
use envelope_ical::{Contact, ContactAddress, ContactField};
use serde::{Deserialize, Serialize};

use crate::db::contacts::{self as cdb, ContactRow, Photo, PhotoEdit};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::EngineEvent;

pub mod csv;

/// Name of the address book used when no account can store contacts.
pub const LOCAL_BOOK_NAME: &str = "On this computer";

/// Side of the square list thumbnails, in pixels.
const THUMB_SIZE: u32 = 72;
/// Largest photo accepted from the editor.
const MAX_PHOTO_BYTES: usize = 10 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactBook {
    pub id: i64,
    pub account_id: Option<i64>,
    /// `carddav`, `google`, `graph` or `local`.
    pub kind: String,
    pub name: String,
    pub read_only: bool,
}

/// One row of the contact list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactSummary {
    pub id: i64,
    pub book_id: i64,
    pub name: String,
    /// Primary e-mail address.
    pub email: Option<String>,
    pub organization: Option<String>,
    /// Small `data:` URL thumbnail.
    pub photo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactPage {
    pub total: i64,
    pub items: Vec<ContactSummary>,
}

/// An e-mail address, phone number or postal address with its label
/// (`home`, `work`, `mobile`, `other` or free text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabeledValue {
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// A contact as the UI shows and edits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactView {
    pub id: i64,
    pub book_id: i64,
    pub name: String,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub emails: Vec<LabeledValue>,
    pub phones: Vec<LabeledValue>,
    /// Multi-line postal addresses.
    pub addresses: Vec<LabeledValue>,
    pub urls: Vec<String>,
    pub organization: Option<String>,
    pub title: Option<String>,
    /// `YYYY-MM-DD`, or `--MM-DD` without a year.
    pub birthday: Option<String>,
    pub notes: Option<String>,
    /// A `data:` URL.
    pub photo: Option<String>,
}

/// A contact to create (`id` is `None`) or update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContactInput {
    pub id: Option<i64>,
    pub book_id: i64,
    pub name: String,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub emails: Vec<LabeledValue>,
    pub phones: Vec<LabeledValue>,
    pub addresses: Vec<LabeledValue>,
    pub urls: Vec<String>,
    pub organization: Option<String>,
    pub title: Option<String>,
    pub birthday: Option<String>,
    pub notes: Option<String>,
    pub photo: Option<String>,
}

// ---------------------------------------------------------------------------
// Mapping between the UI and the vCard model

/// The name to show for a contact: its formatted name, else the structured
/// name, organization, first e-mail address or phone number.
pub fn display_name(contact: &Contact) -> String {
    let name = contact.name.trim();
    if !name.is_empty() {
        return name.to_owned();
    }
    let joined = [&contact.given_name, &contact.family_name]
        .into_iter()
        .flatten()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !joined.is_empty() {
        return joined;
    }
    contact
        .organization
        .iter()
        .map(|o| o.trim().to_owned())
        .chain(contact.emails.iter().map(|e| e.value.trim().to_owned()))
        .chain(contact.phones.iter().map(|p| p.value.trim().to_owned()))
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "Unnamed".to_owned())
}

/// The UI label of a vCard-style kind (`cell` → `mobile`).
pub fn kind_to_label(kind: Option<&str>) -> Option<String> {
    let kind = kind.map(str::trim).filter(|k| !k.is_empty())?;
    Some(match kind.to_ascii_lowercase().as_str() {
        "cell" | "mobile" | "iphone" => "mobile".to_owned(),
        "home" | "work" | "other" => kind.to_ascii_lowercase(),
        _ => kind.to_owned(),
    })
}

/// The vCard-style kind of a UI label (`mobile` → `cell`).
pub fn label_to_kind(label: Option<&str>) -> Option<String> {
    let label = label.map(str::trim).filter(|l| !l.is_empty())?;
    Some(match label.to_ascii_lowercase().as_str() {
        "mobile" | "cell" => "cell".to_owned(),
        known @ ("home" | "work" | "other" | "fax" | "pager" | "main") => known.to_owned(),
        _ => label.to_owned(),
    })
}

/// `YYYY-MM-DD`, or `--MM-DD` for birthdays without a year.
pub fn format_birthday(date: NaiveDate) -> String {
    if date.year() == NO_YEAR {
        format!("--{:02}-{:02}", date.month(), date.day())
    } else {
        date.format("%Y-%m-%d").to_string()
    }
}

/// Reads a birthday typed by the user: `YYYY-MM-DD` (any separator or none),
/// `DD.MM.YYYY`, or a month and day without a year (`--MM-DD`, `MM-DD`).
pub fn parse_birthday(text: &str) -> Result<Option<NaiveDate>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let invalid = || {
        Error::Invalid(format!(
            "“{text}” is not a date. Write the birthday as YYYY-MM-DD, or --MM-DD without the year."
        ))
    };
    let parts: Vec<&str> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .collect();
    let num = |s: &str| s.parse::<u32>().map_err(|_| invalid());
    let (year, month, day) = match parts.as_slice() {
        [all] if all.len() == 8 => (
            all[..4].parse::<i32>().map_err(|_| invalid())?,
            num(&all[4..6])?,
            num(&all[6..])?,
        ),
        [md] if md.len() == 4 && text.starts_with("--") => {
            (NO_YEAR, num(&md[..2])?, num(&md[2..])?)
        }
        [y, m, d] if y.len() == 4 => (y.parse().map_err(|_| invalid())?, num(m)?, num(d)?),
        [d, m, y] if y.len() == 4 && text.contains('.') => {
            (y.parse().map_err(|_| invalid())?, num(m)?, num(d)?)
        }
        [m, d] if m.len() <= 2 && d.len() <= 2 => (NO_YEAR, num(m)?, num(d)?),
        _ => return Err(invalid()),
    };
    if year <= 0 {
        return Err(invalid());
    }
    NaiveDate::from_ymd_opt(year, month, day)
        .map(Some)
        .ok_or_else(invalid)
}

/// A `data:` URL for a photo.
pub fn data_url(photo: &Photo) -> String {
    format!(
        "data:{};base64,{}",
        photo.mime,
        base64::engine::general_purpose::STANDARD.encode(&photo.bytes)
    )
}

/// Decodes a base64 `data:image/…` URL.
pub fn parse_data_url(url: &str) -> Option<Photo> {
    let rest = url.trim().strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mut meta_parts = meta.split(';');
    let mime = meta_parts.next()?.trim().to_ascii_lowercase();
    if !mime.starts_with("image/") || !meta_parts.any(|p| p.trim().eq_ignore_ascii_case("base64")) {
        return None;
    }
    let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(compact.trim_end_matches('='))
        })
        .ok()?;
    (!bytes.is_empty()).then_some(Photo { bytes, mime })
}

/// MIME type of image bytes from their signature.
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    Some(match bytes {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'B', b'M', ..] => "image/bmp",
        _ => return None,
    })
}

/// A small square JPEG `data:` URL of a photo for lists, or `None` when the
/// image cannot be decoded.
pub fn thumbnail(bytes: &[u8]) -> Option<String> {
    use image::ImageEncoder as _;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10_000);
    limits.max_image_height = Some(10_000);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let (w, h) = (image.width(), image.height());
    let side = w.min(h);
    if side == 0 {
        return None;
    }
    let square = image.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let small = square.thumbnail_exact(THUMB_SIZE, THUMB_SIZE).to_rgba8();
    // JPEG has no transparency: put transparent pixels on white.
    let mut rgb = Vec::with_capacity((small.width() * small.height() * 3) as usize);
    for pixel in small.pixels() {
        let [r, g, b, a] = pixel.0;
        for channel in [r, g, b] {
            let blended = (u32::from(channel) * u32::from(a) + 255 * (255 - u32::from(a))) / 255;
            rgb.push(blended as u8);
        }
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .write_image(
            &rgb,
            small.width(),
            small.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(out)
    ))
}

fn clean(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// E-mail addresses or phone numbers from the editor. Entries that did not
/// change keep their stored form (preference, original label spelling).
fn fields_from_input(
    input: &[LabeledValue],
    existing: &[ContactField],
    case_insensitive: bool,
) -> Vec<ContactField> {
    let same = |a: &str, b: &str| {
        if case_insensitive {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    let mut used = vec![false; existing.len()];
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in input {
        let value = item.value.trim();
        if value.is_empty() {
            continue;
        }
        let key = if case_insensitive {
            value.to_lowercase()
        } else {
            value.to_owned()
        };
        if !seen.insert(key) {
            continue;
        }
        let label = item
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty());
        let unchanged = existing.iter().enumerate().position(|(i, f)| {
            !used[i]
                && same(f.value.trim(), value)
                && kind_to_label(f.kind.as_deref()).as_deref() == label
        });
        let relabeled = unchanged.or_else(|| {
            existing
                .iter()
                .enumerate()
                .position(|(i, f)| !used[i] && same(f.value.trim(), value))
        });
        match relabeled {
            Some(i) => {
                used[i] = true;
                let mut field = existing[i].clone();
                if unchanged.is_none() {
                    field.kind = label_to_kind(label);
                }
                if field.value != value {
                    field.value = value.to_owned();
                }
                out.push(field);
            }
            None => out.push(ContactField {
                value: value.to_owned(),
                kind: label_to_kind(label),
                preferred: false,
            }),
        }
    }
    out
}

/// Postal addresses from the editor; unchanged text keeps the stored
/// structure (street, city, postal code, …).
fn addresses_from_input(
    input: &[LabeledValue],
    existing: &[ContactAddress],
) -> Vec<ContactAddress> {
    let mut used = vec![false; existing.len()];
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in input {
        let label = item
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty());
        let typed = ContactAddress::from_text(&item.value, label_to_kind(label));
        if typed.is_empty() {
            continue;
        }
        let text = typed.formatted();
        if !seen.insert(text.clone()) {
            continue;
        }
        let found = existing
            .iter()
            .enumerate()
            .position(|(i, a)| !used[i] && a.formatted() == text);
        match found {
            Some(i) => {
                used[i] = true;
                let mut address = existing[i].clone();
                if kind_to_label(address.kind.as_deref()).as_deref() != label {
                    address.kind = label_to_kind(label);
                }
                out.push(address);
            }
            None => out.push(typed),
        }
    }
    out
}

/// Web sites from the editor. New addresses without a scheme get `https://`.
fn urls_from_input(input: &[String], existing: &[ContactField]) -> Vec<ContactField> {
    let mut used = vec![false; existing.len()];
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for url in input {
        let value = url.trim();
        if value.is_empty() || !seen.insert(value.to_owned()) {
            continue;
        }
        match existing
            .iter()
            .enumerate()
            .position(|(i, f)| !used[i] && f.value.trim() == value)
        {
            Some(i) => {
                used[i] = true;
                out.push(existing[i].clone());
            }
            None => {
                let value = if !value.contains(':') && value.contains('.') && !value.contains(' ') {
                    format!("https://{value}")
                } else {
                    value.to_owned()
                };
                out.push(ContactField {
                    value,
                    kind: None,
                    preferred: false,
                });
            }
        }
    }
    out
}

/// Applies the editor's input to a contact, keeping what the editor does not
/// show (nickname, preferences, structured addresses, the formatted name
/// when the names were not edited, …).
pub fn apply_input(old: &Contact, input: &ContactInput) -> Result<Contact> {
    let mut contact = old.clone();
    contact.given_name = clean(&input.given_name);
    contact.family_name = clean(&input.family_name);
    contact.organization = clean(&input.organization);
    contact.title = clean(&input.title);
    contact.note = input
        .notes
        .as_deref()
        .map(|n| n.trim().replace("\r\n", "\n"))
        .filter(|n| !n.is_empty());
    contact.birthday = parse_birthday(input.birthday.as_deref().unwrap_or(""))?;
    contact.emails = fields_from_input(&input.emails, &old.emails, true);
    contact.phones = fields_from_input(&input.phones, &old.phones, false);
    contact.addresses = addresses_from_input(&input.addresses, &old.addresses);
    contact.urls = urls_from_input(&input.urls, &old.urls);
    let names_unchanged =
        contact.given_name == old.given_name && contact.family_name == old.family_name;
    contact.name = if names_unchanged && !old.name.trim().is_empty() {
        // The editor rebuilds the name from its parts; keep a custom one.
        old.name.clone()
    } else {
        input.name.trim().to_owned()
    };
    if contact.name.is_empty() {
        contact.name = display_name(&contact);
        if contact.name == "Unnamed" {
            return Err(Error::Invalid("Add a name or an e-mail address.".into()));
        }
    }
    Ok(contact)
}

/// Takes `local`'s changes relative to `base` over `server`: a field the
/// user changed wins, every other field is the server's.
pub fn merge3(base: &Contact, local: &Contact, server: &Contact) -> Contact {
    let mut out = server.clone();
    macro_rules! take_local_changes {
        ($($field:ident),*) => {
            $(if local.$field != base.$field {
                out.$field = local.$field.clone();
            })*
        };
    }
    take_local_changes!(
        name,
        given_name,
        family_name,
        nickname,
        emails,
        phones,
        addresses,
        urls,
        organization,
        title,
        note,
        birthday,
        photo
    );
    out
}

/// The UI view of a stored contact.
pub fn view(row: &ContactRow, photo: Option<&Photo>) -> ContactView {
    let c = &row.contact;
    let mut seen = HashSet::new();
    let emails = c
        .emails
        .iter()
        .filter(|e| !e.value.trim().is_empty() && seen.insert(e.value.trim().to_lowercase()))
        .map(|e| LabeledValue {
            value: e.value.trim().to_owned(),
            label: kind_to_label(e.kind.as_deref()),
        })
        .collect();
    let mut seen = HashSet::new();
    let phones = c
        .phones
        .iter()
        .filter(|p| !p.value.trim().is_empty() && seen.insert(p.value.trim().to_owned()))
        .map(|p| LabeledValue {
            value: p.value.trim().to_owned(),
            label: kind_to_label(p.kind.as_deref()),
        })
        .collect();
    let mut seen = HashSet::new();
    let addresses = c
        .addresses
        .iter()
        .map(|a| (a.formatted(), a))
        .filter(|(text, _)| !text.is_empty() && seen.insert(text.clone()))
        .map(|(value, a)| LabeledValue {
            value,
            label: kind_to_label(a.kind.as_deref()),
        })
        .collect();
    let mut seen = HashSet::new();
    let urls = c
        .urls
        .iter()
        .map(|u| u.value.trim().to_owned())
        .filter(|u| !u.is_empty() && seen.insert(u.clone()))
        .collect();
    ContactView {
        id: row.id,
        book_id: row.book_id,
        name: display_name(c),
        given_name: clean(&c.given_name),
        family_name: clean(&c.family_name),
        emails,
        phones,
        addresses,
        urls,
        organization: clean(&c.organization),
        title: clean(&c.title),
        birthday: c.birthday.map(format_birthday),
        notes: clean(&c.note),
        photo: photo.map(data_url),
    }
}

/// The photo edit described by the editor's `photo` compared with the stored
/// one.
fn photo_edit(input: Option<&str>, current: Option<&Photo>) -> Result<PhotoEdit> {
    let input = input.map(str::trim).filter(|p| !p.is_empty());
    match (input, current) {
        (None, None) => Ok(PhotoEdit::Unchanged),
        (None, Some(_)) => Ok(PhotoEdit::Removed),
        (Some(url), Some(current)) if url == data_url(current) => Ok(PhotoEdit::Unchanged),
        (Some(url), _) => {
            let photo = parse_data_url(url).ok_or_else(|| {
                Error::Invalid("The photo is not an image Envelope can store.".into())
            })?;
            if photo.bytes.len() > MAX_PHOTO_BYTES {
                return Err(Error::Invalid("The photo is too large.".into()));
            }
            let thumb = thumbnail(&photo.bytes);
            Ok(PhotoEdit::Set { photo, thumb })
        }
    }
}

/// Whether a contact with one of `emails` (lower-case) or with `uid` is
/// already in an address book.
fn already_known(c: &rusqlite::Connection, uid: &str, emails: &[String]) -> Result<bool> {
    let mut st = c.prepare_cached(
        "SELECT 1 FROM contacts WHERE local_state != ?1 AND (
           (?2 != '' AND uid = ?2)
           OR instr(char(10) || lower(emails) || char(10), char(10) || ?3 || char(10)) > 0)
         LIMIT 1",
    )?;
    if !uid.is_empty() && st.exists(rusqlite::params![cdb::DELETED, uid, "\u{1}"])? {
        return Ok(true);
    }
    for email in emails {
        if st.exists(rusqlite::params![cdb::DELETED, "", email])? {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Engine {
    /// Whether these contacts are already in an address book (same e-mail
    /// address or UID), in order.
    pub fn contacts_known(&self, contacts: &[Contact]) -> Result<Vec<bool>> {
        self.db.read(|c| {
            contacts
                .iter()
                .map(|contact| {
                    let emails: Vec<String> = contact
                        .emails
                        .iter()
                        .map(|e| e.value.trim().to_lowercase())
                        .filter(|e| !e.is_empty())
                        .collect();
                    already_known(c, contact.uid.trim(), &emails)
                })
                .collect()
        })
    }

    /// Adds contacts (from a vCard file) to an address book, leaving out the
    /// ones already known. Returns how many were added and left out.
    pub async fn import_contacts(
        &self,
        book_id: i64,
        contacts: Vec<Contact>,
    ) -> Result<(usize, usize)> {
        let known = self.contacts_known(&contacts)?;
        let (added, skipped, remote) = self
            .db
            .write_async(move |tx| {
                let book = cdb::get_book(tx, book_id)?;
                if book.read_only {
                    return Err(Error::Invalid("This address book is read-only.".into()));
                }
                let remote = book.account_id.is_some();
                let (mut added, mut skipped) = (0, 0);
                for (mut contact, known) in contacts.into_iter().zip(known) {
                    if known {
                        skipped += 1;
                        continue;
                    }
                    if contact.name.trim().is_empty() {
                        contact.name = display_name(&contact);
                    }
                    // A fresh UID: the card may come from someone else's address book.
                    let uid = uuid::Uuid::new_v4().to_string();
                    contact.uid = uid.clone();
                    let photo = match contact.photo.take() {
                        Some(url) if url.starts_with("data:") => {
                            photo_edit(Some(&url), None).unwrap_or(PhotoEdit::Unchanged)
                        }
                        _ => PhotoEdit::Unchanged,
                    };
                    let remote_id = match book.kind.as_str() {
                        "carddav" => Some(
                            envelope_dav::object_href(
                                &book.remote_id,
                                &uid,
                                envelope_dav::ObjectKind::Contact,
                            )
                            .map_err(|e| Error::Other(e.to_string()))?,
                        ),
                        _ => None,
                    };
                    let state = if remote { cdb::CREATED } else { cdb::SYNCED };
                    cdb::insert_local(
                        tx,
                        book.id,
                        &uid,
                        remote_id.as_deref(),
                        &contact,
                        state,
                        &photo,
                    )?;
                    added += 1;
                }
                Ok((added, skipped, remote))
            })
            .await?;
        if added > 0 {
            if remote {
                crate::sync::contacts::wake(self);
            }
            self.emit(EngineEvent::ContactsChanged {
                book_ids: vec![book_id],
            });
        }
        Ok((added, skipped))
    }

    /// Makes sure contacts can be added: an address book on this computer
    /// exists while no account offers a writable one, and goes away (when
    /// empty) once one does.
    pub fn ensure_local_book(&self) -> Result<()> {
        let check = |c: &rusqlite::Connection| -> Result<(bool, Option<(i64, bool)>)> {
            let remote: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM address_books b JOIN accounts a ON a.id = b.account_id
                   WHERE a.enabled = 1 AND b.read_only = 0)",
                [],
                |r| r.get(0),
            )?;
            let local: Option<(i64, bool)> = c
                .query_row(
                    "SELECT b.id, EXISTS (SELECT 1 FROM contacts c WHERE c.book_id = b.id)
                     FROM address_books b WHERE b.account_id IS NULL AND b.kind = 'local' ORDER BY b.id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            Ok((remote, local))
        };
        match self.db.read(check)? {
            (false, None) => {
                self.db.write(|tx| {
                    // Re-check inside the write: another caller may have won.
                    if check(tx)?.1.is_none() {
                        cdb::create_local_book(tx, LOCAL_BOOK_NAME)?;
                    }
                    Ok(())
                })?;
            }
            (true, Some((id, false))) => {
                self.db.write(|tx| {
                    tx.execute(
                        "DELETE FROM address_books WHERE id = ? AND NOT EXISTS (SELECT 1 FROM contacts WHERE book_id = ?)",
                        [id, id],
                    )?;
                    Ok(())
                })?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Address books of enabled accounts and of this computer.
    pub fn contact_books(&self) -> Result<Vec<ContactBook>> {
        self.ensure_local_book()?;
        Ok(self
            .db
            .read(cdb::list_books)?
            .into_iter()
            .map(|b| ContactBook {
                id: b.id,
                account_id: b.account_id,
                kind: b.kind,
                name: b.name,
                read_only: b.read_only,
            })
            .collect())
    }

    /// Contacts matching `query` (names, e-mail addresses, organization,
    /// phone numbers), in one address book or all, sorted by name.
    pub async fn contacts_list(
        &self,
        query: &str,
        book_id: Option<i64>,
        offset: i64,
        limit: i64,
    ) -> Result<ContactPage> {
        let terms = cdb::search_terms(query);
        let (total, rows) = self
            .db
            .read_async(move |c| cdb::list_contacts(c, &terms, book_id, offset, limit))
            .await?;
        Ok(ContactPage {
            total,
            items: rows
                .into_iter()
                .map(|r| ContactSummary {
                    id: r.id,
                    book_id: r.book_id,
                    name: r.name,
                    email: r.email,
                    organization: r.organization,
                    photo: r.thumb,
                })
                .collect(),
        })
    }

    /// One contact with all its details.
    pub async fn contact(&self, id: i64) -> Result<ContactView> {
        let (row, photo) = self
            .db
            .read_async(move |c| Ok((cdb::get_contact(c, id)?, cdb::contact_photo(c, id)?)))
            .await?;
        Ok(view(&row, photo.as_ref()))
    }

    /// Creates or updates a contact and returns its id. The change is stored
    /// here at once and written to the server in the background.
    pub async fn save_contact(&self, input: ContactInput) -> Result<i64> {
        let (id, remote) = self
            .db
            .write_async(move |tx| save_contact_tx(tx, &input))
            .await?;
        if remote {
            crate::sync::contacts::wake(self);
        }
        Ok(id)
    }

    /// Writes the people of an address book, or of every address book, to a
    /// vCard file with their photos. Returns how many were written.
    pub async fn export_contacts(
        &self,
        book_id: Option<i64>,
        path: &std::path::Path,
    ) -> Result<usize> {
        let cards = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT id FROM contacts WHERE local_state != ?1 AND is_group = 0
                       AND (?2 IS NULL OR book_id = ?2)
                     ORDER BY sort_key, id",
            )?;
            let ids = st
                .query_map(rusqlite::params![cdb::DELETED, book_id], |r| {
                    r.get::<_, i64>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter()
                .map(|id| {
                    let mut contact = cdb::get_contact(c, id)?.contact;
                    contact.photo = cdb::contact_photo(c, id)?.map(|p| data_url(&p));
                    Ok(envelope_ical::vcard::contact_to_vcard(&contact))
                })
                .collect::<Result<Vec<String>>>()
        })?;
        tokio::fs::write(path, cards.concat()).await?;
        Ok(cards.len())
    }

    /// Deletes a contact here and, in the background, on the server.
    pub async fn delete_contact(&self, id: i64) -> Result<()> {
        let remote = self
            .db
            .write_async(move |tx| {
                let row = cdb::get_contact(tx, id)?;
                let book = cdb::get_book(tx, row.book_id)?;
                if book.read_only {
                    return Err(Error::Invalid("This address book is read-only.".into()));
                }
                if book.account_id.is_none()
                    || (row.local_state == cdb::CREATED && row.remote_id.is_none())
                {
                    cdb::delete_row(tx, id)?;
                    Ok(false)
                } else {
                    if row.local_state != cdb::DELETED {
                        cdb::mark_deleted(tx, id)?;
                    }
                    Ok(true)
                }
            })
            .await?;
        if remote {
            crate::sync::contacts::wake(self);
        }
        Ok(())
    }
}

/// Stores an edit; returns the contact id and whether it has to go to a
/// server.
fn save_contact_tx(tx: &rusqlite::Transaction<'_>, input: &ContactInput) -> Result<(i64, bool)> {
    let book = cdb::get_book(tx, input.book_id)?;
    if book.read_only {
        return Err(Error::Invalid("This address book is read-only.".into()));
    }
    let remote = book.account_id.is_some();
    let existing = match input.id {
        Some(id) => Some(cdb::get_contact(tx, id)?),
        None => None,
    };
    if let Some(row) = existing.as_ref().filter(|r| r.book_id == input.book_id) {
        if row.local_state == cdb::DELETED {
            return Err(Error::NotFound(format!("contact {}", row.id)));
        }
        let contact = apply_input(&row.contact, input)?;
        let current = cdb::contact_photo(tx, row.id)?;
        let photo = photo_edit(input.photo.as_deref(), current.as_ref())?;
        if contact == row.contact && photo == PhotoEdit::Unchanged {
            return Ok((row.id, false));
        }
        let state = match (remote, row.local_state) {
            (false, _) => cdb::SYNCED,
            (true, cdb::CREATED) => cdb::CREATED,
            (true, _) => cdb::MODIFIED,
        };
        cdb::update_local(tx, row.id, &contact, state, &photo)?;
        return Ok((row.id, remote));
    }

    // A new contact, or one moved to another address book (created there,
    // deleted from the old one).
    let mut contact = match &existing {
        Some(row) => {
            let old_book = cdb::get_book(tx, row.book_id)?;
            if old_book.read_only {
                return Err(Error::Invalid(
                    "Contacts can't be moved out of a read-only address book.".into(),
                ));
            }
            apply_input(&row.contact, input)?
        }
        None => apply_input(&Contact::default(), input)?,
    };
    let uid = uuid::Uuid::new_v4().to_string();
    contact.uid = uid.clone();
    let remote_id = match book.kind.as_str() {
        "carddav" => Some(
            envelope_dav::object_href(&book.remote_id, &uid, envelope_dav::ObjectKind::Contact)
                .map_err(|e| Error::Other(e.to_string()))?,
        ),
        _ => None,
    };
    let photo = photo_edit(input.photo.as_deref(), None)?;
    let state = if remote { cdb::CREATED } else { cdb::SYNCED };
    let id = cdb::insert_local(
        tx,
        book.id,
        &uid,
        remote_id.as_deref(),
        &contact,
        state,
        &photo,
    )?;
    let mut old_remote = false;
    if let Some(row) = existing {
        let old_book = cdb::get_book(tx, row.book_id)?;
        if old_book.account_id.is_none() || row.remote_id.is_none() {
            cdb::delete_row(tx, row.id)?;
        } else {
            cdb::mark_deleted(tx, row.id)?;
            old_remote = true;
        }
    }
    Ok((id, remote || old_remote))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lv(value: &str, label: Option<&str>) -> LabeledValue {
        LabeledValue {
            value: value.into(),
            label: label.map(str::to_owned),
        }
    }

    fn field(value: &str, kind: Option<&str>, preferred: bool) -> ContactField {
        ContactField {
            value: value.into(),
            kind: kind.map(str::to_owned),
            preferred,
        }
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(kind_to_label(Some("cell")).as_deref(), Some("mobile"));
        assert_eq!(kind_to_label(Some("WORK")).as_deref(), Some("work"));
        assert_eq!(
            kind_to_label(Some("summer house")).as_deref(),
            Some("summer house")
        );
        assert_eq!(kind_to_label(Some(" ")), None);
        assert_eq!(label_to_kind(Some("mobile")).as_deref(), Some("cell"));
        assert_eq!(label_to_kind(Some("Home")).as_deref(), Some("home"));
        assert_eq!(label_to_kind(Some("Ofis")).as_deref(), Some("Ofis"));
        assert_eq!(label_to_kind(None), None);
    }

    #[test]
    fn birthdays() {
        let date = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(parse_birthday("1985-04-15").ok(), Some(date(1985, 4, 15)));
        assert_eq!(parse_birthday(" 19850415 ").ok(), Some(date(1985, 4, 15)));
        assert_eq!(parse_birthday("1985/4/5").ok(), Some(date(1985, 4, 5)));
        assert_eq!(parse_birthday("15.04.1985").ok(), Some(date(1985, 4, 15)));
        assert_eq!(parse_birthday("--04-15").ok(), Some(date(NO_YEAR, 4, 15)));
        assert_eq!(parse_birthday("--0229").ok(), Some(date(NO_YEAR, 2, 29)));
        assert_eq!(parse_birthday("04-15").ok(), Some(date(NO_YEAR, 4, 15)));
        assert_eq!(parse_birthday("").ok(), Some(None));
        assert!(parse_birthday("1985-02-30").is_err());
        assert!(parse_birthday("next tuesday").is_err());
        assert!(parse_birthday("04/15/1985").is_err(), "ambiguous order");
        assert_eq!(
            format_birthday(NaiveDate::from_ymd_opt(1985, 4, 5).unwrap_or_default()),
            "1985-04-05"
        );
        assert_eq!(
            format_birthday(NaiveDate::from_ymd_opt(NO_YEAR, 12, 1).unwrap_or_default()),
            "--12-01"
        );
    }

    #[test]
    fn photo_data_urls() {
        let photo = Photo {
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
            mime: "image/jpeg".into(),
        };
        let url = data_url(&photo);
        assert_eq!(url, "data:image/jpeg;base64,/9j/4AECAw==");
        assert_eq!(parse_data_url(&url), Some(photo.clone()));
        assert_eq!(
            parse_data_url("data:image/jpeg;base64,/9j/4A\r\n ECAw"),
            Some(photo),
            "whitespace and missing padding are tolerated"
        );
        assert_eq!(parse_data_url("data:text/plain;base64,aGk="), None);
        assert_eq!(parse_data_url("https://example.com/a.jpg"), None);
        assert_eq!(
            sniff_image_mime(&[0x89, b'P', b'N', b'G', 0]),
            Some("image/png")
        );
    }

    #[test]
    fn thumbnails_are_small_square_jpegs() {
        let mut png = Vec::new();
        let image = image::RgbaImage::from_fn(300, 200, |x, _| {
            image::Rgba([(x % 256) as u8, 80, 160, if x < 150 { 255 } else { 0 }])
        });
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("png");
        let thumb = thumbnail(&png).expect("thumbnail");
        let decoded = parse_data_url(&thumb).expect("data url");
        assert_eq!(decoded.mime, "image/jpeg");
        let back = image::load_from_memory(&decoded.bytes).expect("jpeg");
        assert_eq!((back.width(), back.height()), (THUMB_SIZE, THUMB_SIZE));
        assert!(thumbnail(b"not an image").is_none());
    }

    #[test]
    fn editor_input_keeps_what_it_does_not_show() {
        let old = Contact {
            uid: "u1".into(),
            name: "Dr. Ada Lovelace".into(),
            given_name: Some("Ada".into()),
            family_name: Some("Lovelace".into()),
            nickname: Some("Countess".into()),
            emails: vec![
                field("ada@example.com", Some("work"), true),
                field("ada@home.example", None, false),
            ],
            phones: vec![field("+44 20 1234", Some("cell"), false)],
            addresses: vec![ContactAddress {
                kind: Some("home".into()),
                street: "12 St James's Square".into(),
                locality: "London".into(),
                ..ContactAddress::default()
            }],
            urls: vec![field("https://ada.example", Some("homepage"), false)],
            ..Contact::default()
        };
        let row = ContactRow {
            id: 1,
            book_id: 1,
            uid: "u1".into(),
            remote_id: None,
            etag: None,
            contact: old.clone(),
            base: None,
            is_group: false,
            photo_url: None,
            photo_src: None,
            photo_dirty: false,
            has_photo: false,
            local_state: 0,
            version: 1,
            push_attempts: 0,
        };
        let shown = view(&row, None);
        assert_eq!(shown.phones[0].label.as_deref(), Some("mobile"));
        assert_eq!(shown.addresses[0].value, "12 St James's Square\nLondon");
        // The editor sends back what it was given, with the name rebuilt.
        let input = ContactInput {
            id: Some(1),
            book_id: 1,
            name: "Ada Lovelace".into(),
            given_name: shown.given_name.clone(),
            family_name: shown.family_name.clone(),
            emails: shown.emails.clone(),
            phones: shown.phones.clone(),
            addresses: shown.addresses.clone(),
            urls: shown.urls.clone(),
            organization: Some(String::new()),
            title: Some(String::new()),
            birthday: None,
            notes: Some(String::new()),
            photo: None,
        };
        assert_eq!(apply_input(&old, &input).expect("input"), old, "no change");

        let mut edited = input.clone();
        edited.family_name = Some("King".into());
        edited.name = "Ada King".into();
        edited.emails = vec![
            lv("ADA@example.com", Some("home")),
            lv("new@x.example", Some("other")),
        ];
        edited.phones.push(lv("+44 20 1234", Some("work")));
        edited.addresses = vec![lv("1 New Street\n\nLondon", Some("work"))];
        edited.urls = vec!["https://ada.example".into(), "lovelace.example".into()];
        edited.birthday = Some("--12-10".into());
        let contact = apply_input(&old, &edited).expect("input");
        assert_eq!(contact.name, "Ada King");
        assert_eq!(contact.nickname.as_deref(), Some("Countess"));
        assert_eq!(
            contact.emails,
            [
                field("ADA@example.com", Some("home"), true),
                field("new@x.example", Some("other"), false)
            ]
        );
        assert_eq!(contact.phones.len(), 1, "duplicates are dropped");
        assert_eq!(contact.addresses[0].street, "1 New Street\nLondon");
        assert_eq!(contact.addresses[0].kind.as_deref(), Some("work"));
        assert_eq!(contact.urls[0].kind.as_deref(), Some("homepage"));
        assert_eq!(contact.urls[1].value, "https://lovelace.example");
        assert_eq!(contact.birthday, NaiveDate::from_ymd_opt(NO_YEAR, 12, 10));

        let blank = ContactInput::default();
        assert!(apply_input(&Contact::default(), &blank).is_err());
        let only_email = ContactInput {
            emails: vec![lv("x@example.com", None)],
            ..ContactInput::default()
        };
        assert_eq!(
            apply_input(&Contact::default(), &only_email)
                .expect("input")
                .name,
            "x@example.com"
        );
    }

    #[test]
    fn three_way_merge_keeps_both_sides() {
        let base = Contact {
            name: "A".into(),
            note: Some("old".into()),
            title: Some("Engineer".into()),
            ..Contact::default()
        };
        let mut local = base.clone();
        local.note = Some("mine".into());
        let mut server = base.clone();
        server.title = Some("Manager".into());
        server.uid = "server".into();
        let merged = merge3(&base, &local, &server);
        assert_eq!(merged.note.as_deref(), Some("mine"));
        assert_eq!(merged.title.as_deref(), Some("Manager"));
        assert_eq!(merged.uid, "server");
    }

    #[test]
    fn display_names_fall_back() {
        let mut c = Contact {
            phones: vec![field("+1 555 0100", None, false)],
            ..Contact::default()
        };
        assert_eq!(display_name(&c), "+1 555 0100");
        c.organization = Some("Acme".into());
        assert_eq!(display_name(&c), "Acme");
        c.given_name = Some("Wile".into());
        assert_eq!(display_name(&c), "Wile");
        assert_eq!(display_name(&Contact::default()), "Unnamed");
    }
}
