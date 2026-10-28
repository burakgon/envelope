//! Online meeting link detection (Google Meet, Teams, Zoom, Webex, …).

use crate::component::{Component, Property};

/// Finds the online meeting link of a `VEVENT`.
///
/// Dedicated properties are consulted first, in this order: `CONFERENCE`
/// (RFC 7986, preferring `FEATURE=VIDEO`), `X-GOOGLE-CONFERENCE`,
/// `X-MICROSOFT-SKYPETEAMSMEETINGURL`, `X-MICROSOFT-ONLINEMEETINGCONFLINK`.
/// Then `LOCATION`, `DESCRIPTION`, `URL` and the HTML description are
/// scanned for links of known meeting services.
pub fn detect_conference_url(vevent: &Component) -> Option<String> {
    let mut conference: Vec<&Property> = vevent.properties_named("CONFERENCE").collect();
    conference.sort_by_key(|p| !p.has_param_value("FEATURE", "VIDEO"));
    for property in conference {
        if let Some(url) = web_url(&property.value) {
            return Some(url);
        }
    }
    for name in [
        "X-GOOGLE-CONFERENCE",
        "X-MICROSOFT-SKYPETEAMSMEETINGURL",
        "X-MICROSOFT-ONLINEMEETINGCONFLINK",
    ] {
        if let Some(url) = vevent.property(name).and_then(|p| web_url(&p.value)) {
            return Some(url);
        }
    }
    for name in ["LOCATION", "DESCRIPTION", "URL", "X-ALT-DESC"] {
        for property in vevent.properties_named(name) {
            if let Some(url) = find_meeting_url(&property.text_value()) {
                return Some(url);
            }
        }
    }
    // A location that is nothing but a link is where the meeting happens,
    // whatever service it points to.
    vevent
        .property("LOCATION")
        .map(|p| p.text_value())
        .filter(|v| !v.trim().contains(char::is_whitespace))
        .and_then(|v| web_url(&v))
}

/// The value as an http(s) URL, if it is one.
fn web_url(value: &str) -> Option<String> {
    let value = crate::text::unescape_text(value.trim());
    let lower = value.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        .then(|| trim_url(&value).to_owned())
        .filter(|url| url.len() > "https://".len())
}

