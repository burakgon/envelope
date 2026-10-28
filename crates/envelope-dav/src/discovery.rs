//! Account discovery: RFC 6764 well-known URIs, the RFC 5397
//! `current-user-principal`, and the CalDAV / CardDAV home sets of the
//! principal (RFC 4791 §6.2.1, RFC 6352 §7.1.1).
//!
//! Starting from whatever the user typed (a host, or a URL with or without
//! a path), the client probes in order:
//!
//! 1. the URL itself, when it has a path (`https://cloud.example.com/remote.php/dav`);
//! 2. `/.well-known/caldav` or `/.well-known/carddav` on that host, following
//!    redirects while keeping the `PROPFIND` method and body;
//! 3. the root of the host;
//! 4. the usual paths of common self-hosted servers (see
//!    [`SELF_HOSTED_SERVERS`](crate::SELF_HOSTED_SERVERS)).
//!
//! The first `current-user-principal` found wins. The principal is then
//! asked for its home sets, which may live on another host (iCloud answers
//! `caldav.icloud.com` requests with `pNN-caldav.icloud.com` homes), and for
//! the user's `calendar-user-address-set`. DNS SRV/TXT records (RFC 6764 §3)
//! are not consulted.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use crate::client::{DavClient, Depth};
use crate::error::{DavError, Result};
use crate::href;
use crate::multistatus::Response;
use crate::providers::SELF_HOSTED_SERVERS;
use crate::xml::{Element, Ns, PropName, propfind_body};

/// The two DAV services Envelope talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceType {
    /// Calendars (RFC 4791).
    CalDav,
    /// Address books (RFC 6352).
    CardDav,
}

impl ServiceType {
    /// The RFC 6764 well-known path of the service.
    pub fn well_known_path(self) -> &'static str {
        match self {
            ServiceType::CalDav => "/.well-known/caldav",
            ServiceType::CardDav => "/.well-known/carddav",
        }
    }
}

impl fmt::Display for ServiceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ServiceType::CalDav => "CalDAV",
            ServiceType::CardDav => "CardDAV",
        })
    }
}

/// What discovery found out about the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    /// The service that was looked for.
    pub service: ServiceType,
    /// URL at which the principal was found (after redirects).
    pub context_url: String,
    /// The user's principal URL.
    pub principal: String,
    /// Collections containing the user's calendars. Non-empty when
    /// `service` is [`ServiceType::CalDav`].
    pub calendar_home_set: Vec<String>,
    /// Collections containing the user's address books. Non-empty when
    /// `service` is [`ServiceType::CardDav`]; servers hosting both services on
    /// one principal report both home sets at once.
    pub addressbook_home_set: Vec<String>,
    /// E-mail addresses of the user (`calendar-user-address-set` `mailto:`
    /// entries, preferred address first). Used to recognise the user among
    /// event attendees.
    pub calendar_user_addresses: Vec<String>,
    /// Display name of the principal, when the server provides one.
    pub display_name: Option<String>,
}

impl Discovery {
    /// The home set of the discovered service.
    pub fn home_set(&self) -> &[String] {
        match self.service {
            ServiceType::CalDav => &self.calendar_home_set,
            ServiceType::CardDav => &self.addressbook_home_set,
        }
    }
}

const PRINCIPAL_PROBE: &[PropName] = &[
    (Ns::Dav, "current-user-principal"),
    (Ns::Dav, "principal-URL"),
    (Ns::Dav, "resourcetype"),
    (Ns::CalDav, "calendar-home-set"),
    (Ns::CardDav, "addressbook-home-set"),
];

const PRINCIPAL_PROPS: &[PropName] = &[
    (Ns::CalDav, "calendar-home-set"),
    (Ns::CardDav, "addressbook-home-set"),
    (Ns::CalDav, "calendar-user-address-set"),
    (Ns::Dav, "displayname"),
    (Ns::CalendarServer, "email-address-set"),
];

impl DavClient {
    /// Discovers the principal and home set of `service`, starting from the
    /// client's base URL.
    ///
    /// Fails with [`DavError::ServiceNotFound`] when no candidate URL yields
    /// a principal with a home set for the service, with
    /// [`DavError::Unauthorized`] when the credentials are rejected, and with
    /// a network error when the server cannot be reached.
    pub async fn discover(&self, service: ServiceType) -> Result<Discovery> {
        let start = self.base_url().clone();
        self.discover_at(start, service).await
    }

    /// Like [`discover`](DavClient::discover), but starting from another URL
    /// or host, for providers serving calendars and contacts from different
    /// hosts (for example `https://contacts.icloud.com` for iCloud CardDAV).
    pub async fn discover_from(&self, start: &str, service: ServiceType) -> Result<Discovery> {
        let start = href::parse_user_url(start)?;
        self.discover_at(start, service).await
    }

