//! A small namespace-aware XML tree used to read WebDAV responses, plus the
//! helpers used to write request bodies.
//!
//! WebDAV servers are wildly inconsistent in how they serialise XML: some use
//! a default namespace (`<multistatus xmlns="DAV:">`), others prefixes
//! (`<d:multistatus xmlns:d="DAV:">`), and prefixes are frequently redeclared
//! or bound to different namespaces in nested scopes. Every element is
//! therefore resolved to its expanded name (namespace URI + local name) while
//! parsing, and all matching is done on expanded names only.
//!
//! Text content (including CDATA sections and character / entity references)
//! is preserved verbatim, line endings included, so iCalendar and vCard
//! payloads embedded in `calendar-data` / `address-data` keep their CRLFs.

use quick_xml::NsReader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;

use crate::error::DavError;

/// `DAV:` namespace (RFC 4918).
pub(crate) const NS_DAV: &str = "DAV:";
/// CalDAV namespace (RFC 4791).
pub(crate) const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
/// CardDAV namespace (RFC 6352).
pub(crate) const NS_CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
/// Apple CalendarServer extensions (`getctag`, ...).
pub(crate) const NS_CALENDARSERVER: &str = "http://calendarserver.org/ns/";
/// Apple iCal extensions (`calendar-color`, `calendar-order`).
pub(crate) const NS_APPLE_ICAL: &str = "http://apple.com/ns/ical/";

/// Maximum element nesting accepted in a response; real responses stay far
/// below this, it only guards against hostile input.
const MAX_DEPTH: usize = 256;

/// The namespaces this crate cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ns {
    /// `DAV:`
    Dav,
    /// `urn:ietf:params:xml:ns:caldav`
    CalDav,
    /// `urn:ietf:params:xml:ns:carddav`
    CardDav,
    /// `http://calendarserver.org/ns/`
    CalendarServer,
    /// `http://apple.com/ns/ical/`
    AppleIcal,
    /// Any other namespace.
    Other,
    /// The element is not in any namespace (or its prefix was never
    /// declared and could not be guessed). Such elements match queries for
    /// any namespace by local name, which keeps broken servers usable.
    Unbound,
}

impl Ns {
    fn from_uri(uri: &str) -> Ns {
        let trimmed = uri.trim();
        let loose = trimmed.trim_end_matches('/');
        if trimmed.eq_ignore_ascii_case(NS_DAV) {
            Ns::Dav
        } else if trimmed.eq_ignore_ascii_case(NS_CALDAV) {
            Ns::CalDav
        } else if trimmed.eq_ignore_ascii_case(NS_CARDDAV) {
            Ns::CardDav
        } else if loose.eq_ignore_ascii_case(NS_CALENDARSERVER.trim_end_matches('/')) {
            Ns::CalendarServer
        } else if loose.eq_ignore_ascii_case(NS_APPLE_ICAL.trim_end_matches('/')) {
            Ns::AppleIcal
        } else if trimmed.is_empty() {
            Ns::Unbound
        } else {
            Ns::Other
        }
    }

    /// Best-effort guess for a prefix that was used without being declared.
    fn from_undeclared_prefix(prefix: &str) -> Ns {
        match prefix {
            "d" | "D" | "dav" | "DAV" => Ns::Dav,
            "c" | "C" | "cal" | "caldav" | "CAL" => Ns::CalDav,
            "card" | "carddav" | "cr" | "CR" | "CARD" => Ns::CardDav,
            "cs" | "CS" => Ns::CalendarServer,
            "ical" | "ICAL" | "apple" => Ns::AppleIcal,
            _ => Ns::Unbound,
        }
    }
}

/// A node of the parsed tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Element(Element),
    Text(String),
}

/// An XML element with its namespace resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Element {
    pub(crate) ns: Ns,
    pub(crate) name: String,
    /// Attributes by local name (namespace declarations excluded).
    pub(crate) attributes: Vec<(String, String)>,
    pub(crate) children: Vec<Node>,
}

impl Element {
    /// True when the element has the given expanded name.
    pub(crate) fn is(&self, ns: Ns, name: &str) -> bool {
        self.name == name && (self.ns == ns || self.ns == Ns::Unbound)
    }