/// Returns the first link to a known online meeting service found in free
/// text (an event description, a location, an HTML body, …).
///
/// ```
/// use envelope_ical::conference::find_meeting_url;
/// let text = "Join: <https://us02web.zoom.us/j/81234567890?pwd=abc> (passcode 1234).";
/// assert_eq!(
///     find_meeting_url(text).as_deref(),
///     Some("https://us02web.zoom.us/j/81234567890?pwd=abc")
/// );
/// ```
pub fn find_meeting_url(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(relative) = lower[search_from..].find("http") {
        let start = search_from + relative;
        search_from = start + 4;
        let rest = &lower[start..];
        if !(rest.starts_with("https://") || rest.starts_with("http://")) {
            continue;
        }
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '|' | '\\')
            })
            .map_or(lower.len(), |e| start + e);
        let candidate = trim_url(&text[start..end]);
        if is_meeting_url(candidate) {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// Strips punctuation that commonly trails URLs in prose.
fn trim_url(url: &str) -> &str {
    let mut url = url.trim();
    loop {
        let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
        // A closing bracket without its opening counterpart belongs to the
        // surrounding prose, not to the URL.
        let unbalanced = (trimmed.ends_with(')') && !trimmed.contains('('))
            || (trimmed.ends_with(']') && !trimmed.contains('['));
        let trimmed = if unbalanced {
            &trimmed[..trimmed.len() - 1]
        } else {
            trimmed
        };
        if trimmed.len() == url.len() {
            return url;
        }
        url = trimmed;
    }
}

fn is_meeting_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let without_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let (host_port, path) = match without_scheme.find(['/', '?', '#']) {
        Some(i) => (&without_scheme[..i], &without_scheme[i..]),
        None => (without_scheme, ""),
    };
    let host = host_port.split(':').next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let has_path = path.len() > 1;
    let sub_of = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    let path_starts = |prefixes: &[&str]| prefixes.iter().any(|p| path.starts_with(p));

    if host == "meet.google.com" {
        return has_path && !path.starts_with("/_meet");
    }
    if sub_of("zoom.us") || sub_of("zoomgov.com") {
        return path_starts(&["/j/", "/my/", "/w/", "/s/", "/wc/", "/meeting/"]);
    }
    if host == "teams.microsoft.com" || host == "teams.cloud.microsoft" {
        return path_starts(&["/l/meetup-join/", "/meet/"]);
    }
    if host == "teams.live.com" {
        return path_starts(&["/meet/", "/l/meetup-join/"]);
    }
    if sub_of("webex.com") {
        return !matches!(host, "webex.com" | "www.webex.com" | "help.webex.com") && has_path;
    }
    if host == "whereby.com" {
        return has_path && !path_starts(&["/information", "/user", "/org"]);
    }
    if host == "meet.jit.si" || host == "8x8.vc" {
        return has_path;
    }
    if sub_of("gotomeeting.com") || host == "gotomeet.me" || host == "meet.goto.com" {
        return has_path && host != "support.goto.com";
    }
    if host == "chime.aws" || host == "app.chime.aws" {
        return has_path;
    }
    if host == "facetime.apple.com" {
        return path.starts_with("/join");
    }
    if host == "join.skype.com" || sub_of("bluejeans.com") {
        return has_path && host != "support.bluejeans.com";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_components;

    #[test]
    fn known_services() {
        let cases = [
            (
                "Join https://meet.google.com/abc-defg-hij now",
                Some("https://meet.google.com/abc-defg-hij"),
            ),
            (
                "https://zoom.us/j/123456789",
                Some("https://zoom.us/j/123456789"),
            ),
            (
                "(https://acme.zoom.us/my/room).",
                Some("https://acme.zoom.us/my/room"),
            ),
            ("https://support.zoom.us/hc/en-us", None),
            (
                "Join the meeting now<https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d>",
                Some(
                    "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d",
                ),
            ),
            (
                "https://teams.microsoft.com/meetingOptions/?organizerId=1",
                None,
            ),
            (
                "https://teams.microsoft.com/meet/2345678?p=xyz",
                Some("https://teams.microsoft.com/meet/2345678?p=xyz"),
            ),
            (
                "https://teams.live.com/meet/9876",
                Some("https://teams.live.com/meet/9876"),
            ),
            (
                "https://acme.webex.com/acme/j.php?MTID=m1",
                Some("https://acme.webex.com/acme/j.php?MTID=m1"),
            ),
            ("https://help.webex.com/article", None),
            (
                "https://whereby.com/team-room",
                Some("https://whereby.com/team-room"),
            ),
            (
                "https://meet.jit.si/MyRoom",
                Some("https://meet.jit.si/MyRoom"),
            ),
            (
                "https://global.gotomeeting.com/join/123456789",
                Some("https://global.gotomeeting.com/join/123456789"),
            ),
            (
                "https://chime.aws/1234567890",
                Some("https://chime.aws/1234567890"),
            ),
            (
                "https://facetime.apple.com/join#v=1&p=abc",
                Some("https://facetime.apple.com/join#v=1&p=abc"),
            ),
            ("https://aka.ms/JoinTeamsMeeting", None),
            ("https://example.com/zoom.us/j/1", None),
            ("no links here", None),
        ];
        for (text, expected) in cases {
            assert_eq!(find_meeting_url(text).as_deref(), expected, "{text}");
        }
    }

    #[test]
    fn skips_help_links_before_the_meeting_link() {
        let text = "Need help? https://aka.ms/JoinTeamsMeeting\nJoin: https://teams.microsoft.com/l/meetup-join/abc";
        assert_eq!(
            find_meeting_url(text).as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/abc")
        );
    }

    #[test]
    fn property_priority() {
        let text = "BEGIN:VEVENT\nLOCATION:https://zoom.us/j/1\nCONFERENCE;VALUE=URI;FEATURE=PHONE:tel:+1-555\nCONFERENCE;VALUE=URI;FEATURE=VIDEO;LABEL=Meet:https://meet.google.com/aaa-bbbb-ccc\nX-GOOGLE-CONFERENCE:https://meet.google.com/zzz-zzzz-zzz\nEND:VEVENT\n";
        let event = &parse_components(text)[0];
        assert_eq!(
            detect_conference_url(event).as_deref(),
            Some("https://meet.google.com/aaa-bbbb-ccc")
        );

        let text = "BEGIN:VEVENT\nX-MICROSOFT-ONLINEMEETINGCONFLINK:conf:sip:a@b.com\\;gruu\nDESCRIPTION:Join https://teams.microsoft.com/l/meetup-join/xyz\nEND:VEVENT\n";
        let event = &parse_components(text)[0];
        assert_eq!(
            detect_conference_url(event).as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/xyz")
        );
    }

    #[test]
    fn bare_link_location() {
        let text = "BEGIN:VEVENT\nLOCATION:https://meet.example.org/standup\nEND:VEVENT\n";
        let event = &parse_components(text)[0];
        assert_eq!(
            detect_conference_url(event).as_deref(),
            Some("https://meet.example.org/standup")
        );

        let text = "BEGIN:VEVENT\nLOCATION:Room 4 (see https://example.org/map)\nEND:VEVENT\n";
        let event = &parse_components(text)[0];
        assert_eq!(detect_conference_url(event), None);
    }
}
