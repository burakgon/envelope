//! `DAV:multistatus` (RFC 4918 §13) and `DAV:error` (§16) bodies.

use crate::error::DavError;
use crate::xml::{self, Element, Ns};

/// A parsed `207 Multi-Status` body.
#[derive(Debug, Clone, Default)]
pub(crate) struct MultiStatus {
    pub(crate) responses: Vec<Response>,
    /// Top-level `DAV:sync-token` (RFC 6578 sync-collection reports).
    pub(crate) sync_token: Option<String>,
}

/// One `DAV:response` element.
#[derive(Debug, Clone, Default)]
pub(crate) struct Response {
    /// Raw `DAV:href` values (usually exactly one).
    pub(crate) hrefs: Vec<String>,
    /// Response-level `DAV:status`, used for whole-resource results such as
    /// deleted members in a sync report or missing hrefs in a multiget.
    pub(crate) status: Option<u16>,
    pub(crate) propstats: Vec<PropStat>,
    /// `DAV:error` element attached to the response, if any.
    pub(crate) error: Option<Element>,
}

/// One `DAV:propstat` element: a group of properties sharing a status.
#[derive(Debug, Clone)]
pub(crate) struct PropStat {
    pub(crate) status: u16,
    /// The children of `DAV:prop`.
    pub(crate) props: Vec<Element>,
}

impl Response {
    /// First href of the response.
    pub(crate) fn href(&self) -> Option<&str> {
        self.hrefs.first().map(String::as_str)
    }

    /// A property reported with a 2xx status.
    pub(crate) fn prop(&self, ns: Ns, name: &str) -> Option<&Element> {
        self.propstats
            .iter()
            .filter(|ps| is_success(ps.status))
            .flat_map(|ps| ps.props.iter())
            .find(|prop| prop.is(ns, name))
    }

    /// Trimmed text of a property reported with a 2xx status; `None` when the
    /// property is missing or empty.
    pub(crate) fn prop_text(&self, ns: Ns, name: &str) -> Option<String> {
        self.prop(ns, name).and_then(Element::trimmed_text)
    }

    /// True when the response reports a 404 for the resource itself.
    pub(crate) fn is_not_found(&self) -> bool {
        matches!(self.status, Some(404 | 410))
    }

    /// True when at least one propstat has a 2xx status.
    pub(crate) fn has_successful_propstat(&self) -> bool {
        self.propstats.iter().any(|ps| is_success(ps.status))
    }

    /// True when every propstat reports 404 (no properties found at all).
    pub(crate) fn all_propstats_missing(&self) -> bool {
        !self.propstats.is_empty() && self.propstats.iter().all(|ps| ps.status == 404)
    }

    /// True when `DAV:resourcetype` contains the given element.
    pub(crate) fn has_resource_type(&self, ns: Ns, name: &str) -> bool {
        self.prop(Ns::Dav, "resourcetype")
            .is_some_and(|rt| rt.has_child(ns, name))
    }

    /// True when the resource is a collection.
    pub(crate) fn is_collection(&self) -> bool {
        self.has_resource_type(Ns::Dav, "collection")
    }
}

pub(crate) fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Parses a `DAV:multistatus` document.
pub(crate) fn parse_multistatus(body: &str) -> Result<MultiStatus, DavError> {
    let root = xml::parse(body)?;
    if !root.is(Ns::Dav, "multistatus") {
        return Err(DavError::InvalidResponse(format!(
            "expected a DAV:multistatus document, got <{}>",
            root.name
        )));
    }

    let mut multistatus = MultiStatus {
        responses: Vec::new(),
        sync_token: root
            .child(Ns::Dav, "sync-token")
            .and_then(Element::trimmed_text),
    };

    for response in root.children_named(Ns::Dav, "response") {
        let mut parsed = Response {
            hrefs: response.hrefs(),
            status: response
                .child(Ns::Dav, "status")
                .and_then(|status| parse_status_line(&status.text())),
            propstats: Vec::new(),
            error: response.child(Ns::Dav, "error").cloned(),
        };
        for propstat in response.children_named(Ns::Dav, "propstat") {
            // A propstat without a parsable status is treated as successful:
            // the properties are there, the server just forgot to say so.
            let status = propstat
                .child(Ns::Dav, "status")
                .and_then(|status| parse_status_line(&status.text()))
                .unwrap_or(200);
            let props = propstat
                .child(Ns::Dav, "prop")
                .map(|prop| prop.elements().cloned().collect())
                .unwrap_or_default();
            parsed.propstats.push(PropStat { status, props });
        }
        if parsed.hrefs.is_empty() {
            return Err(DavError::InvalidResponse(
                "multistatus response without DAV:href".into(),
            ));
        }
        multistatus.responses.push(parsed);
    }

    Ok(multistatus)
}

