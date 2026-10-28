//! Well-known CalDAV / CardDAV endpoints.
//!
//! Hosted providers have fixed entry points; feeding them to
//! [`DavClient::discover_from`](crate::DavClient::discover_from) skips the
//! guesswork and is required where calendars and contacts live on different
//! hosts (iCloud, Yahoo, Fastmail).
//!
//! | Provider  | CalDAV                                              | CardDAV                                         | Credentials |
//! |-----------|-----------------------------------------------------|-------------------------------------------------|-------------|
//! | iCloud    | `https://caldav.icloud.com`                         | `https://contacts.icloud.com`                   | app-specific password |
//! | Google    | `https://apidata.googleusercontent.com/caldav/v2/`  | `https://www.googleapis.com/.well-known/carddav` | OAuth 2 (bearer) |
//! | Yahoo     | `https://caldav.calendar.yahoo.com`                 | `https://carddav.address.yahoo.com`             | app password |
//! | Fastmail  | `https://caldav.fastmail.com`                       | `https://carddav.fastmail.com`                  | app password |
//!
//! iCloud answers with home sets on per-account hosts (`pNN-caldav.icloud.com`,
//! `pNN-contacts.icloud.com`); the client follows them transparently.
//!
//! Self-hosted servers are found through `/.well-known/caldav` when the
//! administrator configured it, otherwise through the paths listed in
//! [`SELF_HOSTED_SERVERS`], which discovery probes automatically.

/// How a provider expects the client to authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderAuth {
    /// The regular account password.
    Password,
    /// An application-specific password generated in the account settings.
    AppPassword,
    /// OAuth 2 access tokens ([`Auth::Bearer`](crate::Auth::Bearer)).
    OAuth2,
}

/// A hosted provider with fixed CalDAV / CardDAV endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    /// Stable identifier (`icloud`, `google`, ...).
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Where CalDAV discovery should start.
    pub caldav_url: &'static str,
    /// Where CardDAV discovery should start.
    pub carddav_url: &'static str,
    /// Expected credentials.
    pub auth: ProviderAuth,
    /// E-mail domains belonging to the provider.
    pub email_domains: &'static [&'static str],
    /// Host names (or parent domains) of the provider's DAV servers.
    pub hosts: &'static [&'static str],
}

/// The hosted providers Envelope knows about.
pub const KNOWN_PROVIDERS: &[Provider] = &[
    Provider {
        id: "icloud",
        name: "iCloud",
        caldav_url: "https://caldav.icloud.com/",
        carddav_url: "https://contacts.icloud.com/",
        auth: ProviderAuth::AppPassword,
        email_domains: &["icloud.com", "me.com", "mac.com"],
        hosts: &["icloud.com"],
    },
    Provider {
        id: "google",
        name: "Google",
        caldav_url: "https://apidata.googleusercontent.com/caldav/v2/",
        carddav_url: "https://www.googleapis.com/.well-known/carddav",
        auth: ProviderAuth::OAuth2,
        email_domains: &["gmail.com", "googlemail.com"],
        hosts: &[
            "apidata.googleusercontent.com",
            "googleapis.com",
            "google.com",
        ],
    },
    Provider {
        id: "yahoo",
        name: "Yahoo",
        caldav_url: "https://caldav.calendar.yahoo.com/",
        carddav_url: "https://carddav.address.yahoo.com/",
        auth: ProviderAuth::AppPassword,
        email_domains: &[
            "yahoo.com",
            "ymail.com",
            "rocketmail.com",
            "yahoo.co.uk",
            "yahoo.co.jp",
            "yahoo.fr",
            "yahoo.de",
            "yahoo.it",
            "yahoo.es",
            "yahoo.ca",
            "yahoo.com.au",
            "yahoo.com.br",
            "yahoo.co.in",
        ],
        hosts: &["yahoo.com"],
    },
    Provider {
        id: "fastmail",
        name: "Fastmail",
        caldav_url: "https://caldav.fastmail.com/",
        carddav_url: "https://carddav.fastmail.com/",
        auth: ProviderAuth::AppPassword,
        email_domains: &["fastmail.com", "fastmail.fm", "messagingengine.com"],
        hosts: &["fastmail.com", "messagingengine.com"],
    },
];

/// The provider hosting the given e-mail address, if it is a known one.
///
/// ```
/// let provider = envelope_dav::provider_for_email("Jane@iCloud.com").unwrap();
/// assert_eq!(provider.caldav_url, "https://caldav.icloud.com/");
/// assert!(envelope_dav::provider_for_email("jane@example.com").is_none());
/// ```
pub fn provider_for_email(email: &str) -> Option<&'static Provider> {
    let domain = email.trim().rsplit_once('@')?.1.trim_end_matches('.');
    KNOWN_PROVIDERS.iter().find(|provider| {
        provider
            .email_domains
            .iter()
            .any(|known| known.eq_ignore_ascii_case(domain))
    })
}

