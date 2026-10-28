//! Contacts and people search, mapped onto [`envelope_ical::Contact`]:
//! paging, per-folder delta sync, writes (create, field-level update,
//! delete) and contact photos.

use bytes::Bytes;
use chrono::{Datelike, NaiveDate};
use envelope_ical::{Contact, ContactAddress, ContactField};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::{GraphClient, GraphRequest, Page};
use crate::delta::{DeltaCursor, DeltaPage};
use crate::error::GraphError;
use crate::serde_util::null_default;

/// Contact properties used by the mapping.
const CONTACT_FIELDS: &str = "id,displayName,givenName,surname,nickName,emailAddresses,\
businessPhones,homePhones,mobilePhone,companyName,jobTitle,personalNotes,birthday,\
homeAddress,businessAddress,otherAddress,businessHomePage";

/// Outlook keeps at most three e-mail addresses per contact.
const MAX_EMAILS: usize = 3;
/// Outlook keeps two business and two home numbers (plus one mobile).
const MAX_PHONES_PER_KIND: usize = 2;

/// Person properties used by the mapping.
const PERSON_FIELDS: &str = "id,displayName,givenName,surname,scoredEmailAddresses,phones,companyName,jobTitle,userPrincipalName";

/// A contact folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactFolder {
    /// Folder id.
    pub id: String,
    /// Display name.
    #[serde(default, deserialize_with = "null_default")]
    pub display_name: String,
    /// Parent folder id (the default Contacts folder for its subfolders).
    #[serde(default)]
    pub parent_folder_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireContactEmail {
    #[serde(default)]
    address: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireContact {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    given_name: Option<String>,
    #[serde(default)]
    surname: Option<String>,
    #[serde(default)]
    nick_name: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    email_addresses: Vec<WireContactEmail>,
    #[serde(default, deserialize_with = "null_default")]
    business_phones: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    home_phones: Vec<String>,
    #[serde(default)]
    mobile_phone: Option<String>,
    #[serde(default)]
    company_name: Option<String>,
    #[serde(default)]
    job_title: Option<String>,
    #[serde(default)]
    personal_notes: Option<String>,
    #[serde(default)]
    birthday: Option<String>,
    #[serde(default)]
    home_address: Option<WireAddress>,
    #[serde(default)]
    business_address: Option<WireAddress>,
    #[serde(default)]
    other_address: Option<WireAddress>,
    #[serde(default)]
    business_home_page: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAddress {
    #[serde(default)]
    street: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    postal_code: Option<String>,
    #[serde(default)]
    country_or_region: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScoredEmailAddress {
    #[serde(default)]
    address: Option<String>,
}

#[derive(Deserialize)]
struct Phone {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    number: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePerson {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    given_name: Option<String>,
    #[serde(default)]
    surname: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    scored_email_addresses: Vec<ScoredEmailAddress>,
    #[serde(default, deserialize_with = "null_default")]
    phones: Vec<Phone>,
    #[serde(default)]
    company_name: Option<String>,
    #[serde(default)]
    job_title: Option<String>,
    #[serde(default)]
    user_principal_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParentOnly {
    #[serde(default)]
    parent_folder_id: Option<String>,
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn field(value: &str, kind: Option<&str>, preferred: bool) -> ContactField {
    ContactField {
        value: value.trim().to_owned(),
        kind: kind.map(str::to_owned),
        preferred,
    }
}

/// Lower-cased vCard-style label for a Graph e-mail or phone type.
fn kind_label(kind: Option<&str>) -> Option<&'static str> {
    match kind?.to_ascii_lowercase().as_str() {
        "personal" | "home" => Some("home"),
        "business" | "work" => Some("work"),
        "mobile" => Some("cell"),
        "homefax" | "businessfax" | "otherfax" => Some("fax"),
        "pager" => Some("pager"),
        "assistant" => Some("assistant"),
        "other" => Some("other"),
        _ => None,
    }
}

fn display_name(
    display: Option<&str>,
    given: Option<&str>,
    family: Option<&str>,
    first_email: Option<&str>,
) -> String {
    non_empty(display)
        .or_else(|| {
            let joined = [given, family]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        })
        .or_else(|| non_empty(first_email))
        .unwrap_or_default()
}

fn map_contact(wire: WireContact) -> Contact {
    let emails: Vec<ContactField> = wire
        .email_addresses
        .iter()
        .filter_map(|e| {
            let address = non_empty(e.address.as_deref())?;
            Some((address, kind_label(e.kind.as_deref())))
        })
        .enumerate()
        .map(|(i, (address, kind))| field(&address, kind, i == 0))
        .collect();
    let mut phones: Vec<ContactField> = Vec::new();
    for (numbers, kind) in [
        (
            wire.mobile_phone.iter().cloned().collect::<Vec<_>>(),
            "cell",
        ),
        (wire.business_phones.clone(), "work"),
        (wire.home_phones.clone(), "home"),
    ] {
        for number in numbers.iter().filter(|n| !n.trim().is_empty()) {
            let preferred = phones.is_empty();
            phones.push(field(number, Some(kind), preferred));
        }
    }
    let birthday = wire
        .birthday
        .as_deref()
        .and_then(|b| b.get(..10))
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let addresses = [
        (&wire.home_address, "home"),
        (&wire.business_address, "work"),
        (&wire.other_address, "other"),
    ]
    .into_iter()
    .filter_map(|(address, kind)| map_address(address.as_ref()?, kind))
    .collect();
    let urls = non_empty(wire.business_home_page.as_deref())
        .map(|url| field(&url, Some("work"), false))
        .into_iter()
        .collect();
    Contact {
        name: display_name(
            wire.display_name.as_deref(),
            wire.given_name.as_deref(),
            wire.surname.as_deref(),
            emails.first().map(|e| e.value.as_str()),
        ),
        uid: wire.id,
        given_name: non_empty(wire.given_name.as_deref()),
        family_name: non_empty(wire.surname.as_deref()),
        nickname: non_empty(wire.nick_name.as_deref()),
        emails,
        phones,
        organization: non_empty(wire.company_name.as_deref()),
        title: non_empty(wire.job_title.as_deref()),
        note: non_empty(wire.personal_notes.as_deref()),
        birthday,
        photo: None,
        addresses,
        urls,
    }
}

fn map_address(wire: &WireAddress, kind: &str) -> Option<ContactAddress> {
    let part = |v: &Option<String>| {
        v.as_deref()
            .unwrap_or("")
            .replace("\r\n", "\n")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let address = ContactAddress {
        kind: Some(kind.to_owned()),
        street: part(&wire.street),
        locality: part(&wire.city),
        region: part(&wire.state),
        postal_code: part(&wire.postal_code),
        country: part(&wire.country_or_region),
        ..ContactAddress::default()
    };
    (!address.is_empty()).then_some(address)
}

/// A `physicalAddress` for an address (Outlook has no PO box or extended
/// address fields; they go first in the street).
fn graph_address(address: &ContactAddress) -> Value {
    let street = [&address.po_box, &address.extended, &address.street]
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let mut map = Map::new();
    for (key, value) in [
        ("street", street.as_str()),
        ("city", address.locality.trim()),
        ("state", address.region.trim()),
        ("postalCode", address.postal_code.trim()),
        ("countryOrRegion", address.country.trim()),
    ] {
        if !value.is_empty() {
            map.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    }
    Value::Object(map)
}

fn opt_string(value: Option<&str>) -> Value {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map_or(Value::Null, |v| Value::String(v.to_owned()))
}

/// Every contact property Envelope writes, for `contact`. Outlook's fixed
/// slots decide where phones and addresses go: `cell` numbers fill the
/// mobile number, `work` ones the business numbers and everything else the
/// home numbers, overflowing into free slots; addresses go to the home,
/// business and other address by kind in the same way.
fn graph_properties(contact: &Contact) -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(
        "displayName".into(),
        Value::String(contact.name.trim().to_owned()),
    );
    props.insert(
        "givenName".into(),
        opt_string(contact.given_name.as_deref()),
    );
    props.insert("surname".into(), opt_string(contact.family_name.as_deref()));
    props.insert("nickName".into(), opt_string(contact.nickname.as_deref()));
    props.insert(
        "companyName".into(),
        opt_string(contact.organization.as_deref()),
    );
    props.insert("jobTitle".into(), opt_string(contact.title.as_deref()));
    props.insert("personalNotes".into(), opt_string(contact.note.as_deref()));
    props.insert(
        "birthday".into(),
        contact.birthday.map_or(Value::Null, |d| {
            // Noon UTC keeps the date the same in every time zone.
            Value::String(format!(
                "{:04}-{:02}-{:02}T12:00:00Z",
                d.year(),
                d.month(),
                d.day()
            ))
        }),
    );
    if contact.emails.len() > MAX_EMAILS {
        tracing::warn!(
            count = contact.emails.len(),
            "Outlook keeps only {MAX_EMAILS} e-mail addresses per contact"
        );
    }
    props.insert(
        "emailAddresses".into(),
        Value::Array(
            contact
                .emails
                .iter()
                .filter(|e| !e.value.trim().is_empty())
                .take(MAX_EMAILS)
                .map(|e| json!({ "address": e.value.trim(), "name": contact.name.trim() }))
                .collect(),
        ),
    );

    let mut mobile: Option<String> = None;
    let mut business: Vec<String> = Vec::new();
    let mut home: Vec<String> = Vec::new();
    let mut overflow: Vec<String> = Vec::new();
    for phone in contact.phones.iter().filter(|p| !p.value.trim().is_empty()) {
        let value = phone.value.trim().to_owned();
        match phone.kind.as_deref() {
            Some("cell" | "mobile" | "iphone") if mobile.is_none() => mobile = Some(value),
            Some("work") if business.len() < MAX_PHONES_PER_KIND => business.push(value),
            Some("cell" | "mobile" | "iphone" | "work") => overflow.push(value),
            _ if home.len() < MAX_PHONES_PER_KIND => home.push(value),
            _ => overflow.push(value),
        }
    }
    for value in overflow {
        if home.len() < MAX_PHONES_PER_KIND {
            home.push(value);
        } else if business.len() < MAX_PHONES_PER_KIND {
            business.push(value);
        } else if mobile.is_none() {
            mobile = Some(value);
        } else {
            tracing::warn!("Outlook has no room for another phone number on this contact");
        }
    }
    props.insert("mobilePhone".into(), opt_string(mobile.as_deref()));
    props.insert("businessPhones".into(), json!(business));
    props.insert("homePhones".into(), json!(home));

    let mut slots: [(&str, Option<&ContactAddress>); 3] = [
        ("homeAddress", None),
        ("businessAddress", None),
        ("otherAddress", None),
    ];
    let mut spare: Vec<&ContactAddress> = Vec::new();
    for address in contact.addresses.iter().filter(|a| !a.is_empty()) {
        let slot = match address.kind.as_deref() {
            Some("home") => 0,
            Some("work") => 1,
            _ => 2,
        };
        if slots[slot].1.is_none() {
            slots[slot].1 = Some(address);
        } else {
            spare.push(address);
        }
    }
    for address in spare {
        match [2, 0, 1].into_iter().find(|i| slots[*i].1.is_none()) {
            Some(i) => slots[i].1 = Some(address),
            None => tracing::warn!("Outlook keeps only three addresses per contact"),
        }
    }
    for (key, address) in slots {
        props.insert(key.into(), address.map_or_else(|| json!({}), graph_address));
    }
    let url = contact
        .urls
        .iter()
        .find(|u| u.kind.as_deref() == Some("work"))
        .or_else(|| contact.urls.first());
    props.insert(
        "businessHomePage".into(),
        opt_string(url.map(|u| u.value.as_str())),
    );
    props
}

/// The properties to PATCH to turn `base` into `contact`: the ones whose
/// Outlook representation differs. `displayName` is always included, since
/// Outlook recomputes it from the names otherwise. Empty when nothing
/// changed.
fn changed_properties(base: &Contact, contact: &Contact) -> Map<String, Value> {
    let old = graph_properties(base);
    let new = graph_properties(contact);
    let mut changed: Map<String, Value> = new
        .into_iter()
        .filter(|(key, value)| old.get(key) != Some(value))
        .collect();
    if !changed.is_empty() {
        changed.insert(
            "displayName".into(),
            Value::String(contact.name.trim().to_owned()),
        );
    }
    changed
}

/// A static content type for photo uploads.
fn image_content_type(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/bmp" => "image/bmp",
        _ => "image/jpeg",
    }
}

fn map_person(wire: WirePerson) -> Contact {
    let mut addresses: Vec<String> = wire
        .scored_email_addresses
        .iter()
        .filter_map(|e| non_empty(e.address.as_deref()))
        .collect();
    if addresses.is_empty()
        && let Some(upn) =
            non_empty(wire.user_principal_name.as_deref()).filter(|u| u.contains('@'))
    {
        addresses.push(upn);
    }
    let emails: Vec<ContactField> = addresses
        .iter()
        .enumerate()
        .map(|(i, address)| field(address, None, i == 0))
        .collect();
    let phones = wire
        .phones
        .iter()
        .filter_map(|p| {
            Some((
                non_empty(p.number.as_deref())?,
                kind_label(p.kind.as_deref()),
            ))
        })
        .enumerate()
        .map(|(i, (number, kind))| field(&number, kind, i == 0))
        .collect();
    Contact {
        name: display_name(
            wire.display_name.as_deref(),
            wire.given_name.as_deref(),
            wire.surname.as_deref(),
            addresses.first().map(String::as_str),
        ),
        uid: wire.id,
        given_name: non_empty(wire.given_name.as_deref()),
        family_name: non_empty(wire.surname.as_deref()),
        nickname: None,
        emails,
        phones,
        organization: non_empty(wire.company_name.as_deref()),
        title: non_empty(wire.job_title.as_deref()),
        note: None,
        birthday: None,
        photo: None,
        addresses: Vec::new(),
        urls: Vec::new(),
    }
}

impl GraphClient {
    /// One page of the contacts in the default Contacts folder. Pass the
    /// previous page's `next_link` to continue.
    pub async fn list_contacts(
        &self,
        page_size: u32,
        next_link: Option<&str>,
    ) -> Result<Page<Contact>, GraphError> {
        let request = match next_link {
            Some(link) => GraphRequest::link(Method::GET, link),
            None => GraphRequest::get(["me", "contacts"])
                .query("$select", CONTACT_FIELDS)
                .query("$top", page_size.clamp(1, 1000).to_string()),
        };
        let page: Page<WireContact> = self.get_page(&request).await?;
        Ok(Page {
            items: page.items.into_iter().map(map_contact).collect(),
            next_link: page.next_link,
        })
    }

    /// All contacts of the default Contacts folder.
    pub async fn all_contacts(&self) -> Result<Vec<Contact>, GraphError> {
        let request = GraphRequest::get(["me", "contacts"])
            .query("$select", CONTACT_FIELDS)
            .query("$top", "500");
        let wires: Vec<WireContact> = self.get_all(&request, None).await?;
        Ok(wires.into_iter().map(map_contact).collect())
    }

    /// The contact folders below the default Contacts folder.
    pub async fn contact_folders(&self) -> Result<Vec<ContactFolder>, GraphError> {
        let request = GraphRequest::get(["me", "contactFolders"])
            .query("$select", "id,displayName,parentFolderId")
            .query("$top", "100");
        self.get_all(&request, None).await
    }

    /// The id of the default Contacts folder, derived from the parent of its
    /// subfolders or contacts (Graph has no well-known name for it). `None`
    /// when the folder is empty.
    pub async fn default_contact_folder_id(&self) -> Result<Option<String>, GraphError> {
        for collection in ["contactFolders", "contacts"] {
            let request = GraphRequest::get(["me", collection])
                .query("$select", "parentFolderId")
                .query("$top", "1");
            let page: Page<ParentOnly> = self.get_page(&request).await?;
            if let Some(id) = page
                .items
                .into_iter()
                .find_map(|item| item.parent_folder_id.filter(|id| !id.is_empty()))
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// One page of the contact delta of a contact folder (Graph v1.0 offers
    /// contact delta per folder only). Cursor handling is the same as for
    /// [`GraphClient::message_delta`].
    pub async fn contacts_delta(
        &self,
        folder_id: &str,
        cursor: Option<&DeltaCursor>,
        page_size: Option<u32>,
    ) -> Result<DeltaPage<Contact>, GraphError> {
        let mut request = match cursor {
            Some(cursor) => GraphRequest::link(Method::GET, cursor.url()),
            None => GraphRequest::get(["me", "contactFolders", folder_id, "contacts", "delta"])
                .query("$select", CONTACT_FIELDS),
        };
        if let Some(size) = page_size {
            request = request.prefer(format!("odata.maxpagesize={size}"));
        }
        let page: DeltaPage<WireContact> = self.delta_page_raw(&request).await?;
        Ok(page.map(map_contact))
    }

    /// One contact by id.
    pub async fn get_contact(&self, id: &str) -> Result<Contact, GraphError> {
        let request = GraphRequest::get(["me", "contacts", id]).query("$select", CONTACT_FIELDS);
        let wire: WireContact = self.send_json(&request).await?;
        Ok(map_contact(wire))
    }

    /// Creates a contact in a contact folder (`None`: the default Contacts
    /// folder) and returns it as stored; its `uid` is the new contact id.
    pub async fn create_contact(
        &self,
        folder_id: Option<&str>,
        contact: &Contact,
    ) -> Result<Contact, GraphError> {
        let body: Map<String, Value> = graph_properties(contact)
            .into_iter()
            .filter(|(_, v)| match v {
                Value::Null => false,
                Value::Array(items) => !items.is_empty(),
                Value::Object(map) => !map.is_empty(),
                _ => true,
            })
            .collect();
        let request = match folder_id {
            Some(folder) => GraphRequest::post(["me", "contactFolders", folder, "contacts"]),
            None => GraphRequest::post(["me", "contacts"]),
        }
        .json(&Value::Object(body))?;
        let wire: WireContact = self.send_json(&request).await?;
        Ok(map_contact(wire))
    }

    /// Changes the properties of contact `id` that differ between `base`
    /// (the version last read from Graph) and `contact`, and returns the
    /// contact as stored. Properties the model does not cover are left
    /// alone; nothing is sent when nothing changed (`base` is returned).
    pub async fn update_contact(
        &self,
        id: &str,
        base: &Contact,
        contact: &Contact,
    ) -> Result<Contact, GraphError> {
        let changed = changed_properties(base, contact);
        if changed.is_empty() {
            return Ok(base.clone());
        }
        let request = GraphRequest::patch(["me", "contacts", id]).json(&Value::Object(changed))?;
        let wire: WireContact = self.send_json(&request).await?;
        Ok(map_contact(wire))
    }

    /// Deletes a contact.
    pub async fn delete_contact(&self, id: &str) -> Result<(), GraphError> {
        self.send_empty(&GraphRequest::delete(["me", "contacts", id]))
            .await
    }

    /// A contact's photo with its content type, or `None` when it has none.
    pub async fn contact_photo(&self, id: &str) -> Result<Option<(Bytes, String)>, GraphError> {
        let request = GraphRequest::get(["me", "contacts", id, "photo", "$value"])
            .header("accept", "image/*");
        match self.execute(&request).await {
            Ok(response) => {
                let mime = response
                    .header("content-type")
                    .unwrap_or("image/jpeg")
                    .split(';')
                    .next()
                    .unwrap_or("image/jpeg")
                    .trim()
                    .to_owned();
                let body = response.into_body();
                Ok((!body.is_empty()).then_some((body, mime)))
            }
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Sets a contact's photo (JPEG, PNG, GIF or WebP bytes of the given
    /// MIME type).
    pub async fn set_contact_photo(
        &self,
        id: &str,
        data: impl Into<Bytes>,
        mime: &str,
    ) -> Result<(), GraphError> {
        let request = GraphRequest::new(Method::PUT, ["me", "contacts", id, "photo", "$value"])
            .bytes(image_content_type(mime), data);
        self.send_empty(&request).await
    }

    /// Removes a contact's photo. A contact without one is not an error.
    pub async fn delete_contact_photo(&self, id: &str) -> Result<(), GraphError> {
        let request = GraphRequest::delete(["me", "contacts", id, "photo", "$value"]);
        match self.send_empty(&request).await {
            Err(err) if err.is_not_found() => Ok(()),
            other => other,
        }
    }

    /// People relevant to the user matching `query` (fuzzy, by name or
    /// address), best matches first. Includes people the user corresponds
    /// with who are not saved contacts.
    pub async fn search_people(&self, query: &str, top: u32) -> Result<Vec<Contact>, GraphError> {
        let query = query.replace('"', "");
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let request = GraphRequest::get(["me", "people"])
            .query("$search", format!("\"{}\"", query.trim()))
            .query("$select", PERSON_FIELDS)
            .query("$top", top.clamp(1, 100).to_string());
        let page: Page<WirePerson> = self.get_page(&request).await?;
        Ok(page.items.into_iter().map(map_person).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_contacts() {
        let wire: WireContact = serde_json::from_value(json!({
            "id": "AAMkAGI2THk0AAA=",
            "displayName": "Pavel Bansky",
            "givenName": "Pavel",
            "surname": "Bansky",
            "emailAddresses": [
                {"name": "Pavel Bansky", "address": "pavelb@contoso.com", "type": "personal"},
                {"address": " "}
            ],
            "businessPhones": ["+1 732 555 0102"],
            "homePhones": [],
            "mobilePhone": null,
            "companyName": "Contoso",
            "birthday": "1974-07-22T00:00:00Z"
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let contact = map_contact(wire);
        assert_eq!(contact.name, "Pavel Bansky");
        assert_eq!(contact.emails.len(), 1);
        assert_eq!(contact.emails[0].kind.as_deref(), Some("home"));
        assert!(contact.emails[0].preferred);
        assert_eq!(contact.phones[0].kind.as_deref(), Some("work"));
        assert_eq!(contact.organization.as_deref(), Some("Contoso"));
        assert_eq!(contact.birthday, NaiveDate::from_ymd_opt(1974, 7, 22));
    }

    #[test]
    fn maps_people_and_falls_back_for_names() {
        let wire: WirePerson = serde_json::from_value(json!({
            "id": "33b43a5b-87d6-41ec-91f8-a2610048105f",
            "displayName": "",
            "givenName": "Isaiah",
            "surname": "Langer",
            "scoredEmailAddresses": [{"address": "isaiahl@contoso.com", "relevanceScore": 20.0}],
            "phones": [{"type": "Business", "number": "+1 918 555 0101"}],
            "jobTitle": "Web Marketing Manager"
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let contact = map_person(wire);
        assert_eq!(contact.name, "Isaiah Langer");
        assert_eq!(contact.emails[0].value, "isaiahl@contoso.com");
        assert_eq!(contact.phones[0].kind.as_deref(), Some("work"));
        assert_eq!(contact.title.as_deref(), Some("Web Marketing Manager"));
    }

    fn contact(value: serde_json::Value) -> Contact {
        map_contact(serde_json::from_value(value).unwrap_or_else(|e| panic!("{e}")))
    }

    #[test]
    fn maps_addresses_and_home_page() {
        let c = contact(json!({
            "id": "c1",
            "displayName": "Megan Bowen",
            "homeAddress": {"street": "1 Main St\r\nApt 2", "city": "Redmond", "state": "WA",
                            "postalCode": "98052", "countryOrRegion": "USA"},
            "businessAddress": {},
            "otherAddress": {"street": "", "city": "Berlin"},
            "businessHomePage": "https://contoso.com"
        }));
        assert_eq!(c.addresses.len(), 2, "empty addresses are skipped");
        assert_eq!(c.addresses[0].kind.as_deref(), Some("home"));
        assert_eq!(
            c.addresses[0].formatted(),
            "1 Main St\nApt 2\nRedmond, WA 98052\nUSA"
        );
        assert_eq!(c.addresses[1].kind.as_deref(), Some("other"));
        assert_eq!(c.urls[0].value, "https://contoso.com");
        assert_eq!(c.urls[0].kind.as_deref(), Some("work"));
    }

    #[test]
    fn writes_phones_and_addresses_into_outlook_slots() {
        let phone = |value: &str, kind: Option<&str>| field(value, kind, false);
        let c = Contact {
            name: "Megan Bowen".into(),
            given_name: Some("Megan".into()),
            emails: vec![
                field("a@x.example", None, true),
                field("b@x.example", Some("home"), false),
                field("c@x.example", None, false),
                field("d@x.example", None, false),
            ],
            phones: vec![
                phone("1", Some("cell")),
                phone("2", Some("cell")),
                phone("3", Some("work")),
                phone("4", Some("fax")),
                phone("5", None),
            ],
            addresses: vec![
                ContactAddress::from_text("Home 1", Some("home".into())),
                ContactAddress::from_text("Home 2", Some("home".into())),
            ],
            birthday: NaiveDate::from_ymd_opt(1990, 5, 4),
            ..Contact::default()
        };
        let props = graph_properties(&c);
        assert_eq!(props["mobilePhone"], json!("1"));
        assert_eq!(
            props["businessPhones"],
            json!(["3", "2"]),
            "a second mobile number overflows into a free slot"
        );
        assert_eq!(props["homePhones"], json!(["4", "5"]));
        assert_eq!(props["emailAddresses"].as_array().map(Vec::len), Some(3));
        assert_eq!(props["homeAddress"], json!({"street": "Home 1"}));
        assert_eq!(props["otherAddress"], json!({"street": "Home 2"}));
        assert_eq!(props["businessAddress"], json!({}));
        assert_eq!(props["birthday"], json!("1990-05-04T12:00:00Z"));
        assert_eq!(props["surname"], json!(null));
    }

    #[test]
    fn patches_only_what_changed() {
        let base = contact(json!({
            "id": "c1", "displayName": "Pavel Bansky", "givenName": "Pavel", "surname": "Bansky",
            "emailAddresses": [{"name": "Pavel Bansky", "address": "pavelb@contoso.com"}],
            "businessPhones": ["+1 732 555 0102"], "companyName": "Contoso"
        }));
        assert!(changed_properties(&base, &base).is_empty());
        let mut edited = base.clone();
        edited.title = Some("Manager".into());
        edited.phones.clear();
        let changed = changed_properties(&base, &edited);
        assert_eq!(
            Value::Object(changed),
            json!({"jobTitle": "Manager", "businessPhones": [], "displayName": "Pavel Bansky"})
        );
    }
}
