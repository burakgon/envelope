//! vCard 2.1 / 3.0 / 4.0 parsing and serialization (RFC 2426, RFC 6350).
//!
//! Parsing understands quoted-printable values with `CHARSET` (2.1), the
//! various `TYPE` syntaxes (`TYPE=WORK,VOICE`, `TYPE=work;TYPE=voice`,
//! `TYPE="work,voice"`, bare 2.1 `WORK;VOICE`), `PREF`, Apple's grouped
//! labels (`item1.EMAIL` + `item1.X-ABLabel`), birthdays without a year and
//! photos as base64, `data:` URIs or URLs.
//!
//! New cards are written as vCard 3.0, the version with the widest CardDAV
//! support. [`upsert_contact`] edits existing cards in place, keeping
//! unknown properties and the card's version (2.1 is upgraded to 3.0).

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate};

use crate::charset;
use crate::component::{Component, Param, Property};
use crate::error::{Error, Result};
use crate::model::{Contact, ContactField};
use crate::mutate::PRODID;
use crate::parser::{ParseOptions, decode_bytes, parse_with};
use crate::text::{escape_text, split_unescaped, text_list, unescape_text};
use crate::writer::write_components;

/// Year used for birthdays without a year (`BDAY:--0415`). 1604 is a leap
/// year, so February 29 can be represented; it is also Apple's convention
/// (`X-APPLE-OMIT-YEAR=1604`).
pub const NO_YEAR: i32 = 1604;

fn parse_cards(text: &str) -> Vec<Component> {
    parse_with(
        text,
        ParseOptions {
            quoted_printable: true,
        },
    )
    .into_iter()
    .filter(|c| c.is("VCARD"))
    .collect()
}

/// Parses every card of a vCard stream. Malformed input yields fewer (or
/// no) contacts but never fails.
///
/// ```
/// let contacts = envelope_ical::vcard::parse_vcards(
///     "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nEMAIL;TYPE=INTERNET,HOME:ada@example.org\r\nEND:VCARD\r\n",
/// );
/// assert_eq!(contacts[0].name, "Ada Lovelace");
/// assert_eq!(contacts[0].emails[0].kind.as_deref(), Some("home"));
/// ```
pub fn parse_vcards(text: &str) -> Vec<Contact> {
    parse_cards(text).iter().map(contact_from_vcard).collect()
}

/// [`parse_vcards`] from raw bytes (byte-level unfolding, legacy charsets).
pub fn parse_vcards_bytes(bytes: &[u8]) -> Vec<Contact> {
    parse_vcards(&decode_bytes(bytes))
}

/// Parses the first card of a vCard stream.
pub fn parse_vcard(text: &str) -> Result<Contact> {
    parse_cards(text)
        .first()
        .map(contact_from_vcard)
        .ok_or(Error::NoVCard)
}

fn is_quoted_printable(property: &Property) -> bool {
    property
        .param_values("ENCODING")
        .iter()
        .any(|e| e.eq_ignore_ascii_case("QUOTED-PRINTABLE"))
        || property
            .params
            .iter()
            .any(|p| p.values.is_empty() && p.name == "QUOTED-PRINTABLE")
}

fn is_base64(property: &Property) -> bool {
    property
        .param_values("ENCODING")
        .iter()
        .any(|e| e.eq_ignore_ascii_case("B") || e.eq_ignore_ascii_case("BASE64"))
        || property
            .params
            .iter()
            .any(|p| p.values.is_empty() && p.name == "BASE64")
}

fn decode_quoted_printable(raw: &str) -> Vec<u8> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if let Some(byte) = raw
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                out.push(byte);
                i += 3;
                continue;
            }
            // A stray '=' (e.g. a trailing soft break): drop it.
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The property's raw value with quoted-printable and charset decoded.
fn raw_value(property: &Property) -> String {
    if is_quoted_printable(property) {
        charset::decode(
            &decode_quoted_printable(&property.value),
            property.param("CHARSET"),
        )
    } else {
        property.value.clone()
    }
}

fn text_of(property: &Property) -> String {
    unescape_text(&raw_value(property))
        .replace("\r\n", "\n")
        .trim()
        .to_owned()
}

