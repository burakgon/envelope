//! Reading and writing individual calendar objects and vCards, and fetching
//! many of them at once with `calendar-multiget` / `addressbook-multiget`.

use std::collections::HashSet;

use reqwest::header::{HeaderValue, IF_MATCH, IF_NONE_MATCH};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;

use crate::client::{
    DavClient, DavRequest, Depth, HttpResponse, MultiStatusReply, if_match_value, normalize_etag,
};
use crate::error::Result;
use crate::href::{self, ObjectKind};
use crate::multistatus::is_success;
use crate::props::payload;
use crate::xml::{self, Ns, PropName, REQUEST_NAMESPACES, XML_DECLARATION};

/// Number of hrefs requested per multiget report.
pub const MULTIGET_BATCH_SIZE: usize = 100;

/// A calendar object or vCard with its raw payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DavObject {
    /// Absolute, normalised URL of the resource.
    pub href: String,
    /// Entity tag of this version, when the server reported one.
    pub etag: Option<String>,
    /// The `text/calendar` or `text/vcard` payload, unparsed.
    pub data: String,
}

/// A resource in a collection, identified by href and version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    /// Absolute, normalised URL of the resource.
    pub href: String,
    /// Entity tag of the current version, when the server reported one.
    pub etag: Option<String>,
}

/// Result of [`DavClient::multiget`] / [`DavClient::addressbook_multiget`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultigetResult {
    /// The resources that were found, in server order.
    pub objects: Vec<DavObject>,
    /// Requested hrefs the server reported as missing (typically deleted
    /// since they were listed).
    pub missing: Vec<String>,
}

/// Condition attached to a write.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Precondition {
    /// Unconditional write: overwrites whatever is on the server.
    #[default]
    None,
    /// Only write if the resource still has this entity tag (`If-Match`);
    /// fails with [`PreconditionFailed`](crate::DavError::PreconditionFailed)
    /// if it changed.
    IfMatch(String),
    /// Only write if the resource does not exist yet (`If-None-Match: *`);
    /// fails with [`PreconditionFailed`](crate::DavError::PreconditionFailed)
    /// if it does.
    IfNoneMatch,
}

impl DavClient {
    /// Downloads a single resource with `GET`.
    pub async fn get(&self, href: &str) -> Result<DavObject> {
        let url = self.resolve_input(href)?;
        let response = self.send(DavRequest::new(Method::GET, url)).await?;
        if !response.status.is_success() {
            return Err(response.into_error());
        }
        Ok(DavObject {
            href: href::normalize(response.url.clone()).into(),
            etag: response.etag(),
            data: response.body,
        })
    }

    /// Uploads a calendar object or vCard with `PUT` and returns its new
    /// entity tag.
    ///
    /// The content type (`text/calendar` or `text/vcard`) is derived from the
    /// payload (see [`ObjectKind::detect`]). Use [`Precondition::IfNoneMatch`]
    /// when creating and [`Precondition::IfMatch`] when updating, so that
    /// concurrent changes surface as
    /// [`PreconditionFailed`](crate::DavError::PreconditionFailed) instead of
    /// being overwritten.
    ///
    /// When the server does not return an `ETag` header (it must not when it
    /// altered the payload while storing it), the tag is fetched with a
    /// `PROPFIND`; `None` means the server did not provide one at all.
    pub async fn put(
        &self,
        href: &str,
        body: &str,
        condition: Precondition,
    ) -> Result<Option<String>> {
        let url = self.resolve_input(href)?;
        let kind = ObjectKind::detect(body);
        let mut request =
            DavRequest::new(Method::PUT, url).body(kind.content_type(), body.to_owned());
        match &condition {
            Precondition::None => {}
            Precondition::IfMatch(etag) => {
                request = request.header(IF_MATCH, if_match_value(etag)?);
            }
            Precondition::IfNoneMatch => {
                request = request.header(IF_NONE_MATCH, HeaderValue::from_static("*"));
            }
        }
        let response = self.send(request).await?;
        if !response.status.is_success() {
            return Err(response.into_error());
        }
        if let Some(etag) = response.etag() {
            return Ok(Some(etag));
        }
        // The write succeeded; failing to learn the new tag must not turn it
        // into an error, or the caller would retry a write that happened.
        match self.fetch_etag(&response.url).await {
            Ok(etag) => Ok(etag),
            Err(err) => {
                warn!(url = %response.url, error = %err, "could not fetch the ETag after PUT");
                Ok(None)
            }
        }
    }

