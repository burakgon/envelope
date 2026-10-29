//! Contacts: paging through contact folders and resolving names against
//! the directory and the user's contacts (`ResolveNames`), for address
//! autocompletion.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseCode, ResponseError};
use crate::folders::DeleteType;
use crate::items::{
    FieldUpdate, FindItemQuery, GET_ITEM_BATCH_SIZE, ItemPage, ItemView, UpdateAttributes,
    first_item_id, value_xml,
};
use crate::messages::MAX_FIND_PAGE_SIZE;
use crate::soap;
use crate::types::{
    BaseShape, BodyType, FolderId, FolderRef, ItemId, ItemRef, ItemShape, Mailbox, PropertyPath,
    child_datetime, string_list, wire_enum,
};
use crate::xml::{Element, XmlWriter};

/// An e-mail address of a contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactEmail {
    /// `EmailAddress1` to `EmailAddress3`.
    pub key: String,
    /// The address, without an `SMTP:` prefix.
    pub address: String,
    /// Display name for this address, when the server reports one.
    pub name: Option<String>,
}

/// A phone number of a contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactPhone {
    /// `BusinessPhone`, `MobilePhone`, `HomePhone`, ...
    pub key: String,
    /// The number as entered.
    pub number: String,
}

/// A part of a contact's postal address, in the schema's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressPart {
    /// `Street` (may span several lines).
    Street,
    /// `City`.
    City,
    /// `State`.
    State,
    /// `CountryOrRegion`.
    CountryOrRegion,
    /// `PostalCode`.
    PostalCode,
}

impl AddressPart {
    /// Every part, in the order Exchange wants them written.
    pub const ALL: [AddressPart; 5] = [
        AddressPart::Street,
        AddressPart::City,
        AddressPart::State,
        AddressPart::CountryOrRegion,
        AddressPart::PostalCode,
    ];

    fn name(self) -> &'static str {
        match self {
            AddressPart::Street => "Street",
            AddressPart::City => "City",
            AddressPart::State => "State",
            AddressPart::CountryOrRegion => "CountryOrRegion",
            AddressPart::PostalCode => "PostalCode",
        }
    }
}

/// A postal address of a contact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalAddress {
    /// `Business`, `Home` or `Other`.
    pub key: String,
    /// Street lines.
    pub street: Option<String>,
    /// City.
    pub city: Option<String>,
    /// State or province.
    pub state: Option<String>,
    /// Country or region.
    pub country_or_region: Option<String>,
    /// Postal code.
    pub postal_code: Option<String>,
}

impl PhysicalAddress {
    /// One part, when it is not blank.
    pub fn part(&self, part: AddressPart) -> Option<&str> {
        match part {
            AddressPart::Street => &self.street,
            AddressPart::City => &self.city,
            AddressPart::State => &self.state,
            AddressPart::CountryOrRegion => &self.country_or_region,
            AddressPart::PostalCode => &self.postal_code,
        }
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    }

    fn from_entry(entry: &Element) -> Option<PhysicalAddress> {
        // Exchange ends the lines of a street with CR LF.
        let text = |name: &str| entry.child_text(name).map(|t| t.replace("\r\n", "\n"));
        let address = PhysicalAddress {
            key: entry.attr("Key")?.to_owned(),
            street: text("Street"),
            city: text("City"),
            state: text("State"),
            country_or_region: text("CountryOrRegion"),
            postal_code: text("PostalCode"),
        };
        AddressPart::ALL
            .iter()
            .any(|p| address.part(*p).is_some())
            .then_some(address)
    }

    fn write_entry(&self, w: &mut XmlWriter, parts: &[AddressPart]) {
        w.start("t:Entry", &[("Key", self.key.as_str())]);
        for part in parts {
            if let Some(value) = self.part(*part) {
                w.text_element(&format!("t:{}", part.name()), value);
            }
        }
        w.end("t:Entry");
    }
}