/// Lower-cased `TYPE` values in all their syntaxes.
fn types_of(property: &Property) -> Vec<String> {
    let mut types = Vec::new();
    for param in &property.params {
        if param.name == "TYPE" {
            for value in &param.values {
                types.extend(
                    value
                        .split(',')
                        .map(|t| t.trim().to_ascii_lowercase())
                        .filter(|t| !t.is_empty()),
                );
            }
        } else if param.values.is_empty()
            && !matches!(
                param.name.as_str(),
                "QUOTED-PRINTABLE" | "BASE64" | "8BIT" | "7BIT"
            )
        {
            types.push(param.name.to_ascii_lowercase());
        }
    }
    types
}

/// Kind label of an Apple `X-ABLabel` (`_$!<Work>!$_` → `work`).
fn apple_label(label: &str) -> String {
    let label = label.trim();
    let inner = label
        .strip_prefix("_$!<")
        .and_then(|l| l.strip_suffix(">!$_"))
        .unwrap_or(label);
    match inner.to_ascii_lowercase().as_str() {
        "mobile" | "iphone" => "cell".to_owned(),
        "homefax" | "workfax" | "otherfax" => "fax".to_owned(),
        other => other.to_owned(),
    }
}

fn field(
    property: &Property,
    value: String,
    labels: &HashMap<String, String>,
    telephone: bool,
) -> ContactField {
    let types = types_of(property);
    let preferred = types.iter().any(|t| t == "pref")
        || property.param("PREF").is_some_and(|p| p.trim() == "1");
    let ignored: &[&str] = if telephone {
        &["pref", "voice", "msg", "text", "video", "internet"]
    } else {
        &["pref", "internet", "x400"]
    };
    let meaningful: Vec<&String> = types
        .iter()
        .filter(|t| !ignored.contains(&t.as_str()))
        .collect();
    let kind = if telephone
        && meaningful
            .iter()
            .any(|t| matches!(t.as_str(), "cell" | "mobile" | "iphone"))
    {
        Some("cell".to_owned())
    } else if telephone && meaningful.iter().any(|t| t.as_str() == "fax") {
        Some("fax".to_owned())
    } else {
        meaningful.first().map(|t| (*t).clone())
    };
    let kind = kind.or_else(|| {
        property
            .group
            .as_ref()
            .and_then(|g| labels.get(&g.to_ascii_lowercase()))
            .map(|l| apple_label(l))
            .filter(|l| !l.is_empty())
    });
    ContactField {
        value,
        kind,
        preferred,
    }
}

fn parse_birthday(property: &Property) -> Option<NaiveDate> {
    if property
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("text"))
    {
        return None;
    }
    let raw = raw_value(property);
    let date_part = raw.trim().split(['T', 't']).next().unwrap_or("").trim();
    if let Some(rest) = date_part.strip_prefix("--") {
        let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
        if digits.len() != 4 {
            return None;
        }
        let month = digits[..2].parse().ok()?;
        let day = digits[2..].parse().ok()?;
        return NaiveDate::from_ymd_opt(NO_YEAR, month, day);
    }
    let digits: String = date_part.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 8 {
        return None;
    }
    NaiveDate::from_ymd_opt(
        digits[..4].parse().ok()?,
        digits[4..6].parse().ok()?,
        digits[6..].parse().ok()?,
    )
}

/// MIME type of base64 image data, from its `TYPE` parameter or magic bytes.
fn image_mime(type_param: Option<&str>, base64: &str) -> String {
    if let Some(t) = type_param.map(|t| t.trim().to_ascii_lowercase()) {
        if t.contains('/') {
            return t;
        }
        match t.as_str() {
            "jpeg" | "jpg" => return "image/jpeg".to_owned(),
            "png" => return "image/png".to_owned(),
            "gif" => return "image/gif".to_owned(),
            "bmp" => return "image/bmp".to_owned(),
            "webp" => return "image/webp".to_owned(),
            "heic" => return "image/heic".to_owned(),
            _ => {}
        }
    }
    let mime = if base64.starts_with("/9j/") {
        "image/jpeg"
    } else if base64.starts_with("iVBORw0KGgo") {
        "image/png"
    } else if base64.starts_with("R0lGOD") {
        "image/gif"
    } else if base64.starts_with("UklGR") {
        "image/webp"
    } else if base64.starts_with("Qk") {
        "image/bmp"
    } else {
        "image/jpeg"
    };
    mime.to_owned()
}

