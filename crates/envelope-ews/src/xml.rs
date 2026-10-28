//! A small namespace-aware XML tree used to read EWS and Autodiscover
//! responses, plus the writer used to build request bodies.
//!
//! Exchange always qualifies its elements, but the prefixes differ between
//! versions and operations (`s:`/`soap:`/`soap11:` for the envelope, `m:`
//! and `t:` for messages and types, `h:` for headers, default namespaces in
//! Autodiscover). Every element is therefore resolved to its namespace while
//! parsing; lookups match on the local name, which is unambiguous within the
//! places this crate reads from, and namespaces are only consulted where
//! they matter (SOAP faults).
//!
//! Text content (CDATA sections and character / entity references
//! included) is kept verbatim, so MIME content and message bodies keep
//! their line endings.

use quick_xml::NsReader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;

use crate::error::EwsError;

/// SOAP 1.1 envelope namespace.
pub(crate) const NS_SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
/// EWS types namespace (`t:`).
pub(crate) const NS_TYPES: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
/// EWS messages namespace (`m:`).
pub(crate) const NS_MESSAGES: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
/// EWS errors namespace (used inside SOAP fault details).
pub(crate) const NS_ERRORS: &str = "http://schemas.microsoft.com/exchange/services/2006/errors";

/// Maximum element nesting accepted in a response; real responses stay far
/// below this, it only guards against hostile input.
const MAX_DEPTH: usize = 256;

/// The namespaces this crate distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ns {
    /// `http://schemas.xmlsoap.org/soap/envelope/`
    Soap,
    /// EWS types.
    Types,
    /// EWS messages.
    Messages,
    /// EWS errors.
    Errors,
    /// Any other namespace (Autodiscover schemas, ...).
    Other,
    /// No namespace, or an undeclared prefix.
    Unbound,
}