/// A contact from a contacts folder or the directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// Item id (absent for directory entries).
    pub id: Option<ItemId>,
    /// The folder holding the contact.
    pub parent_folder_id: Option<FolderId>,
    /// Whether this is a contact group (distribution list).
    pub is_group: bool,
    /// Display name.
    pub display_name: Option<String>,
    /// "File as" name.
    pub file_as: Option<String>,
    /// Given name.
    pub given_name: Option<String>,
    /// Middle name.
    pub middle_name: Option<String>,
    /// Surname.
    pub surname: Option<String>,
    /// Nickname.
    pub nickname: Option<String>,
    /// Company.
    pub company_name: Option<String>,
    /// Department.
    pub department: Option<String>,
    /// Job title.
    pub job_title: Option<String>,
    /// E-mail addresses.
    pub email_addresses: Vec<ContactEmail>,
    /// Phone numbers.
    pub phone_numbers: Vec<ContactPhone>,
    /// Birthday.
    pub birthday: Option<DateTime<Utc>>,
    /// Postal addresses.
    pub physical_addresses: Vec<PhysicalAddress>,
    /// `BusinessHomePage`, the contact's web site.
    pub business_home_page: Option<String>,
    /// Notes: the item's text body. `None` when the server did not send it
    /// as text (`FindItem` leaves bodies out; see [`EwsClient::get_contacts`]).
    pub notes: Option<String>,
    /// Categories.
    pub categories: Vec<String>,
    /// `ActiveDirectory` or `Store`.
    pub source: Option<String>,
    /// Last modification.
    pub last_modified: Option<DateTime<Utc>>,
}

