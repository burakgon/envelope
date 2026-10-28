//! Contacts and people search, mapped onto [`envelope_ical::Contact`] for
//! address autocomplete.

use chrono::NaiveDate;
use envelope_ical::{Contact, ContactField};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{GraphClient, GraphRequest, Page};
use crate::delta::{DeltaCursor, DeltaPage};
use crate::error::GraphError;
use crate::serde_util::null_default;

/// Contact properties used by the mapping.
const CONTACT_FIELDS: &str = "id,displayName,givenName,surname,nickName,emailAddresses,\
businessPhones,homePhones,mobilePhone,companyName,jobTitle,personalNotes,birthday";

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
}
