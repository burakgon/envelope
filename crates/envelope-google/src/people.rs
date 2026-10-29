//! People API: the user's contacts and "other contacts" (people they
//! e-mailed), mapped into [`Contact`], with incremental sync, the search
//! endpoints used for address autocomplete, and contact writes (create,
//! update, delete and photos).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use base64::Engine as _;
use chrono::{Datelike, NaiveDate};
use envelope_ical::vcard::NO_YEAR;
use envelope_ical::{Contact, ContactAddress, ContactField};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::client::{ApiRequest, GoogleClient, Service};
use crate::error::{Error, Result, ensure_page_progress};
use crate::html;
use crate::serde_util::{non_empty, null_default};

/// `personFields` requested for `people/me/connections` and returned by
/// contact writes. `metadata` is needed to recognise deletions during
/// incremental syncs.
pub const CONNECTION_PERSON_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,addresses,organizations,urls,photos,birthdays,biographies,metadata";

/// `readMask` requested for `otherContacts`.
pub const OTHER_CONTACTS_READ_MASK: &str = "names,emailAddresses,metadata";

const CONTACT_SEARCH_READ_MASK: &str =
    "names,nicknames,emailAddresses,phoneNumbers,organizations,photos";
const OTHER_CONTACT_SEARCH_READ_MASK: &str = "names,emailAddresses,phoneNumbers";
const PAGE_SIZE: &str = "1000";
const MAX_SEARCH_RESULTS: u32 = 30;

/// The person fields Envelope writes, in `updatePersonFields` spelling.
const EDITABLE_FIELDS: [&str; 9] = [
    "names",
    "nicknames",
    "emailAddresses",
    "phoneNumbers",
    "addresses",
    "organizations",
    "urls",
    "birthdays",
    "biographies",
];

/// A saved contact together with what is needed to change it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoogleContact {
    /// The mapped contact; `uid` is the resource name (`people/c123`) and
    /// `photo` the URL of a photo the user set (default avatars are left
    /// out).
    pub contact: Contact,
    /// The person's etag, which updates must quote.
    pub etag: Option<String>,
    /// The person resource as Google returned it. Updates start from it so
    /// that details the [`Contact`] model does not cover survive.
    pub person: Value,
}