    /// Child elements, in document order.
    pub(crate) fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(el) => Some(el),
            Node::Text(_) => None,
        })
    }

    /// First child element with the given expanded name.
    pub(crate) fn child(&self, ns: Ns, name: &str) -> Option<&Element> {
        self.elements().find(|el| el.is(ns, name))
    }

    /// All child elements with the given expanded name.
    pub(crate) fn children_named<'a>(
        &'a self,
        ns: Ns,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Element> + 'a {
        self.elements().filter(move |el| el.is(ns, name))
    }

    /// True when a direct child has the given expanded name.
    pub(crate) fn has_child(&self, ns: Ns, name: &str) -> bool {
        self.child(ns, name).is_some()
    }

    /// First descendant (depth-first, excluding `self`) with the given name.
    pub(crate) fn descendant(&self, ns: Ns, name: &str) -> Option<&Element> {
        for el in self.elements() {
            if el.is(ns, name) {
                return Some(el);
            }
            if let Some(found) = el.descendant(ns, name) {
                return Some(found);
            }
        }
        None
    }

    /// Concatenation of all descendant text, verbatim.
    pub(crate) fn text(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        for node in &self.children {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Element(el) => el.collect_text(out),
            }
        }
    }

    /// Descendant text with surrounding whitespace removed; `None` when empty.
    pub(crate) fn trimmed_text(&self) -> Option<String> {
        let text = self.text();
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    /// Value of the attribute with the given local name.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Text of every `DAV:href` child, in document order.
    pub(crate) fn hrefs(&self) -> Vec<String> {
        self.children_named(Ns::Dav, "href")
            .filter_map(Element::trimmed_text)
            .collect()
    }

    /// Text of the first `DAV:href` child.
    pub(crate) fn href(&self) -> Option<String> {
        self.child(Ns::Dav, "href").and_then(Element::trimmed_text)
    }
}

/// Parses a complete XML document into a tree and returns its root element.
pub(crate) fn parse(xml: &str) -> Result<Element, DavError> {
    // A UTF-8 byte order mark is legal before the XML declaration.
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let mut reader = NsReader::from_str(xml);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;

    loop {
        let position = reader.buffer_position();
        let (resolved, event) = reader
            .read_resolved_event()
            .map_err(|err| DavError::Xml(format!("{err} (near byte {position})")))?;
        match event {
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(DavError::Xml("document is nested too deeply".into()));
                }
                let element = new_element(resolved, &start)?;
                stack.push(element);
            }
            Event::Empty(start) => {
                let element = new_element(resolved, &start)?;
                attach(&mut stack, &mut root, element)?;
            }
            Event::End(_) => {
                let element = stack
                    .pop()
                    .ok_or_else(|| DavError::Xml("unexpected closing tag".into()))?;
                attach(&mut stack, &mut root, element)?;
            }
            Event::Text(text) => push_text(&mut stack, &text),
            Event::CData(cdata) => push_text(&mut stack, &cdata),
            Event::GeneralRef(reference) => {
                let resolved_char = reference
                    .resolve_char_ref()
                    .map_err(|err| DavError::Xml(format!("invalid character reference: {err}")))?;
                match resolved_char {
                    Some(ch) => push_text(&mut stack, ch.encode_utf8(&mut [0u8; 4])),
                    None => match resolve_predefined_entity(&reference) {
                        Some(value) => push_text(&mut stack, value),
                        // Unknown entity (no DTD support): keep it literally.
                        None => push_text(&mut stack, &format!("&{};", &*reference)),
                    },
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
        }
    }

    if !stack.is_empty() {
        return Err(DavError::Xml(
            "document ended before all elements were closed".into(),
        ));
    }
    root.ok_or_else(|| DavError::Xml("document has no root element".into()))
}

fn new_element(resolved: ResolveResult<'_>, start: &BytesStart<'_>) -> Result<Element, DavError> {
    let qname = start.name();
    let (local, prefix) = qname.decompose();
    let ns = match resolved {
        ResolveResult::Bound(namespace) => Ns::from_uri(namespace.into_inner()),
        ResolveResult::Unbound => Ns::Unbound,
        ResolveResult::Unknown(_) => prefix
            .map(|p| Ns::from_undeclared_prefix(p.into_inner()))
            .unwrap_or(Ns::Unbound),
    };

    let mut attributes = Vec::new();
    for attr in start.attributes().with_checks(false) {
        let attr = attr.map_err(|err| DavError::Xml(format!("invalid attribute: {err}")))?;
        if attr.key.as_namespace_binding().is_some() {
            continue;
        }
        let key = attr.key.local_name().into_inner().to_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| DavError::Xml(format!("invalid attribute value: {err}")))?;
        attributes.push((key, value.into_owned()));
    }

    Ok(Element {
        ns,
        name: local.into_inner().to_owned(),
        attributes,
        children: Vec::new(),
    })
}