impl Contact {
    pub(crate) fn from_element(element: &Element) -> Option<Contact> {
        let is_group = element.is("DistributionList");
        if !element.is("Contact") && !is_group {
            return None;
        }
        Some(Contact {
            id: element.child("ItemId").and_then(ItemId::from_element),
            parent_folder_id: element
                .child("ParentFolderId")
                .and_then(FolderId::from_element),
            is_group,
            display_name: element.child_text("DisplayName"),
            file_as: element.child_text("FileAs"),
            given_name: element.child_text("GivenName"),
            middle_name: element.child_text("MiddleName"),
            surname: element.child_text("Surname"),
            nickname: element.child_text("Nickname"),
            company_name: element.child_text("CompanyName"),
            department: element.child_text("Department"),
            job_title: element.child_text("JobTitle"),
            email_addresses: element
                .child("EmailAddresses")
                .map(|list| {
                    list.children_named("Entry")
                        .filter_map(|entry| {
                            let address = strip_smtp(&entry.trimmed_text()?)?;
                            Some(ContactEmail {
                                key: entry.attr("Key").unwrap_or_default().to_owned(),
                                address,
                                name: entry
                                    .attr("Name")
                                    .filter(|n| !n.trim().is_empty())
                                    .map(str::to_owned),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            phone_numbers: element
                .child("PhoneNumbers")
                .map(|list| {
                    list.children_named("Entry")
                        .filter_map(|entry| {
                            Some(ContactPhone {
                                key: entry.attr("Key").unwrap_or_default().to_owned(),
                                number: entry.trimmed_text()?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            birthday: child_datetime(element, "Birthday"),
            physical_addresses: element
                .child("PhysicalAddresses")
                .map(|list| {
                    list.children_named("Entry")
                        .filter_map(PhysicalAddress::from_entry)
                        .collect()
                })
                .unwrap_or_default(),
            business_home_page: element.child_text("BusinessHomePage"),
            notes: element
                .child("Body")
                .filter(|b| b.attr("BodyType") == Some("Text"))
                .map(|b| b.text().replace("\r\n", "\n").trim().to_owned()),
            categories: string_list(element.child("Categories")),
            source: element.child_text("ContactSource"),
            last_modified: child_datetime(element, "LastModifiedTime"),
        })
    }

    /// The first e-mail address.
    pub fn primary_email(&self) -> Option<&str> {
        self.email_addresses.first().map(|e| e.address.as_str())
    }
}

/// Removes an `SMTP:` prefix; addresses of other types (`sip:`, `X500:`,
/// legacy DNs) are dropped.
fn strip_smtp(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let address = match raw.split_once(':') {
        Some((kind, rest)) if kind.eq_ignore_ascii_case("smtp") => rest.trim(),
        Some((kind, _)) if !kind.contains('@') => return None,
        _ => raw,
    };
    (address.contains('@') && !address.starts_with('/')).then(|| address.to_owned())
}

wire_enum! {
    /// Where `ResolveNames` looks.
    pub enum ResolveScope {
        /// The directory only.
        ActiveDirectory = "ActiveDirectory",
        /// The directory, then the user's contacts.
        ActiveDirectoryContacts = "ActiveDirectoryContacts",
        /// The user's contacts only.
        Contacts = "Contacts",
        /// The user's contacts, then the directory.
        ContactsActiveDirectory = "ContactsActiveDirectory",
    }
}

/// One match of [`EwsClient::resolve_names`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedName {
    /// Address and display name.
    pub mailbox: Mailbox,
    /// Contact details, when requested and available.
    pub contact: Option<Contact>,
}

pub(crate) fn resolve_names_request(
    query: &str,
    scope: ResolveScope,
    full_contact_data: bool,
) -> String {
    let mut w = XmlWriter::new();
    w.start(
        "m:ResolveNames",
        &[
            (
                "ReturnFullContactData",
                if full_contact_data { "true" } else { "false" },
            ),
            ("SearchScope", scope.as_str()),
        ],
    )
    .text_element("m:UnresolvedEntry", query)
    .end("m:ResolveNames");
    w.into_string()
}

pub(crate) fn parse_resolutions(message: &Element) -> Vec<ResolvedName> {
    message
        .child("ResolutionSet")
        .map(|set| {
            set.children_named("Resolution")
                .filter_map(|resolution| {
                    Some(ResolvedName {
                        mailbox: resolution.child("Mailbox").map(Mailbox::from_element)?,
                        contact: resolution.child("Contact").and_then(Contact::from_element),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl EwsClient {
    /// One page of a contacts folder, sorted by display name.
    pub async fn find_contacts(
        &self,
        folder: impl Into<FolderRef>,
        offset: u32,
        page_size: u32,
    ) -> Result<ItemPage<Contact>, EwsError> {
        let folders = [folder.into()];
        // Contacts have no properties FindItem refuses, so the server's
        // full set is safe and saves a GetItem round trip.
        let shape = ItemShape {
            base: BaseShape::AllProperties,
            include_mime_content: false,
            body_type: None,
            additional: Vec::new(),
        };
        let query = FindItemQuery {
            shape: &shape,
            view: ItemView::Indexed {
                offset,
                max: page_size.clamp(1, MAX_FIND_PAGE_SIZE),
            },
            restriction: None,
            sort: Some(("contacts:DisplayName", false)),
            folders: &folders,
            query: None,
        };
        self.find_items_with(&query, Contact::from_element).await
    }

    /// Fetches contacts by id with all properties, notes included (as
    /// text). One result per id.
    pub async fn get_contacts(
        &self,
        ids: &[ItemId],
    ) -> Result<Vec<Result<Contact, ResponseError>>, EwsError> {
        let shape = ItemShape {
            base: BaseShape::AllProperties,
            include_mime_content: false,
            body_type: Some(BodyType::Text),
            additional: Vec::new(),
        };
        let refs: Vec<ItemRef> = ids.iter().map(ItemRef::from).collect();
        self.get_items_with(&refs, &shape, GET_ITEM_BATCH_SIZE, Contact::from_element)
            .await
    }

    /// Resolves a partial name or address (at least a few characters)
    /// against the directory and/or the user's contacts. Returns no matches
    /// rather than an error when nothing is found; the server returns at
    /// most 100 matches.
    pub async fn resolve_names(
        &self,
        query: &str,
        scope: ResolveScope,
        full_contact_data: bool,
    ) -> Result<Vec<ResolvedName>, EwsError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let body = resolve_names_request(query, scope, full_contact_data);
        let response = self.call(&SoapRequest::read("ResolveNames", body)).await?;
        let messages = soap::response_messages(&response)?;
        let Some(message) = messages.into_iter().next() else {
            return Ok(Vec::new());
        };
        match soap::check(message) {
            Ok(message) => Ok(parse_resolutions(message)),
            Err(error) if error.code == ResponseCode::ErrorNameResolutionNoResults => {
                Ok(Vec::new())
            }
            Err(error) => Err(error.into()),
        }
    }
}

// ----- writing ---------------------------------------------------------------------

/// A text field of a contact that Envelope writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactText {
    /// `DisplayName`.
    DisplayName,
    /// `GivenName`.
    GivenName,
    /// `Nickname`.
    Nickname,
    /// `CompanyName`.
    CompanyName,
    /// `JobTitle`.
    JobTitle,
    /// `Surname`.
    Surname,
}

impl ContactText {
    fn name(self) -> &'static str {
        match self {
            ContactText::DisplayName => "DisplayName",
            ContactText::GivenName => "GivenName",
            ContactText::Nickname => "Nickname",
            ContactText::CompanyName => "CompanyName",
            ContactText::JobTitle => "JobTitle",
            ContactText::Surname => "Surname",
        }
    }
}

/// A contact to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewContact {
    /// Text fields.
    pub text: Vec<(ContactText, String)>,
    /// `(key, address)`, keys `EmailAddress1` to `EmailAddress3`.
    pub emails: Vec<(String, String)>,
    /// `(key, number)`, keys such as `MobilePhone` or `BusinessPhone`.
    pub phones: Vec<(String, String)>,
    /// Birthday.
    pub birthday: Option<NaiveDate>,
    /// Postal addresses, one per key (`Business`, `Home`, `Other`).
    pub addresses: Vec<PhysicalAddress>,
    /// Web site (`BusinessHomePage`).
    pub home_page: Option<String>,
    /// Notes (the text body).
    pub notes: Option<String>,
}

/// One change to an existing contact; `None` clears the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactChange {
    /// A text field.
    Text(ContactText, Option<String>),
    /// An e-mail entry (`EmailAddress1` to `EmailAddress3`).
    Email(String, Option<String>),
    /// A phone entry (`MobilePhone`, `HomePhone`, ...).
    Phone(String, Option<String>),
    /// The birthday.
    Birthday(Option<NaiveDate>),
    /// One part of the postal address with a key (`Business`, `Home`, `Other`).
    Address(String, AddressPart, Option<String>),
    /// The web site.
    HomePage(Option<String>),
    /// The notes.
    Notes(Option<String>),
}

/// Midday UTC keeps the date the same in every time zone.
fn birthday_value(day: NaiveDate) -> String {
    format!("{}T12:00:00Z", day.format("%Y-%m-%d"))
}

fn write_entries(w: &mut XmlWriter, list: &str, entries: &[(String, String)]) {
    if entries.is_empty() {
        return;
    }
    w.start(list, &[]);
    for (key, value) in entries {
        w.text_element_with("t:Entry", &[("Key", key)], value);
    }
    w.end(list);
}

fn create_contact_request(folder: &FolderRef, contact: &NewContact) -> String {
    let text = |field: ContactText| {
        contact
            .text
            .iter()
            .find(|(f, v)| *f == field && !v.trim().is_empty())
            .map(|(_, v)| v.as_str())
    };
    let mut w = XmlWriter::new();
    w.start("m:CreateItem", &[])
        .start("m:SavedItemFolderId", &[]);
    folder.write(&mut w);
    w.end("m:SavedItemFolderId")
        .start("m:Items", &[])
        .start("t:Contact", &[]);
    // Exchange insists on the schema's order of elements: the item's body
    // first, then the contact's fields.
    if let Some(notes) = contact.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        w.text_element_with("t:Body", &[("BodyType", "Text")], notes);
    }
    for field in [
        ContactText::DisplayName,
        ContactText::GivenName,
        ContactText::Nickname,
        ContactText::CompanyName,
    ] {
        if let Some(v) = text(field) {
            w.text_element(&format!("t:{}", field.name()), v);
        }
    }
    write_entries(&mut w, "t:EmailAddresses", &contact.emails);
    let addresses: Vec<&PhysicalAddress> = contact
        .addresses
        .iter()
        .filter(|a| AddressPart::ALL.iter().any(|p| a.part(*p).is_some()))
        .collect();
    if !addresses.is_empty() {
        w.start("t:PhysicalAddresses", &[]);
        for address in addresses {
            address.write_entry(&mut w, &AddressPart::ALL);
        }
        w.end("t:PhysicalAddresses");
    }
    write_entries(&mut w, "t:PhoneNumbers", &contact.phones);
    if let Some(day) = contact.birthday {
        w.text_element("t:Birthday", &birthday_value(day));
    }
    if let Some(page) = contact
        .home_page
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        w.text_element("t:BusinessHomePage", page.trim());
    }
    for field in [ContactText::JobTitle, ContactText::Surname] {
        if let Some(v) = text(field) {
            w.text_element(&format!("t:{}", field.name()), v);
        }
    }
    w.end("t:Contact").end("m:Items").end("m:CreateItem");
    w.into_string()
}

fn contact_value(inner: impl FnOnce(&mut XmlWriter)) -> String {
    let mut w = XmlWriter::new();
    w.start("t:Contact", &[]);
    inner(&mut w);
    w.end("t:Contact");
    w.into_string()
}

fn field_updates(changes: &[ContactChange]) -> Vec<FieldUpdate> {
    let indexed = |uri: &str, key: &str| PropertyPath::Indexed {
        uri: uri.to_owned(),
        index: key.to_owned(),
    };
    changes
        .iter()
        .map(|change| match change {
            ContactChange::Text(field, Some(value)) if !value.trim().is_empty() => {
                FieldUpdate::set(
                    PropertyPath::Field(format!("contacts:{}", field.name())),
                    value_xml("Contact", field.name(), value),
                )
            }
            ContactChange::Text(field, _) => {
                FieldUpdate::Delete(PropertyPath::Field(format!("contacts:{}", field.name())))
            }
            ContactChange::Email(key, Some(address)) => FieldUpdate::set(
                indexed("contacts:EmailAddress", key),
                contact_value(|w| {
                    write_entries(w, "t:EmailAddresses", &[(key.clone(), address.clone())])
                }),
            ),
            ContactChange::Email(key, None) => {
                FieldUpdate::Delete(indexed("contacts:EmailAddress", key))
            }
            ContactChange::Phone(key, Some(number)) => FieldUpdate::set(
                indexed("contacts:PhoneNumber", key),
                contact_value(|w| {
                    write_entries(w, "t:PhoneNumbers", &[(key.clone(), number.clone())])
                }),
            ),
            ContactChange::Phone(key, None) => {
                FieldUpdate::Delete(indexed("contacts:PhoneNumber", key))
            }
            ContactChange::Birthday(Some(day)) => FieldUpdate::set(
                PropertyPath::Field("contacts:Birthday".into()),
                value_xml("Contact", "Birthday", &birthday_value(*day)),
            ),
            ContactChange::Birthday(None) => {
                FieldUpdate::Delete(PropertyPath::Field("contacts:Birthday".into()))
            }
            ContactChange::Address(key, part, Some(value)) if !value.trim().is_empty() => {
                let mut address = PhysicalAddress {
                    key: key.clone(),
                    ..PhysicalAddress::default()
                };
                let value = Some(value.trim().to_owned());
                match part {
                    AddressPart::Street => address.street = value,
                    AddressPart::City => address.city = value,
                    AddressPart::State => address.state = value,
                    AddressPart::CountryOrRegion => address.country_or_region = value,
                    AddressPart::PostalCode => address.postal_code = value,
                }
                FieldUpdate::set(
                    indexed(&format!("contacts:PhysicalAddress:{}", part.name()), key),
                    contact_value(|w| {
                        w.start("t:PhysicalAddresses", &[]);
                        address.write_entry(w, &[*part]);
                        w.end("t:PhysicalAddresses");
                    }),
                )
            }
            ContactChange::Address(key, part, _) => FieldUpdate::Delete(indexed(
                &format!("contacts:PhysicalAddress:{}", part.name()),
                key,
            )),
            ContactChange::HomePage(Some(page)) if !page.trim().is_empty() => FieldUpdate::set(
                PropertyPath::Field("contacts:BusinessHomePage".into()),
                value_xml("Contact", "BusinessHomePage", page.trim()),
            ),
            ContactChange::HomePage(_) => {
                FieldUpdate::Delete(PropertyPath::Field("contacts:BusinessHomePage".into()))
            }
            // A body is not deleted, only emptied.
            ContactChange::Notes(notes) => FieldUpdate::set(
                PropertyPath::Field("item:Body".into()),
                contact_value(|w| {
                    w.text_element_with(
                        "t:Body",
                        &[("BodyType", "Text")],
                        notes.as_deref().unwrap_or_default(),
                    );
                }),
            ),
        })
        .collect()
}

impl EwsClient {
    /// Creates a contact in `folder` and returns its id.
    pub async fn create_contact(
        &self,
        folder: impl Into<FolderRef>,
        contact: &NewContact,
    ) -> Result<ItemId, EwsError> {
        let body = create_contact_request(&folder.into(), contact);
        let response = self.call(&SoapRequest::write("CreateItem", body)).await?;
        first_item_id(soap::single(&response)?)
            .ok_or_else(|| EwsError::InvalidResponse("CreateItem returned no item id".into()))
    }

    /// Changes a contact and returns its new id (with the new change key).
    pub async fn update_contact(
        &self,
        id: &ItemId,
        changes: &[ContactChange],
    ) -> Result<ItemId, EwsError> {
        if changes.is_empty() {
            return Ok(id.clone());
        }
        let updates = vec![(ItemRef::from(id), field_updates(changes))];
        let result = self
            .update_items(&updates, &UpdateAttributes::default())
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("UpdateItem returned nothing".into()))?;
        Ok(result?.unwrap_or_else(|| id.clone()))
    }

    /// Moves a contact to Deleted Items.
    pub async fn delete_contact(&self, id: &ItemId) -> Result<(), EwsError> {
        let result = self
            .delete_items(std::slice::from_ref(id), DeleteType::MoveToDeletedItems)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("DeleteItem returned nothing".into()))?;
        result.map_err(EwsError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    #[test]
    fn resolve_names_request_escapes_the_query() {
        assert_eq!(
            resolve_names_request("O'Brien <ob", ResolveScope::ActiveDirectoryContacts, true),
            r#"<m:ResolveNames ReturnFullContactData="true" SearchScope="ActiveDirectoryContacts"><m:UnresolvedEntry>O&apos;Brien &lt;ob</m:UnresolvedEntry></m:ResolveNames>"#
        );
    }

    #[test]
    fn parses_resolutions_with_contact_data() {
        let message = xml::parse(
            r#"<m:ResolveNamesResponseMessage ResponseClass="Warning" xmlns:m="m" xmlns:t="t">
                 <m:MessageText>Multiple results were found.</m:MessageText>
                 <m:ResponseCode>ErrorNameResolutionMultipleResults</m:ResponseCode>
                 <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
                 <m:ResolutionSet TotalItemsInView="2" IncludesLastItemInRange="true">
                   <t:Resolution>
                     <t:Mailbox><t:Name>John Doe</t:Name><t:EmailAddress>john@example.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Mailbox</t:MailboxType></t:Mailbox>
                     <t:Contact>
                       <t:DisplayName>John Doe</t:DisplayName>
                       <t:GivenName>John</t:GivenName>
                       <t:EmailAddresses>
                         <t:Entry Key="EmailAddress1">SMTP:john@example.com</t:Entry>
                         <t:Entry Key="EmailAddress2">sip:john@example.com</t:Entry>
                         <t:Entry Key="EmailAddress3">X500:/o=Example/ou=Exchange/cn=Recipients/cn=john</t:Entry>
                       </t:EmailAddresses>
                       <t:PhoneNumbers><t:Entry Key="BusinessPhone">+1 425 555 0100</t:Entry><t:Entry Key="MobilePhone"/></t:PhoneNumbers>
                       <t:ContactSource>ActiveDirectory</t:ContactSource>
                       <t:CompanyName>Example Inc.</t:CompanyName>
                       <t:JobTitle>Engineer</t:JobTitle>
                     </t:Contact>
                   </t:Resolution>
                   <t:Resolution>
                     <t:Mailbox><t:Name>Johanna</t:Name><t:EmailAddress>johanna@example.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Contact</t:MailboxType><t:ItemId Id="c1"/></t:Mailbox>
                   </t:Resolution>
                 </m:ResolutionSet>
               </m:ResolveNamesResponseMessage>"#,
        )
        .unwrap();
        let checked = soap::check(&message).expect("warnings pass");
        let resolutions = parse_resolutions(checked);
        assert_eq!(resolutions.len(), 2);
        let john = &resolutions[0];
        assert_eq!(john.mailbox.smtp_address(), Some("john@example.com"));
        let contact = john.contact.as_ref().unwrap();
        assert!(contact.id.is_none());
        assert_eq!(contact.primary_email(), Some("john@example.com"));
        assert_eq!(contact.email_addresses.len(), 1);
        assert_eq!(
            contact.phone_numbers,
            vec![ContactPhone {
                key: "BusinessPhone".into(),
                number: "+1 425 555 0100".into()
            }]
        );
        assert_eq!(contact.source.as_deref(), Some("ActiveDirectory"));
        assert!(resolutions[1].contact.is_none());
    }

    #[test]
    fn parses_store_contacts_and_groups() {
        let root = xml::parse(
            r#"<t:Items xmlns:t="t">
                 <t:Contact><t:ItemId Id="c1" ChangeKey="k"/><t:Body BodyType="Text">Met at the Royal Society.&#13;
Prefers letters.</t:Body><t:DisplayName>Ada Lovelace</t:DisplayName><t:Surname>Lovelace</t:Surname><t:EmailAddresses><t:Entry Key="EmailAddress1" Name="Ada">ada@example.com</t:Entry></t:EmailAddresses><t:PhysicalAddresses><t:Entry Key="Home"><t:Street>12 St James's Square&#13;
Flat 2</t:Street><t:City>London</t:City><t:PostalCode>SW1Y 4JH</t:PostalCode></t:Entry><t:Entry Key="Business"/></t:PhysicalAddresses><t:Birthday>1815-12-10T12:00:00Z</t:Birthday><t:BusinessHomePage>https://ada.example</t:BusinessHomePage><t:Categories><t:String>VIP</t:String></t:Categories></t:Contact>
                 <t:Contact><t:ItemId Id="c2"/><t:Body BodyType="HTML">&lt;p&gt;Hi&lt;/p&gt;</t:Body><t:DisplayName>Charles Babbage</t:DisplayName></t:Contact>
                 <t:DistributionList><t:ItemId Id="g1"/><t:DisplayName>Team</t:DisplayName></t:DistributionList>
                 <t:Message><t:ItemId Id="m1"/></t:Message>
               </t:Items>"#,
        )
        .unwrap();
        let contacts: Vec<Contact> = root.elements().filter_map(Contact::from_element).collect();
        assert_eq!(contacts.len(), 3);
        assert_eq!(contacts[0].email_addresses[0].name.as_deref(), Some("Ada"));
        assert!(contacts[0].birthday.is_some());
        assert_eq!(contacts[0].categories, vec!["VIP".to_owned()]);
        // Notes and streets with their CR LF line ends as plain new lines; an
        // address without parts is no address.
        assert_eq!(
            contacts[0].notes.as_deref(),
            Some("Met at the Royal Society.\nPrefers letters.")
        );
        assert_eq!(
            contacts[0].physical_addresses,
            [PhysicalAddress {
                key: "Home".into(),
                street: Some("12 St James's Square\nFlat 2".into()),
                city: Some("London".into()),
                postal_code: Some("SW1Y 4JH".into()),
                ..PhysicalAddress::default()
            }]
        );
        assert_eq!(
            contacts[0].business_home_page.as_deref(),
            Some("https://ada.example")
        );
        // An HTML body is not taken for notes.
        assert_eq!(contacts[1].notes, None);
        assert!(contacts[2].is_group);
    }

    #[test]
    fn smtp_prefixes() {
        assert_eq!(
            strip_smtp("SMTP:a@example.com").as_deref(),
            Some("a@example.com")
        );
        assert_eq!(
            strip_smtp("smtp:a@example.com").as_deref(),
            Some("a@example.com")
        );
        assert_eq!(
            strip_smtp("a@example.com").as_deref(),
            Some("a@example.com")
        );
        assert_eq!(strip_smtp("sip:a@example.com"), None);
        assert_eq!(strip_smtp("X500:/o=x/cn=y"), None);
        assert_eq!(strip_smtp("/o=x/cn=y"), None);
    }

    #[test]
    fn writes_new_contacts_in_schema_order() {
        let contact = NewContact {
            text: vec![
                (ContactText::Surname, "Lovelace".into()),
                (ContactText::GivenName, "Ada".into()),
                (ContactText::DisplayName, "Ada Lovelace".into()),
                (ContactText::JobTitle, "Analyst".into()),
                (ContactText::Nickname, " ".into()),
            ],
            emails: vec![("EmailAddress1".into(), "ada@example.com".into())],
            phones: vec![("MobilePhone".into(), "+44 20 7946 0000".into())],
            birthday: NaiveDate::from_ymd_opt(1815, 12, 10),
            addresses: vec![PhysicalAddress {
                key: "Home".into(),
                street: Some("12 St James's Square".into()),
                city: Some("London".into()),
                postal_code: Some("SW1Y 4JH".into()),
                ..PhysicalAddress::default()
            }],
            home_page: Some("https://ada.example".into()),
            notes: Some("Prefers letters.".into()),
        };
        assert_eq!(
            create_contact_request(
                &FolderRef::from(crate::DistinguishedFolder::Contacts),
                &contact
            ),
            "<m:CreateItem><m:SavedItemFolderId><t:DistinguishedFolderId Id=\"contacts\"/></m:SavedItemFolderId>\
             <m:Items><t:Contact><t:Body BodyType=\"Text\">Prefers letters.</t:Body>\
             <t:DisplayName>Ada Lovelace</t:DisplayName><t:GivenName>Ada</t:GivenName>\
             <t:EmailAddresses><t:Entry Key=\"EmailAddress1\">ada@example.com</t:Entry></t:EmailAddresses>\
             <t:PhysicalAddresses><t:Entry Key=\"Home\"><t:Street>12 St James&apos;s Square</t:Street><t:City>London</t:City>\
             <t:PostalCode>SW1Y 4JH</t:PostalCode></t:Entry></t:PhysicalAddresses>\
             <t:PhoneNumbers><t:Entry Key=\"MobilePhone\">+44 20 7946 0000</t:Entry></t:PhoneNumbers>\
             <t:Birthday>1815-12-10T12:00:00Z</t:Birthday><t:BusinessHomePage>https://ada.example</t:BusinessHomePage>\
             <t:JobTitle>Analyst</t:JobTitle><t:Surname>Lovelace</t:Surname>\
             </t:Contact></m:Items></m:CreateItem>"
        );
    }

    #[test]
    fn writes_changes_as_field_updates() {
        let body = crate::items::update_item_request(
            &[(
                ItemRef::from(&ItemId {
                    id: "c1".into(),
                    change_key: Some("k1".into()),
                }),
                field_updates(&[
                    ContactChange::Text(
                        ContactText::CompanyName,
                        Some("Analytical Engines".into()),
                    ),
                    ContactChange::Text(ContactText::Nickname, None),
                    ContactChange::Email("EmailAddress2".into(), Some("ada@work.example".into())),
                    ContactChange::Phone("HomePhone".into(), None),
                    ContactChange::Address("Home".into(), AddressPart::City, Some("London".into())),
                    ContactChange::Address("Business".into(), AddressPart::PostalCode, None),
                    ContactChange::HomePage(Some("https://ada.example".into())),
                    ContactChange::Notes(None),
                ]),
            )],
            &UpdateAttributes::default(),
        );
        assert!(body.contains("<t:SetItemField><t:IndexedFieldURI FieldURI=\"contacts:PhysicalAddress:City\" FieldIndex=\"Home\"/><t:Contact><t:PhysicalAddresses><t:Entry Key=\"Home\"><t:City>London</t:City></t:Entry></t:PhysicalAddresses></t:Contact></t:SetItemField>"), "{body}");
        assert!(body.contains("<t:DeleteItemField><t:IndexedFieldURI FieldURI=\"contacts:PhysicalAddress:PostalCode\" FieldIndex=\"Business\"/></t:DeleteItemField>"), "{body}");
        assert!(body.contains("<t:SetItemField><t:FieldURI FieldURI=\"contacts:BusinessHomePage\"/><t:Contact><t:BusinessHomePage>https://ada.example</t:BusinessHomePage></t:Contact></t:SetItemField>"), "{body}");
        // Notes are emptied rather than deleted.
        assert!(body.contains("<t:SetItemField><t:FieldURI FieldURI=\"item:Body\"/><t:Contact><t:Body BodyType=\"Text\"></t:Body></t:Contact></t:SetItemField>"), "{body}");
        assert!(body.contains("<t:SetItemField><t:FieldURI FieldURI=\"contacts:CompanyName\"/><t:Contact><t:CompanyName>Analytical Engines</t:CompanyName></t:Contact></t:SetItemField>"), "{body}");
        assert!(body.contains("<t:DeleteItemField><t:FieldURI FieldURI=\"contacts:Nickname\"/></t:DeleteItemField>"), "{body}");
        assert!(body.contains("<t:SetItemField><t:IndexedFieldURI FieldURI=\"contacts:EmailAddress\" FieldIndex=\"EmailAddress2\"/><t:Contact><t:EmailAddresses><t:Entry Key=\"EmailAddress2\">ada@work.example</t:Entry></t:EmailAddresses></t:Contact></t:SetItemField>"), "{body}");
        assert!(body.contains("<t:DeleteItemField><t:IndexedFieldURI FieldURI=\"contacts:PhoneNumber\" FieldIndex=\"HomePhone\"/></t:DeleteItemField>"), "{body}");
    }
}
