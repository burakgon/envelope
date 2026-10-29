//! Address books and contacts.
//!
//! A contact row keeps the contact as last edited here (`data`), the server
//! copy that state started from (`base`: a vCard, a Google person, …) and a
//! `local_state` saying whether it still has to be written to the server.
//! Sync never overwrites rows with local changes; the push step merges them
//! with the server copy instead.

use std::collections::{HashMap, HashSet};

use envelope_ical::Contact;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::{now_ms, pack, unpack};
use crate::error::{Error, Result};

/// `local_state`: in sync with the server (or a local address book).
pub const SYNCED: i64 = 0;
/// `local_state`: created here, not on the server yet.
pub const CREATED: i64 = 1;
/// `local_state`: changed here, not written to the server yet.
pub const MODIFIED: i64 = 2;
/// `local_state`: deleted here, still on the server.
pub const DELETED: i64 = 3;

/// Per-address-book sync bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BookSyncState {
    /// CardDAV `getctag`.
    pub ctag: Option<String>,
    /// CardDAV / Google sync token.
    pub sync_token: Option<String>,
    /// Graph delta cursor.
    pub cursor: Option<serde_json::Value>,
    /// Graph: id of the folder behind the default address book.
    pub folder_id: Option<String>,
    pub last_sync: Option<i64>,
    /// Exchange: which fields the stored contacts were read with. Contacts
    /// read with fewer are read again in full once.
    pub fields: u32,
}

#[derive(Debug, Clone)]
pub struct BookRow {
    pub id: i64,
    pub account_id: Option<i64>,
    pub kind: String,
    pub remote_id: String,
    pub name: String,
    pub read_only: bool,
    pub is_default: bool,
    pub sync_state: BookSyncState,
}

fn book_from_row(r: &Row<'_>) -> rusqlite::Result<BookRow> {
    let state: String = r.get("sync_state")?;
    Ok(BookRow {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        kind: r.get("kind")?,
        remote_id: r.get("remote_id")?,
        name: r.get("name")?,
        read_only: r.get::<_, i64>("read_only")? != 0,
        is_default: r.get::<_, i64>("is_default")? != 0,
        sync_state: serde_json::from_str(&state).unwrap_or_default(),
    })
}