/// The provider operating the given server URL or host, if it is a known
/// one (for example `p07-caldav.icloud.com` belongs to iCloud).
pub fn provider_for_host(url_or_host: &str) -> Option<&'static Provider> {
    let trimmed = url_or_host.trim();
    let without_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host_port
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    KNOWN_PROVIDERS.iter().find(|provider| {
        provider
            .hosts
            .iter()
            .any(|known| host == *known || host.ends_with(&format!(".{known}")))
    })
}

/// Paths used by a family of self-hosted servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPaths {
    /// Server software.
    pub name: &'static str,
    /// Path of the CalDAV context root.
    pub caldav_path: &'static str,
    /// Path of the CardDAV context root.
    pub carddav_path: &'static str,
}

/// Context roots of popular self-hosted servers, probed by discovery when the
/// well-known URIs are not configured.
///
/// * Nextcloud / ownCloud: `/remote.php/dav/` (principal
///   `/remote.php/dav/principals/users/<user>/`).
/// * Baïkal: `/dav.php/` (older releases `/cal.php/` and `/card.php/`).
/// * SOGo: `/SOGo/dav/` (principal `/SOGo/dav/<user>/`).
/// * Zimbra and Kerio: `/dav/`.
/// * Synology Calendar / Contacts: `/caldav/` and `/carddav/` on the DSM port.
/// * DAViCal: `/caldav.php/`.
/// * Radicale serves principals at `/<user>/` from the root, which is always
///   probed.
pub const SELF_HOSTED_SERVERS: &[ServerPaths] = &[
    ServerPaths {
        name: "Nextcloud / ownCloud",
        caldav_path: "/remote.php/dav/",
        carddav_path: "/remote.php/dav/",
    },
    ServerPaths {
        name: "Baïkal",
        caldav_path: "/dav.php/",
        carddav_path: "/dav.php/",
    },
    ServerPaths {
        name: "SOGo",
        caldav_path: "/SOGo/dav/",
        carddav_path: "/SOGo/dav/",
    },
    ServerPaths {
        name: "Zimbra / Kerio",
        caldav_path: "/dav/",
        carddav_path: "/dav/",
    },
    ServerPaths {
        name: "Synology",
        caldav_path: "/caldav/",
        carddav_path: "/carddav/",
    },
    ServerPaths {
        name: "DAViCal",
        caldav_path: "/caldav.php/",
        carddav_path: "/caldav.php/",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_by_email() {
        assert_eq!(
            provider_for_email("jane@me.com").map(|p| p.id),
            Some("icloud")
        );
        assert_eq!(
            provider_for_email(" jane@GMAIL.com ").map(|p| p.id),
            Some("google")
        );
        assert_eq!(
            provider_for_email("jane@yahoo.co.uk").map(|p| p.id),
            Some("yahoo")
        );
        assert_eq!(
            provider_for_email("jane@fastmail.fm").map(|p| p.id),
            Some("fastmail")
        );
        assert_eq!(provider_for_email("not-an-email"), None);
        assert_eq!(provider_for_email("jane@example.com"), None);
    }

    #[test]
    fn providers_by_host() {
        assert_eq!(
            provider_for_host("https://p07-caldav.icloud.com:443/123/calendars/").map(|p| p.id),
            Some("icloud")
        );
        assert_eq!(
            provider_for_host("contacts.icloud.com").map(|p| p.id),
            Some("icloud")
        );
        assert_eq!(
            provider_for_host("https://apidata.googleusercontent.com/caldav/v2/").map(|p| p.id),
            Some("google")
        );
        assert_eq!(
            provider_for_host("https://caldav.calendar.yahoo.com").map(|p| p.id),
            Some("yahoo")
        );
        assert_eq!(
            provider_for_host("https://jane@caldav.fastmail.com/dav/").map(|p| p.id),
            Some("fastmail")
        );
        assert_eq!(provider_for_host("https://notfastmail.com/"), None);
        assert_eq!(
            provider_for_host("https://cloud.example.com/remote.php/dav"),
            None
        );
        assert_eq!(provider_for_host(""), None);
    }

    #[test]
    fn provider_urls_are_valid() {
        for provider in KNOWN_PROVIDERS {
            for url in [provider.caldav_url, provider.carddav_url] {
                let parsed = url::Url::parse(url).expect("valid provider URL");
                assert_eq!(parsed.scheme(), "https");
                assert_eq!(
                    provider_for_host(url).map(|p| p.id),
                    Some(provider.id),
                    "{url}"
                );
            }
        }
        for server in SELF_HOSTED_SERVERS {
            assert!(server.caldav_path.starts_with('/') && server.caldav_path.ends_with('/'));
            assert!(server.carddav_path.starts_with('/') && server.carddav_path.ends_with('/'));
        }
    }
}
