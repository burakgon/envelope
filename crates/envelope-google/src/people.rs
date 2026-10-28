//! People API: the user's contacts and "other contacts" (people they
//! e-mailed), mapped into [`Contact`], with incremental sync and the search
//! endpoints used for address autocomplete.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use chrono::NaiveDate;
use envelope_ical::{Contact, ContactField};
use serde::Deserialize;

use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Result, ensure_page_progress};
use crate::html;
use crate::serde_util::{non_empty, null_default};

/// `personFields` requested for `people/me/connections`. `metadata` is
/// needed to recognise deletions during incremental syncs.
pub const CONNECTION_PERSON_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,organizations,photos,birthdays,biographies,metadata";

/// `readMask` requested for `otherContacts`.
pub const OTHER_CONTACTS_READ_MASK: &str = "names,emailAddresses,metadata";

const CONTACT_SEARCH_READ_MASK: &str =
    "names,nicknames,emailAddresses,phoneNumbers,organizations,photos";
const OTHER_CONTACT_SEARCH_READ_MASK: &str = "names,emailAddresses,phoneNumbers";
const PAGE_SIZE: &str = "1000";
const MAX_SEARCH_RESULTS: u32 = 30;

/// Which contact collection to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactSource {
    /// The user's saved contacts (`people/me/connections`).
    Connections,
    /// Addresses Google collected from the user's interactions
    /// (`otherContacts`).
    OtherContacts,
}

/// One page of a contact synchronisation run.
///
/// As with events, persist page by page and store `next_sync_token` (last
/// page only) for the next incremental run. People API sync tokens expire
/// after seven days, which surfaces as
/// [`Error::SyncTokenExpired`](crate::Error::SyncTokenExpired).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactPage {
    /// New or changed contacts (`uid` is the resource name, e.g. `people/c123`).
    pub contacts: Vec<Contact>,
    /// Resource names of deleted contacts (incremental syncs only).
    pub deleted: Vec<String>,
    /// Token of the next page, if any.
    pub next_page_token: Option<String>,
    /// Token for the next incremental sync (last page only).
    pub next_sync_token: Option<String>,
    /// Total number of contacts in the collection, if reported.
    pub total_items: Option<u64>,
}

/// Iterates over the pages of a contact synchronisation run.
#[derive(Debug, Clone)]
pub struct ContactSync {
    client: GoogleClient,
    source: ContactSource,
    sync_token: Option<String>,
    next_page: Option<Option<String>>,
}

impl ContactSync {
    /// Fetches the next page; `Ok(None)` once the last page was returned.
    pub async fn next_page(&mut self) -> Result<Option<ContactPage>> {
        let Some(page_token) = self.next_page.clone() else {
            return Ok(None);
        };
        let page = self
            .client
            .contacts_page(
                self.source,
                self.sync_token.as_deref(),
                page_token.as_deref(),
            )
            .await?;
        ensure_page_progress(
            page_token.as_deref(),
            page.next_page_token.as_deref(),
            "people.list",
        )?;
        self.next_page = page.next_page_token.clone().map(Some);
        Ok(Some(page))
    }
}

impl GoogleClient {
    /// Starts a contact synchronisation run: a full sync without
    /// `sync_token`, otherwise an incremental one.
    pub fn contacts_sync(&self, source: ContactSource, sync_token: Option<String>) -> ContactSync {
        ContactSync {
            client: self.clone(),
            source,
            sync_token,
            next_page: Some(None),
        }
    }

    /// Fetches one page of contacts (always requesting a sync token, so the
    /// last page of a full sync carries one).
    pub async fn contacts_page(
        &self,
        source: ContactSource,
        sync_token: Option<&str>,
        page_token: Option<&str>,
    ) -> Result<ContactPage> {
        let request = match source {
            ContactSource::Connections => {
                ApiRequest::get(Service::People, &["people", "me", "connections"])
                    .query("personFields", CONNECTION_PERSON_FIELDS)
            }
            ContactSource::OtherContacts => ApiRequest::get(Service::People, &["otherContacts"])
                .query("readMask", OTHER_CONTACTS_READ_MASK),
        }
        .query("pageSize", PAGE_SIZE)
        .query("requestSyncToken", "true")
        .query_opt("syncToken", sync_token)
        .query_opt("pageToken", page_token)
        .sync_request();
        let list: PersonList = self.send_json(&request, "people.list").await?;
        let mut page = ContactPage {
            next_page_token: non_empty(list.next_page_token),
            next_sync_token: non_empty(list.next_sync_token),
            total_items: list.total_items.or(list.total_size),
            ..ContactPage::default()
        };
        let people = match source {
            ContactSource::Connections => list.connections,
            ContactSource::OtherContacts => list.other_contacts,
        };
        for item in people {
            let person = match Person::deserialize(&item) {
                Ok(person) => person,
                Err(err) => {
                    tracing::warn!(error = %err, "skipping malformed person");
                    continue;
                }
            };
            match map_person(person) {
                Some(Mapped::Contact(contact)) => page.contacts.push(*contact),
                Some(Mapped::Deleted(resource_name)) => page.deleted.push(resource_name),
                None => {}
            }
        }
        Ok(page)
    }