impl Ns {
    fn from_uri(uri: &str) -> Ns {
        let uri = uri.trim();
        if uri.eq_ignore_ascii_case(NS_SOAP) {
            Ns::Soap
        } else if uri.eq_ignore_ascii_case(NS_TYPES) {
            Ns::Types
        } else if uri.eq_ignore_ascii_case(NS_MESSAGES) {
            Ns::Messages
        } else if uri.eq_ignore_ascii_case(NS_ERRORS) {
            Ns::Errors
        } else if uri.is_empty() {
            Ns::Unbound
        } else {
            Ns::Other
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
    /// Local name (without prefix).
    pub(crate) name: String,
    /// Attributes by local name (namespace declarations excluded).
    pub(crate) attributes: Vec<(String, String)>,
    pub(crate) children: Vec<Node>,
}

impl Element {
    /// True when the element has the given local name.
    pub(crate) fn is(&self, name: &str) -> bool {
        self.name == name
    }

    /// Child elements, in document order.
    pub(crate) fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(el) => Some(el),
            Node::Text(_) => None,
        })
    }

    /// First child element with the given local name.
    pub(crate) fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|el| el.is(name))
    }

    /// All child elements with the given local name.
    pub(crate) fn children_named<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Element> + 'a {
        self.elements().filter(move |el| el.is(name))
    }

    /// Follows a path of child names.
    pub(crate) fn path(&self, names: &[&str]) -> Option<&Element> {
        let mut current = self;
        for name in names {
            current = current.child(name)?;
        }
        Some(current)
    }

    /// First descendant (depth-first, excluding `self`) with the given name.
    #[cfg(test)]
    pub(crate) fn descendant(&self, name: &str) -> Option<&Element> {
        for el in self.elements() {
            if el.is(name) {
                return Some(el);
            }
            if let Some(found) = el.descendant(name) {
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

    /// Trimmed text of the first child with the given name.
    pub(crate) fn child_text(&self, name: &str) -> Option<String> {
        self.child(name).and_then(Element::trimmed_text)
    }

    /// Text of the first child with the given name, verbatim (bodies).
    pub(crate) fn child_raw_text(&self, name: &str) -> Option<String> {
        self.child(name).map(Element::text)
    }

    /// The first child parsed as an `xs:boolean`.
    pub(crate) fn child_bool(&self, name: &str) -> Option<bool> {
        self.child_text(name).and_then(|text| parse_bool(&text))
    }

    /// The first child parsed as an unsigned integer.
    pub(crate) fn child_u64(&self, name: &str) -> Option<u64> {
        self.child_text(name).and_then(|text| text.parse().ok())
    }

    /// The first child parsed as a `u32`.
    pub(crate) fn child_u32(&self, name: &str) -> Option<u32> {
        self.child_text(name).and_then(|text| text.parse().ok())
    }

    /// The first child parsed as an `i32`.
    pub(crate) fn child_i32(&self, name: &str) -> Option<i32> {
        self.child_text(name).and_then(|text| text.parse().ok())
    }
}

/// Parses an `xs:boolean` (`true`/`false`/`1`/`0`, any case).
pub(crate) fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Parses a complete XML document into a tree and returns its root element.
pub(crate) fn parse(xml: &str) -> Result<Element, EwsError> {
    // A UTF-8 byte order mark is legal before the XML declaration.
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let mut reader = NsReader::from_str(xml);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;

    loop {
        let position = reader.buffer_position();
        let (resolved, event) = reader
            .read_resolved_event()
            .map_err(|err| EwsError::Xml(format!("{err} (near byte {position})")))?;
        match event {
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(EwsError::Xml("document is nested too deeply".into()));
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
                    .ok_or_else(|| EwsError::Xml("unexpected closing tag".into()))?;
                attach(&mut stack, &mut root, element)?;
            }
            Event::Text(text) => push_text(&mut stack, &text),
            Event::CData(cdata) => push_text(&mut stack, &cdata),
            Event::GeneralRef(reference) => {
                let resolved_char = reference
                    .resolve_char_ref()
                    .map_err(|err| EwsError::Xml(format!("invalid character reference: {err}")))?;
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
        return Err(EwsError::Xml(
            "document ended before all elements were closed".into(),
        ));
    }
    root.ok_or_else(|| EwsError::Xml("document has no root element".into()))
}

fn new_element(resolved: ResolveResult<'_>, start: &BytesStart<'_>) -> Result<Element, EwsError> {
    let qname = start.name();
    let (local, _prefix) = qname.decompose();
    let ns = match resolved {
        ResolveResult::Bound(namespace) => Ns::from_uri(namespace.into_inner()),
        ResolveResult::Unbound | ResolveResult::Unknown(_) => Ns::Unbound,
    };

    let mut attributes = Vec::new();
    for attr in start.attributes().with_checks(false) {
        let attr = attr.map_err(|err| EwsError::Xml(format!("invalid attribute: {err}")))?;
        if attr.key.as_namespace_binding().is_some() {
            continue;
        }
        let key = attr.key.local_name().into_inner().to_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| EwsError::Xml(format!("invalid attribute value: {err}")))?;
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
) -> Result<(), EwsError> {
    match stack.last_mut() {
        Some(parent) => {
            parent.children.push(Node::Element(element));
            Ok(())
        }
        None if root.is_none() => {
            *root = Some(element);
            Ok(())
        }
        None => Err(EwsError::Xml(
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

/// Escapes text for element content or a double-quoted attribute value.
///
/// Characters that XML 1.0 does not allow at all (most C0 controls, lone
/// surrogates cannot occur in Rust strings) are dropped: Exchange rejects
/// the whole request otherwise. A carriage return is written as a character
/// reference so that the receiving parser does not normalize it away.
#[cfg(test)]
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

fn escape_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\r' => out.push_str("&#13;"),
            '\t' | '\n' => out.push(ch),
            '\u{FFFE}' | '\u{FFFF}' => {}
            c if (c as u32) < 0x20 => {}
            _ => out.push(ch),
        }
    }
}

/// Builds XML request bodies. Tag names include their prefix (`t:ItemId`);
/// attribute values and text are escaped.
#[derive(Debug, Default)]
pub(crate) struct XmlWriter {
    buf: String,
}

impl XmlWriter {
    pub(crate) fn new() -> Self {
        XmlWriter {
            buf: String::with_capacity(1024),
        }
    }

    fn open_tag(&mut self, tag: &str, attrs: &[(&str, &str)]) {
        self.buf.push('<');
        self.buf.push_str(tag);
        for (name, value) in attrs {
            self.buf.push(' ');
            self.buf.push_str(name);
            self.buf.push_str("=\"");
            escape_into(&mut self.buf, value);
            self.buf.push('"');
        }
    }

    /// `<tag attrs…>`
    pub(crate) fn start(&mut self, tag: &str, attrs: &[(&str, &str)]) -> &mut Self {
        self.open_tag(tag, attrs);
        self.buf.push('>');
        self
    }

    /// `</tag>`
    pub(crate) fn end(&mut self, tag: &str) -> &mut Self {
        self.buf.push_str("</");
        self.buf.push_str(tag);
        self.buf.push('>');
        self
    }

    /// `<tag attrs…/>`
    pub(crate) fn empty(&mut self, tag: &str, attrs: &[(&str, &str)]) -> &mut Self {
        self.open_tag(tag, attrs);
        self.buf.push_str("/>");
        self
    }

    /// `<tag>escaped text</tag>`
    pub(crate) fn text_element(&mut self, tag: &str, text: &str) -> &mut Self {
        self.text_element_with(tag, &[], text)
    }

    /// `<tag attrs…>escaped text</tag>`
    pub(crate) fn text_element_with(
        &mut self,
        tag: &str,
        attrs: &[(&str, &str)],
        text: &str,
    ) -> &mut Self {
        self.open_tag(tag, attrs);
        self.buf.push('>');
        escape_into(&mut self.buf, text);
        self.end(tag)
    }

    /// Text that is already valid XML content (base64, numbers, enum
    /// values); appended as is.
    pub(crate) fn raw(&mut self, xml: &str) -> &mut Self {
        self.buf.push_str(xml);
        self
    }

    pub(crate) fn into_string(self) -> String {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_namespaces_and_matches_local_names() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header><h:ServerVersionInfo MajorVersion="15" xmlns:h="http://schemas.microsoft.com/exchange/services/2006/types"/></s:Header>
  <s:Body xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
    <m:GetFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages><m:GetFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:GetFolderResponseMessage></m:ResponseMessages>
    </m:GetFolderResponse>
  </s:Body>
</s:Envelope>"#;
        let root = parse(xml).expect("parse");
        assert_eq!(root.ns, Ns::Soap);
        assert!(root.is("Envelope"));
        let info = root.path(&["Header", "ServerVersionInfo"]).expect("info");
        assert_eq!(info.ns, Ns::Types);
        assert_eq!(info.attr("majorversion"), Some("15"));
        let message = root
            .descendant("GetFolderResponseMessage")
            .expect("message");
        assert_eq!(message.ns, Ns::Messages);
        assert_eq!(message.attr("ResponseClass"), Some("Success"));
        assert_eq!(
            message.child_text("ResponseCode").as_deref(),
            Some("NoError")
        );
    }

    #[test]
    fn decodes_entities_char_refs_and_cdata_verbatim() {
        let xml = "<a>x &amp; y &lt;z&gt; &#65;&#x42; &quot;q&quot;\r\n<![CDATA[<raw & data>]]>&#13;\n&unknown;</a>";
        let root = parse(xml).expect("parse");
        assert_eq!(
            root.text(),
            "x & y <z> AB \"q\"\r\n<raw & data>\r\n&unknown;"
        );
    }

    #[test]
    fn typed_child_accessors() {
        let root = parse("<r><b> true </b><c>0</c><n>42</n><neg>-7</neg><bad>x</bad><e>  </e></r>")
            .expect("parse");
        assert_eq!(root.child_bool("b"), Some(true));
        assert_eq!(root.child_bool("c"), Some(false));
        assert_eq!(root.child_bool("bad"), None);
        assert_eq!(root.child_u64("n"), Some(42));
        assert_eq!(root.child_u32("n"), Some(42));
        assert_eq!(root.child_i32("neg"), Some(-7));
        assert_eq!(root.child_u64("bad"), None);
        assert_eq!(root.child_text("e"), None);
        assert_eq!(root.child_raw_text("e").as_deref(), Some("  "));
        assert_eq!(root.child_text("missing"), None);
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(matches!(parse("<a><b></a>"), Err(EwsError::Xml(_))));
        assert!(matches!(parse(""), Err(EwsError::Xml(_))));
        assert!(matches!(parse("<a/><b/>"), Err(EwsError::Xml(_))));
        assert!(matches!(parse("<a>"), Err(EwsError::Xml(_))));
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        assert!(matches!(parse(&deep), Err(EwsError::Xml(_))));
    }

    #[test]
    fn accepts_byte_order_mark() {
        let root = parse("\u{feff}<?xml version=\"1.0\"?><a/>").expect("parse");
        assert!(root.is("a"));
        assert_eq!(root.ns, Ns::Unbound);
    }

    #[test]
    fn writer_escapes_text_and_attributes() {
        let mut w = XmlWriter::new();
        w.start("t:Item", &[("Id", "a\"b<c>&'")])
            .text_element("t:Subject", "Tom & Jerry <3>\r\nnext\u{1}\u{0}line\ttab")
            .empty("t:FieldURI", &[("FieldURI", "item:Subject")])
            .raw("<t:Raw>QUJD</t:Raw>")
            .end("t:Item");
        let xml = w.into_string();
        assert_eq!(
            xml,
            "<t:Item Id=\"a&quot;b&lt;c&gt;&amp;&apos;\"><t:Subject>Tom &amp; Jerry &lt;3&gt;&#13;\nnextline\ttab</t:Subject><t:FieldURI FieldURI=\"item:Subject\"/><t:Raw>QUJD</t:Raw></t:Item>"
        );
        // What was written reads back identically (minus the dropped controls).
        let root = parse(&xml).expect("parse");
        assert_eq!(root.attr("Id"), Some("a\"b<c>&'"));
        assert_eq!(
            root.child_raw_text("Subject").as_deref(),
            Some("Tom & Jerry <3>\r\nnextline\ttab")
        );
    }

    #[test]
    fn escape_round_trips() {
        let original = "a & b < c > \"d\" 'e'\r\nf";
        let xml = format!("<a v=\"{}\">{}</a>", escape("x\"y"), escape(original));
        let root = parse(&xml).expect("parse");
        assert_eq!(root.text(), original);
        assert_eq!(root.attr("v"), Some("x\"y"));
    }
}