impl GoogleContact {
    /// Maps a person resource as the People API returns it, for example one
    /// kept from an earlier sync.
    pub fn from_person(person: Value) -> Result<GoogleContact> {
        google_contact(person)
    }
}

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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContactPage {
    /// New or changed contacts (`uid` is the resource name, e.g. `people/c123`).
    pub contacts: Vec<GoogleContact>,
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
            let etag = non_empty(person.etag.clone());
            match map_person(person) {
                Some(Mapped::Contact(contact)) => page.contacts.push(GoogleContact {
                    contact: *contact,
                    etag,
                    person: item,
                }),
                Some(Mapped::Deleted(resource_name)) => page.deleted.push(resource_name),
                None => {}
            }
        }
        Ok(page)
    }

    /// Fetches one saved contact by resource name (`people/c123`).
    pub async fn get_contact(&self, resource_name: &str) -> Result<GoogleContact> {
        let segments = person_path(resource_name, "")?;
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let request = ApiRequest::get(Service::People, &segments)
            .query("personFields", CONNECTION_PERSON_FIELDS);
        let person: Value = self.send_json(&request, "people.get").await?;
        google_contact(person)
    }

    /// Creates a contact (in "My Contacts") and returns it as stored.
    pub async fn create_contact(&self, contact: &Contact) -> Result<GoogleContact> {
        let mut body = Map::new();
        for field in EDITABLE_FIELDS {
            let items = field_items(field, contact, None);
            if items.as_array().is_some_and(|a| !a.is_empty()) {
                body.insert(field.to_owned(), items);
            }
        }
        let request = ApiRequest::new(Method::POST, Service::People, &["people:createContact"])
            .query("personFields", CONNECTION_PERSON_FIELDS)
            .json(&Value::Object(body))?;
        let person: Value = self.send_json(&request, "people.createContact").await?;
        google_contact(person)
    }

    /// Writes the fields of `contact` that differ from `base`, the version
    /// last read from Google, and returns the updated contact. Entries that
    /// did not change are sent back as Google returned them, so labels and
    /// details outside the [`Contact`] model survive; nothing is sent when
    /// nothing changed.
    ///
    /// When the contact changed on Google since `base` was read, the update
    /// fails with an error for which [`Error::is_stale_etag`] is true: fetch
    /// the contact again, merge, and retry.
    pub async fn update_contact(
        &self,
        base: &GoogleContact,
        contact: &Contact,
    ) -> Result<GoogleContact> {
        let changed = changed_fields(&base.contact, contact);
        if changed.is_empty() {
            return Ok(base.clone());
        }
        let resource_name = base.contact.uid.as_str();
        let segments = person_path(resource_name, ":updateContact")?;
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let mut body = Map::new();
        body.insert(
            "resourceName".into(),
            Value::String(resource_name.to_owned()),
        );
        if let Some(etag) = &base.etag {
            body.insert("etag".into(), Value::String(etag.clone()));
        }
        for field in &changed {
            body.insert(
                (*field).to_owned(),
                field_items(field, contact, Some(&base.person)),
            );
        }
        let request = ApiRequest::new(Method::PATCH, Service::People, &segments)
            .query("updatePersonFields", changed.join(","))
            .query("personFields", CONNECTION_PERSON_FIELDS)
            .json(&Value::Object(body))?;
        let person: Value = self.send_json(&request, "people.updateContact").await?;
        google_contact(person)
    }

    /// Deletes a saved contact.
    pub async fn delete_contact(&self, resource_name: &str) -> Result<()> {
        let segments = person_path(resource_name, ":deleteContact")?;
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let request = ApiRequest::new(Method::DELETE, Service::People, &segments);
        self.execute(&request).await.map(drop)
    }

    /// Sets a contact's photo (JPEG or PNG bytes) and returns the updated
    /// contact.
    pub async fn update_contact_photo(
        &self,
        resource_name: &str,
        photo: &[u8],
    ) -> Result<GoogleContact> {
        let segments = person_path(resource_name, ":updateContactPhoto")?;
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let body = json!({
            "photoBytes": base64::engine::general_purpose::STANDARD.encode(photo),
            "personFields": CONNECTION_PERSON_FIELDS,
        });
        let request = ApiRequest::new(Method::PATCH, Service::People, &segments).json(&body)?;
        let response: PhotoResponse = self
            .send_json(&request, "people.updateContactPhoto")
            .await?;
        google_contact(response.person)
    }

    /// Removes a contact's photo and returns the updated contact.
    pub async fn delete_contact_photo(&self, resource_name: &str) -> Result<GoogleContact> {
        let segments = person_path(resource_name, ":deleteContactPhoto")?;
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let request = ApiRequest::new(Method::DELETE, Service::People, &segments)
            .query("personFields", CONNECTION_PERSON_FIELDS);
        let response: PhotoResponse = self
            .send_json(&request, "people.deleteContactPhoto")
            .await?;
        google_contact(response.person)
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

// ---------------------------------------------------------------------------
// Contact groups ("labels")

/// `groupFields` asked for contact groups.
const GROUP_FIELDS: &str = "name,groupType,memberCount,metadata";
/// Most groups asked for in one `contactGroups:batchGet`.
const GROUP_BATCH: usize = 200;

/// A contact group the user made (a "label" in Google Contacts) with the
/// people in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoogleContactGroup {
    /// `contactGroups/abc`.
    pub resource_name: String,
    /// The label's name.
    pub name: String,
    /// The group's etag.
    pub etag: Option<String>,
    /// The resource names of the people in it (`people/c123`).
    pub members: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactGroupList {
    #[serde(default, deserialize_with = "null_default")]
    contact_groups: Vec<WireContactGroup>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactGroupBatch {
    #[serde(default, deserialize_with = "null_default")]
    responses: Vec<ContactGroupResponse>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactGroupResponse {
    contact_group: Option<WireContactGroup>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireContactGroup {
    #[serde(default)]
    resource_name: String,
    etag: Option<String>,
    name: Option<String>,
    formatted_name: Option<String>,
    group_type: Option<String>,
    #[serde(default)]
    member_count: u32,
    #[serde(default, deserialize_with = "null_default")]
    member_resource_names: Vec<String>,
    metadata: Option<ContactGroupMetadata>,
}

#[derive(Debug, Default, Deserialize)]
struct ContactGroupMetadata {
    #[serde(default)]
    deleted: bool,
}

impl WireContactGroup {
    /// A group the user made (not "My Contacts", "Starred" and Google's
    /// other system groups), not deleted.
    fn is_users(&self) -> bool {
        self.group_type.as_deref() == Some("USER_CONTACT_GROUP")
            && !self.metadata.as_ref().is_some_and(|m| m.deleted)
            && !self.resource_name.is_empty()
    }
}

impl GoogleClient {
    /// The contact groups ("labels") the user made, with the people in
    /// them; Google's own groups (My Contacts, Starred, …) are left out.
    pub async fn contact_groups(&self) -> Result<Vec<GoogleContactGroup>> {
        let mut groups = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let request = ApiRequest::get(Service::People, &["contactGroups"])
                .query("pageSize", PAGE_SIZE)
                .query("groupFields", GROUP_FIELDS)
                .query_opt("pageToken", page_token.as_deref());
            let page: ContactGroupList = self.send_json(&request, "contactGroups.list").await?;
            let next = non_empty(page.next_page_token);
            ensure_page_progress(page_token.as_deref(), next.as_deref(), "contactGroups.list")?;
            groups.extend(page.contact_groups.into_iter().filter(|g| g.is_users()));
            match next {
                Some(next) => page_token = Some(next),
                None => break,
            }
        }
        // The list leaves the members out; they are read in batches.
        let mut members: HashMap<String, Vec<String>> = HashMap::new();
        let filled: Vec<&WireContactGroup> = groups.iter().filter(|g| g.member_count > 0).collect();
        for batch in filled.chunks(GROUP_BATCH) {
            let most = batch.iter().map(|g| g.member_count).max().unwrap_or(0);
            let mut request = ApiRequest::get(Service::People, &["contactGroups:batchGet"])
                .query("maxMembers", most.to_string())
                .query("groupFields", GROUP_FIELDS);
            for group in batch {
                request = request.query("resourceNames", group.resource_name.clone());
            }
            let answer: ContactGroupBatch =
                self.send_json(&request, "contactGroups.batchGet").await?;
            for group in answer.responses.into_iter().filter_map(|r| r.contact_group) {
                members.insert(group.resource_name, group.member_resource_names);
            }
        }
        Ok(groups
            .into_iter()
            .map(|g| GoogleContactGroup {
                members: members.remove(&g.resource_name).unwrap_or_default(),
                name: non_empty(g.formatted_name)
                    .or_else(|| non_empty(g.name))
                    .unwrap_or_default(),
                etag: non_empty(g.etag),
                resource_name: g.resource_name,
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
        .filter_map(email_field)
        .filter(|f| seen.insert(f.value.to_lowercase()))
        .collect();
    let mut seen = HashSet::new();
    let phones: Vec<ContactField> = person
        .phone_numbers
        .iter()
        .filter_map(phone_field)
        .filter(|f| seen.insert(f.value.clone()))
        .collect();
    let addresses: Vec<ContactAddress> = person.addresses.iter().filter_map(address_of).collect();
    let mut seen = HashSet::new();
    let urls: Vec<ContactField> = person
        .urls
        .iter()
        .filter_map(url_field)
        .filter(|f| seen.insert(f.value.clone()))
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
    let organization = primary_organization(&person.organizations);
    let note = primary(&person.biographies, |b| &b.metadata).and_then(|bio| {
        let value = non_empty(bio.value.clone())?;
        if bio.content_type.as_deref() == Some("TEXT_HTML") {
            non_empty(Some(html::html_to_text(&value)))
        } else {
            Some(value)
        }
    });
    let birthday = person
        .birthdays
        .iter()
        .filter_map(|b| birthday_date(b.date.as_ref()?))
        // A full date wins over a year-less one.
        .max_by_key(|d| d.year() != NO_YEAR);
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
        addresses,
        urls,
        members: Vec::new(),
    })))
}

fn email_field(email: &EmailAddress) -> Option<ContactField> {
    Some(ContactField {
        value: non_empty(email.value.clone())?.trim().to_owned(),
        kind: non_empty(email.kind.clone()).map(|k| k.to_lowercase()),
        preferred: is_primary(&email.metadata),
    })
}

fn phone_field(phone: &PhoneNumber) -> Option<ContactField> {
    Some(ContactField {
        value: non_empty(phone.value.clone())
            .or_else(|| non_empty(phone.canonical_form.clone()))?
            .trim()
            .to_owned(),
        kind: phone
            .kind
            .as_deref()
            .filter(|k| !k.is_empty())
            .map(phone_kind),
        preferred: is_primary(&phone.metadata),
    })
}

fn url_field(url: &UrlEntry) -> Option<ContactField> {
    Some(ContactField {
        value: non_empty(url.value.clone())?.trim().to_owned(),
        kind: non_empty(url.kind.clone()).map(|k| k.to_lowercase()),
        preferred: is_primary(&url.metadata),
    })
}

fn address_of(address: &Address) -> Option<ContactAddress> {
    let part = |v: &Option<String>| v.as_deref().map(str::trim).unwrap_or("").to_owned();
    let mut out = ContactAddress {
        kind: non_empty(address.kind.clone()).map(|k| k.to_lowercase()),
        preferred: is_primary(&address.metadata),
        po_box: part(&address.po_box),
        extended: part(&address.extended_address),
        street: part(&address.street_address),
        locality: part(&address.city),
        region: part(&address.region),
        postal_code: part(&address.postal_code),
        country: part(&address.country),
    };
    if out.is_empty() {
        // Addresses typed as one block of text only have a formatted value.
        let text = non_empty(address.formatted_value.clone())?;
        out = ContactAddress::from_text(&text, out.kind);
    }
    (!out.is_empty()).then_some(out)
}

fn birthday_date(date: &DateParts) -> Option<NaiveDate> {
    let year = date.year.filter(|y| *y > 0).unwrap_or(NO_YEAR);
    NaiveDate::from_ymd_opt(year, date.month?, date.day?)
}

fn primary_organization(organizations: &[Organization]) -> Option<&Organization> {
    organizations
        .iter()
        .find(|o| is_primary(&o.metadata))
        .or_else(|| organizations.iter().find(|o| o.current == Some(true)))
        .or_else(|| organizations.first())
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

/// Google's phone type for a vCard-style label.
fn google_phone_type(kind: &str) -> String {
    match kind {
        "cell" | "mobile" | "iphone" => "mobile".to_owned(),
        "fax" => "otherFax".to_owned(),
        other => other.to_owned(),
    }
}

/// Google's URL type for a vCard-style label.
fn google_url_type(kind: &str) -> String {
    match kind {
        "homepage" => "homePage".to_owned(),
        "appinstallpage" => "appInstallPage".to_owned(),
        other => other.to_owned(),
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

// ---------------------------------------------------------------------------
// Writes

/// Path segments for `people/c123` plus an optional `:method` suffix.
fn person_path(resource_name: &str, method: &str) -> Result<Vec<String>> {
    match resource_name.split_once('/') {
        Some(("people", id)) if !id.is_empty() && !id.contains('/') => {
            Ok(vec!["people".to_owned(), format!("{id}{method}")])
        }
        _ => Err(Error::InvalidInput(format!(
            "{resource_name:?} is not a contact resource name"
        ))),
    }
}

/// Maps a person returned by a read or write.
fn google_contact(person: Value) -> Result<GoogleContact> {
    let parsed = Person::deserialize(&person)
        .map_err(|e| Error::InvalidResponse(format!("malformed person: {e}")))?;
    let etag = non_empty(parsed.etag.clone());
    match map_person(parsed) {
        Some(Mapped::Contact(contact)) => Ok(GoogleContact {
            contact: *contact,
            etag,
            person,
        }),
        _ => Err(Error::InvalidResponse(
            "Google returned a contact without a resource name".to_owned(),
        )),
    }
}

/// Which person fields differ between two versions of a contact.
fn changed_fields(old: &Contact, new: &Contact) -> Vec<&'static str> {
    EDITABLE_FIELDS
        .into_iter()
        .filter(|field| match *field {
            "names" => {
                old.given_name != new.given_name
                    || old.family_name != new.family_name
                    || old.name != new.name
            }
            "nicknames" => old.nickname != new.nickname,
            "emailAddresses" => old.emails != new.emails,
            "phoneNumbers" => old.phones != new.phones,
            "addresses" => old.addresses != new.addresses,
            "organizations" => old.organization != new.organization || old.title != new.title,
            "urls" => old.urls != new.urls,
            "birthdays" => old.birthday != new.birthday,
            "biographies" => old.note != new.note,
            _ => false,
        })
        .collect()
}

/// The entries of a person field `base` had, without their metadata.
fn base_items<'a>(base: Option<&'a Value>, field: &str) -> Vec<&'a Value> {
    base.and_then(|p| p.get(field))
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn without_metadata(item: &Value) -> Value {
    let mut item = item.clone();
    if let Some(map) = item.as_object_mut() {
        map.remove("metadata");
    }
    item
}

/// Builds a list field: entries equal to one in `base` are sent back as
/// Google returned them, new ones are built from the model.
fn reuse_or_build<T: PartialEq, W: for<'de> Deserialize<'de>>(
    base: &[&Value],
    wanted: &[T],
    map: impl Fn(&W) -> Option<T>,
    build: impl Fn(&T) -> Value,
) -> Value {
    let mapped: Vec<Option<T>> = base
        .iter()
        .map(|item| W::deserialize(*item).ok().as_ref().and_then(&map))
        .collect();
    let mut used = vec![false; base.len()];
    let items = wanted
        .iter()
        .map(|item| {
            let found = mapped
                .iter()
                .enumerate()
                .position(|(i, m)| !used[i] && m.as_ref() == Some(item));
            match found {
                Some(i) => {
                    used[i] = true;
                    without_metadata(base[i])
                }
                None => build(item),
            }
        })
        .collect();
    Value::Array(items)
}

/// Inserts `key: value` unless the value is blank.
fn put(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            map.insert(key.to_owned(), Value::String(v.to_owned()));
        }
        None => {
            map.remove(key);
        }
    }
}

/// The value of one editable person field for `contact`.
fn field_items(field: &str, contact: &Contact, base: Option<&Value>) -> Value {
    let base_list = base_items(base, field);
    match field {
        "names" => {
            let mut name = base_list
                .iter()
                .find(|n| n.pointer("/metadata/primary") == Some(&Value::Bool(true)))
                .or(base_list.first())
                .map(|n| without_metadata(n))
                .and_then(|n| n.as_object().cloned())
                .unwrap_or_default();
            for key in ["displayName", "displayNameLastFirst", "unstructuredName"] {
                name.remove(key);
            }
            put(&mut name, "givenName", contact.given_name.as_deref());
            put(&mut name, "familyName", contact.family_name.as_deref());
            if contact.given_name.is_none() && contact.family_name.is_none() {
                put(&mut name, "unstructuredName", Some(&contact.name));
            }
            let has_name = ["givenName", "familyName", "middleName", "unstructuredName"]
                .iter()
                .any(|k| name.contains_key(*k));
            Value::Array(if has_name {
                vec![Value::Object(name)]
            } else {
                Vec::new()
            })
        }
        "nicknames" => Value::Array(
            contact
                .nickname
                .iter()
                .filter(|n| !n.trim().is_empty())
                .map(|n| json!({ "value": n.trim() }))
                .collect(),
        ),
        "emailAddresses" => reuse_or_build(&base_list, &contact.emails, email_field, |f| {
            typed_value(&f.value, f.kind.as_deref().map(str::to_owned))
        }),
        "phoneNumbers" => reuse_or_build(&base_list, &contact.phones, phone_field, |f| {
            typed_value(&f.value, f.kind.as_deref().map(google_phone_type))
        }),
        "urls" => reuse_or_build(&base_list, &contact.urls, url_field, |f| {
            typed_value(&f.value, f.kind.as_deref().map(google_url_type))
        }),
        "addresses" => reuse_or_build(&base_list, &contact.addresses, address_of, |a| {
            let mut map = Map::new();
            put(&mut map, "type", a.kind.as_deref());
            put(&mut map, "poBox", Some(&a.po_box));
            put(&mut map, "extendedAddress", Some(&a.extended));
            put(&mut map, "streetAddress", Some(&a.street));
            put(&mut map, "city", Some(&a.locality));
            put(&mut map, "region", Some(&a.region));
            put(&mut map, "postalCode", Some(&a.postal_code));
            put(&mut map, "country", Some(&a.country));
            put(&mut map, "formattedValue", Some(&a.formatted()));
            Value::Object(map)
        }),
        "organizations" => {
            // Edit the primary organization in place (keeping department,
            // dates, …) and leave the others alone.
            let primary_index = base_list
                .iter()
                .position(|o| o.pointer("/metadata/primary") == Some(&Value::Bool(true)))
                .or_else(|| (!base_list.is_empty()).then_some(0));
            let mut org = primary_index
                .and_then(|i| without_metadata(base_list[i]).as_object().cloned())
                .unwrap_or_default();
            put(&mut org, "name", contact.organization.as_deref());
            put(&mut org, "title", contact.title.as_deref());
            let mut items = Vec::new();
            if org.contains_key("name") || org.contains_key("title") {
                items.push(Value::Object(org));
            }
            for (i, other) in base_list.iter().enumerate() {
                if Some(i) != primary_index {
                    items.push(without_metadata(other));
                }
            }
            Value::Array(items)
        }
        "birthdays" => Value::Array(
            contact
                .birthday
                .map(|date| {
                    let mut parts = json!({ "month": date.month(), "day": date.day() });
                    if date.year() != NO_YEAR {
                        parts["year"] = json!(date.year());
                    }
                    json!({ "date": parts })
                })
                .into_iter()
                .collect(),
        ),
        "biographies" => Value::Array(
            contact
                .note
                .iter()
                .filter(|n| !n.trim().is_empty())
                .map(|n| json!({ "value": n, "contentType": "TEXT_PLAIN" }))
                .collect(),
        ),
        _ => Value::Array(Vec::new()),
    }
}

fn typed_value(value: &str, kind: Option<String>) -> Value {
    let mut map = Map::new();
    map.insert("value".into(), Value::String(value.trim().to_owned()));
    put(&mut map, "type", kind.as_deref());
    Value::Object(map)
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

#[derive(Debug, Deserialize)]
struct PhotoResponse {
    person: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Person {
    resource_name: Option<String>,
    etag: Option<String>,
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
    addresses: Vec<Address>,
    #[serde(default, deserialize_with = "null_default")]
    organizations: Vec<Organization>,
    #[serde(default, deserialize_with = "null_default")]
    urls: Vec<UrlEntry>,
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
#[serde(rename_all = "camelCase")]
struct Address {
    metadata: Option<FieldMetadata>,
    formatted_value: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    po_box: Option<String>,
    street_address: Option<String>,
    extended_address: Option<String>,
    city: Option<String>,
    region: Option<String>,
    postal_code: Option<String>,
    country: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct UrlEntry {
    metadata: Option<FieldMetadata>,
    value: Option<String>,
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

    fn ada() -> GoogleContact {
        google_contact(json!({
            "resourceName": "people/c1",
            "etag": "%Ada1",
            "names": [{"metadata": {"primary": true}, "displayName": "Ada Lovelace", "givenName": "Ada",
                       "familyName": "Lovelace", "middleName": "King", "unstructuredName": "Ada King Lovelace"}],
            "emailAddresses": [
                {"metadata": {"primary": true}, "value": "ada@example.com", "type": "work", "formattedType": "Work",
                 "displayName": "Ada (work)"},
                {"value": "ada@home.example", "type": "home"}
            ],
            "phoneNumbers": [{"value": "+44 20 1234", "type": "workMobile", "formattedType": "Work Mobile"}],
            "addresses": [
                {"type": "home", "streetAddress": "12 St James's Square", "city": "London", "postalCode": "SW1Y 4JH",
                 "country": "United Kingdom", "countryCode": "GB", "formattedValue": "12 St James's Square\nLondon SW1Y 4JH"},
                {"type": "work", "formattedValue": "Analytical Engine Works\nMarylebone"}
            ],
            "urls": [{"value": "https://ada.example", "type": "homePage"}],
            "organizations": [
                {"metadata": {"primary": true}, "name": "Analytical Engines", "title": "Programmer", "department": "R&D"},
                {"name": "Royal Society", "title": "Fellow"}
            ],
            "birthdays": [{"date": {"month": 12, "day": 10}}],
            "biographies": [{"value": "First programmer", "contentType": "TEXT_PLAIN"}]
        }))
        .expect("contact")
    }

    #[test]
    fn maps_addresses_urls_and_yearless_birthdays() {
        let ada = ada();
        assert_eq!(ada.etag.as_deref(), Some("%Ada1"));
        let contact = &ada.contact;
        assert_eq!(contact.phones[0].kind.as_deref(), Some("cell"));
        assert_eq!(contact.addresses.len(), 2);
        assert_eq!(
            contact.addresses[0].formatted(),
            "12 St James's Square\nSW1Y 4JH London\nUnited Kingdom"
        );
        assert_eq!(contact.addresses[1].kind.as_deref(), Some("work"));
        assert_eq!(
            contact.addresses[1].formatted(),
            "Analytical Engine Works\nMarylebone",
            "free-text addresses come from formattedValue"
        );
        assert_eq!(contact.urls[0].kind.as_deref(), Some("homepage"));
        assert_eq!(contact.birthday, NaiveDate::from_ymd_opt(NO_YEAR, 12, 10));
        assert_eq!(contact.organization.as_deref(), Some("Analytical Engines"));
    }

    #[test]
    fn updates_send_only_changed_fields_and_keep_unchanged_entries() {
        let base = ada();
        let mut edited = base.contact.clone();
        edited.emails.push(ContactField {
            value: "countess@example.com".into(),
            kind: Some("other".into()),
            preferred: false,
        });
        edited.title = Some("Analyst".into());
        assert_eq!(
            changed_fields(&base.contact, &edited),
            ["emailAddresses", "organizations"]
        );
        let emails = field_items("emailAddresses", &edited, Some(&base.person));
        assert_eq!(
            emails,
            json!([
                {"value": "ada@example.com", "type": "work", "formattedType": "Work", "displayName": "Ada (work)"},
                {"value": "ada@home.example", "type": "home"},
                {"value": "countess@example.com", "type": "other"}
            ]),
            "unchanged entries keep their Google details"
        );
        let orgs = field_items("organizations", &edited, Some(&base.person));
        assert_eq!(
            orgs,
            json!([
                {"name": "Analytical Engines", "title": "Analyst", "department": "R&D"},
                {"name": "Royal Society", "title": "Fellow"}
            ])
        );

        let mut renamed = base.contact.clone();
        renamed.given_name = Some("Augusta".into());
        renamed.name = "Augusta Lovelace".into();
        assert_eq!(changed_fields(&base.contact, &renamed), ["names"]);
        assert_eq!(
            field_items("names", &renamed, Some(&base.person)),
            json!([{"givenName": "Augusta", "familyName": "Lovelace", "middleName": "King"}])
        );
        assert!(changed_fields(&base.contact, &base.contact).is_empty());
    }

    #[test]
    fn new_contacts_map_every_field() {
        let contact = Contact {
            name: "Acme Corp".into(),
            phones: vec![ContactField {
                value: "+1 555 0100".into(),
                kind: Some("cell".into()),
                preferred: false,
            }],
            addresses: vec![ContactAddress::from_text(
                "1 Main St\nSpringfield",
                Some("work".into()),
            )],
            urls: vec![ContactField {
                value: "https://acme.example".into(),
                kind: None,
                preferred: false,
            }],
            birthday: NaiveDate::from_ymd_opt(1990, 1, 31),
            note: Some("Supplier".into()),
            ..Contact::default()
        };
        assert_eq!(
            field_items("names", &contact, None),
            json!([{"unstructuredName": "Acme Corp"}])
        );
        assert_eq!(
            field_items("phoneNumbers", &contact, None),
            json!([{"value": "+1 555 0100", "type": "mobile"}])
        );
        assert_eq!(
            field_items("addresses", &contact, None),
            json!([{"type": "work", "streetAddress": "1 Main St\nSpringfield",
                    "formattedValue": "1 Main St\nSpringfield"}])
        );
        assert_eq!(
            field_items("urls", &contact, None),
            json!([{"value": "https://acme.example"}])
        );
        assert_eq!(
            field_items("birthdays", &contact, None),
            json!([{"date": {"year": 1990, "month": 1, "day": 31}}])
        );
        let yearless = Contact {
            birthday: NaiveDate::from_ymd_opt(NO_YEAR, 2, 29),
            ..Contact::default()
        };
        assert_eq!(
            field_items("birthdays", &yearless, None),
            json!([{"date": {"month": 2, "day": 29}}])
        );
        assert_eq!(field_items("nicknames", &contact, None), json!([]));
    }

    #[test]
    fn resource_paths() {
        assert_eq!(
            person_path("people/c42", ":updateContact").expect("path"),
            ["people", "c42:updateContact"]
        );
        assert!(person_path("otherContacts/c1", "").is_err());
        assert!(person_path("people/", "").is_err());
        assert!(person_path("people/a/b", "").is_err());
    }
}
