//! CardDAV address books (RFC 6352): listing, creation and property updates.
//! vCards themselves are read and written with the generic object methods
//! ([`DavClient::addressbook_multiget`], [`DavClient::put`],
//! [`DavClient::delete`]) and synchronised with
//! [`DavClient::sync_collection`].

use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use crate::client::{DavClient, Depth};
use crate::collection::collection_url_in;
use crate::error::{DavError, Result};
use crate::href;
use crate::multistatus::Response;
use crate::props;
use crate::xml::{self, Ns, PropName, text_element};

/// An address book collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressBook {
    /// Absolute, normalised URL of the collection.
    pub href: String,
    /// `DAV:displayname`.
    pub display_name: Option<String>,
    /// `addressbook-description`.
    pub description: Option<String>,
    /// CalendarServer `getctag`.
    pub ctag: Option<String>,
    /// RFC 6578 sync token of the current state.
    pub sync_token: Option<String>,
    /// True when the user may not add, change or delete vCards.
    pub read_only: bool,
    /// True when the address book supports `sync-collection`.
    pub supports_sync: bool,
}

/// Properties of an address book to create with
/// [`DavClient::create_addressbook`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAddressBook {
    /// Name shown to the user.
    pub display_name: String,
    /// Free-form description.
    pub description: Option<String>,
}

impl NewAddressBook {
    /// An address book with the given name.
    pub fn new(display_name: impl Into<String>) -> Self {
        NewAddressBook {
            display_name: display_name.into(),
            description: None,
        }
    }

    /// Sets the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Changes applied by [`DavClient::update_addressbook`]; `None` fields are
/// left untouched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressBookPatch {
    /// New display name.
    pub display_name: Option<String>,
    /// New description.
    pub description: Option<String>,
}

const ADDRESSBOOK_PROPS: &[PropName] = &[
    (Ns::Dav, "resourcetype"),
    (Ns::Dav, "displayname"),
    (Ns::CardDav, "addressbook-description"),
    (Ns::CalendarServer, "getctag"),
    (Ns::Dav, "sync-token"),
    (Ns::Dav, "current-user-privilege-set"),
    (Ns::Dav, "supported-report-set"),
];

impl DavClient {
    /// Lists the address books in an address book home set (from
    /// [`Discovery::addressbook_home_set`](crate::Discovery::addressbook_home_set)).
    pub async fn list_addressbooks(&self, home_set: &str) -> Result<Vec<AddressBook>> {
        let home = self.resolve_input(home_set)?;
        let reply = self
            .propfind(&home, Depth::One, xml::propfind_body(ADDRESSBOOK_PROPS))
            .await?;
        let books: Vec<AddressBook> = reply
            .entries()
            .into_iter()
            .filter(|(url, _)| !href::same_resource(url, &home))
            .filter_map(|(url, response)| addressbook_from_response(url, response))
            .collect();
        debug!(home = %home, count = books.len(), "listed address books");
        Ok(books)
    }

    /// Reads the properties of a single address book.
    pub async fn get_addressbook(&self, href: &str) -> Result<AddressBook> {
        let url = self.resolve_input(href)?;
        let reply = self
            .propfind(&url, Depth::Zero, xml::propfind_body(ADDRESSBOOK_PROPS))
            .await?;
        reply
            .own_entry()
            .and_then(|(url, response)| addressbook_from_response(url, response))
            .ok_or_else(|| DavError::InvalidResponse(format!("{url} is not an address book")))
    }

    /// Creates an address book with an extended `MKCOL` (RFC 5689) inside
    /// `home_set` and returns its href. Not every server allows clients to
    /// create address books.
    pub async fn create_addressbook(
        &self,
        home_set: &str,
        addressbook: &NewAddressBook,
    ) -> Result<String> {
        let home = self.resolve_input(home_set)?;
        let url = collection_url_in(&home)?;
        let mut props = text_element(Ns::Dav, "displayname", &addressbook.display_name);
        if let Some(description) = &addressbook.description {
            props.push_str(&text_element(
                Ns::CardDav,
                "addressbook-description",
                description,
            ));
        }
        self.mkcol_extended(&url, "<CR:addressbook/>", &props)
            .await?;
        Ok(url.into())
    }