/// Address books of enabled accounts and local ones: accounts in their
/// order, each account's default book first, local books last.
pub fn list_books(c: &Connection) -> Result<Vec<BookRow>> {
    let mut st = c.prepare_cached(
        "SELECT b.* FROM address_books b LEFT JOIN accounts a ON a.id = b.account_id
         WHERE b.account_id IS NULL OR a.enabled = 1
         ORDER BY b.account_id IS NULL, a.sort_order, b.account_id, b.is_default DESC, b.sort_order,
                  b.name COLLATE NOCASE",
    )?;
    let rows = st.query_map([], book_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn account_books(c: &Connection, account_id: i64) -> Result<Vec<BookRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM address_books WHERE account_id = ? ORDER BY is_default DESC, sort_order, id",
    )?;
    let rows = st.query_map([account_id], book_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_book(c: &Connection, id: i64) -> Result<BookRow> {
    c.query_row(
        "SELECT * FROM address_books WHERE id = ?",
        [id],
        book_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("address book {id}")))
}

/// An address book as reported by the server.
#[derive(Debug, Clone)]
pub struct BookUpsert {
    pub remote_id: String,
    pub name: String,
    pub read_only: bool,
    pub is_default: bool,
    pub sort_order: i64,
}

/// Creates/updates the address books of an account and removes the ones that
/// vanished. Returns the ids of books that were added, changed or removed.
pub fn sync_books(
    tx: &Transaction<'_>,
    account_id: i64,
    kind: &str,
    list: &[BookUpsert],
) -> Result<Vec<i64>> {
    let mut changed = Vec::new();
    let existing: HashMap<String, (i64, String, bool, bool)> = {
        let mut st = tx.prepare(
            "SELECT remote_id, id, name, read_only, is_default FROM address_books WHERE account_id = ?",
        )?;
        st.query_map([account_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, i64>(4)? != 0,
                ),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    for b in list {
        match existing.get(&b.remote_id) {
            Some((id, name, read_only, is_default)) => {
                if *name != b.name || *read_only != b.read_only || *is_default != b.is_default {
                    changed.push(*id);
                }
                tx.execute(
                    "UPDATE address_books SET name = ?, read_only = ?, is_default = ?, sort_order = ?, kind = ?
                     WHERE id = ?",
                    params![
                        b.name,
                        b.read_only as i64,
                        b.is_default as i64,
                        b.sort_order,
                        kind,
                        id
                    ],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO address_books (account_id, kind, remote_id, name, read_only, is_default, sort_order)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![
                        account_id,
                        kind,
                        b.remote_id,
                        b.name,
                        b.read_only as i64,
                        b.is_default as i64,
                        b.sort_order
                    ],
                )?;
                changed.push(tx.last_insert_rowid());
            }
        }
    }
    let keep: HashSet<&str> = list.iter().map(|b| b.remote_id.as_str()).collect();
    for (remote, (id, ..)) in &existing {
        if !keep.contains(remote.as_str()) {
            tx.execute("DELETE FROM address_books WHERE id = ?", [id])?;
            changed.push(*id);
        }
    }
    Ok(changed)
}

pub fn set_book_sync_state(tx: &Transaction<'_>, id: i64, state: &BookSyncState) -> Result<()> {
    tx.execute(
        "UPDATE address_books SET sync_state = ? WHERE id = ?",
        params![serde_json::to_string(state)?, id],
    )?;
    Ok(())
}

pub fn create_local_book(tx: &Transaction<'_>, name: &str) -> Result<i64> {
    let remote = format!("local-{}", uuid::Uuid::new_v4());
    tx.execute(
        "INSERT INTO address_books (account_id, kind, remote_id, name, is_default) VALUES (NULL, 'local', ?, ?, 1)",
        params![remote, name],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Remembers something about an account's server (such as the CardDAV
/// address book home) without touching the rest of its state.
pub fn set_account_state_value(
    tx: &Transaction<'_>,
    account_id: i64,
    key: &str,
    value: &serde_json::Value,
) -> Result<()> {
    tx.execute(
        "UPDATE accounts SET state = json_set(CASE WHEN json_valid(state) THEN state ELSE '{}' END, ?, json(?))
         WHERE id = ?",
        params![format!("$.{key}"), value.to_string(), account_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Text folding, ordering and search

/// The base letters of an accented Latin letter, if it has one.
fn base_letters(c: char) -> Option<&'static str> {
    Some(match c {
        'À'..='Å' | 'à'..='å' | 'Ā' | 'ā' | 'Ă' | 'ă' | 'Ą' | 'ą' => "a",
        'Æ' | 'æ' => "ae",
        'Ç' | 'ç' | 'Ć' | 'ć' | 'Ĉ' | 'ĉ' | 'Ċ' | 'ċ' | 'Č' | 'č' => "c",
        'Ð' | 'ð' | 'Ď' | 'ď' | 'Đ' | 'đ' => "d",
        'È'..='Ë' | 'è'..='ë' | 'Ē' | 'ē' | 'Ĕ' | 'ĕ' | 'Ė' | 'ė' | 'Ę' | 'ę' | 'Ě' | 'ě' => {
            "e"
        }
        'Ĝ' | 'ĝ' | 'Ğ' | 'ğ' | 'Ġ' | 'ġ' | 'Ģ' | 'ģ' => "g",
        'Ĥ' | 'ĥ' | 'Ħ' | 'ħ' => "h",
        'Ì'..='Ï' | 'ì'..='ï' | 'Ĩ' | 'ĩ' | 'Ī' | 'ī' | 'Ĭ' | 'ĭ' | 'Į' | 'į' | 'İ' | 'ı' => {
            "i"
        }
        'Ĳ' | 'ĳ' => "ij",
        'Ĵ' | 'ĵ' => "j",
        'Ķ' | 'ķ' | 'ĸ' => "k",
        'Ĺ' | 'ĺ' | 'Ļ' | 'ļ' | 'Ľ' | 'ľ' | 'Ŀ' | 'ŀ' | 'Ł' | 'ł' => "l",
        'Ñ' | 'ñ' | 'Ń' | 'ń' | 'Ņ' | 'ņ' | 'Ň' | 'ň' | 'ŉ' | 'Ŋ' | 'ŋ' => "n",
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' | 'Ō' | 'ō' | 'Ŏ' | 'ŏ' | 'Ő' | 'ő' => "o",
        'Œ' | 'œ' => "oe",
        'Ŕ' | 'ŕ' | 'Ŗ' | 'ŗ' | 'Ř' | 'ř' => "r",
        'Ś' | 'ś' | 'Ŝ' | 'ŝ' | 'Ş' | 'ş' | 'Š' | 'š' | 'ſ' | 'Ș' | 'ș' => "s",
        'ß' | 'ẞ' => "ss",
        'Ţ' | 'ţ' | 'Ť' | 'ť' | 'Ŧ' | 'ŧ' | 'Ț' | 'ț' => "t",
        'Þ' | 'þ' => "th",
        'Ù'..='Ü'
        | 'ù'..='ü'
        | 'Ũ'
        | 'ũ'
        | 'Ū'
        | 'ū'
        | 'Ŭ'
        | 'ŭ'
        | 'Ů'
        | 'ů'
        | 'Ű'
        | 'ű'
        | 'Ų'
        | 'ų' => "u",
        'Ŵ' | 'ŵ' => "w",
        'Ý' | 'ý' | 'ÿ' | 'Ŷ' | 'ŷ' | 'Ÿ' => "y",
        'Ź' | 'ź' | 'Ż' | 'ż' | 'Ž' | 'ž' => "z",
        _ => return None,
    })
}

/// Lower-cases text and strips diacritics (`Öztürk` → `ozturk`, `İlker` →
/// `ilker`), for case- and accent-insensitive search and ordering.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
        } else if let Some(base) = base_letters(c) {
            out.push_str(base);
        } else if !('\u{300}'..='\u{36f}').contains(&c) {
            out.extend(
                c.to_lowercase()
                    .filter(|l| !('\u{300}'..='\u{36f}').contains(l)),
            );
        }
    }
    out
}

/// The section a name is listed under, as the contacts list shows it: its
/// upper-cased first letter, or `None` (the `#` section) for names that do
/// not start with a letter.
pub fn section(name: &str) -> Option<String> {
    let first = name.trim().chars().next()?;
    // The list takes the first UTF-16 unit; letters outside the BMP land in `#`.
    if (first as u32) > 0xFFFF || !first.is_alphabetic() || first.is_numeric() {
        return None;
    }
    Some(first.to_uppercase().collect())
}

/// Ordering key: names are grouped by [`section`] (sections with the same
/// base letter next to each other, `O` before `Ö`; `#` last) and sorted case-
/// and accent-insensitively within a section.
pub fn sort_key(name: &str) -> String {
    let name = name.trim();
    let section = match section(name) {
        Some(letter) => format!("{}\u{1}{letter}", fold(&letter)),
        None => "\u{10FFFF}".to_owned(),
    };
    format!("{section}\u{1}{}\u{1}{name}", fold(name))
}

/// Digits of a phone number (`+90 (532) 111-22-33` → `905321112233`).
pub fn phone_digits(phone: &str) -> String {
    phone.chars().filter(char::is_ascii_digit).collect()
}

/// Text searched by the contact list filter and autocomplete.
fn search_text(contact: &Contact) -> String {
    let mut parts: Vec<String> = vec![fold(&contact.name)];
    for value in [
        &contact.given_name,
        &contact.family_name,
        &contact.nickname,
        &contact.organization,
        &contact.title,
    ]
    .into_iter()
    .flatten()
    {
        parts.push(fold(value));
    }
    for email in &contact.emails {
        parts.push(email.value.trim().to_lowercase());
    }
    for phone in &contact.phones {
        parts.push(fold(&phone.value));
        let digits = phone_digits(&phone.value);
        if !digits.is_empty() {
            parts.push(digits);
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

/// Columns derived from a contact for listing and searching.
struct Derived {
    name: String,
    sort_key: String,
    search: String,
    email: Option<String>,
    emails: String,
    organization: Option<String>,
}

fn derived(contact: &Contact) -> Derived {
    let name = crate::contacts::display_name(contact);
    let email = contact
        .emails
        .iter()
        .find(|e| e.preferred)
        .or_else(|| contact.emails.first())
        .map(|e| e.value.trim().to_owned());
    let emails = contact
        .emails
        .iter()
        .map(|e| e.value.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Derived {
        sort_key: sort_key(&name),
        search: search_text(contact),
        name,
        email,
        emails,
        organization: contact
            .organization
            .clone()
            .filter(|o| !o.trim().is_empty()),
    }
}

/// The contact as stored in `data`: without the photo, which has its own
/// columns.
fn data_json(contact: &Contact) -> Result<String> {
    if contact.photo.is_none() {
        return Ok(serde_json::to_string(contact)?);
    }
    let mut stripped = contact.clone();
    stripped.photo = None;
    Ok(serde_json::to_string(&stripped)?)
}

pub fn photo_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Contact rows

/// A stored contact (without the photo bytes).
#[derive(Debug, Clone)]
pub struct ContactRow {
    pub id: i64,
    pub book_id: i64,
    pub uid: String,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub contact: Contact,
    pub base: Option<String>,
    pub is_group: bool,
    pub photo_url: Option<String>,
    pub photo_src: Option<String>,
    pub photo_dirty: bool,
    pub has_photo: bool,
    pub local_state: i64,
    pub version: i64,
    pub push_attempts: i64,
}

const ROW_COLUMNS: &str =
    "id, book_id, uid, remote_id, etag, data, base, is_group, photo_url, photo_src,
    photo_dirty, photo IS NOT NULL AS has_photo, local_state, version, push_attempts";

fn contact_from_row(r: &Row<'_>) -> rusqlite::Result<ContactRow> {
    let data: String = r.get("data")?;
    let contact: Contact = serde_json::from_str(&data).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let base: Option<Vec<u8>> = r.get("base")?;
    Ok(ContactRow {
        id: r.get("id")?,
        book_id: r.get("book_id")?,
        uid: r.get("uid")?,
        remote_id: r.get("remote_id")?,
        etag: r.get("etag")?,
        contact,
        base: base.map(|b| unpack(&b)),
        is_group: r.get::<_, i64>("is_group")? != 0,
        photo_url: r.get("photo_url")?,
        photo_src: r.get("photo_src")?,
        photo_dirty: r.get::<_, i64>("photo_dirty")? != 0,
        has_photo: r.get::<_, i64>("has_photo")? != 0,
        local_state: r.get("local_state")?,
        version: r.get("version")?,
        push_attempts: r.get("push_attempts")?,
    })
}

pub fn get_contact(c: &Connection, id: i64) -> Result<ContactRow> {
    c.query_row(
        &format!("SELECT {ROW_COLUMNS} FROM contacts WHERE id = ?"),
        [id],
        contact_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("contact {id}")))
}

/// A stored photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    pub bytes: Vec<u8>,
    pub mime: String,
}

pub fn contact_photo(c: &Connection, id: i64) -> Result<Option<Photo>> {
    Ok(c.query_row(
        "SELECT photo, photo_mime FROM contacts WHERE id = ? AND photo IS NOT NULL",
        [id],
        |r| {
            Ok(Photo {
                bytes: r.get(0)?,
                mime: r
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| "image/jpeg".into()),
            })
        },
    )
    .optional()?)
}

/// `remote_id → (etag, local_state)` of every contact of a book that exists
/// on the server.
pub fn remote_index(
    c: &Connection,
    book_id: i64,
) -> Result<HashMap<String, (Option<String>, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT remote_id, etag, local_state FROM contacts WHERE book_id = ? AND remote_id IS NOT NULL",
    )?;
    let rows = st.query_map([book_id], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What to do with the photo of a contact stored by sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhotoChange {
    /// Leave the stored photo alone.
    Keep,
    /// The contact has no photo.
    Clear,
    /// The photo itself (inline in a vCard).
    Data(Photo),
    /// A photo to download (Google photo URL, vCard URI, Graph photo).
    Remote(String),
}

/// A contact as the server has it.
#[derive(Debug, Clone)]
pub struct SyncedContact {
    pub remote_id: String,
    pub etag: Option<String>,
    pub uid: String,
    /// The contact (its `photo` is ignored; see `photo`).
    pub contact: Contact,
    /// The server representation, kept for lossless updates and merges.
    pub base: Option<String>,
    pub photo: PhotoChange,
    pub is_group: bool,
}

fn apply_photo(tx: &Transaction<'_>, id: i64, photo: &PhotoChange) -> Result<()> {
    match photo {
        PhotoChange::Keep => {}
        PhotoChange::Clear => {
            tx.execute(
                "UPDATE contacts SET photo = NULL, photo_mime = NULL, photo_hash = NULL, thumb = NULL,
                   photo_url = NULL, photo_src = NULL WHERE id = ?",
                [id],
            )?;
        }
        PhotoChange::Data(p) => {
            // Unchanged photos keep their thumbnail.
            let hash = photo_hash(&p.bytes);
            tx.execute(
                "UPDATE contacts SET photo = ?1, photo_mime = ?2, photo_hash = ?3, thumb = NULL
                 WHERE id = ?4 AND photo_hash IS NOT ?3",
                params![p.bytes, p.mime, hash, id],
            )?;
            tx.execute(
                "UPDATE contacts SET photo_url = NULL, photo_src = NULL WHERE id = ?",
                [id],
            )?;
        }
        PhotoChange::Remote(url) => {
            tx.execute(
                "UPDATE contacts SET photo_url = ? WHERE id = ?",
                params![url, id],
            )?;
        }
    }
    Ok(())
}

/// Inserts or updates a contact from the server. Rows with local changes
/// are left alone (`None`); the push step reconciles them.
pub fn upsert_synced(
    tx: &Transaction<'_>,
    book_id: i64,
    item: &SyncedContact,
) -> Result<Option<i64>> {
    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, local_state FROM contacts WHERE book_id = ? AND remote_id = ? ORDER BY id LIMIT 1",
            params![book_id, item.remote_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let d = derived(&item.contact);
    let data = data_json(&item.contact)?;
    let base = item.base.as_deref().map(pack);
    let id = match existing {
        Some((_, state)) if state != SYNCED => return Ok(None),
        Some((id, _)) => {
            tx.execute(
                "UPDATE contacts SET uid = ?, etag = ?, name = ?, sort_key = ?, search = ?, email = ?, emails = ?,
                   organization = ?, data = ?, base = ?, is_group = ? WHERE id = ?",
                params![
                    item.uid,
                    item.etag,
                    d.name,
                    d.sort_key,
                    d.search,
                    d.email,
                    d.emails,
                    d.organization,
                    data,
                    base,
                    item.is_group as i64,
                    id
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO contacts (book_id, uid, remote_id, etag, name, sort_key, search, email, emails,
                   organization, data, base, is_group)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    book_id,
                    item.uid,
                    item.remote_id,
                    item.etag,
                    d.name,
                    d.sort_key,
                    d.search,
                    d.email,
                    d.emails,
                    d.organization,
                    data,
                    base,
                    item.is_group as i64
                ],
            )?;
            tx.last_insert_rowid()
        }
    };
    apply_photo(tx, id, &item.photo)?;
    Ok(Some(id))
}

/// Removes a contact the server no longer has. Contacts changed here stay:
/// pushing them puts them back on the server. Returns whether a row went.
pub fn delete_synced(tx: &Transaction<'_>, book_id: i64, remote_id: &str) -> Result<bool> {
    Ok(tx.execute(
        "DELETE FROM contacts WHERE book_id = ? AND remote_id = ? AND local_state IN (?, ?)",
        params![book_id, remote_id, SYNCED, DELETED],
    )? > 0)
}

/// After a full listing: removes synced contacts the server did not report.
pub fn sweep(tx: &Transaction<'_>, book_id: i64, seen: &HashSet<String>) -> Result<usize> {
    let stale: Vec<i64> = {
        let mut st = tx.prepare(
            "SELECT id, remote_id FROM contacts WHERE book_id = ? AND remote_id IS NOT NULL AND local_state IN (?, ?)",
        )?;
        st.query_map(params![book_id, SYNCED, DELETED], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter(|(_, remote)| !seen.contains(remote))
        .map(|(id, _)| id)
        .collect()
    };
    for id in &stale {
        tx.execute("DELETE FROM contacts WHERE id = ?", [id])?;
    }
    Ok(stale.len())
}

/// A local edit of the photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhotoEdit {
    Unchanged,
    Removed,
    Set { photo: Photo, thumb: Option<String> },
}

fn apply_photo_edit(tx: &Transaction<'_>, id: i64, edit: &PhotoEdit, dirty: bool) -> Result<()> {
    match edit {
        PhotoEdit::Unchanged => {}
        PhotoEdit::Removed => {
            tx.execute(
                "UPDATE contacts SET photo = NULL, photo_mime = NULL, photo_hash = NULL, thumb = NULL,
                   photo_dirty = ?, photo_src = photo_url WHERE id = ?",
                params![dirty as i64, id],
            )?;
        }
        PhotoEdit::Set { photo, thumb } => {
            tx.execute(
                "UPDATE contacts SET photo = ?, photo_mime = ?, photo_hash = ?, thumb = ?, photo_dirty = ?,
                   photo_src = photo_url WHERE id = ?",
                params![
                    photo.bytes,
                    photo.mime,
                    photo_hash(&photo.bytes),
                    thumb,
                    dirty as i64,
                    id
                ],
            )?;
        }
    }
    Ok(())
}

/// Stores a contact created here. `remote_id` may be assigned up front (a
/// CardDAV href), which makes repeated uploads idempotent.
pub fn insert_local(
    tx: &Transaction<'_>,
    book_id: i64,
    uid: &str,
    remote_id: Option<&str>,
    contact: &Contact,
    state: i64,
    photo: &PhotoEdit,
) -> Result<i64> {
    let d = derived(contact);
    tx.execute(
        "INSERT INTO contacts (book_id, uid, remote_id, name, sort_key, search, email, emails, organization,
           data, local_state, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
        params![
            book_id,
            uid,
            remote_id,
            d.name,
            d.sort_key,
            d.search,
            d.email,
            d.emails,
            d.organization,
            data_json(contact)?,
            state
        ],
    )?;
    let id = tx.last_insert_rowid();
    apply_photo_edit(tx, id, photo, state != SYNCED)?;
    Ok(id)
}

/// Stores a local edit of a contact.
pub fn update_local(
    tx: &Transaction<'_>,
    id: i64,
    contact: &Contact,
    state: i64,
    photo: &PhotoEdit,
) -> Result<()> {
    let d = derived(contact);
    tx.execute(
        "UPDATE contacts SET name = ?, sort_key = ?, search = ?, email = ?, emails = ?, organization = ?, data = ?,
           local_state = ?, version = version + 1, push_attempts = 0, push_after = 0, push_error = NULL
         WHERE id = ?",
        params![
            d.name,
            d.sort_key,
            d.search,
            d.email,
            d.emails,
            d.organization,
            data_json(contact)?,
            state,
            id
        ],
    )?;
    apply_photo_edit(tx, id, photo, state != SYNCED)?;
    Ok(())
}

/// Marks a contact deleted here; the push step deletes it on the server.
pub fn mark_deleted(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE contacts SET local_state = ?, version = version + 1, push_attempts = 0, push_after = 0,
           push_error = NULL WHERE id = ?",
        params![DELETED, id],
    )?;
    Ok(())
}

pub fn delete_row(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("DELETE FROM contacts WHERE id = ?", [id])?;
    Ok(())
}

/// Contacts of a book with local changes due to be pushed.
pub fn dirty_rows(c: &Connection, book_id: i64) -> Result<Vec<ContactRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {ROW_COLUMNS} FROM contacts WHERE book_id = ? AND local_state != 0 AND push_after <= ?
         ORDER BY id"
    ))?;
    let rows = st.query_map(params![book_id, now_ms()], contact_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// When the next upload waiting out a retry delay comes due. Uploads that
/// are already due are left to the next regular round.
pub fn next_push_at(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(push_after) FROM contacts WHERE local_state != 0 AND push_after > ?",
        [now_ms()],
        |r| r.get(0),
    )?)
}

/// The server's answer to a successful upload.
#[derive(Debug, Clone)]
pub struct Pushed {
    pub remote_id: String,
    pub etag: Option<String>,
    pub uid: String,
    /// The contact as the server stores it now.
    pub contact: Contact,
    pub base: Option<String>,
    /// Remote photo now on the server, already stored locally.
    pub photo_url: Option<String>,
}

/// Records a successful upload of the contact as it was at `version`. If it
/// changed meanwhile, the newer local state is kept (and pushed next time).
pub fn finish_push(tx: &Transaction<'_>, id: i64, version: i64, pushed: &Pushed) -> Result<()> {
    let current: Option<(i64, i64)> = tx
        .query_row(
            "SELECT version, local_state FROM contacts WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((current_version, state)) = current else {
        return Ok(());
    };
    let base = pushed.base.as_deref().map(pack);
    if current_version == version {
        let d = derived(&pushed.contact);
        tx.execute(
            "UPDATE contacts SET remote_id = ?, etag = ?, uid = ?, base = ?, name = ?, sort_key = ?, search = ?,
               email = ?, emails = ?, organization = ?, data = ?, local_state = 0, photo_dirty = 0,
               photo_url = ?, photo_src = ?, push_attempts = 0, push_after = 0, push_error = NULL
             WHERE id = ?",
            params![
                pushed.remote_id,
                pushed.etag,
                pushed.uid,
                base,
                d.name,
                d.sort_key,
                d.search,
                d.email,
                d.emails,
                d.organization,
                data_json(&pushed.contact)?,
                pushed.photo_url,
                pushed.photo_url,
                id
            ],
        )?;
    } else {
        // Edited again while uploading: it exists on the server now, so the
        // newer state is an update of what was just written.
        let state = if state == CREATED { MODIFIED } else { state };
        tx.execute(
            "UPDATE contacts SET remote_id = ?, etag = ?, uid = ?, base = ?, local_state = ?, push_attempts = 0,
               push_after = 0, push_error = NULL WHERE id = ?",
            params![
                pushed.remote_id,
                pushed.etag,
                pushed.uid,
                base,
                state,
                id
            ],
        )?;
    }
    Ok(())
}

/// Records a failed upload to retry at `retry_at`.
pub fn push_failed(tx: &Transaction<'_>, id: i64, error: &str, retry_at: i64) -> Result<()> {
    tx.execute(
        "UPDATE contacts SET push_attempts = push_attempts + 1, push_error = ?, push_after = ? WHERE id = ?",
        params![error, retry_at, id],
    )?;
    Ok(())
}

/// Gives up on a local change the server keeps refusing: the row goes and
/// the book is synced from scratch, which brings back the server's copy.
pub fn discard_local_change(tx: &Transaction<'_>, id: i64, book_id: i64) -> Result<()> {
    tx.execute("DELETE FROM contacts WHERE id = ?", [id])?;
    tx.execute(
        "UPDATE address_books SET sync_state = '{}' WHERE id = ?",
        [book_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Photos

/// Contacts whose remote photo still has to be fetched: `(id, book, remote
/// id, photo url)`.
pub fn photos_to_fetch(c: &Connection, limit: i64) -> Result<Vec<(i64, i64, String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT c.id, c.book_id, coalesce(c.remote_id, ''), c.photo_url FROM contacts c
         JOIN address_books b ON b.id = c.book_id JOIN accounts a ON a.id = b.account_id
         WHERE a.enabled = 1 AND c.photo_url IS NOT NULL AND c.photo_url IS NOT c.photo_src
           AND c.photo_dirty = 0 AND c.local_state != ?
         ORDER BY c.id LIMIT ?",
    )?;
    let rows = st.query_map(params![DELETED, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Stores a fetched photo (`None`: there is none), unless the contact asked
/// for another one meanwhile.
pub fn store_fetched_photo(
    tx: &Transaction<'_>,
    id: i64,
    url: &str,
    photo: Option<&Photo>,
) -> Result<bool> {
    let changed = match photo {
        Some(p) => tx.execute(
            "UPDATE contacts SET photo = ?, photo_mime = ?, photo_hash = ?, thumb = NULL, photo_src = ?
             WHERE id = ? AND photo_url = ? AND photo_dirty = 0",
            params![p.bytes, p.mime, photo_hash(&p.bytes), url, id, url],
        )?,
        None => tx.execute(
            "UPDATE contacts SET photo = NULL, photo_mime = NULL, photo_hash = NULL, thumb = NULL, photo_src = ?
             WHERE id = ? AND photo_url = ? AND photo_dirty = 0",
            params![url, id, url],
        )?,
    };
    Ok(changed > 0)
}

/// Contacts with a photo but no list thumbnail yet: `(id, book, hash,
/// photo)`.
pub fn photos_without_thumbs(
    c: &Connection,
    limit: i64,
) -> Result<Vec<(i64, i64, String, Vec<u8>)>> {
    let mut st = c.prepare_cached(
        "SELECT id, book_id, coalesce(photo_hash, ''), photo FROM contacts
         WHERE photo IS NOT NULL AND thumb IS NULL ORDER BY id LIMIT ?",
    )?;
    let rows = st.query_map([limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Stores a list thumbnail (an empty string marks photos that cannot be
/// decoded, so they are not tried again).
pub fn set_thumb(tx: &Transaction<'_>, id: i64, hash: &str, thumb: &str) -> Result<()> {
    tx.execute(
        "UPDATE contacts SET thumb = ? WHERE id = ? AND coalesce(photo_hash, '') = ?",
        params![thumb, id, hash],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Listing and autocomplete

/// One row of the contact list.
#[derive(Debug, Clone)]
pub struct SummaryRow {
    pub id: i64,
    pub book_id: i64,
    pub name: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub thumb: Option<String>,
}

/// Turns a search query into folded terms that must all match. Phone-like
/// terms are reduced to their digits, so `+1 (555) 01` finds `15550100`.
pub fn search_terms(query: &str) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let phone_like = query
        .chars()
        .all(|c| c.is_ascii_digit() || " +-().\u{a0}/".contains(c));
    let digits = phone_digits(query);
    if phone_like && digits.len() >= 3 {
        return vec![digits];
    }
    query
        .split_whitespace()
        .map(fold)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Visible contacts: not deleted, not group cards, in enabled accounts.
const VISIBLE: &str =
    "c.local_state != 3 AND c.is_group = 0 AND (b.account_id IS NULL OR a.enabled = 1)";

/// Contacts matching `terms` (all of them) in one book or all, in list order.
pub fn list_contacts(
    c: &Connection,
    terms: &[String],
    book_id: Option<i64>,
    offset: i64,
    limit: i64,
) -> Result<(i64, Vec<SummaryRow>)> {
    let mut filter = String::from(VISIBLE);
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(book) = book_id {
        filter.push_str(" AND c.book_id = ?");
        args.push(book.into());
    }
    for term in terms {
        filter.push_str(" AND instr(c.search, ?) > 0");
        args.push(term.clone().into());
    }
    let from = "FROM contacts c JOIN address_books b ON b.id = c.book_id LEFT JOIN accounts a ON a.id = b.account_id";
    let total: i64 = c.query_row(
        &format!("SELECT count(*) {from} WHERE {filter}"),
        rusqlite::params_from_iter(args.iter()),
        |r| r.get(0),
    )?;
    let mut page_args = args.clone();
    page_args.push(limit.max(0).into());
    page_args.push(offset.max(0).into());
    let mut st = c.prepare(&format!(
        "SELECT c.id, c.book_id, c.name, c.email, c.organization, nullif(c.thumb, '') {from}
         WHERE {filter} ORDER BY c.sort_key, c.id LIMIT ? OFFSET ?"
    ))?;
    let rows = st.query_map(rusqlite::params_from_iter(page_args.iter()), |r| {
        Ok(SummaryRow {
            id: r.get(0)?,
            book_id: r.get(1)?,
            name: r.get(2)?,
            email: r.get(3)?,
            organization: r.get(4)?,
            thumb: r.get(5)?,
        })
    })?;
    Ok((total, rows.collect::<rusqlite::Result<_>>()?))
}

/// A contact found for address autocomplete.
#[derive(Debug, Clone)]
pub struct EmailMatch {
    pub name: String,
    /// The contact's e-mail addresses, primary first.
    pub emails: Vec<String>,
    /// The query matched the name (rather than only an address).
    pub name_match: bool,
}

/// Contacts with e-mail addresses where a name word or an address starts
/// with `query` (folded).
pub fn email_matches(c: &Connection, query: &str, limit: usize) -> Result<Vec<EmailMatch>> {
    let q = fold(query.trim());
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let mut st = c.prepare_cached(&format!(
        "SELECT c.name, c.email, c.emails, c.search FROM contacts c
         JOIN address_books b ON b.id = c.book_id LEFT JOIN accounts a ON a.id = b.account_id
         WHERE {VISIBLE} AND c.emails != '' AND instr(c.search, ?) > 0 LIMIT 400"
    ))?;
    let rows = st.query_map([&q], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let starts_word = |text: &str| {
        text.split(|ch: char| !ch.is_alphanumeric())
            .any(|w| w.starts_with(&q))
    };
    let mut out = Vec::new();
    for row in rows {
        let (name, primary, emails, search) = row?;
        let emails: Vec<String> = emails.lines().map(str::to_owned).collect();
        let name_part = search
            .lines()
            .take_while(|l| !l.contains('@'))
            .collect::<Vec<_>>();
        let name_match = name_part.iter().any(|l| starts_word(l));
        let email_match = emails
            .iter()
            .any(|e| e.starts_with(&q) || e.split('@').next().is_some_and(starts_word));
        if !name_match && !email_match {
            continue;
        }
        let mut ordered = emails.clone();
        if let Some(primary) = primary.map(|p| p.to_lowercase())
            && let Some(i) = ordered.iter().position(|e| *e == primary)
        {
            let p = ordered.remove(i);
            ordered.insert(0, p);
        }
        out.push(EmailMatch {
            name,
            emails: ordered,
            name_match,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(fold("Öztürk"), "ozturk");
        assert_eq!(fold("İLKER Işık"), "ilker isik");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("Łódź"), "lodz");
        assert_eq!(fold("E\u{301}mile"), "emile");
        assert_eq!(fold("Ἀθῆναι"), "ἀθῆναι");
    }

    #[test]
    fn ordering_groups_sections() {
        let mut names = vec![
            "Öykü", "oliver", "Oscar", "zeynep", "Ömer", "Ivan", "İlker", "123 Taxi", "", "Ahmet",
            "Çağla", "Can",
        ];
        names.sort_by_key(|n| sort_key(n));
        assert_eq!(
            names,
            [
                "Ahmet", "Can", "Çağla", "Ivan", "İlker", "oliver", "Oscar", "Ömer", "Öykü",
                "zeynep", "", "123 Taxi"
            ]
        );
        assert_eq!(section("öykü").as_deref(), Some("Ö"));
        assert_eq!(section("  ahmet").as_deref(), Some("A"));
        assert_eq!(section("123"), None);
        assert_eq!(section("😀 Smiley"), None);
    }

    #[test]
    fn search_terms_for_names_and_phones() {
        assert_eq!(search_terms("  Ada  LOVE "), ["ada", "love"]);
        assert_eq!(search_terms("+90 (532) 111"), ["90532111"]);
        assert_eq!(search_terms("12"), ["12"], "too short for a phone number");
        assert!(search_terms("   ").is_empty());
    }
}