    /// Deletes a resource. With `if_match`, the deletion only happens if the
    /// resource still has that entity tag
    /// ([`PreconditionFailed`](crate::DavError::PreconditionFailed) otherwise).
    /// A missing resource yields [`NotFound`](crate::DavError::NotFound).
    pub async fn delete(&self, href: &str, if_match: Option<&str>) -> Result<()> {
        let url = self.resolve_input(href)?;
        let mut request = DavRequest::new(Method::DELETE, url);
        if let Some(etag) = if_match {
            request = request.header(IF_MATCH, if_match_value(etag)?);
        }
        let response = self.send(request).await?;
        if response.status == StatusCode::MULTI_STATUS {
            // RFC 4918 §9.6.1: deleting a collection answers 207 when some
            // members could not be deleted, listing them with their status.
            let reply = response.into_multistatus()?;
            let failure = reply.entries().into_iter().find_map(|(member, entry)| {
                entry
                    .status
                    .filter(|status| !is_success(*status))
                    .map(|status| (member, status))
            });
            return match failure {
                Some((member, status)) => {
                    Err(HttpResponse::member_error(&Method::DELETE, &member, status))
                }
                None => Ok(()),
            };
        }
        if response.status.is_success() {
            Ok(())
        } else {
            Err(response.into_error())
        }
    }

    /// Deletes a whole collection (calendar or address book) with everything
    /// in it.
    pub async fn delete_collection(&self, href: &str) -> Result<()> {
        self.delete(href, None).await
    }

    /// Current entity tag of a resource (`PROPFIND` depth 0 on `getetag`).
    pub async fn get_etag(&self, href: &str) -> Result<Option<String>> {
        let url = self.resolve_input(href)?;
        self.fetch_etag(&url).await
    }

    async fn fetch_etag(&self, url: &Url) -> Result<Option<String>> {
        let reply = self
            .propfind(
                url,
                Depth::Zero,
                xml::propfind_body(&[(Ns::Dav, "getetag")]),
            )
            .await?;
        Ok(reply
            .own_entry()
            .and_then(|(_, response)| response.prop_text(Ns::Dav, "getetag"))
            .and_then(|etag| normalize_etag(&etag)))
    }

    /// Fetches the given calendar objects of a calendar with
    /// `calendar-multiget` reports (RFC 4791 §7.9), [`MULTIGET_BATCH_SIZE`]
    /// hrefs at a time.
    pub async fn multiget<S>(&self, calendar: &str, hrefs: &[S]) -> Result<MultigetResult>
    where
        S: AsRef<str> + Sync,
    {
        self.multiget_impl(calendar, hrefs, ObjectKind::Calendar)
            .await
    }

    /// Fetches the given vCards of an address book with
    /// `addressbook-multiget` reports (RFC 6352 §8.7), [`MULTIGET_BATCH_SIZE`]
    /// hrefs at a time.
    pub async fn addressbook_multiget<S>(
        &self,
        addressbook: &str,
        hrefs: &[S],
    ) -> Result<MultigetResult>
    where
        S: AsRef<str> + Sync,
    {
        self.multiget_impl(addressbook, hrefs, ObjectKind::Contact)
            .await
    }

    async fn multiget_impl<S>(
        &self,
        collection: &str,
        hrefs: &[S],
        kind: ObjectKind,
    ) -> Result<MultigetResult>
    where
        S: AsRef<str> + Sync,
    {
        let collection_url = self.resolve_input(collection)?;
        let mut wanted: Vec<Url> = Vec::with_capacity(hrefs.len());
        let mut seen = HashSet::new();
        for raw in hrefs {
            let url = self.resolve_input(raw.as_ref())?;
            if seen.insert(url.as_str().to_owned()) {
                wanted.push(url);
            }
        }

        let mut result = MultigetResult::default();
        for batch in wanted.chunks(MULTIGET_BATCH_SIZE) {
            let body = multiget_body(kind, batch, &collection_url);
            let reply = self.report(&collection_url, Depth::One, body).await?;
            let (objects, mut missing) =
                objects_from_reply(&reply, &collection_url, data_prop(kind));
            // Hrefs the server silently skipped count as missing too.
            let returned: HashSet<&str> = objects
                .iter()
                .map(|o| o.href.as_str())
                .chain(missing.iter().map(String::as_str))
                .collect();
            let skipped: Vec<String> = batch
                .iter()
                .filter(|url| !returned.contains(url.as_str()))
                .map(|url| url.as_str().to_owned())
                .collect();
            missing.extend(skipped);
            debug!(
                collection = %collection_url,
                requested = batch.len(),
                found = objects.len(),
                missing = missing.len(),
                "multiget batch"
            );
            result.objects.extend(objects);
            result.missing.extend(missing);
        }
        Ok(result)
    }
}