    /// Renames an address book or changes its description with `PROPPATCH`.
    pub async fn update_addressbook(&self, href: &str, patch: &AddressBookPatch) -> Result<()> {
        let url = self.resolve_input(href)?;
        let mut props = String::new();
        if let Some(name) = &patch.display_name {
            props.push_str(&text_element(Ns::Dav, "displayname", name));
        }
        if let Some(description) = &patch.description {
            props.push_str(&text_element(
                Ns::CardDav,
                "addressbook-description",
                description,
            ));
        }
        if props.is_empty() {
            return Ok(());
        }
        self.proppatch(&url, &props).await
    }
}

fn addressbook_from_response(url: Url, response: &Response) -> Option<AddressBook> {
    if response.is_not_found() || !response.has_resource_type(Ns::CardDav, "addressbook") {
        return None;
    }
    Some(AddressBook {
        href: url.into(),
        display_name: response.prop_text(Ns::Dav, "displayname"),
        description: response.prop_text(Ns::CardDav, "addressbook-description"),
        ctag: response.prop_text(Ns::CalendarServer, "getctag"),
        sync_token: response.prop_text(Ns::Dav, "sync-token"),
        read_only: props::is_read_only(response),
        supports_sync: props::supports_sync(response),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MultiStatusReply;
    use crate::multistatus::parse_multistatus;

    fn books(xml: &str, home: &str) -> Vec<AddressBook> {
        let home = Url::parse(home).unwrap();
        let reply = MultiStatusReply {
            url: home.clone(),
            multistatus: parse_multistatus(xml).unwrap(),
        };
        reply
            .entries()
            .into_iter()
            .filter(|(url, _)| !href::same_resource(url, &home))
            .filter_map(|(url, response)| addressbook_from_response(url, response))
            .collect()
    }

    #[test]
    fn icloud_addressbooks() {
        let books = books(
            include_str!("../tests/fixtures/icloud_addressbooks.xml"),
            "https://p42-contacts.icloud.com/1234567890/carddavhome/",
        );
        assert_eq!(books.len(), 1);
        let card = &books[0];
        assert_eq!(
            card.href,
            "https://p42-contacts.icloud.com/1234567890/carddavhome/card/"
        );
        assert_eq!(card.display_name, None);
        assert_eq!(
            card.ctag.as_deref(),
            Some("HwoQEgwAAAABcd0AAAAAAAAAAAAAAAAYACAo")
        );
        assert_eq!(
            card.sync_token.as_deref(),
            Some("HwoQEgwAAAABcd0AAAAAAAAAAAAAAAAYACAo")
        );
        assert!(!card.read_only);
        assert!(card.supports_sync);
    }

    #[test]
    fn nextcloud_addressbooks() {
        let books = books(
            include_str!("../tests/fixtures/nextcloud_addressbooks.xml"),
            "https://cloud.example.com/remote.php/dav/addressbooks/users/jane/",
        );
        let names: Vec<_> = books
            .iter()
            .map(|b| b.display_name.clone().unwrap_or_default())
            .collect();
        assert_eq!(names, ["Contacts", "Recently contacted"]);
        assert_eq!(
            books[0].href,
            "https://cloud.example.com/remote.php/dav/addressbooks/users/jane/contacts/"
        );
        assert_eq!(books[0].description.as_deref(), Some("Personal & work"));
        assert_eq!(
            books[0].sync_token.as_deref(),
            Some("http://sabre.io/ns/sync/12")
        );
        assert!(!books[0].read_only);
        assert!(books[1].read_only);
    }
}