    /// Searches the user's saved contacts by name, nickname, e-mail address,
    /// phone number or organization prefix (at most 30 results).
    ///
    /// Google's search index is refreshed lazily, so the first search on a
    /// client is preceded by the warm-up request Google asks for.
    pub async fn search_contacts(&self, query: &str, limit: u32) -> Result<Vec<Contact>> {
        self.search(false, query, limit).await
    }

    /// Searches the user's "other contacts" (at most 30 results).
    pub async fn search_other_contacts(&self, query: &str, limit: u32) -> Result<Vec<Contact>> {
        self.search(true, query, limit).await
    }

    /// Sends the empty warm-up queries Google recommends before searching
    /// (for example right after start-up), so the first real search
    /// returns fresh results.
    pub async fn warm_up_contact_search(&self) -> Result<()> {
        for other in [false, true] {
            self.search_request(other, "", 1).await?;
            self.search_warm_flag(other).store(true, Ordering::Release);
        }
        Ok(())
    }

    async fn search(&self, other_contacts: bool, query: &str, limit: u32) -> Result<Vec<Contact>> {
        let flag = self.search_warm_flag(other_contacts);
        if !flag.swap(true, Ordering::AcqRel)
            && let Err(err) = self.search_request(other_contacts, "", 1).await
        {
            flag.store(false, Ordering::Release);
            tracing::debug!(error = %err, "contact search warm-up failed");
        }
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.search_request(other_contacts, query, limit).await
    }

    async fn search_request(
        &self,
        other_contacts: bool,
        query: &str,
        limit: u32,
    ) -> Result<Vec<Contact>> {
        let (path, read_mask) = if other_contacts {
            ("otherContacts:search", OTHER_CONTACT_SEARCH_READ_MASK)
        } else {
            ("people:searchContacts", CONTACT_SEARCH_READ_MASK)
        };
        let request = ApiRequest::get(Service::People, &[path])
            .query("query", query)
            .query("readMask", read_mask)
            .query("pageSize", limit.clamp(1, MAX_SEARCH_RESULTS).to_string());
        let response: SearchResponse = self.send_json(&request, "people.search").await?;
        Ok(response
            .results
            .into_iter()
            .filter_map(|result| match map_person(result.person?)? {
                Mapped::Contact(contact) => Some(*contact),
                Mapped::Deleted(_) => None,
            })
            .collect())
    }
}

enum Mapped {
    Contact(Box<Contact>),
    Deleted(String),
}

