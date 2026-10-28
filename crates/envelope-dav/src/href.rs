//! URL handling: parsing user input, resolving and normalising hrefs, and
//! naming new resources.
//!
//! Servers report hrefs in many shapes for the same resource: absolute paths
//! or full URLs, `%40` or `@`, `%7e` or `~`, explicit default ports
//! (`https://p07-caldav.icloud.com:443/...`), spaces left unencoded, and so
//! on. Every href leaving this crate is therefore resolved against the URL of
//! the request that produced it and then normalised:
//!
//! * scheme and host are lower-cased, default ports and fragments removed;
//! * every path segment is percent-decoded and re-encoded so that exactly the
//!   characters that are not allowed in a path segment are escaped, with
//!   upper-case hex digits.
//!
//! Two hrefs naming the same resource therefore always compare equal as
//! plain strings, which is what callers store in their databases.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, percent_encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::error::DavError;
use crate::util::hex;

/// Bytes escaped in a normalised path segment: everything except RFC 3986
/// `pchar` characters (unreserved, sub-delims, `:` and `@`).
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=')
    .remove(b':')
    .remove(b'@');

/// Longest sanitised UID used verbatim in a resource name.
const MAX_NAME_LEN: usize = 120;

/// Parses a server address typed by a user: a full URL, or just a host
/// (optionally with port and path), in which case `https` is assumed.
pub(crate) fn parse_user_url(input: &str) -> Result<Url, DavError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(DavError::invalid_url(input, "empty URL"));
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&candidate).map_err(|err| DavError::invalid_url(input, err))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DavError::invalid_url(
            input,
            "only http and https URLs are supported",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(DavError::invalid_url(input, "the URL has no host"));
    }
    // Credentials never travel inside URLs; they come from `Auth`.
    // Both setters only fail for URLs without a host, excluded above.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    Ok(normalize(url))
}

/// Normalises a URL as described in the module documentation.
pub(crate) fn normalize(mut url: Url) -> Url {
    url.set_fragment(None);
    if url.cannot_be_a_base() {
        return url;
    }
    let path = url.path();
    let mut normalized = String::with_capacity(path.len() + 8);
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            normalized.push('/');
        }
        let decoded: Vec<u8> = percent_decode_str(segment).collect();
        normalized.extend(percent_encode(&decoded, SEGMENT));
    }
    if normalized != path {
        url.set_path(&normalized);
    }
    url
}

/// Resolves an href found in a response (or passed in by a caller) against
/// the URL it is relative to, and normalises the result.
pub(crate) fn resolve(base: &Url, href: &str) -> Result<Url, DavError> {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return Err(DavError::invalid_url(href, "empty href"));
    }
    let mut url = base
        .join(trimmed)
        .map_err(|err| DavError::invalid_url(href, err))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DavError::invalid_url(
            href,
            "only http and https URLs are supported",
        ));
    }
    // Servers behind a TLS-terminating reverse proxy sometimes advertise
    // `http://` URLs for their own host. Following them would silently
    // downgrade the connection, so keep using the scheme and port that work.
    if base.scheme() == "https"
        && url.scheme() == "http"
        && url.host() == base.host()
        && url.set_scheme("https").is_ok()
    {
        let _ = url.set_port(base.port());
    }
    Ok(normalize(url))
}

/// True when both URLs name the same resource, ignoring a trailing slash
/// (collections are often reported both with and without it).
pub(crate) fn same_resource(a: &Url, b: &Url) -> bool {
    a.as_str().trim_end_matches('/') == b.as_str().trim_end_matches('/')
}

/// True when the URL names a collection (its path ends with a slash).
pub(crate) fn looks_like_collection(url: &Url) -> bool {
    url.path().ends_with('/')
}

/// Representation of `target` for a `DAV:href` in a request body sent to
/// `request_url`: an absolute path when both share an origin (the form every
/// server accepts), the full URL otherwise.
pub(crate) fn request_href(target: &Url, request_url: &Url) -> String {
    if target.origin() == request_url.origin() {
        match target.query() {
            Some(query) => format!("{}?{query}", target.path()),
            None => target.path().to_owned(),
        }
    } else {
        target.as_str().to_owned()
    }
}

/// True for `localhost` and loopback addresses.
pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// True when credentials may be sent to the URL without waiting for a
/// challenge: the connection is encrypted, or never leaves the machine.
pub(crate) fn is_secure_or_local(url: &Url) -> bool {
    url.scheme() == "https" || is_loopback(url)
}