/// Extracts the status code from an HTTP status line such as
/// `HTTP/1.1 404 Not Found`. Tolerates missing reason phrases, other HTTP
/// versions and a bare status code.
pub(crate) fn parse_status_line(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    let code = if first.to_ascii_uppercase().starts_with("HTTP/") {
        parts.next()?
    } else {
        first
    };
    if code.len() != 3 {
        return None;
    }
    code.parse::<u16>()
        .ok()
        .filter(|status| (100..600).contains(status))
}

/// Precondition / postcondition reported in a `DAV:error` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ErrorCondition {
    pub(crate) ns: Ns,
    pub(crate) name: String,
}

/// Parses a `DAV:error` response body and returns the first condition element.
/// Returns `None` for bodies that are not a DAV error document.
pub(crate) fn parse_error_condition(body: &str) -> Option<ErrorCondition> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with('<') {
        return None;
    }
    let root = xml::parse(trimmed).ok()?;
    if !root.is(Ns::Dav, "error") {
        return None;
    }
    root.elements().next().map(|el| ErrorCondition {
        ns: el.ns,
        name: el.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(parse_status_line("  HTTP/1.1 404 Not Found "), Some(404));
        assert_eq!(parse_status_line("HTTP/2 507"), Some(507));
        assert_eq!(parse_status_line("http/1.0 403 Forbidden"), Some(403));
        assert_eq!(parse_status_line("201"), Some(201));
        assert_eq!(parse_status_line("HTTP/1.1"), None);
        assert_eq!(parse_status_line("HTTP/1.1 2000 OK"), None);
        assert_eq!(parse_status_line("HTTP/1.1 abc"), None);
        assert_eq!(parse_status_line(""), None);
    }

    #[test]
    fn multiple_propstats_and_response_status() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/">
  <d:response>
    <d:href>/cal/</d:href>
    <d:propstat>
      <d:prop><d:displayname>Home</d:displayname><cs:getctag>"42"</cs:getctag></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
    <d:propstat>
      <d:prop><d:sync-token/><d:displayname>shadowed</d:displayname></d:prop>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/cal/gone.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
  <d:sync-token>http://example.com/sync/7</d:sync-token>
</d:multistatus>"#;
        let ms = parse_multistatus(body).expect("parse");
        assert_eq!(ms.sync_token.as_deref(), Some("http://example.com/sync/7"));
        assert_eq!(ms.responses.len(), 2);
        let first = &ms.responses[0];
        assert_eq!(first.href(), Some("/cal/"));
        assert_eq!(
            first.prop_text(Ns::Dav, "displayname").as_deref(),
            Some("Home")
        );
        assert_eq!(
            first.prop_text(Ns::CalendarServer, "getctag").as_deref(),
            Some("\"42\"")
        );
        // Properties only listed in a 404 propstat are absent.
        assert!(first.prop(Ns::Dav, "sync-token").is_none());
        assert!(!first.is_not_found());
        assert!(first.has_successful_propstat());
        let second = &ms.responses[1];
        assert!(second.is_not_found());
        assert!(second.propstats.is_empty());
    }

    #[test]
    fn rejects_non_multistatus_and_missing_hrefs() {
        assert!(matches!(
            parse_multistatus("<html><body>Login</body></html>"),
            Err(DavError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_multistatus(
                r#"<multistatus xmlns="DAV:"><response><status>HTTP/1.1 200 OK</status></response></multistatus>"#
            ),
            Err(DavError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_multistatus("not xml at all <"),
            Err(DavError::Xml(_))
        ));
    }

    #[test]
    fn error_conditions() {
        let condition = parse_error_condition(
            r#"<?xml version="1.0"?><D:error xmlns:D="DAV:"><D:valid-sync-token/></D:error>"#,
        )
        .expect("condition");
        assert_eq!(condition.ns, Ns::Dav);
        assert_eq!(condition.name, "valid-sync-token");

        let condition = parse_error_condition(
            r#"<d:error xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:cal="urn:ietf:params:xml:ns:caldav">
                 <cal:no-uid-conflict><d:href>/cal/other.ics</d:href></cal:no-uid-conflict>
                 <s:message>UID already in use</s:message>
               </d:error>"#,
        )
        .expect("condition");
        assert_eq!(condition.ns, Ns::CalDav);
        assert_eq!(condition.name, "no-uid-conflict");

        assert_eq!(parse_error_condition("Forbidden"), None);
        assert_eq!(parse_error_condition("<html/>"), None);
        assert_eq!(parse_error_condition(""), None);
    }
}