/// The property carrying the payload for a kind of object.
pub(crate) fn data_prop(kind: ObjectKind) -> PropName {
    match kind {
        ObjectKind::Calendar => (Ns::CalDav, "calendar-data"),
        ObjectKind::Contact => (Ns::CardDav, "address-data"),
    }
}

fn multiget_body(kind: ObjectKind, hrefs: &[Url], collection: &Url) -> String {
    let (root, data) = match kind {
        ObjectKind::Calendar => ("C:calendar-multiget", "C:calendar-data"),
        ObjectKind::Contact => ("CR:addressbook-multiget", "CR:address-data"),
    };
    let mut body = format!(
        "{XML_DECLARATION}<{root} {REQUEST_NAMESPACES}><D:prop><D:getetag/><{data}/></D:prop>"
    );
    for url in hrefs {
        body.push_str("<D:href>");
        body.push_str(&xml::escape(&href::request_href(url, collection)));
        body.push_str("</D:href>");
    }
    body.push_str("</");
    body.push_str(root);
    body.push('>');
    body
}

/// Splits a multiget / calendar-query reply into found objects and missing
/// hrefs. The collection itself and sub-collections are ignored.
pub(crate) fn objects_from_reply(
    reply: &MultiStatusReply,
    collection: &Url,
    data: PropName,
) -> (Vec<DavObject>, Vec<String>) {
    let mut objects = Vec::new();
    let mut missing = Vec::new();
    for (url, response) in reply.entries() {
        if href::same_resource(&url, collection) {
            continue;
        }
        if response.is_not_found() {
            missing.push(url.into());
            continue;
        }
        match response.prop(data.0, data.1) {
            Some(element) => objects.push(DavObject {
                href: url.into(),
                etag: response
                    .prop_text(Ns::Dav, "getetag")
                    .and_then(|etag| normalize_etag(&etag)),
                data: payload(element),
            }),
            None if response.is_collection() || href::looks_like_collection(&url) => {}
            None => {
                if !response.all_propstats_missing() {
                    warn!(href = %url, "server returned no data for a requested resource");
                }
                missing.push(url.into());
            }
        }
    }
    (objects, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multistatus::parse_multistatus;
    use crate::xml::parse;

    #[test]
    fn multiget_bodies_use_paths_and_escape() {
        let collection = Url::parse("https://dav.example.com/cal/").unwrap();
        let hrefs = vec![
            Url::parse("https://dav.example.com/cal/a&b.ics").unwrap(),
            Url::parse("https://other.example.com/cal/c.ics").unwrap(),
        ];
        let body = multiget_body(ObjectKind::Calendar, &hrefs, &collection);
        let root = parse(&body).unwrap();
        assert!(root.is(Ns::CalDav, "calendar-multiget"));
        assert!(
            root.child(Ns::Dav, "prop")
                .unwrap()
                .has_child(Ns::CalDav, "calendar-data")
        );
        assert_eq!(
            root.hrefs(),
            ["/cal/a&b.ics", "https://other.example.com/cal/c.ics"]
        );

        let body = multiget_body(ObjectKind::Contact, &hrefs[..1], &collection);
        let root = parse(&body).unwrap();
        assert!(root.is(Ns::CardDav, "addressbook-multiget"));
        assert!(
            root.child(Ns::Dav, "prop")
                .unwrap()
                .has_child(Ns::CardDav, "address-data")
        );
    }

    #[test]
    fn objects_and_missing_entries() {
        let collection = Url::parse("https://dav.example.com/cal/").unwrap();
        let body = r#"<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
          <d:response><d:href>/cal/</d:href><d:propstat><d:prop><d:getetag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
          <d:response><d:href>/cal/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag><cal:calendar-data>BEGIN:VCALENDAR&#13;
END:VCALENDAR&#13;
</cal:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
          <d:response><d:href>/cal/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
          <d:response><d:href>/cal/nodata.ics</d:href><d:propstat><d:prop><d:getetag/><cal:calendar-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
        </d:multistatus>"#;
        let reply = MultiStatusReply {
            url: collection.clone(),
            multistatus: parse_multistatus(body).unwrap(),
        };
        let (objects, missing) =
            objects_from_reply(&reply, &collection, data_prop(ObjectKind::Calendar));
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].href, "https://dav.example.com/cal/a.ics");
        assert_eq!(objects[0].etag.as_deref(), Some("\"1\""));
        assert_eq!(objects[0].data, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n");
        assert_eq!(
            missing,
            [
                "https://dav.example.com/cal/gone.ics",
                "https://dav.example.com/cal/nodata.ics"
            ]
        );
    }
}