fn map_person(person: Person) -> Option<Mapped> {
    let uid = non_empty(person.resource_name)?;
    if person.metadata.and_then(|m| m.deleted).unwrap_or(false) {
        return Some(Mapped::Deleted(uid));
    }
    let mut seen = HashSet::new();
    let emails: Vec<ContactField> = person
        .email_addresses
        .iter()
        .filter_map(|email| {
            let value = non_empty(email.value.clone())?.trim().to_owned();
            seen.insert(value.to_lowercase()).then(|| ContactField {
                value,
                kind: non_empty(email.kind.clone()).map(|k| k.to_lowercase()),
                preferred: is_primary(&email.metadata),
            })
        })
        .collect();
    let mut seen = HashSet::new();
    let phones: Vec<ContactField> = person
        .phone_numbers
        .iter()
        .filter_map(|phone| {
            let value = non_empty(phone.value.clone())
                .or_else(|| non_empty(phone.canonical_form.clone()))?
                .trim()
                .to_owned();
            seen.insert(value.clone()).then(|| ContactField {
                value,
                kind: phone
                    .kind
                    .as_deref()
                    .filter(|k| !k.is_empty())
                    .map(phone_kind),
                preferred: is_primary(&phone.metadata),
            })
        })
        .collect();
    let name = primary(&person.names, |n| &n.metadata);
    let given_name = name.and_then(|n| non_empty(n.given_name.clone()));
    let family_name = name.and_then(|n| non_empty(n.family_name.clone()));
    let display_name = name
        .and_then(|n| {
            non_empty(n.display_name.clone()).or_else(|| non_empty(n.unstructured_name.clone()))
        })
        .or_else(|| {
            let joined = [given_name.as_deref(), family_name.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            non_empty(Some(joined))
        })
        .or_else(|| emails.first().map(|e| e.value.clone()))
        .unwrap_or_default();
    let organization = person
        .organizations
        .iter()
        .find(|o| is_primary(&o.metadata))
        .or_else(|| {
            person
                .organizations
                .iter()
                .find(|o| o.current == Some(true))
        })
        .or_else(|| person.organizations.first());
    let note = primary(&person.biographies, |b| &b.metadata).and_then(|bio| {
        let value = non_empty(bio.value.clone())?;
        if bio.content_type.as_deref() == Some("TEXT_HTML") {
            non_empty(Some(html::html_to_text(&value)))
        } else {
            Some(value)
        }
    });
    let birthday = person.birthdays.iter().find_map(|b| {
        let date = b.date.as_ref()?;
        let year = date.year.filter(|y| *y > 0)?;
        NaiveDate::from_ymd_opt(year, date.month?, date.day?)
    });
    let photo = person
        .photos
        .iter()
        .find(|p| !p.default.unwrap_or(false))
        .and_then(|p| non_empty(p.url.clone()));
    Some(Mapped::Contact(Box::new(Contact {
        uid,
        name: display_name,
        given_name,
        family_name,
        nickname: primary(&person.nicknames, |n| &n.metadata)
            .and_then(|n| non_empty(n.value.clone())),
        emails,
        phones,
        organization: organization.and_then(|o| non_empty(o.name.clone())),
        title: organization.and_then(|o| non_empty(o.title.clone())),
        note,
        birthday,
        photo,
    })))
}

/// Maps Google's phone types to vCard-style labels.
fn phone_kind(kind: &str) -> String {
    match kind {
        "mobile" | "workMobile" => "cell".to_owned(),
        "homeFax" | "workFax" | "otherFax" => "fax".to_owned(),
        "pager" | "workPager" => "pager".to_owned(),
        "googleVoice" => "voice".to_owned(),
        other => other.to_lowercase(),
    }
}

fn is_primary(metadata: &Option<FieldMetadata>) -> bool {
    metadata.as_ref().and_then(|m| m.primary).unwrap_or(false)
}

fn primary<T>(items: &[T], metadata: impl Fn(&T) -> &Option<FieldMetadata>) -> Option<&T> {
    items
        .iter()
        .find(|item| is_primary(metadata(item)))
        .or_else(|| items.first())
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersonList {
    #[serde(default, deserialize_with = "null_default")]
    connections: Vec<serde_json::Value>,
    #[serde(default, deserialize_with = "null_default")]
    other_contacts: Vec<serde_json::Value>,
    next_page_token: Option<String>,
    next_sync_token: Option<String>,
    total_items: Option<u64>,
    total_size: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct SearchResponse {
    #[serde(default, deserialize_with = "null_default")]
    results: Vec<SearchResult>,
}

#[derive(Debug, Default, Deserialize)]
struct SearchResult {
    person: Option<Person>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Person {
    resource_name: Option<String>,
    metadata: Option<PersonMetadata>,
    #[serde(default, deserialize_with = "null_default")]
    names: Vec<Name>,
    #[serde(default, deserialize_with = "null_default")]
    nicknames: Vec<Nickname>,
    #[serde(default, deserialize_with = "null_default")]
    email_addresses: Vec<EmailAddress>,
    #[serde(default, deserialize_with = "null_default")]
    phone_numbers: Vec<PhoneNumber>,
    #[serde(default, deserialize_with = "null_default")]
    organizations: Vec<Organization>,
    #[serde(default, deserialize_with = "null_default")]
    photos: Vec<Photo>,
    #[serde(default, deserialize_with = "null_default")]
    birthdays: Vec<Birthday>,
    #[serde(default, deserialize_with = "null_default")]
    biographies: Vec<Biography>,
}

#[derive(Debug, Default, Deserialize)]
struct PersonMetadata {
    deleted: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct FieldMetadata {
    primary: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Name {
    metadata: Option<FieldMetadata>,
    display_name: Option<String>,
    unstructured_name: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Nickname {
    metadata: Option<FieldMetadata>,
    value: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct EmailAddress {
    metadata: Option<FieldMetadata>,
    value: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhoneNumber {
    metadata: Option<FieldMetadata>,
    value: Option<String>,
    canonical_form: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Organization {
    metadata: Option<FieldMetadata>,
    name: Option<String>,
    title: Option<String>,
    current: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct Photo {
    url: Option<String>,
    default: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct Birthday {
    date: Option<DateParts>,
}

#[derive(Debug, Default, Deserialize)]
struct DateParts {
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Biography {
    metadata: Option<FieldMetadata>,
    value: Option<String>,
    content_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn person(value: serde_json::Value) -> Option<Mapped> {
        map_person(Person::deserialize(&value).expect("person"))
    }

    #[test]
    fn maps_a_full_person() {
        let mapped = person(json!({
            "resourceName": "people/c1",
            "names": [
                {"metadata": {"primary": false}, "displayName": "Alias"},
                {"metadata": {"primary": true}, "displayName": "Ada Lovelace", "givenName": "Ada", "familyName": "Lovelace"}
            ],
            "nicknames": [{"value": "Countess"}],
            "emailAddresses": [
                {"metadata": {"primary": true}, "value": "ada@example.com", "type": "work"},
                {"value": "ADA@example.com", "type": "home"},
                {"value": "ada@home.example", "type": "Home"}
            ],
            "phoneNumbers": [{"value": "+44 20 1234", "type": "mobile"}, {"canonicalForm": "+44205678", "type": "workFax"}],
            "organizations": [{"name": "Analytical Engines", "title": "Programmer", "current": true}],
            "photos": [{"url": "https://example.com/default.png", "default": true}, {"url": "https://example.com/ada.png"}],
            "birthdays": [{"date": {"month": 12, "day": 10}}, {"date": {"year": 1815, "month": 12, "day": 10}}],
            "biographies": [{"value": "<p>Wrote the <b>first</b> program</p>", "contentType": "TEXT_HTML"}]
        }));
        let Some(Mapped::Contact(contact)) = mapped else {
            panic!("expected a contact");
        };
        assert_eq!(contact.uid, "people/c1");
        assert_eq!(contact.name, "Ada Lovelace");
        assert_eq!(contact.given_name.as_deref(), Some("Ada"));
        assert_eq!(contact.nickname.as_deref(), Some("Countess"));
        assert_eq!(contact.emails.len(), 2, "duplicates are merged");
        assert!(contact.emails[0].preferred);
        assert_eq!(contact.emails[1].kind.as_deref(), Some("home"));
        assert_eq!(contact.phones[0].kind.as_deref(), Some("cell"));
        assert_eq!(contact.phones[1].value, "+44205678");
        assert_eq!(contact.phones[1].kind.as_deref(), Some("fax"));
        assert_eq!(contact.organization.as_deref(), Some("Analytical Engines"));
        assert_eq!(contact.title.as_deref(), Some("Programmer"));
        assert_eq!(
            contact.photo.as_deref(),
            Some("https://example.com/ada.png")
        );
        assert_eq!(contact.birthday, NaiveDate::from_ymd_opt(1815, 12, 10));
        assert_eq!(contact.note.as_deref(), Some("Wrote the first program"));
    }

    #[test]
    fn falls_back_to_email_for_the_name_and_reports_deletions() {
        let Some(Mapped::Contact(contact)) = person(
            json!({"resourceName": "otherContacts/c9", "emailAddresses": [{"value": "x@example.com"}]}),
        ) else {
            panic!("expected a contact");
        };
        assert_eq!(contact.name, "x@example.com");
        assert!(matches!(
            person(json!({"resourceName": "people/c2", "metadata": {"deleted": true}})),
            Some(Mapped::Deleted(name)) if name == "people/c2"
        ));
        assert!(person(json!({"names": [{"displayName": "No id"}]})).is_none());
    }
}
