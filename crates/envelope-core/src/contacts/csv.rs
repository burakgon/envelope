//! Contacts from CSV files as Gmail, Outlook and Thunderbird export them.
//! Columns are recognized by their (English) names: Gmail's numbered groups
//! ("E-mail 1 - Label", "E-mail 1 - Value", …), Outlook's fixed columns
//! ("Business Phone", "Home Street", …) and Thunderbird's ("Primary Email",
//! "Work City", …).

use std::collections::BTreeMap;

use chrono::NaiveDate;
use envelope_ical::{Contact, ContactAddress, ContactField};

use super::{NO_YEAR, parse_birthday};

/// Whether a file's text looks like a CSV export of contacts: its first line
/// names a column for an e-mail address or a name.
pub fn looks_like_contacts(text: &str) -> bool {
    let Some(header) = records(text).into_iter().next() else {
        return false;
    };
    header.iter().any(|h| {
        let h = h.trim().to_lowercase();
        h.contains("mail") || h.contains("name")
    }) && header.len() > 1
}

/// The contacts of a CSV export, without empty rows.
pub fn parse(text: &str) -> Vec<Contact> {
    let mut rows = records(text).into_iter();
    let Some(header) = rows.next() else {
        return Vec::new();
    };
    let columns: Vec<Column> = header.iter().map(|h| Column::of(h)).collect();
    rows.filter_map(|row| contact(&columns, &row)).collect()
}

/// Text of a CSV file: UTF-8 or UTF-16 by its byte order mark, else UTF-8,
/// else Windows-1252 (what Outlook writes on Windows).
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], big: bool| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                if big {
                    u16::from_be_bytes(*pair)
                } else {
                    u16::from_le_bytes(*pair)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, true);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|b| windows_1252(*b)).collect(),
    }
}

/// A byte of Windows-1252: Latin-1, with printable characters in 0x80–0x9F.
fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        _ => char::from(byte),
    }
}

/// The records of a CSV text (RFC 4180: quoted fields may hold separators,
/// quotes and line breaks). The separator is a comma, or the semicolon or tab
/// the first line uses instead.
fn records(text: &str) -> Vec<Vec<String>> {
    let first = text.lines().next().unwrap_or_default();
    let separator = [',', ';', '\t']
        .into_iter()
        .max_by_key(|s| first.matches(*s).count())
        .unwrap_or(',');
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
        } else if c == '"' {
            quoted = true;
        } else if c == separator {
            record.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            record.push(std::mem::take(&mut field));
            if record.iter().any(|f| !f.trim().is_empty()) {
                records.push(std::mem::take(&mut record));
            } else {
                record.clear();
            }
        } else {
            field.push(c);
        }
    }
    record.push(field);
    if record.iter().any(|f| !f.trim().is_empty()) {
        records.push(record);
    }
    records
}

/// A part of a postal address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Street,
    Extended,
    PoBox,
    City,
    Region,
    PostalCode,
    Country,
    /// The whole address as text.
    Formatted,
}