fn attach(
    stack: &mut [Element],
    root: &mut Option<Element>,
    element: Element,
) -> Result<(), DavError> {
    match stack.last_mut() {
        Some(parent) => {
            parent.children.push(Node::Element(element));
            Ok(())
        }
        None if root.is_none() => {
            *root = Some(element);
            Ok(())
        }
        None => Err(DavError::Xml(
            "document has more than one root element".into(),
        )),
    }
}

fn push_text(stack: &mut [Element], text: &str) {
    if text.is_empty() {
        return;
    }
    // Text outside the root element (whitespace) is irrelevant.
    let Some(parent) = stack.last_mut() else {
        return;
    };
    if let Some(Node::Text(existing)) = parent.children.last_mut() {
        existing.push_str(text);
    } else {
        parent.children.push(Node::Text(text.to_owned()));
    }
}

/// Escapes text for use in element content or a double-quoted attribute.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // A literal CR would be normalised away by the receiving parser.
            '\r' => out.push_str("&#13;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Namespace declarations used on the root element of every request body.
pub(crate) const REQUEST_NAMESPACES: &str = concat!(
    r#"xmlns:D="DAV:" "#,
    r#"xmlns:C="urn:ietf:params:xml:ns:caldav" "#,
    r#"xmlns:CR="urn:ietf:params:xml:ns:carddav" "#,
    r#"xmlns:CS="http://calendarserver.org/ns/" "#,
    r#"xmlns:I="http://apple.com/ns/ical/""#
);

/// XML declaration prepended to every request body.
pub(crate) const XML_DECLARATION: &str = r#"<?xml version="1.0" encoding="utf-8"?>"#;

/// A property name used in request bodies.
pub(crate) type PropName = (Ns, &'static str);

/// Prefix bound to a namespace by [`REQUEST_NAMESPACES`].
pub(crate) fn prefix(ns: Ns) -> &'static str {
    match ns {
        Ns::CalDav => "C",
        Ns::CardDav => "CR",
        Ns::CalendarServer => "CS",
        Ns::AppleIcal => "I",
        Ns::Dav | Ns::Other | Ns::Unbound => "D",
    }
}

/// Body of a `PROPFIND` requesting the given properties.
pub(crate) fn propfind_body(props: &[PropName]) -> String {
    let mut body = format!("{XML_DECLARATION}<D:propfind {REQUEST_NAMESPACES}><D:prop>");
    for (ns, name) in props {
        body.push('<');
        body.push_str(prefix(*ns));
        body.push(':');
        body.push_str(name);
        body.push_str("/>");
    }
    body.push_str("</D:prop></D:propfind>");
    body
}

/// A `<prefix:name>escaped text</prefix:name>` element.
pub(crate) fn text_element(ns: Ns, name: &str, text: &str) -> String {
    let prefix = prefix(ns);
    format!("<{prefix}:{name}>{}</{prefix}:{name}>", escape(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_prefixes_and_default_namespaces() {
        let xml = r#"<?xml version="1.0"?>
            <multistatus xmlns="DAV:" xmlns:x="urn:ietf:params:xml:ns:caldav">
              <response>
                <x:calendar-data>A</x:calendar-data>
                <y:getctag xmlns:y="http://calendarserver.org/ns/">B</y:getctag>
                <calendar-color xmlns="http://apple.com/ns/ical/">C</calendar-color>
                <x:thing xmlns:x="urn:example:other">D</x:thing>
              </response>
            </multistatus>"#;
        let root = parse(xml).expect("parse");
        assert!(root.is(Ns::Dav, "multistatus"));
        let response = root.child(Ns::Dav, "response").expect("response");
        assert_eq!(
            response.child(Ns::CalDav, "calendar-data").unwrap().text(),
            "A"
        );
        assert_eq!(
            response
                .child(Ns::CalendarServer, "getctag")
                .unwrap()
                .text(),
            "B"
        );
        assert_eq!(
            response
                .child(Ns::AppleIcal, "calendar-color")
                .unwrap()
                .text(),
            "C"
        );
        // `x` is rebound locally, so this must not be mistaken for CalDAV.
        let other = response.child(Ns::Other, "thing").expect("thing");
        assert_eq!(other.ns, Ns::Other);
        assert!(response.child(Ns::CalDav, "thing").is_none());
    }

    #[test]
    fn decodes_entities_char_refs_and_cdata_verbatim() {
        let xml = "<d:a xmlns:d=\"DAV:\">x &amp; y &lt;z&gt; &#65;&#x42; &quot;q&quot;\r\n<![CDATA[BEGIN:VCALENDAR\r\n<raw & data>]]>&#13;\n&unknown;</d:a>";
        let root = parse(xml).expect("parse");
        assert_eq!(
            root.text(),
            "x & y <z> AB \"q\"\r\nBEGIN:VCALENDAR\r\n<raw & data>\r\n&unknown;"
        );
    }

    #[test]
    fn guesses_undeclared_well_known_prefixes() {
        let root = parse("<D:multistatus><D:response><C:calendar-data>x</C:calendar-data></D:response></D:multistatus>")
            .expect("parse");
        assert!(root.is(Ns::Dav, "multistatus"));
        let response = root.child(Ns::Dav, "response").expect("response");
        assert!(response.has_child(Ns::CalDav, "calendar-data"));
    }

    #[test]
    fn unbound_elements_match_by_local_name() {
        let root = parse("<multistatus><response><href>/a</href></response></multistatus>")
            .expect("parse");
        assert!(root.is(Ns::Dav, "multistatus"));
        assert_eq!(
            root.child(Ns::Dav, "response").unwrap().href().as_deref(),
            Some("/a")
        );
    }

    #[test]
    fn namespace_uris_are_matched_leniently() {
        let root = parse(r#"<a xmlns="DAV:"><b xmlns="http://calendarserver.org/ns">1</b><c xmlns="http://apple.com/ns/ical">2</c></a>"#)
            .expect("parse");
        assert!(root.has_child(Ns::CalendarServer, "b"));
        assert!(root.has_child(Ns::AppleIcal, "c"));
    }

    #[test]
    fn attributes_are_unescaped_and_namespace_declarations_skipped() {
        let root = parse(
            r#"<C:comp xmlns:C="urn:ietf:params:xml:ns:caldav" name="V&amp;EVENT" C:x="1"/>"#,
        )
        .expect("parse");
        assert_eq!(root.attr("name"), Some("V&EVENT"));
        assert_eq!(root.attr("x"), Some("1"));
        assert_eq!(root.attributes.len(), 2);
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(matches!(parse("<a><b></a>"), Err(DavError::Xml(_))));
        assert!(matches!(parse(""), Err(DavError::Xml(_))));
        assert!(matches!(parse("<a/><b/>"), Err(DavError::Xml(_))));
        assert!(matches!(parse("<a>"), Err(DavError::Xml(_))));
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        assert!(matches!(parse(&deep), Err(DavError::Xml(_))));
    }

    #[test]
    fn accepts_byte_order_mark() {
        let root = parse("\u{feff}<?xml version=\"1.0\"?><a xmlns=\"DAV:\"/>").expect("parse");
        assert!(root.is(Ns::Dav, "a"));
    }

    #[test]
    fn descendant_search_and_hrefs() {
        let root = parse(
            r#"<d:x xmlns:d="DAV:"><d:y><d:href> /one </d:href><d:href>/two</d:href></d:y><d:z><d:w/></d:z></d:x>"#,
        )
        .expect("parse");
        assert!(root.descendant(Ns::Dav, "w").is_some());
        let y = root.child(Ns::Dav, "y").unwrap();
        assert_eq!(y.hrefs(), vec!["/one".to_owned(), "/two".to_owned()]);
    }

    #[test]
    fn request_bodies_are_well_formed() {
        let body = propfind_body(&[
            (Ns::Dav, "displayname"),
            (Ns::CalDav, "calendar-home-set"),
            (Ns::CardDav, "addressbook-home-set"),
            (Ns::CalendarServer, "getctag"),
            (Ns::AppleIcal, "calendar-color"),
        ]);
        let root = parse(&body).expect("parse");
        assert!(root.is(Ns::Dav, "propfind"));
        let prop = root.child(Ns::Dav, "prop").expect("prop");
        let names: Vec<(Ns, &str)> = prop
            .elements()
            .map(|el| (el.ns, el.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                (Ns::Dav, "displayname"),
                (Ns::CalDav, "calendar-home-set"),
                (Ns::CardDav, "addressbook-home-set"),
                (Ns::CalendarServer, "getctag"),
                (Ns::AppleIcal, "calendar-color"),
            ]
        );
        let element = text_element(Ns::CalDav, "calendar-description", "Tom & Jerry <3");
        assert_eq!(
            element,
            "<C:calendar-description>Tom &amp; Jerry &lt;3</C:calendar-description>"
        );
    }

    #[test]
    fn escape_round_trips() {
        let original = "a & b < c > \"d\" 'e'\r\nf";
        let xml = format!(
            "<a xmlns=\"DAV:\" v=\"{}\">{}</a>",
            escape("x\"y"),
            escape(original)
        );
        let root = parse(&xml).expect("parse");
        assert_eq!(root.text(), original);
        assert_eq!(root.attr("v"), Some("x\"y"));
    }
}
