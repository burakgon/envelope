//! Contact groups (lists): the groups of each address book with their
//! members, creating, renaming and deleting groups and changing who is in
//! them, and the groups offered in the composer's address fields.
//!
//! A group is a row of the contacts table marked `is_group`, its members in
//! [`Contact::members`] as the group card lists them: `urn:uuid:` references
//! to cards of the same address book, and `mailto:` addresses. Groups of
//! address books on this computer and on CardDAV servers can be changed;
//! changes are stored here first and written to the server in the
//! background, like contacts' ([`crate::sync::contacts`]). Google's labels
//! (their people by resource name) and Exchange's personal distribution
//! lists (by address) are stored the same way, and only read.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use envelope_ical::Contact;
use envelope_ical::vcard::{self, GroupStyle};
use rusqlite::{Connection, Transaction, params};
use serde::Serialize;

use crate::contacts::display_name;
use crate::db::contacts::{self as cdb, BookRow, ContactRow, PhotoEdit};
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{Address, EngineEvent};

/// A group of an address book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactGroup {
    pub id: i64,
    pub book_id: i64,
    pub name: String,
    /// The group can't be changed here: its address book is read-only, or
    /// it is a Google label or an Exchange distribution list.
    pub read_only: bool,
    /// The members, by name. Members that are not in the address book
    /// (cards Envelope doesn't have) are not listed but stay in the group.
    pub members: Vec<GroupMember>,
}

/// A member of a group: one of the address book's contacts, or an address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMember {
    /// The contact, when the member is one.
    pub contact_id: Option<i64>,
    /// The contact's name; the address for an address alone, unless the
    /// server named it.
    pub name: String,
    /// Primary e-mail address.
    pub email: Option<String>,
    pub organization: Option<String>,
    /// Small `data:` URL thumbnail.
    pub photo: Option<String>,
}

/// A group offered in the composer's address fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSuggestion {
    pub id: i64,
    pub name: String,
    /// How many people are in the group.
    pub count: usize,
    /// The primary address of every member that has one, once each.
    pub addresses: Vec<Address>,
}

/// A contact of an address book, as group members are shown.
struct Person {
    id: i64,
    uid: String,
    name: String,
    email: Option<String>,
    emails: Vec<String>,
    organization: Option<String>,
    thumb: Option<String>,
}

/// The people of one address book, to find members by UID or address.
struct BookPeople {
    people: Vec<Person>,
    by_uid: HashMap<String, usize>,
    by_email: HashMap<String, usize>,
}

/// What a `urn:uuid:` member and a UID are compared by.
fn uid_key(value: &str) -> String {
    vcard::member_for_uid(value).to_lowercase()
}

