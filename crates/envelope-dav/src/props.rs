//! Interpretation of collection properties shared by CalDAV and CardDAV.

use crate::error::DavError;
use crate::multistatus::Response;
use crate::xml::{Element, Ns};

/// Normalises a calendar colour as reported by servers (`#RRGGBB`,
/// `#RRGGBBAA` as written by Apple clients, `#RGB`, with or without `#`) to
/// lower-case `#rrggbb`. Anything else yields `None`.
pub(crate) fn normalize_color(raw: &str) -> Option<String> {
    let hex = raw.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = match hex.len() {
        6 | 8 => hex.get(..6)?.to_ascii_lowercase(),
        3 | 4 => hex
            .chars()
            .take(3)
            .flat_map(|c| [c, c])
            .collect::<String>()
            .to_ascii_lowercase(),
        _ => return None,
    };
    Some(format!("#{rgb}"))
}

/// Colour in the form written to servers: `#RRGGBBFF`, the format Apple
/// clients use and every other client understands.
pub(crate) fn color_for_server(input: &str) -> Result<String, DavError> {
    let rgb = normalize_color(input)
        .ok_or_else(|| DavError::InvalidInput(format!("`{input}` is not a #RRGGBB colour")))?;
    Ok(format!("{}FF", rgb.to_ascii_uppercase()))
}

/// Interprets `DAV:current-user-privilege-set` (RFC 3744 §5.4): a collection
/// is read-only when privileges are reported and none of them allows writing.
/// When the server does not report privileges the collection is assumed to
/// be writable.
pub(crate) fn is_read_only(response: &Response) -> bool {
    let Some(set) = response.prop(Ns::Dav, "current-user-privilege-set") else {
        return false;
    };
    let mut privileges = set
        .children_named(Ns::Dav, "privilege")
        .flat_map(Element::elements)
        .peekable();
    if privileges.peek().is_none() {
        return false;
    }
    !privileges.any(|privilege| {
        privilege.is(Ns::Dav, "all")
            || privilege.is(Ns::Dav, "write")
            || privilege.is(Ns::Dav, "write-content")
            || privilege.is(Ns::Dav, "bind")
    })
}

/// True when the collection supports RFC 6578 `sync-collection` reports.
pub(crate) fn supports_sync(response: &Response) -> bool {
    response.prop_text(Ns::Dav, "sync-token").is_some()
        || response
            .prop(Ns::Dav, "supported-report-set")
            .is_some_and(|set| set.descendant(Ns::Dav, "sync-collection").is_some())
}

/// Parses `calendar-order`, which is an integer in practice but has been seen
/// with surrounding whitespace or as a decimal.
pub(crate) fn parse_order(text: &str) -> Option<i32> {
    let text = text.trim();
    text.parse::<i32>().ok().or_else(|| {
        text.parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && value.abs() < f64::from(i32::MAX))
            .map(|value| value.round() as i32)
    })
}

/// The `TZID` of the first time zone in a `calendar-timezone` value (a
/// `VCALENDAR` containing one `VTIMEZONE`).
pub(crate) fn timezone_id(vcalendar: &str) -> Option<String> {
    // Unfold content lines (RFC 5545 §3.1) before looking at them.
    let mut lines: Vec<String> = Vec::new();
    for raw in vcalendar.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(continuation), Some(previous)) => previous.push_str(continuation),
            _ => lines.push(line.to_owned()),
        }
    }
    lines.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        let name = name.split(';').next()?;
        (name.trim().eq_ignore_ascii_case("TZID") && !value.trim().is_empty())
            .then(|| value.trim().to_owned())
    })
}