fn parse_photo(property: &Property) -> Option<String> {
    let raw = raw_value(property);
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    let lower = compact.to_ascii_lowercase();
    if lower.starts_with("data:") {
        return Some(compact);
    }
    let is_uri = property
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("uri") || v.eq_ignore_ascii_case("url"));
    if lower.starts_with("http://") || lower.starts_with("https://") || is_uri {
        return Some(raw.trim().to_owned());
    }
    let looks_base64 = compact
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    if is_base64(property) || (looks_base64 && compact.len() > 32) {
        let type_param = property
            .param_values("TYPE")
            .first()
            .copied()
            .or_else(|| {
                property
                    .params
                    .iter()
                    .find(|p| {
                        p.values.is_empty()
                            && matches!(p.name.as_str(), "JPEG" | "PNG" | "GIF" | "BMP")
                    })
                    .map(|p| p.name.as_str())
            })
            .or_else(|| property.param("MEDIATYPE"));
        let mime = image_mime(type_param, &compact);
        return Some(format!("data:{mime};base64,{compact}"));
    }
    None
}

/// Converts one `VCARD` component into a [`Contact`].
pub fn contact_from_vcard(card: &Component) -> Contact {
    let labels: HashMap<String, String> = card
        .properties
        .iter()
        .filter(|p| p.name == "X-ABLABEL")
        .filter_map(|p| Some((p.group.as_ref()?.to_ascii_lowercase(), text_of(p))))
        .collect();
    let mut contact = Contact {
        uid: card
            .value("UID")
            .map(|u| u.trim().to_owned())
            .unwrap_or_default(),
        ..Contact::default()
    };
    let mut structured: Vec<String> = Vec::new();
    for property in &card.properties {
        match property.name.as_str() {
            "FN" => {
                if contact.name.is_empty() {
                    contact.name = text_of(property);
                }
            }
            "N" if structured.is_empty() => {
                structured = split_unescaped(&raw_value(property), ';')
                    .into_iter()
                    .map(|part| unescape_text(part).trim().to_owned())
                    .collect();
            }
            "NICKNAME" if contact.nickname.is_none() => {
                contact.nickname = text_list(&raw_value(property)).into_iter().next();
            }
            "EMAIL" => {
                let value = text_of(property);
                let value = value
                    .strip_prefix("mailto:")
                    .or_else(|| value.strip_prefix("MAILTO:"))
                    .unwrap_or(&value)
                    .trim()
                    .to_owned();
                if !value.is_empty() {
                    contact.emails.push(field(property, value, &labels, false));
                }
            }
            "TEL" => {
                let value = text_of(property);
                let value = value
                    .strip_prefix("tel:")
                    .or_else(|| value.strip_prefix("TEL:"))
                    .unwrap_or(&value)
                    .trim()
                    .to_owned();
                if !value.is_empty() {
                    contact.phones.push(field(property, value, &labels, true));
                }
            }
            "ORG" if contact.organization.is_none() => {
                contact.organization = split_unescaped(&raw_value(property), ';')
                    .first()
                    .map(|org| unescape_text(org).trim().to_owned())
                    .filter(|org| !org.is_empty());
            }
            "TITLE" if contact.title.is_none() => {
                contact.title = Some(text_of(property)).filter(|t| !t.is_empty());
            }
            "NOTE" if contact.note.is_none() => {
                contact.note = Some(text_of(property)).filter(|t| !t.is_empty());
            }
            "BDAY" if contact.birthday.is_none() => contact.birthday = parse_birthday(property),
            "PHOTO" if contact.photo.is_none() => contact.photo = parse_photo(property),
            _ => {}
        }
    }
    let component = |index: usize| {
        structured
            .get(index)
            .map(|s| s.replace(',', " ").trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    contact.family_name = component(0);
    contact.given_name = component(1);
    if contact.name.is_empty() {
        let parts: Vec<String> = [3, 1, 2, 0, 4]
            .iter()
            .filter_map(|&i| component(i))
            .collect();
        contact.name = if !parts.is_empty() {
            parts.join(" ")
        } else if let Some(org) = &contact.organization {
            org.clone()
        } else {
            contact
                .emails
                .first()
                .map(|e| e.value.clone())
                .unwrap_or_default()
        };
    }
    contact
}

/// vCard dialect used when writing managed properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    V3,
    V4,
}

fn is_simple_token(kind: &str) -> bool {
    !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Builds `EMAIL`/`TEL` properties. Kinds that are not simple tokens
/// become Apple-style group labels, which most clients display.
fn field_properties(
    name: &str,
    field: &ContactField,
    version: Version,
    next_group: &mut usize,
) -> Vec<Property> {
    let mut property = Property::text(name, &field.value);
    let mut extra = Vec::new();
    let mut types: Vec<String> = Vec::new();
    if name == "EMAIL" && version == Version::V3 {
        types.push("INTERNET".to_owned());
    }
    match field
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        Some(kind) if is_simple_token(kind) => types.push(match version {
            Version::V3 => kind.to_ascii_uppercase(),
            Version::V4 => kind.to_ascii_lowercase(),
        }),
        Some(label) => {
            let group = format!("item{next_group}");
            *next_group += 1;
            property.group = Some(group.clone());
            let mut label_property = Property::text("X-ABLabel", label);
            label_property.group = Some(group);
            extra.push(label_property);
        }
        None => {}
    }
    if field.preferred && version == Version::V3 {
        types.push("PREF".to_owned());
    }
    if !types.is_empty() {
        property.params.push(Param::with_values("TYPE", types));
    }
    if field.preferred && version == Version::V4 {
        property.set_param("PREF", "1");
    }
    let mut out = vec![property];
    out.extend(extra);
    out
}

fn birthday_property(date: NaiveDate, version: Version) -> Property {
    if date.year() == NO_YEAR {
        return match version {
            Version::V3 => Property::new("BDAY", date.format("%Y-%m-%d").to_string())
                .with_param("X-APPLE-OMIT-YEAR", NO_YEAR.to_string()),
            Version::V4 => Property::new("BDAY", date.format("--%m%d").to_string()),
        };
    }
    match version {
        Version::V3 => Property::new("BDAY", date.format("%Y-%m-%d").to_string()),
        Version::V4 => Property::new("BDAY", date.format("%Y%m%d").to_string()),
    }
}

fn photo_property(photo: &str, version: Version) -> Option<Property> {
    let photo = photo.trim();
    if photo.is_empty() {
        return None;
    }
    let lower = photo.to_ascii_lowercase();
    if version == Version::V4 {
        return Some(Property::new("PHOTO", photo));
    }
    if let Some(rest) = lower.strip_prefix("data:") {
        let (meta, _) = rest.split_once(',')?;
        let data = &photo[photo.find(',')? + 1..];
        let mime = meta.split(';').next().unwrap_or("image/jpeg");
        let kind = mime
            .strip_prefix("image/")
            .unwrap_or("jpeg")
            .to_ascii_uppercase();
        if !meta.contains("base64") {
            return None;
        }
        return Some(
            Property::new("PHOTO", data)
                .with_param("ENCODING", "b")
                .with_param("TYPE", kind),
        );
    }
    Some(Property::new("PHOTO", photo).with_param("VALUE", "uri"))
}

/// The `N` value. vCard 3.0 requires `N`; contacts without structured
/// names get an empty one (`N:;;;;`), as Apple writes for companies.
fn structured_name(contact: &Contact) -> String {
    format!(
        "{};{};;;",
        escape_text(contact.family_name.as_deref().unwrap_or("")),
        escape_text(contact.given_name.as_deref().unwrap_or(""))
    )
}

/// Serializes a [`Contact`] as a vCard 3.0 card (folded, CRLF).
pub fn contact_to_vcard(contact: &Contact) -> String {
    let mut card = Component::new("VCARD");
    card.push(Property::new("VERSION", "3.0"));
    card.push(Property::new("PRODID", PRODID));
    if !contact.uid.trim().is_empty() {
        card.push(Property::new("UID", contact.uid.trim()));
    }
    card.push(Property::text("FN", contact.name.trim()));
    card.push(Property::new("N", structured_name(contact)));
    if let Some(nickname) = contact.nickname.as_deref().filter(|n| !n.is_empty()) {
        card.push(Property::text("NICKNAME", nickname));
    }
    let mut group = 1;
    for email in &contact.emails {
        card.properties
            .extend(field_properties("EMAIL", email, Version::V3, &mut group));
    }
    for phone in &contact.phones {
        card.properties
            .extend(field_properties("TEL", phone, Version::V3, &mut group));
    }
    if let Some(org) = contact.organization.as_deref().filter(|o| !o.is_empty()) {
        card.push(Property::new("ORG", escape_text(org)));
    }
    if let Some(title) = contact.title.as_deref().filter(|t| !t.is_empty()) {
        card.push(Property::text("TITLE", title));
    }
    if let Some(note) = contact.note.as_deref().filter(|n| !n.is_empty()) {
        card.push(Property::text("NOTE", note));
    }
    if let Some(birthday) = contact.birthday {
        card.push(birthday_property(birthday, Version::V3));
    }
    if let Some(photo) = contact
        .photo
        .as_deref()
        .and_then(|p| photo_property(p, Version::V3))
    {
        card.push(photo);
    }
    card.to_ical()
}

/// Upgrades a vCard 2.1 card in place to 3.0 syntax: decodes
/// quoted-printable values, turns bare type parameters into `TYPE=` and
/// re-escapes text values.
fn upgrade_from_21(card: &mut Component) {
    const TEXT: [&str; 6] = ["FN", "NOTE", "TITLE", "NICKNAME", "LABEL", "ROLE"];
    const STRUCTURED: [&str; 3] = ["N", "ADR", "ORG"];
    for property in &mut card.properties {
        if is_quoted_printable(property) {
            property.value = raw_value(property);
            property.params.retain(|p| {
                !(p.name == "ENCODING"
                    || p.name == "CHARSET"
                    || (p.values.is_empty() && p.name == "QUOTED-PRINTABLE"))
            });
        }
        let mut types = Vec::new();
        let mut encoding_b = false;
        property.params.retain(|p| {
            if !p.values.is_empty() {
                return true;
            }
            match p.name.as_str() {
                "BASE64" => encoding_b = true,
                "8BIT" | "7BIT" | "QUOTED-PRINTABLE" => {}
                other => types.push(other.to_owned()),
            }
            false
        });
        if property
            .param("ENCODING")
            .is_some_and(|e| e.eq_ignore_ascii_case("BASE64"))
        {
            encoding_b = true;
        }
        if encoding_b {
            property.set_param("ENCODING", "b");
        }
        if !types.is_empty() {
            let mut all: Vec<String> = property
                .param_values("TYPE")
                .iter()
                .map(|t| (*t).to_owned())
                .collect();
            all.extend(types);
            property.set_param_values("TYPE", all);
        }
        if TEXT.contains(&property.name.as_str()) {
            property.value = escape_text(&unescape_text(&property.value));
        } else if STRUCTURED.contains(&property.name.as_str()) {
            property.value = split_unescaped(&property.value, ';')
                .into_iter()
                .map(|part| escape_text(&unescape_text(part)))
                .collect::<Vec<_>>()
                .join(";");
        }
    }
    card.set(Property::new("VERSION", "3.0"));
}

/// Inserts or updates `contact` in existing vCard text, keeping every
/// property the [`Contact`] model does not cover (addresses, URLs,
/// `X-` extensions, …).
///
/// The card with the contact's UID is edited (or the first card when the
/// contact has no UID); other cards in the stream are left untouched.
/// Only fields whose value changed are rewritten, in the syntax of the
/// card's version (3.0 or 4.0; 2.1 cards are upgraded to 3.0). Empty
/// `existing` text produces a new 3.0 card.
pub fn upsert_contact(existing: &str, contact: &Contact) -> Result<String> {
    if existing.trim().is_empty() {
        return Ok(contact_to_vcard(contact));
    }
    let mut cards = parse_cards(existing);
    if cards.is_empty() {
        return Err(Error::NoVCard);
    }
    let uid = contact.uid.trim();
    let index = cards
        .iter()
        .position(|c| !uid.is_empty() && c.value("UID").map(str::trim) == Some(uid))
        .unwrap_or(0);
    let card = &mut cards[index];
    if card.value("VERSION").map(str::trim) == Some("2.1") {
        upgrade_from_21(card);
    }
    let version = if card.value("VERSION").map(str::trim) == Some("4.0") {
        Version::V4
    } else {
        Version::V3
    };
    let old = contact_from_vcard(card);

    if old.uid != contact.uid && !uid.is_empty() {
        card.set(Property::new("UID", uid));
    }
    if old.name != contact.name {
        card.set(Property::text("FN", contact.name.trim()));
    }
    if old.family_name != contact.family_name || old.given_name != contact.given_name {
        let mut parts: Vec<String> = card
            .property("N")
            .map(|n| {
                split_unescaped(&n.value, ';')
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        parts.resize(parts.len().max(5), String::new());
        parts[0] = escape_text(contact.family_name.as_deref().unwrap_or(""));
        parts[1] = escape_text(contact.given_name.as_deref().unwrap_or(""));
        card.set(Property::new("N", parts.join(";")));
    }
    if old.nickname != contact.nickname {
        match contact.nickname.as_deref().filter(|n| !n.is_empty()) {
            Some(nickname) => card.set(Property::text("NICKNAME", nickname)),
            None => {
                card.remove_all("NICKNAME");
            }
        }
    }
    if old.emails != contact.emails || old.phones != contact.phones {
        merge_fields(card, &contact.emails, &contact.phones, version);
    }
    if old.organization != contact.organization {
        match contact.organization.as_deref().filter(|o| !o.is_empty()) {
            Some(org) => {
                let mut parts: Vec<String> = card
                    .property("ORG")
                    .map(|o| {
                        split_unescaped(&o.value, ';')
                            .into_iter()
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if parts.is_empty() {
                    parts.push(String::new());
                }
                parts[0] = escape_text(org);
                card.set(Property::new("ORG", parts.join(";")));
            }
            None => {
                card.remove_all("ORG");
            }
        }
    }
    for (name, old_value, new_value) in [
        ("TITLE", &old.title, &contact.title),
        ("NOTE", &old.note, &contact.note),
    ] {
        if old_value != new_value {
            match new_value.as_deref().filter(|v| !v.is_empty()) {
                Some(value) => card.set(Property::text(name, value)),
                None => {
                    card.remove_all(name);
                }
            }
        }
    }
    if old.birthday != contact.birthday {
        match contact.birthday {
            Some(date) => card.set(birthday_property(date, version)),
            None => {
                card.remove_all("BDAY");
            }
        }
    }
    if old.photo != contact.photo {
        match contact
            .photo
            .as_deref()
            .and_then(|p| photo_property(p, version))
        {
            Some(photo) => card.set(photo),
            None => {
                card.remove_all("PHOTO");
            }
        }
    }
    Ok(write_components(&cards))
}

/// Rewrites `EMAIL`/`TEL` properties: entries that did not change keep
/// their original property (group, label, parameters); orphaned group
/// labels are removed.
fn merge_fields(
    card: &mut Component,
    emails: &[ContactField],
    phones: &[ContactField],
    version: Version,
) {
    let labels: HashMap<String, String> = card
        .properties
        .iter()
        .filter(|p| p.name == "X-ABLABEL")
        .filter_map(|p| Some((p.group.as_ref()?.to_ascii_lowercase(), text_of(p))))
        .collect();
    let mut next_group = card
        .properties
        .iter()
        .filter_map(|p| {
            p.group
                .as_ref()?
                .strip_prefix("item")?
                .parse::<usize>()
                .ok()
        })
        .max()
        .unwrap_or(0)
        + 1;
    for (name, wanted) in [("EMAIL", emails), ("TEL", phones)] {
        let existing: Vec<Property> = card.properties_named(name).cloned().collect();
        let mut used = vec![false; existing.len()];
        let mut out = Vec::new();
        for field_value in wanted {
            let reuse = existing.iter().enumerate().position(|(i, p)| {
                if used[i] {
                    return false;
                }
                let value = text_of(p);
                let value = value
                    .strip_prefix("mailto:")
                    .or_else(|| value.strip_prefix("tel:"))
                    .unwrap_or(&value)
                    .trim()
                    .to_owned();
                field(p, value, &labels, name == "TEL") == *field_value
            });
            match reuse {
                Some(i) => {
                    used[i] = true;
                    out.push(existing[i].clone());
                }
                None => out.extend(field_properties(
                    name,
                    field_value,
                    version,
                    &mut next_group,
                )),
            }
        }
        // Labels are separate properties; keep them out of the field list.
        let (fields, new_labels): (Vec<Property>, Vec<Property>) =
            out.into_iter().partition(|p| p.name == name);
        card.set_all(name, fields);
        card.properties.extend(new_labels);
    }
    // Drop labels whose group no longer has any other property.
    let groups_in_use: Vec<String> = card
        .properties
        .iter()
        .filter(|p| p.name != "X-ABLABEL")
        .filter_map(|p| p.group.as_ref().map(|g| g.to_ascii_lowercase()))
        .collect();
    card.properties.retain(|p| {
        p.name != "X-ABLABEL"
            || p.group
                .as_ref()
                .is_none_or(|g| groups_in_use.contains(&g.to_ascii_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_printable_and_charset() {
        let text = "BEGIN:VCARD\r\nVERSION:2.1\r\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:=C3=96zt=C3=BCrk;Ay=C5=9Fe;;;\r\nFN;CHARSET=ISO-8859-9;ENCODING=QUOTED-PRINTABLE:Ay=FEe =D6zt=FCrk\r\nNOTE;ENCODING=QUOTED-PRINTABLE:line one=0D=0A=\r\nline two\r\nEND:VCARD\r\n";
        let contact = parse_vcard(text).expect("card");
        assert_eq!(contact.family_name.as_deref(), Some("Öztürk"));
        assert_eq!(contact.given_name.as_deref(), Some("Ayşe"));
        assert_eq!(contact.name, "Ayşe Öztürk");
        assert_eq!(contact.note.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn birthdays() {
        let bday = |line: &str| {
            parse_vcard(&format!(
                "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\n{line}\r\nEND:VCARD\r\n"
            ))
            .expect("card")
            .birthday
        };
        assert_eq!(bday("BDAY:19850415"), NaiveDate::from_ymd_opt(1985, 4, 15));
        assert_eq!(
            bday("BDAY:1985-04-15"),
            NaiveDate::from_ymd_opt(1985, 4, 15)
        );
        assert_eq!(
            bday("BDAY:1985-04-15T00:00:00Z"),
            NaiveDate::from_ymd_opt(1985, 4, 15)
        );
        assert_eq!(bday("BDAY:--0415"), NaiveDate::from_ymd_opt(NO_YEAR, 4, 15));
        assert_eq!(
            bday("BDAY:--04-15"),
            NaiveDate::from_ymd_opt(NO_YEAR, 4, 15)
        );
        assert_eq!(bday("BDAY:--0229"), NaiveDate::from_ymd_opt(NO_YEAR, 2, 29));
        assert_eq!(
            bday("BDAY;X-APPLE-OMIT-YEAR=1604:1604-04-15"),
            NaiveDate::from_ymd_opt(NO_YEAR, 4, 15)
        );
        assert_eq!(bday("BDAY;VALUE=text:circa 1800"), None);
        assert_eq!(bday("BDAY:1985-04"), None);
    }

    #[test]
    fn serialization_round_trip() {
        let contact = Contact {
            uid: "c-1".into(),
            name: "Ayşe Öztürk".into(),
            given_name: Some("Ayşe".into()),
            family_name: Some("Öztürk".into()),
            nickname: Some("Ayşo".into()),
            emails: vec![
                ContactField {
                    value: "ayse@example.com".into(),
                    kind: Some("work".into()),
                    preferred: true,
                },
                ContactField {
                    value: "a@home.example".into(),
                    kind: Some("Personal mail".into()),
                    preferred: false,
                },
            ],
            phones: vec![ContactField {
                value: "+90 532 000 00 00".into(),
                kind: Some("cell".into()),
                preferred: false,
            }],
            organization: Some("Örnek A.Ş., İstanbul".into()),
            title: Some("Engineer; Lead".into()),
            note: Some("Line 1\nLine 2".into()),
            birthday: NaiveDate::from_ymd_opt(NO_YEAR, 2, 29),
            photo: Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==".into()),
        };
        let text = contact_to_vcard(&contact);
        assert!(text.contains("VERSION:3.0\r\n"));
        assert!(text.contains("EMAIL;TYPE=INTERNET,WORK,PREF:ayse@example.com\r\n"));
        assert!(text.contains("item1.X-ABLABEL:Personal mail\r\n"));
        assert!(text.contains("PHOTO;ENCODING=b;TYPE=PNG:iVBORw0KGgoAAAANSUhEUg==\r\n"));
        let parsed = parse_vcard(&text).expect("card");
        assert_eq!(
            parsed,
            Contact {
                emails: vec![
                    contact.emails[0].clone(),
                    ContactField {
                        kind: Some("personal mail".into()),
                        ..contact.emails[1].clone()
                    }
                ],
                ..contact.clone()
            }
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(parse_vcard("").unwrap_err(), Error::NoVCard);
        assert!(parse_vcards("garbage").is_empty());
        let created = upsert_contact(
            "",
            &Contact {
                name: "X".into(),
                ..Contact::default()
            },
        )
        .expect("card");
        assert!(created.contains("FN:X\r\n"));
    }
}