    async fn discover_at(&self, start: Url, service: ServiceType) -> Result<Discovery> {
        let mut probed_principals: Vec<Url> = Vec::new();
        for candidate in candidate_urls(&start, service) {
            debug!(url = %candidate, %service, "looking for the current user principal");
            let found = match self.find_principal(&candidate).await {
                Ok(found) => found,
                Err(err) if aborts_discovery(&err) => return Err(err),
                Err(err) => {
                    debug!(url = %candidate, error = %err, "no principal here");
                    continue;
                }
            };
            let Some((context, principal)) = found else {
                continue;
            };
            if probed_principals.contains(&principal) {
                continue;
            }
            probed_principals.push(principal.clone());
            match self.principal_details(&principal, service).await {
                Ok(mut discovery) if !discovery.home_set().is_empty() => {
                    discovery.context_url = context.into();
                    return Ok(discovery);
                }
                Ok(_) => {
                    debug!(principal = %principal, %service, "principal has no home set for the service");
                }
                Err(err) if aborts_discovery(&err) => return Err(err),
                Err(err) => {
                    debug!(principal = %principal, error = %err, "could not read the principal");
                }
            }
        }
        Err(DavError::ServiceNotFound {
            service,
            url: start.into(),
        })
    }

    /// Asks `url` for the current user principal. Returns the URL that
    /// answered (after redirects) and the principal URL.
    async fn find_principal(&self, url: &Url) -> Result<Option<(Url, Url)>> {
        let reply = self
            .propfind(url, Depth::Zero, propfind_body(PRINCIPAL_PROBE))
            .await?;
        let context = href::normalize(reply.url.clone());
        let mut entries = reply.entries();
        // Look at the entry for the requested URL first.
        entries.sort_by_key(|(entry_url, _)| !href::same_resource(entry_url, &reply.url));

        for (entry_url, response) in entries {
            if let Some(cup) = response.prop(Ns::Dav, "current-user-principal") {
                if let Some(raw) = cup.href() {
                    return Ok(Some((context, href::resolve(&reply.url, &raw)?)));
                }
                if cup.has_child(Ns::Dav, "unauthenticated") {
                    // The server did not accept (or did not get) credentials.
                    return Err(DavError::Unauthorized);
                }
            }
            if let Some(raw) = response
                .prop(Ns::Dav, "principal-URL")
                .and_then(Element::href)
            {
                return Ok(Some((context, href::resolve(&reply.url, &raw)?)));
            }
            // Servers without RFC 5397 support: the resource itself carries
            // the home sets, so treat it as the principal.
            if response.prop(Ns::CalDav, "calendar-home-set").is_some()
                || response.prop(Ns::CardDav, "addressbook-home-set").is_some()
            {
                return Ok(Some((context, entry_url)));
            }
        }
        Ok(None)
    }

    /// Reads home sets, addresses and display name from the principal.
    async fn principal_details(&self, principal: &Url, service: ServiceType) -> Result<Discovery> {
        let reply = self
            .propfind(principal, Depth::Zero, propfind_body(PRINCIPAL_PROPS))
            .await?;
        let (_, response) = reply.own_entry().ok_or_else(|| {
            DavError::InvalidResponse("the principal did not describe itself".into())
        })?;
        let homes = |ns: Ns, name: &str| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for raw in response
                .prop(ns, name)
                .map(Element::hrefs)
                .unwrap_or_default()
            {
                match href::resolve(&reply.url, &raw) {
                    Ok(url) => {
                        let url = String::from(url);
                        if !out.contains(&url) {
                            out.push(url);
                        }
                    }
                    Err(err) => {
                        debug!(href = %raw, error = %err, "ignoring unusable home set href")
                    }
                }
            }
            out
        };
        Ok(Discovery {
            service,
            context_url: String::new(),
            principal: href::normalize(reply.url.clone()).into(),
            calendar_home_set: homes(Ns::CalDav, "calendar-home-set"),
            addressbook_home_set: homes(Ns::CardDav, "addressbook-home-set"),
            calendar_user_addresses: user_addresses(response),
            display_name: response.prop_text(Ns::Dav, "displayname"),
        })
    }
}

/// Errors after which probing further candidate URLs is pointless.
fn aborts_discovery(err: &DavError) -> bool {
    matches!(
        err,
        DavError::Unauthorized
            | DavError::TokenSource(_)
            | DavError::Network(_)
            | DavError::Timeout(_)
            | DavError::RateLimited { .. }
    )
}

/// URLs probed for a principal, in order, without duplicates.
fn candidate_urls(start: &Url, service: ServiceType) -> Vec<Url> {
    let mut candidates: Vec<Url> = Vec::new();
    let mut push = |url: Url| {
        let url = href::normalize(url);
        if !candidates.contains(&url) {
            candidates.push(url);
        }
    };
    if start.path() != "/" {
        push(start.clone());
    }
    if let Ok(url) = start.join(service.well_known_path()) {
        push(url);
    }
    if let Ok(url) = start.join("/") {
        push(url);
    }
    for server in SELF_HOSTED_SERVERS {
        let path = match service {
            ServiceType::CalDav => server.caldav_path,
            ServiceType::CardDav => server.carddav_path,
        };
        if let Ok(url) = start.join(path) {
            push(url);
        }
    }
    candidates
}