/// Normalises an absolute http(s) URL the same way every href returned by
/// this crate is normalised, so hrefs obtained elsewhere (for example typed
/// by the user, or stored by an older version) can be compared with them.
///
/// ```
/// # fn main() -> Result<(), envelope_dav::DavError> {
/// let a = envelope_dav::normalize_href("https://DAV.example.com:443/cal/jane%40example.com/a%20b.ics")?;
/// let b = envelope_dav::normalize_href("https://dav.example.com/cal/jane@example.com/a b.ics")?;
/// assert_eq!(a, b);
/// assert_eq!(a, "https://dav.example.com/cal/jane@example.com/a%20b.ics");
/// # Ok(()) }
/// ```
pub fn normalize_href(href: &str) -> Result<String, DavError> {
    let url = Url::parse(href.trim()).map_err(|err| DavError::invalid_url(href, err))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DavError::invalid_url(
            href,
            "only http and https URLs are supported",
        ));
    }
    Ok(normalize(url).into())
}

/// The kind of object stored in a collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    /// An iCalendar object (`text/calendar`, stored as `.ics`).
    Calendar,
    /// A vCard (`text/vcard`, stored as `.vcf`).
    Contact,
}

impl ObjectKind {
    /// File extension used for new resources, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ObjectKind::Calendar => "ics",
            ObjectKind::Contact => "vcf",
        }
    }

    /// `Content-Type` sent when uploading such an object.
    pub fn content_type(self) -> &'static str {
        match self {
            ObjectKind::Calendar => "text/calendar; charset=utf-8",
            ObjectKind::Contact => "text/vcard; charset=utf-8",
        }
    }

    /// Guesses the kind of a raw payload from its first content line: vCards
    /// start with `BEGIN:VCARD`, everything else is treated as iCalendar.
    pub fn detect(body: &str) -> ObjectKind {
        let start = body.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
        match start.get(..11) {
            Some(prefix) if prefix.eq_ignore_ascii_case("BEGIN:VCARD") => ObjectKind::Contact,
            _ => ObjectKind::Calendar,
        }
    }
}

/// File name for a new resource holding the object with the given UID.
///
/// UIDs made of URL-safe characters (`A-Z a-z 0-9 - _ . @ ~`, which covers
/// UUIDs and the `...@google.com` style) are used verbatim, which some
/// servers (Google in particular) expect. Other characters are replaced by
/// `_`, over-long UIDs are shortened, and in both cases a hash of the
/// original UID is appended so different UIDs never share a name. An empty
/// UID yields a random name.
///
/// ```
/// use envelope_dav::{ObjectKind, resource_name};
/// assert_eq!(resource_name("2F8B9A6C-1D2E-4F3A-9B8C-7D6E5F4A3B2C", ObjectKind::Calendar),
///            "2F8B9A6C-1D2E-4F3A-9B8C-7D6E5F4A3B2C.ics");
/// assert_eq!(resource_name("abc123@google.com", ObjectKind::Calendar), "abc123@google.com.ics");
/// assert!(resource_name("a/b c", ObjectKind::Contact).starts_with("a_b_c-"));
/// ```
pub fn resource_name(uid: &str, kind: ObjectKind) -> String {
    let uid = uid.trim();
    if uid.is_empty() {
        return format!("{}.{}", uuid::Uuid::new_v4(), kind.extension());
    }
    let mut name = String::with_capacity(uid.len().min(MAX_NAME_LEN) + 20);
    let mut altered = false;
    for ch in uid.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '@' | '~') {
            name.push(ch);
        } else {
            name.push('_');
            altered = true;
        }
    }
    // Leading dots create hidden files on filesystem-backed servers
    // (Radicale refuses them outright).
    if name.starts_with('.') {
        name.replace_range(..1, "_");
        altered = true;
    }
    if name.len() > MAX_NAME_LEN {
        // `name` is pure ASCII at this point, so any index is a char boundary.
        name.truncate(MAX_NAME_LEN);
        altered = true;
    }
    if altered {
        let digest = Sha256::digest(uid.as_bytes());
        name.push('-');
        name.push_str(&hex(&digest[..8]));
    }
    format!("{name}.{}", kind.extension())
}