/// What a column holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Column {
    Name,
    Given,
    Family,
    Nickname,
    Organization,
    Title,
    Birthday,
    BirthYear,
    BirthMonth,
    BirthDay,
    Notes,
    /// An e-mail address, phone number or web site of a kind (Outlook,
    /// Thunderbird).
    Email(Option<&'static str>),
    Phone(Option<&'static str>),
    Website,
    /// A part of the address of a kind (Outlook, Thunderbird).
    Address(&'static str, Part),
    /// A numbered Gmail group ("E-mail 1 - Value"): what it is, its number and
    /// the field ("label", "value", "formatted", "city", …).
    Group(&'static str, u32, String),
    Ignored,
}

impl Column {
    fn of(header: &str) -> Column {
        let h = header.trim().trim_start_matches('\u{feff}').to_lowercase();
        let h = h.split_whitespace().collect::<Vec<_>>().join(" ");
        // Gmail: "E-mail 1 - Value", "Phone 2 - Label", "Address 1 - City", …
        if let Some((what, field)) = h.split_once(" - ")
            && let Some((kind, number)) = what.rsplit_once(' ')
            && let Ok(number) = number.parse::<u32>()
        {
            let kind = match kind {
                "e-mail" | "email" => "email",
                "phone" => "phone",
                "address" => "address",
                "website" => "website",
                "organization" => "organization",
                _ => return Column::Ignored,
            };
            let field = match field {
                "type" => "label",
                other => other,
            };
            return Column::Group(kind, number, field.to_owned());
        }
        match h.as_str() {
            "name" | "display name" | "full name" => return Column::Name,
            "first name" | "given name" => return Column::Given,
            "last name" | "family name" | "surname" => return Column::Family,
            "nickname" => return Column::Nickname,
            "company" | "organization" | "organization name" => return Column::Organization,
            "job title" | "organization title" => return Column::Title,
            "birthday" => return Column::Birthday,
            "birth year" => return Column::BirthYear,
            "birth month" => return Column::BirthMonth,
            "birth day" => return Column::BirthDay,
            "notes" => return Column::Notes,
            "e-mail address" | "e-mail 2 address" | "e-mail 3 address" | "email"
            | "email address" | "e-mail" | "primary email" | "secondary email" => {
                return Column::Email(None);
            }
            "web page" | "personal web page" | "web page 1" | "web page 2" | "website"
            | "web site" => {
                return Column::Website;
            }
            _ => {}
        }
        // Not Gmail's "Phonetic First Name" or Outlook's "Yomi" readings.
        if ["phone", "mobile", "fax", "pager", "callback", "telex"]
            .iter()
            .any(|w| h.contains(w))
            && !h.contains("yomi")
            && !h.contains("phonetic")
        {
            return Column::Phone(phone_kind(&h));
        }
        // Outlook "Business Street", Thunderbird "Work ZipCode", …
        if let Some((kind, part)) = h.split_once(' ') {
            let kind = match kind {
                "business" | "work" => "work",
                "home" => "home",
                "other" => "other",
                _ => return Column::Ignored,
            };
            let part = match part {
                "street" | "address" => Part::Street,
                "street 2" | "street 3" | "address 2" => Part::Extended,
                "po box" => Part::PoBox,
                "city" => Part::City,
                "state" => Part::Region,
                "postal code" | "zipcode" | "zip code" | "zip" => Part::PostalCode,
                "country" | "country/region" => Part::Country,
                _ => return Column::Ignored,
            };
            return Column::Address(kind, part);
        }
        Column::Ignored
    }
}

/// The kind of a phone number from the words naming it.
fn phone_kind(words: &str) -> Option<&'static str> {
    let w = words.to_lowercase();
    Some(if w.contains("fax") {
        "fax"
    } else if w.contains("mobile") || w.contains("cell") || w.contains("car") {
        "cell"
    } else if w.contains("pager") {
        "pager"
    } else if w.contains("home") {
        "home"
    } else if ["business", "work", "company", "assistant", "main"]
        .iter()
        .any(|k| w.contains(k))
    {
        "work"
    } else {
        "other"
    })
}

/// The kind of an e-mail address, web site or postal address from a Gmail
/// label ("* Home", "Work", "Other", a custom one).
fn label_kind(label: &str) -> Option<&'static str> {
    let l = label.trim().trim_start_matches('*').trim().to_lowercase();
    match l.as_str() {
        "home" => Some("home"),
        "work" => Some("work"),
        "" => None,
        _ => Some("other"),
    }
}

/// A birthday as exports write it: ISO dates and `--MM-DD` (Gmail), the
/// month first (`5/17/1990`, Outlook) or `DD.MM.YYYY`. Outlook's empty date
/// is `0/0/00`.
fn birthday(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    if let Ok(Some(day)) = parse_birthday(text) {
        return Some(day);
    }
    let parts: Vec<u32> = text
        .split('/')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    match parts.as_slice() {
        [m, d, y] if *y >= 1000 && *m <= 12 => NaiveDate::from_ymd_opt(*y as i32, *m, *d),
        // The day first, when the month can't be.
        [d, m, y] if *y >= 1000 => NaiveDate::from_ymd_opt(*y as i32, *m, *d),
        _ => None,
    }
}

/// A contact from a row; `None` for a row with nothing to keep.
fn contact(columns: &[Column], row: &[String]) -> Option<Contact> {
    let mut c = Contact::default();
    let mut emails: Vec<(String, Option<&str>)> = Vec::new();
    let mut phones: Vec<(String, Option<&str>)> = Vec::new();
    let mut sites: Vec<(String, Option<&str>)> = Vec::new();
    let mut addresses: BTreeMap<String, (Option<&str>, ContactAddress)> = BTreeMap::new();
    // Gmail groups: (kind, number) → field → value.
    let mut groups: BTreeMap<(&str, u32), BTreeMap<&str, &str>> = BTreeMap::new();
    let (mut year, mut month, mut day) = (None, None, None);
    for (column, value) in columns.iter().zip(row) {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let text = Some(value.to_owned());
        match column {
            Column::Name => c.name = value.to_owned(),
            Column::Given => c.given_name = text,
            Column::Family => c.family_name = text,
            Column::Nickname => c.nickname = text,
            Column::Organization => c.organization = text,
            Column::Title => c.title = text,
            Column::Notes => c.note = text,
            Column::Birthday => c.birthday = birthday(value),
            Column::BirthYear => year = value.parse::<i32>().ok(),
            Column::BirthMonth => month = value.parse::<u32>().ok(),
            Column::BirthDay => day = value.parse::<u32>().ok(),
            Column::Email(kind) => emails.push((value.to_owned(), *kind)),
            Column::Phone(kind) => phones.push((value.to_owned(), *kind)),
            Column::Website => sites.push((value.to_owned(), None)),
            Column::Address(kind, part) => {
                let entry = addresses
                    .entry((*kind).to_owned())
                    .or_insert_with(|| (Some(*kind), ContactAddress::default()));
                set_part(&mut entry.1, *part, value);
            }
            Column::Group(kind, number, field) => {
                groups
                    .entry((*kind, *number))
                    .or_default()
                    .insert(field.as_str(), value);
            }
            Column::Ignored => {}
        }
    }
    if c.birthday.is_none()
        && let (Some(month), Some(day)) = (month, day)
    {
        c.birthday =
            NaiveDate::from_ymd_opt(year.filter(|y| *y > 0).unwrap_or(NO_YEAR), month, day);
    }
    for ((kind, number), fields) in &groups {
        let label = fields.get("label").copied().unwrap_or_default();
        // Gmail puts several values of one label in a cell, split by " ::: ".
        let values = |key: &str| -> Vec<String> {
            fields
                .get(key)
                .map(|v| {
                    v.split(":::")
                        .map(|p| p.trim().to_owned())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        match *kind {
            "email" => emails.extend(values("value").into_iter().map(|v| (v, label_kind(label)))),
            "phone" => phones.extend(values("value").into_iter().map(|v| (v, phone_kind(label)))),
            "website" => sites.extend(values("value").into_iter().map(|v| (v, label_kind(label)))),
            "organization" => {
                if let Some(name) = fields.get("name") {
                    c.organization.get_or_insert_with(|| (*name).to_owned());
                }
                if let Some(title) = fields.get("title") {
                    c.title.get_or_insert_with(|| (*title).to_owned());
                }
            }
            "address" => {
                let mut address = ContactAddress::default();
                for (field, part) in [
                    ("street", Part::Street),
                    ("extended address", Part::Extended),
                    ("po box", Part::PoBox),
                    ("city", Part::City),
                    ("region", Part::Region),
                    ("postal code", Part::PostalCode),
                    ("country", Part::Country),
                ] {
                    if let Some(value) = fields.get(field) {
                        set_part(&mut address, part, value);
                    }
                }
                // The parts when there are any, else the address as written.
                if address.is_empty()
                    && let Some(formatted) = fields.get("formatted")
                {
                    set_part(&mut address, Part::Formatted, formatted);
                }
                addresses.insert(format!("~{number}"), (label_kind(label), address));
            }
            _ => {}
        }
    }
    let field = |(value, kind): (String, Option<&str>)| ContactField {
        value,
        kind: kind.map(str::to_owned),
        preferred: false,
    };
    let mut seen = std::collections::HashSet::new();
    c.emails = emails
        .into_iter()
        .filter(|(v, _)| v.contains('@') && seen.insert(v.to_lowercase()))
        .map(field)
        .collect();
    if let Some(first) = c.emails.first_mut() {
        first.preferred = true;
    }
    c.phones = phones.into_iter().map(field).collect();
    c.urls = sites.into_iter().map(field).collect();
    c.addresses = addresses
        .into_values()
        .filter(|(_, a)| !a.is_empty())
        .map(|(kind, mut a)| {
            a.kind = kind.map(str::to_owned);
            a
        })
        .collect();
    if c.name.trim().is_empty() {
        c.name = [c.given_name.as_deref(), c.family_name.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
    }
    let empty = c.name.trim().is_empty()
        && c.emails.is_empty()
        && c.phones.is_empty()
        && c.organization.is_none();
    (!empty).then_some(c)
}

fn set_part(address: &mut ContactAddress, part: Part, value: &str) {
    let value = value.trim().replace("\r\n", "\n");
    let add_line = |current: &mut String| {
        if current.is_empty() {
            *current = value.clone();
        } else {
            current.push('\n');
            current.push_str(&value);
        }
    };
    match part {
        Part::Street => add_line(&mut address.street),
        Part::Extended => add_line(&mut address.extended),
        Part::PoBox => address.po_box = value,
        Part::City => address.locality = value,
        Part::Region => address.region = value,
        Part::PostalCode => address.postal_code = value,
        Part::Country => address.country = value,
        Part::Formatted => *address = ContactAddress::from_text(&value, address.kind.clone()),
    }
}

#[cfg(test)]
mod tests;