/// E-mail addresses from `calendar-user-address-set` (preferred first) and
/// Apple's `email-address-set`.
fn user_addresses(response: &Response) -> Vec<String> {
    let mut preferred = Vec::new();
    let mut others = Vec::new();
    if let Some(set) = response.prop(Ns::CalDav, "calendar-user-address-set") {
        for href_element in set.children_named(Ns::Dav, "href") {
            let Some(address) = href_element
                .trimmed_text()
                .as_deref()
                .and_then(mailto_address)
            else {
                continue;
            };
            let is_preferred = href_element
                .attr("preferred")
                .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
            if is_preferred {
                preferred.push(address);
            } else {
                others.push(address);
            }
        }
    }
    if let Some(set) = response.prop(Ns::CalendarServer, "email-address-set") {
        others.extend(
            set.children_named(Ns::CalendarServer, "email-address")
                .filter_map(Element::trimmed_text)
                .filter(|address| address.contains('@')),
        );
    }
    let mut addresses: Vec<String> = Vec::new();
    for address in preferred.into_iter().chain(others) {
        if !addresses
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&address))
        {
            addresses.push(address);
        }
    }
    addresses
}

/// The address of a `mailto:` URI, if `uri` is one.
fn mailto_address(uri: &str) -> Option<String> {
    let scheme = uri.get(..7)?;
    if !scheme.eq_ignore_ascii_case("mailto:") {
        return None;
    }
    let rest = uri.get(7..)?;
    let rest = rest.split('?').next().unwrap_or(rest);
    let decoded = percent_decode_str(rest).decode_utf8().ok()?;
    let address = decoded.trim().trim_start_matches("//");
    address.contains('@').then(|| address.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multistatus::parse_multistatus;

    #[test]
    fn candidates_for_a_bare_host() {
        let start = href::parse_user_url("dav.example.com").unwrap();
        let candidates: Vec<String> = candidate_urls(&start, ServiceType::CalDav)
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(candidates[0], "https://dav.example.com/.well-known/caldav");
        assert_eq!(candidates[1], "https://dav.example.com/");
        assert!(candidates.contains(&"https://dav.example.com/remote.php/dav/".to_owned()));
        assert!(candidates.contains(&"https://dav.example.com/dav.php/".to_owned()));
        // No duplicates.
        let mut unique = candidates.clone();
        unique.dedup();
        assert_eq!(unique.len(), candidates.len());
    }

    #[test]
    fn candidates_start_with_an_explicit_path() {
        let start = href::parse_user_url("https://cloud.example.com/remote.php/dav").unwrap();
        let candidates = candidate_urls(&start, ServiceType::CardDav);
        assert_eq!(
            candidates[0].as_str(),
            "https://cloud.example.com/remote.php/dav"
        );
        assert_eq!(
            candidates[1].as_str(),
            "https://cloud.example.com/.well-known/carddav"
        );
        assert_eq!(candidates[2].as_str(), "https://cloud.example.com/");
    }

    #[test]
    fn mailto_parsing() {
        assert_eq!(
            mailto_address("mailto:jane@example.com").as_deref(),
            Some("jane@example.com")
        );
        assert_eq!(
            mailto_address("MAILTO:Jane%40Example.com").as_deref(),
            Some("Jane@Example.com")
        );
        assert_eq!(
            mailto_address("mailto:jane@example.com?subject=x").as_deref(),
            Some("jane@example.com")
        );
        assert_eq!(mailto_address("urn:uuid:4f3c"), None);
        assert_eq!(mailto_address("/principals/jane/"), None);
        assert_eq!(mailto_address("mailto:"), None);
        assert_eq!(mailto_address("mail"), None);
    }

    #[test]
    fn user_addresses_put_preferred_first_and_deduplicate() {
        let ms = parse_multistatus(
            r#"<multistatus xmlns="DAV:"><response><href>/p/</href><propstat><prop>
                <calendar-user-address-set xmlns="urn:ietf:params:xml:ns:caldav">
                  <href xmlns="DAV:">mailto:jane.appleseed@icloud.com</href>
                  <href xmlns="DAV:" preferred="1">mailto:jane@example.com</href>
                  <href xmlns="DAV:">urn:uuid:10000000-0000-0000-0000-000000000001</href>
                  <href xmlns="DAV:">/1234567890/principal/</href>
                </calendar-user-address-set>
                <email-address-set xmlns="http://calendarserver.org/ns/">
                  <email-address>JANE@example.com</email-address>
                  <email-address>jane@me.com</email-address>
                </email-address-set>
              </prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#,
        )
        .unwrap();
        assert_eq!(
            user_addresses(&ms.responses[0]),
            [
                "jane@example.com",
                "jane.appleseed@icloud.com",
                "jane@me.com"
            ]
        );
    }

    #[test]
    fn service_type_basics() {
        assert_eq!(ServiceType::CalDav.to_string(), "CalDAV");
        assert_eq!(
            ServiceType::CardDav.well_known_path(),
            "/.well-known/carddav"
        );
    }
}