impl BookPeople {
    fn load(c: &Connection, book_id: i64) -> Result<BookPeople> {
        let mut st = c.prepare_cached(
            "SELECT id, uid, name, email, emails, organization, nullif(thumb, '') FROM contacts
             WHERE book_id = ? AND is_group = 0 AND local_state != ? ORDER BY sort_key, id",
        )?;
        let people = st
            .query_map(params![book_id, cdb::DELETED], |r| {
                Ok(Person {
                    id: r.get(0)?,
                    uid: r.get(1)?,
                    name: r.get(2)?,
                    email: r.get(3)?,
                    emails: r.get::<_, String>(4)?.lines().map(str::to_owned).collect(),
                    organization: r.get(5)?,
                    thumb: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut by_uid = HashMap::new();
        let mut by_email = HashMap::new();
        for (i, person) in people.iter().enumerate() {
            if !person.uid.trim().is_empty() {
                by_uid.entry(uid_key(&person.uid)).or_insert(i);
            }
            for email in &person.emails {
                by_email.entry(email.clone()).or_insert(i);
            }
        }
        Ok(BookPeople {
            people,
            by_uid,
            by_email,
        })
    }

    /// The contact a member refers to.
    fn find(&self, member: &str) -> Option<&Person> {
        let index = match vcard::member_email(member) {
            Some(address) => self.by_email.get(&address.to_lowercase()),
            None => self.by_uid.get(&uid_key(member)),
        };
        index.map(|&i| &self.people[i])
    }

    fn by_id(&self, id: i64) -> Option<&Person> {
        self.people.iter().find(|p| p.id == id)
    }

    /// The members as shown, by name, each person once. `names` are the
    /// server's names for addresses that are not contacts.
    fn members(&self, group: &Contact, names: &HashMap<String, String>) -> Vec<GroupMember> {
        let mut people = HashSet::new();
        let mut addresses = HashSet::new();
        let mut out = Vec::new();
        for member in &group.members {
            if let Some(person) = self.find(member) {
                if people.insert(person.id) {
                    out.push(GroupMember {
                        contact_id: Some(person.id),
                        name: person.name.clone(),
                        email: person.email.clone(),
                        organization: person.organization.clone(),
                        photo: person.thumb.clone(),
                    });
                }
            } else if let Some(address) = vcard::member_email(member)
                && addresses.insert(address.to_lowercase())
            {
                out.push(GroupMember {
                    contact_id: None,
                    name: names
                        .get(&address.to_lowercase())
                        .cloned()
                        .unwrap_or_else(|| address.to_owned()),
                    email: Some(address.to_owned()),
                    organization: None,
                    photo: None,
                });
            }
        }
        out.sort_by_cached_key(|m| cdb::sort_key(&m.name));
        out
    }
}

/// How a group card refers to a contact: by its UID, or by its address when
/// the card has no UID. `None` for a contact without either.
fn member_for(person: &Person) -> Option<String> {
    if !person.uid.trim().is_empty() {
        return Some(vcard::member_for_uid(&person.uid));
    }
    person.email.as_deref().map(vcard::member_for_email)
}

/// A group as last saved here. Groups synced before Envelope read their
/// members have them only in the server's copy.
fn group_contact(row: &ContactRow) -> Contact {
    let mut group = row.contact.clone();
    if group.members.is_empty()
        && row.local_state == cdb::SYNCED
        && let Some(base) = row.base.as_deref()
        && let Ok(card) = vcard::parse_vcard(base)
    {
        group.members = card.members;
    }
    group
}

/// The server's names for members that are only addresses (Exchange keeps
/// them in the group's `base`), by lower-case address.
fn member_names(row: &ContactRow) -> HashMap<String, String> {
    row.base
        .as_deref()
        .filter(|b| b.starts_with('['))
        .and_then(|b| serde_json::from_str::<Vec<Address>>(b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| Some((a.email.to_lowercase(), a.name?)))
        .collect()
}

/// Whether the groups of an address book can be changed here: on this
/// computer and on CardDAV servers, unless the address book is read-only.
fn editable(book: &BookRow) -> bool {
    !book.read_only && matches!(book.kind.as_str(), "local" | "carddav")
}

/// A group and its address book, when the group can be changed.
fn editable_group(c: &Connection, id: i64) -> Result<(ContactRow, BookRow)> {
    let row = cdb::get_contact(c, id)?;
    if !row.is_group || row.local_state == cdb::DELETED {
        return Err(Error::NotFound(format!("group {id}")));
    }
    let book = cdb::get_book(c, row.book_id)?;
    if !editable(&book) {
        return Err(Error::Invalid("This group can't be changed here.".into()));
    }
    Ok((row, book))
}

/// A group name as typed, or why it can't be one: blank, or the name of
/// another group of the address book.
fn group_name(c: &Connection, book_id: i64, name: &str, except: Option<i64>) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("Enter a name.".into()));
    }
    let taken = cdb::book_groups(c, book_id)?.iter().any(|g| {
        Some(g.id) != except && cdb::fold(display_name(&g.contact).trim()) == cdb::fold(name)
    });
    if taken {
        return Err(Error::Invalid(format!(
            "There’s already a group called “{name}” in this address book."
        )));
    }
    Ok(name.to_owned())
}

/// Adds contacts of the address book to a group; returns how many joined.
fn add_members(group: &mut Contact, people: &BookPeople, contact_ids: &[i64]) -> usize {
    let mut added = 0;
    for &id in contact_ids {
        let Some(person) = people.by_id(id) else {
            continue;
        };
        let already = group
            .members
            .iter()
            .any(|m| people.find(m).is_some_and(|p| p.id == id));
        if already {
            continue;
        }
        if let Some(member) = member_for(person) {
            group.members.push(member);
            added += 1;
        }
    }
    added
}

/// Adds addresses to a group (as the contact with the address, when the
/// address book has one); returns how many joined.
fn add_addresses(group: &mut Contact, people: &BookPeople, emails: &[String]) -> usize {
    let mut added = 0;
    for email in emails.iter().map(|e| e.trim()).filter(|e| e.contains('@')) {
        if let Some(&i) = people.by_email.get(&email.to_lowercase()) {
            added += add_members(group, people, &[people.people[i].id]);
            continue;
        }
        let already = group
            .members
            .iter()
            .any(|m| vcard::member_email(m).is_some_and(|a| a.eq_ignore_ascii_case(email)));
        if !already {
            group.members.push(vcard::member_for_email(email));
            added += 1;
        }
    }
    added
}

/// Stores a change to a group; returns whether it has to go to a server.
fn store_edit(
    tx: &Transaction<'_>,
    book: &BookRow,
    row: &ContactRow,
    group: &Contact,
) -> Result<bool> {
    let remote = book.account_id.is_some();
    let state = match (remote, row.local_state) {
        (false, _) => cdb::SYNCED,
        (true, cdb::CREATED) => cdb::CREATED,
        (true, _) => cdb::MODIFIED,
    };
    cdb::update_local(tx, row.id, group, state, &PhotoEdit::Unchanged)?;
    Ok(remote)
}

/// How new groups of a CardDAV address book are written: like the groups
/// already there, Apple's way on iCloud, and as vCard 4.0 otherwise.
pub(crate) fn book_style(c: &Connection, book: &BookRow) -> Result<GroupStyle> {
    let on_icloud = envelope_dav::Url::parse(&book.remote_id)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .is_some_and(|h| h == "icloud.com" || h.ends_with(".icloud.com"));
    if on_icloud {
        return Ok(GroupStyle::Apple);
    }
    let apple = cdb::book_groups(c, book.id)?.iter().any(|g| {
        g.base
            .as_deref()
            .and_then(vcard::group_style)
            .is_some_and(|s| s == GroupStyle::Apple)
    });
    Ok(if apple {
        GroupStyle::Apple
    } else {
        GroupStyle::Standard
    })
}

/// Points the groups of an address book that had `gone` (contacts merged
/// into `kept`, and deleted) at `kept` instead. Returns whether a group
/// changed.
pub(crate) fn replace_members(
    tx: &Transaction<'_>,
    book: &BookRow,
    gone: &[ContactRow],
    kept: i64,
) -> Result<bool> {
    if !editable(book) {
        return Ok(false);
    }
    let people = BookPeople::load(tx, book.id)?;
    let gone_uids: HashSet<String> = gone
        .iter()
        .filter(|r| !r.contact.uid.trim().is_empty())
        .map(|r| uid_key(&r.contact.uid))
        .collect();
    // Members by address now find the contact kept, which has the address.
    let refers_to_gone = |member: &str| {
        vcard::member_email(member).is_none() && gone_uids.contains(&uid_key(member))
    };
    let Some(person) = people.by_id(kept) else {
        return Ok(false);
    };
    let mut changed = false;
    for row in cdb::book_groups(tx, book.id)? {
        let mut group = group_contact(&row);
        if !group.members.iter().any(|m| refers_to_gone(m)) {
            continue;
        }
        group.members.retain(|m| !refers_to_gone(m));
        add_members(&mut group, &people, &[person.id]);
        store_edit(tx, book, &row, &group)?;
        changed = true;
    }
    Ok(changed)
}

impl Engine {
    /// The groups of every address book, in the address books' order and
    /// by name, with their members.
    pub async fn contact_groups(&self) -> Result<Vec<ContactGroup>> {
        self.db
            .read_async(|c| {
                let mut out = Vec::new();
                for book in cdb::list_books(c)? {
                    let rows = cdb::book_groups(c, book.id)?;
                    if rows.is_empty() {
                        continue;
                    }
                    let people = BookPeople::load(c, book.id)?;
                    for row in rows {
                        let group = group_contact(&row);
                        out.push(ContactGroup {
                            id: row.id,
                            book_id: book.id,
                            name: display_name(&group),
                            read_only: !editable(&book),
                            members: people.members(&group, &member_names(&row)),
                        });
                    }
                }
                Ok(out)
            })
            .await
    }

    /// Creates a group in an address book with these of its contacts in it,
    /// and returns its id. It reaches the server in the background.
    pub async fn create_group(
        &self,
        book_id: i64,
        name: &str,
        contact_ids: Vec<i64>,
    ) -> Result<i64> {
        let name = name.to_owned();
        let (id, remote) = self
            .db
            .write_async(move |tx| {
                let book = cdb::get_book(tx, book_id)?;
                if !editable(&book) {
                    return Err(Error::Invalid(
                        "Groups can't be added to this address book.".into(),
                    ));
                }
                let name = group_name(tx, book.id, &name, None)?;
                let people = BookPeople::load(tx, book.id)?;
                let uid = uuid::Uuid::new_v4().to_string();
                let mut group = Contact {
                    uid: uid.clone(),
                    name,
                    ..Contact::default()
                };
                add_members(&mut group, &people, &contact_ids);
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
                let remote = book.account_id.is_some();
                let state = if remote { cdb::CREATED } else { cdb::SYNCED };
                let id = cdb::insert_local_group(
                    tx,
                    book.id,
                    &uid,
                    remote_id.as_deref(),
                    &group,
                    state,
                )?;
                Ok((id, remote))
            })
            .await?;
        self.groups_changed(book_id, remote);
        Ok(id)
    }

    /// Renames a group.
    pub async fn rename_group(&self, id: i64, name: &str) -> Result<()> {
        let name = name.to_owned();
        let (book_id, remote) = self
            .db
            .write_async(move |tx| {
                let (row, book) = editable_group(tx, id)?;
                let name = group_name(tx, book.id, &name, Some(id))?;
                let mut group = group_contact(&row);
                if group.name == name {
                    return Ok((book.id, false));
                }
                // Apple writes a group's name in `N` as well.
                if group.family_name.as_deref().map(str::trim) == Some(group.name.trim()) {
                    group.family_name = Some(name.clone());
                }
                group.name = name;
                Ok((book.id, store_edit(tx, &book, &row, &group)?))
            })
            .await?;
        self.groups_changed(book_id, remote);
        Ok(())
    }

    /// Deletes a group (not the people in it), here and, in the background,
    /// on the server.
    pub async fn delete_group(&self, id: i64) -> Result<()> {
        let (book_id, remote) = self
            .db
            .write_async(move |tx| {
                let (row, book) = editable_group(tx, id)?;
                if book.account_id.is_none() || row.remote_id.is_none() {
                    cdb::delete_row(tx, id)?;
                    return Ok((book.id, false));
                }
                cdb::mark_deleted(tx, id)?;
                Ok((book.id, true))
            })
            .await?;
        self.groups_changed(book_id, remote);
        Ok(())
    }

    /// Adds contacts of the group's address book to it, and addresses (as
    /// the contact with the address, if there is one); returns how many
    /// joined. People already in it, and contacts of other address books,
    /// are left out.
    pub async fn add_to_group(
        &self,
        id: i64,
        contact_ids: Vec<i64>,
        emails: Vec<String>,
    ) -> Result<usize> {
        let (book_id, added, remote) = self
            .db
            .write_async(move |tx| {
                let (row, book) = editable_group(tx, id)?;
                let people = BookPeople::load(tx, book.id)?;
                let mut group = group_contact(&row);
                let added = add_members(&mut group, &people, &contact_ids)
                    + add_addresses(&mut group, &people, &emails);
                if added == 0 {
                    return Ok((book.id, 0, false));
                }
                Ok((book.id, added, store_edit(tx, &book, &row, &group)?))
            })
            .await?;
        if added > 0 {
            self.groups_changed(book_id, remote);
        }
        Ok(added)
    }

    /// Takes people out of a group: contacts by id (however the group refers
    /// to them) and members that are only an address. Returns how many
    /// left.
    pub async fn remove_from_group(
        &self,
        id: i64,
        contact_ids: Vec<i64>,
        emails: Vec<String>,
    ) -> Result<usize> {
        let (book_id, removed, remote) = self
            .db
            .write_async(move |tx| {
                let (row, book) = editable_group(tx, id)?;
                let people = BookPeople::load(tx, book.id)?;
                let mut gone: HashSet<String> =
                    emails.iter().map(|e| e.trim().to_lowercase()).collect();
                for &contact in &contact_ids {
                    if let Some(person) = people.by_id(contact) {
                        gone.extend(person.emails.iter().cloned());
                    }
                }
                let mut group = group_contact(&row);
                let before = people.members(&group, &HashMap::new()).len();
                group.members.retain(|member| {
                    let person = people.find(member).map(|p| p.id);
                    let address = vcard::member_email(member).map(str::to_lowercase);
                    !(person.is_some_and(|p| contact_ids.contains(&p))
                        || address.is_some_and(|a| gone.contains(&a)))
                });
                let removed = before - people.members(&group, &HashMap::new()).len();
                if removed == 0 {
                    return Ok((book.id, 0, false));
                }
                Ok((book.id, removed, store_edit(tx, &book, &row, &group)?))
            })
            .await?;
        if removed > 0 {
            self.groups_changed(book_id, remote);
        }
        Ok(removed)
    }

    /// Groups whose name has a word starting with `query` (or starts with
    /// it), for the composer's address fields: each with its members'
    /// addresses. Groups without any address are left out.
    pub fn suggest_groups(&self, query: &str, limit: usize) -> Result<Vec<GroupSuggestion>> {
        let q = cdb::fold(query.trim());
        if q.is_empty() {
            return Ok(Vec::new());
        }
        self.db.read(|c| {
            let mut books: HashMap<i64, BookPeople> = HashMap::new();
            let mut out = Vec::new();
            for row in cdb::groups_matching(c, &q, 50)? {
                let group = group_contact(&row);
                let name = display_name(&group);
                let folded = cdb::fold(&name);
                let matches = folded.starts_with(&q)
                    || folded
                        .split(|ch: char| !ch.is_alphanumeric())
                        .any(|w| w.starts_with(&q));
                if !matches {
                    continue;
                }
                let people = match books.entry(row.book_id) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => e.insert(BookPeople::load(c, row.book_id)?),
                };
                let members = people.members(&group, &member_names(&row));
                let mut seen = HashSet::new();
                let addresses: Vec<Address> = members
                    .iter()
                    .filter_map(|m| {
                        let email = m.email.as_deref()?.trim();
                        if email.is_empty() || !seen.insert(email.to_lowercase()) {
                            return None;
                        }
                        let name = Some(m.name.clone()).filter(|n| n != email);
                        Some(Address::new(name, email))
                    })
                    .collect();
                if addresses.is_empty() {
                    continue;
                }
                out.push(GroupSuggestion {
                    id: row.id,
                    name,
                    count: members.len(),
                    addresses,
                });
                if out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
    }

    /// Tells the interface that groups of an address book changed, and
    /// wakes the contacts sync for changes to upload.
    fn groups_changed(&self, book_id: i64, remote: bool) {
        if remote {
            crate::sync::contacts::wake(self);
        }
        self.emit(EngineEvent::ContactsChanged {
            book_ids: vec![book_id],
        });
    }
}

#[cfg(test)]
mod tests;