/// Builds the href of a new resource for the object with the given UID inside
/// a collection (see [`resource_name`] for the naming rules).
///
/// `collection` must be an absolute URL, as returned by
/// [`DavClient::list_calendars`](crate::DavClient::list_calendars) and
/// friends; a missing trailing slash is tolerated.
///
/// ```
/// # fn main() -> Result<(), envelope_dav::DavError> {
/// use envelope_dav::{ObjectKind, object_href};
/// let href = object_href("https://dav.example.com/calendars/jane/work", "event-1", ObjectKind::Calendar)?;
/// assert_eq!(href, "https://dav.example.com/calendars/jane/work/event-1.ics");
/// # Ok(()) }
/// ```
pub fn object_href(collection: &str, uid: &str, kind: ObjectKind) -> Result<String, DavError> {
    let mut base =
        Url::parse(collection.trim()).map_err(|err| DavError::invalid_url(collection, err))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(DavError::invalid_url(
            collection,
            "only http and https URLs are supported",
        ));
    }
    base.set_query(None);
    base.set_fragment(None);
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    let url = base
        .join(&resource_name(uid, kind))
        .map_err(|err| DavError::invalid_url(collection, err))?;
    Ok(normalize(url).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("valid test URL")
    }

    #[test]
    fn user_urls() {
        assert_eq!(
            parse_user_url("caldav.icloud.com").unwrap().as_str(),
            "https://caldav.icloud.com/"
        );
        assert_eq!(
            parse_user_url(" https://cloud.example.com/remote.php/dav ")
                .unwrap()
                .as_str(),
            "https://cloud.example.com/remote.php/dav"
        );
        assert_eq!(
            parse_user_url("nas.local:5232/").unwrap().as_str(),
            "https://nas.local:5232/"
        );
        assert_eq!(
            parse_user_url("http://jane:secret@127.0.0.1:5232/jane/")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:5232/jane/"
        );
        assert_eq!(
            parse_user_url("HTTPS://Example.COM:443").unwrap().as_str(),
            "https://example.com/"
        );
        assert!(parse_user_url("").is_err());
        assert!(parse_user_url("ftp://example.com/").is_err());
        assert!(parse_user_url("https://").is_err());
    }

    #[test]
    fn percent_encoding_variants_compare_equal() {
        let base = url("https://dav.example.com/calendars/");
        let variants = [
            "/calendars/jane%40example.com/My%20Calendar/",
            "/calendars/jane@example.com/My Calendar/",
            "/calendars/jane%40example.com/My%20Calendar/#fragment",
            "https://DAV.EXAMPLE.COM:443/calendars/jane@example.com/My%20Calendar/",
            "jane%40example.com/My%20Calendar/",
        ];
        let expected = "https://dav.example.com/calendars/jane@example.com/My%20Calendar/";
        for variant in variants {
            assert_eq!(
                resolve(&base, variant).unwrap().as_str(),
                expected,
                "{variant}"
            );
        }
    }

    #[test]
    fn unreserved_and_hex_case_are_normalised() {
        let base = url("https://dav.example.com/");
        assert_eq!(
            resolve(&base, "/a%7euser/%e2%82%ac/caf\u{e9}.ics")
                .unwrap()
                .as_str(),
            "https://dav.example.com/a~user/%E2%82%AC/caf%C3%A9.ics"
        );
        // Encoded slashes and percent signs stay encoded.
        assert_eq!(
            resolve(&base, "/a%2Fb/100%25/50%/").unwrap().as_str(),
            "https://dav.example.com/a%2Fb/100%25/50%25/"
        );
    }

    #[test]
    fn absolute_hrefs_on_other_hosts_are_kept() {
        let base = url("https://caldav.icloud.com/1234567/principal/");
        assert_eq!(
            resolve(
                &base,
                "https://p07-caldav.icloud.com:443/1234567/calendars/"
            )
            .unwrap()
            .as_str(),
            "https://p07-caldav.icloud.com/1234567/calendars/"
        );
    }

    #[test]
    fn https_is_not_downgraded_on_the_same_host() {
        let base = url("https://cloud.example.com:8443/remote.php/dav/");
        assert_eq!(
            resolve(
                &base,
                "http://cloud.example.com/remote.php/dav/principals/users/jane/"
            )
            .unwrap()
            .as_str(),
            "https://cloud.example.com:8443/remote.php/dav/principals/users/jane/"
        );
        // A different host is left alone.
        assert_eq!(
            resolve(&base, "http://other.example.com/x")
                .unwrap()
                .as_str(),
            "http://other.example.com/x"
        );
    }

    #[test]
    fn resolve_rejects_unusable_hrefs() {
        let base = url("https://dav.example.com/");
        assert!(resolve(&base, "mailto:jane@example.com").is_err());
        assert!(resolve(&base, "  ").is_err());
    }

    #[test]
    fn collection_identity_ignores_trailing_slash() {
        assert!(same_resource(
            &url("https://dav.example.com/cal/home"),
            &url("https://dav.example.com/cal/home/")
        ));
        assert!(!same_resource(
            &url("https://dav.example.com/cal/home/"),
            &url("https://dav.example.com/cal/home/a.ics")
        ));
    }

    #[test]
    fn request_hrefs_use_paths_on_the_same_origin() {
        let collection = url("https://dav.example.com/cal/");
        assert_eq!(
            request_href(&url("https://dav.example.com/cal/a%20b.ics"), &collection),
            "/cal/a%20b.ics"
        );
        assert_eq!(
            request_href(&url("https://other.example.com/cal/a.ics"), &collection),
            "https://other.example.com/cal/a.ics"
        );
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback(&url("http://localhost:5232/")));
        assert!(is_loopback(&url("http://127.0.0.1:5232/")));
        assert!(is_loopback(&url("http://[::1]:5232/")));
        assert!(!is_loopback(&url("http://192.168.1.10:5232/")));
        assert!(is_secure_or_local(&url("https://dav.example.com/")));
        assert!(!is_secure_or_local(&url("http://dav.example.com/")));
    }

    #[test]
    fn resource_names() {
        assert_eq!(
            resource_name("simple-uid_1.2~x", ObjectKind::Calendar),
            "simple-uid_1.2~x.ics"
        );
        assert_eq!(
            resource_name("  abc@google.com ", ObjectKind::Calendar),
            "abc@google.com.ics"
        );
        let odd = resource_name("{weird}/uid?", ObjectKind::Contact);
        assert!(odd.starts_with("_weird__uid_-"), "{odd}");
        assert!(odd.ends_with(".vcf"));
        assert_eq!(odd.len(), "_weird__uid_-".len() + 16 + ".vcf".len());
        // Distinct UIDs that sanitise identically still get distinct names.
        assert_ne!(
            resource_name("a/b", ObjectKind::Calendar),
            resource_name("a?b", ObjectKind::Calendar)
        );
        // Hidden files are avoided.
        assert!(resource_name(".hidden", ObjectKind::Calendar).starts_with("_hidden-"));
        // Long UIDs are shortened.
        let long = resource_name(&"x".repeat(500), ObjectKind::Calendar);
        assert_eq!(long.len(), MAX_NAME_LEN + 1 + 16 + 4);
        // Non-ASCII is replaced.
        assert!(resource_name("événement", ObjectKind::Calendar).starts_with("_v_nement-"));
        // Empty UIDs get a random name.
        let random = resource_name("", ObjectKind::Calendar);
        assert_eq!(random.len(), 36 + 4);
        assert_ne!(random, resource_name("", ObjectKind::Calendar));
    }

    #[test]
    fn object_hrefs() {
        assert_eq!(
            object_href(
                "https://dav.example.com/cal/work/",
                "e1",
                ObjectKind::Calendar
            )
            .unwrap(),
            "https://dav.example.com/cal/work/e1.ics"
        );
        assert_eq!(
            object_href(
                "https://dav.example.com/cal/work",
                "jane@example.com",
                ObjectKind::Contact
            )
            .unwrap(),
            "https://dav.example.com/cal/work/jane@example.com.vcf"
        );
        assert_eq!(
            object_href(
                "https://dav.example.com/cal/My%20Cal/",
                "e1",
                ObjectKind::Calendar
            )
            .unwrap(),
            "https://dav.example.com/cal/My%20Cal/e1.ics"
        );
        assert!(object_href("/cal/work/", "e1", ObjectKind::Calendar).is_err());
    }

    #[test]
    fn kind_detection() {
        assert_eq!(
            ObjectKind::detect("BEGIN:VCALENDAR\r\n"),
            ObjectKind::Calendar
        );
        assert_eq!(
            ObjectKind::detect("\u{feff}\r\nbegin:vcard\r\n"),
            ObjectKind::Contact
        );
        assert_eq!(ObjectKind::detect(""), ObjectKind::Calendar);
        assert_eq!(ObjectKind::Contact.extension(), "vcf");
        assert_eq!(
            ObjectKind::Calendar.content_type(),
            "text/calendar; charset=utf-8"
        );
    }

    #[test]
    fn public_normalisation() {
        assert_eq!(
            normalize_href("https://dav.example.com/a%40b/").unwrap(),
            "https://dav.example.com/a@b/"
        );
        assert!(normalize_href("/relative").is_err());
        assert!(normalize_href("mailto:x@example.com").is_err());
    }
}
