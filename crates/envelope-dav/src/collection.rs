//! Operations shared by calendars and address books.

use reqwest::StatusCode;
use url::Url;

use crate::client::{DavClient, DavRequest, HttpResponse, dav_method};
use crate::error::{DavError, Result};
use crate::multistatus::is_success;
use crate::xml::{REQUEST_NAMESPACES, XML_DECLARATION};

/// URL for a new collection with a random name inside `home`.
pub(crate) fn collection_url_in(home: &Url) -> Result<Url> {
    let mut base = home.clone();
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base.join(&format!("{}/", uuid::Uuid::new_v4()))
        .map_err(|err| DavError::invalid_url(home.as_str(), err))
}

/// Accepts the response to `MKCALENDAR` / `MKCOL`.
pub(crate) fn expect_created(response: HttpResponse) -> Result<()> {
    if response.status.is_success() {
        Ok(())
    } else {
        Err(response.into_error())
    }
}

impl DavClient {
    /// Sets properties with `PROPPATCH`. `props` holds the serialised
    /// property elements (using the prefixes of `REQUEST_NAMESPACES`).
    pub(crate) async fn proppatch(&self, url: &Url, props: &str) -> Result<()> {
        let body = format!(
            "{XML_DECLARATION}<D:propertyupdate {REQUEST_NAMESPACES}><D:set><D:prop>{props}</D:prop></D:set></D:propertyupdate>"
        );
        let response = self
            .send(DavRequest::new(dav_method("PROPPATCH")?, url.clone()).xml(body))
            .await?;
        let is_multistatus = response.status == StatusCode::MULTI_STATUS
            || (response.status == StatusCode::OK && response.body.contains("multistatus"));
        if !is_multistatus {
            return if response.status.is_success() {
                Ok(())
            } else {
                Err(response.into_error())
            };
        }
        let reply = response.into_multistatus()?;
        // Report the property that actually failed rather than the ones
        // rejected with 424 (Failed Dependency) because of it.
        let mut failures: Vec<(String, u16)> = reply
            .multistatus
            .responses
            .iter()
            .flat_map(|response| response.propstats.iter())
            .filter(|propstat| !is_success(propstat.status))
            .flat_map(|propstat| {
                propstat
                    .props
                    .iter()
                    .map(move |prop| (prop.name.clone(), propstat.status))
            })
            .collect();
        failures.sort_by_key(|(_, status)| *status == 424);
        match failures.into_iter().next() {
            Some((property, status)) => Err(DavError::PropertyUpdate { property, status }),
            None => Ok(()),
        }
    }

    /// Creates a collection with an extended `MKCOL` (RFC 5689) carrying the
    /// given resource type and properties.
    pub(crate) async fn mkcol_extended(
        &self,
        url: &Url,
        resource_type: &str,
        props: &str,
    ) -> Result<()> {
        let body = format!(
            "{XML_DECLARATION}<D:mkcol {REQUEST_NAMESPACES}><D:set><D:prop><D:resourcetype><D:collection/>{resource_type}</D:resourcetype>{props}</D:prop></D:set></D:mkcol>"
        );
        let response = self
            .send(DavRequest::new(dav_method("MKCOL")?, url.clone()).xml(body))
            .await?;
        expect_created(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_collections_are_children_of_the_home() {
        let home = Url::parse("https://dav.example.com/calendars/jane").unwrap();
        let url = collection_url_in(&home).unwrap();
        let path = url.path();
        assert!(path.starts_with("/calendars/jane/"), "{path}");
        assert!(path.ends_with('/'));
        assert_eq!(path.len(), "/calendars/jane/".len() + 36 + 1);
        assert_ne!(collection_url_in(&home).unwrap(), url);
    }
}