/// Payload of a `calendar-data` / `address-data` element: the text with the
/// indentation of pretty-printed XML removed and exactly one line terminator
/// (in the style used by the payload) at the end.
pub(crate) fn payload(element: &Element) -> String {
    let text = element.text();
    let body = text.trim();
    if body.is_empty() {
        return String::new();
    }
    let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
    format!("{body}{eol}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multistatus::parse_multistatus;

    #[test]
    fn colors() {
        assert_eq!(normalize_color("#FF2968FF").as_deref(), Some("#ff2968"));
        assert_eq!(normalize_color("#0082C9").as_deref(), Some("#0082c9"));
        assert_eq!(normalize_color(" 0082c9 ").as_deref(), Some("#0082c9"));
        assert_eq!(normalize_color("#abc").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_color("#abcf").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_color("red"), None);
        assert_eq!(normalize_color("#12345"), None);
        assert_eq!(normalize_color("#GGGGGG"), None);
        assert_eq!(normalize_color(""), None);
        assert_eq!(color_for_server("#ff2968").unwrap(), "#FF2968FF");
        assert_eq!(color_for_server("#FF2968CC").unwrap(), "#FF2968FF");
        assert!(matches!(
            color_for_server("blue"),
            Err(DavError::InvalidInput(_))
        ));
    }

    fn response_with(props: &str) -> Response {
        let body = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/c/</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        parse_multistatus(&body).unwrap().responses.remove(0)
    }

    #[test]
    fn privileges() {
        assert!(!is_read_only(&response_with("")));
        assert!(!is_read_only(&response_with(
            "<d:current-user-privilege-set/>"
        )));
        assert!(is_read_only(&response_with(
            "<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:read-current-user-privilege-set/></d:privilege></d:current-user-privilege-set>"
        )));
        assert!(!is_read_only(&response_with(
            "<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>"
        )));
        assert!(!is_read_only(&response_with(
            "<d:current-user-privilege-set><d:privilege><d:all/></d:privilege></d:current-user-privilege-set>"
        )));
        assert!(!is_read_only(&response_with(
            "<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write-content/></d:privilege></d:current-user-privilege-set>"
        )));
    }

    #[test]
    fn sync_support() {
        assert!(supports_sync(&response_with(
            "<d:sync-token>http://example.com/s/1</d:sync-token>"
        )));
        assert!(supports_sync(&response_with(
            "<d:supported-report-set><d:supported-report><d:report><d:sync-collection/></d:report></d:supported-report></d:supported-report-set>"
        )));
        assert!(!supports_sync(&response_with(
            "<d:supported-report-set><d:supported-report><d:report><d:expand-property/></d:report></d:supported-report></d:supported-report-set>"
        )));
    }

    #[test]
    fn orders() {
        assert_eq!(parse_order(" 3 "), Some(3));
        assert_eq!(parse_order("-1"), Some(-1));
        assert_eq!(parse_order("2.6"), Some(3));
        assert_eq!(parse_order("first"), None);
        assert_eq!(parse_order("1e300"), None);
    }

    #[test]
    fn timezone_ids() {
        let vcal = "BEGIN:VCALENDAR\r\nPRODID:-//Example//EN\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Is\r\n tanbul\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n";
        assert_eq!(timezone_id(vcal).as_deref(), Some("Europe/Istanbul"));
        assert_eq!(
            timezone_id("BEGIN:VCALENDAR\nTZID;X-FOO=1:America/New_York\nEND:VCALENDAR").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(timezone_id("BEGIN:VCALENDAR\nEND:VCALENDAR"), None);
    }

    #[test]
    fn payload_trimming() {
        let element = |text: &str| Element {
            ns: Ns::CalDav,
            name: "calendar-data".into(),
            attributes: Vec::new(),
            children: vec![crate::xml::Node::Text(text.into())],
        };
        assert_eq!(
            payload(&element("\n    BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n    ")),
            "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"
        );
        assert_eq!(
            payload(&element("BEGIN:VCARD\nEND:VCARD")),
            "BEGIN:VCARD\nEND:VCARD\n"
        );
        assert_eq!(payload(&element("  ")), "");
    }
}
