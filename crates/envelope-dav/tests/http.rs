//! End-to-end tests of the public API against a scripted HTTP server: server
//! behaviour modelled on iCloud, Google, Nextcloud, Baïkal and Radicale.

mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use envelope_dav::{
    Auth, CalendarPatch, DavClient, DavError, NewCalendar, ObjectKind, Precondition, ServiceType,
    TokenSource, object_href,
};
use md5::{Digest, Md5};
use support::{MockServer, Reply, fixture};

fn client(server: &MockServer, auth: Auth) -> DavClient {
    DavClient::builder(&server.origin())
        .auth(auth)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client")
}

const SIMPLE_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Envelope//Tests//EN\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTAMP:20240101T000000Z\r\nDTSTART:20240105T100000Z\r\nSUMMARY:Test\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

// ---------------------------------------------------------------- discovery

#[tokio::test]
async fn nextcloud_discovery_follows_the_well_known_redirect_keeping_propfind() {
    let server = MockServer::start(|req| match (req.method.as_str(), req.path()) {
        ("PROPFIND", "/.well-known/caldav") => Reply::redirect(301, "/remote.php/dav/"),
        ("PROPFIND", "/remote.php/dav/") => {
            Reply::multistatus(fixture("nextcloud_current_user_principal.xml"))
        }
        ("PROPFIND", "/remote.php/dav/principals/users/jane/") => {
            Reply::multistatus(fixture("nextcloud_principal.xml"))
        }
        _ => Reply::new(404),
    })
    .await;
    let client = client(&server, Auth::basic("jane", "secret"));

    let discovery = client
        .discover(ServiceType::CalDav)
        .await
        .expect("discovery");
    assert_eq!(discovery.service, ServiceType::CalDav);
    assert_eq!(discovery.context_url, server.url("/remote.php/dav/"));
    assert_eq!(
        discovery.principal,
        server.url("/remote.php/dav/principals/users/jane/")
    );
    assert_eq!(
        discovery.calendar_home_set,
        [server.url("/remote.php/dav/calendars/jane/")]
    );
    assert_eq!(
        discovery.home_set(),
        [server.url("/remote.php/dav/calendars/jane/")]
    );
    assert_eq!(
        discovery.addressbook_home_set,
        [server.url("/remote.php/dav/addressbooks/users/jane/")]
    );
    assert_eq!(discovery.calendar_user_addresses, ["jane@example.com"]);
    assert_eq!(discovery.display_name.as_deref(), Some("Jane Doe"));

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].path(), "/.well-known/caldav");
    // The redirect target received the very same PROPFIND, not a GET.
    assert_eq!(requests[1].method, "PROPFIND");
    assert_eq!(requests[1].path(), "/remote.php/dav/");
    assert_eq!(requests[1].body, requests[0].body);
    assert_eq!(requests[1].header("depth"), Some("0"));
    assert!(requests[1].body.contains("current-user-principal"));
    // Loopback server: Basic credentials are sent pre-emptively everywhere.
    for request in &requests {
        assert_eq!(
            request.header("authorization"),
            Some("Basic amFuZTpzZWNyZXQ=")
        );
        assert!(
            request
                .header("user-agent")
                .unwrap_or_default()
                .starts_with("Envelope/")
        );
    }
}

#[tokio::test]
async fn icloud_discovery_follows_home_sets_to_another_host() {
    let home_server = MockServer::start(|req| match (req.method.as_str(), req.path()) {
        ("PROPFIND", "/1234567890/calendars/") => {
            Reply::multistatus(fixture("icloud_calendars.xml"))
        }
        _ => Reply::new(404),
    })
    .await;
    let home_origin = home_server.origin();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path()) {
        ("PROPFIND", "/.well-known/caldav") => Reply::redirect(301, "/"),
        ("PROPFIND", "/") => Reply::multistatus(fixture("icloud_current_user_principal.xml")),
        ("PROPFIND", "/1234567890/principal/") => Reply::multistatus(
            fixture("icloud_principal.xml").replace("{{HOME_ORIGIN}}", &home_origin),
        ),
        _ => Reply::new(404),
    })
    .await;
    let client = client(
        &server,
        Auth::basic("jane.appleseed@icloud.com", "abcd-efgh-ijkl-mnop"),
    );

    let discovery = client
        .discover(ServiceType::CalDav)
        .await
        .expect("discovery");
    assert_eq!(discovery.principal, server.url("/1234567890/principal/"));
    assert_eq!(
        discovery.calendar_home_set,
        [home_server.url("/1234567890/calendars/")]
    );
    assert!(discovery.addressbook_home_set.is_empty());
    assert_eq!(
        discovery.calendar_user_addresses,
        ["jane@example.com", "jane.appleseed@icloud.com"]
    );
    assert_eq!(discovery.display_name.as_deref(), Some("Jane Appleseed"));

    let calendars = client
        .list_calendars(&discovery.calendar_home_set[0])
        .await
        .expect("calendars");
    let names: Vec<_> = calendars
        .iter()
        .filter_map(|c| c.display_name.as_deref())
        .collect();
    assert_eq!(names, ["Home", "Work", "Reminders", "Shared with me"]);
    assert!(
        calendars
            .iter()
            .all(|c| c.href.starts_with(&home_server.origin()))
    );
    assert_eq!(calendars[1].description.as_deref(), Some("Office & travel"));

    let listing = &home_server.requests()[0];
    assert_eq!(listing.method, "PROPFIND");
    assert_eq!(listing.header("depth"), Some("1"));
    assert_eq!(listing.header("prefer"), Some("return=minimal"));
    assert!(
        listing
            .header("authorization")
            .unwrap_or_default()
            .starts_with("Basic ")
    );

    // This principal offers no address books.
    let err = client.discover(ServiceType::CardDav).await.unwrap_err();
    assert!(
        matches!(
            err,
            DavError::ServiceNotFound {
                service: ServiceType::CardDav,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn google_carddav_discovery_uses_bearer_tokens_across_redirects() {
    let server = MockServer::start(|req| {
        if req.header("authorization") != Some("Bearer token-0") {
            return Reply::new(401).header("WWW-Authenticate", "Bearer realm=\"https://accounts.google.com/\"");
        }
        match (req.method.as_str(), req.path()) {
            ("PROPFIND", "/.well-known/carddav") => {
                Reply::redirect(301, "/carddav/v1/principals/jane.doe@gmail.com/lists/default/")
            }
            ("PROPFIND", "/carddav/v1/principals/jane.doe@gmail.com/lists/default/") => Reply::multistatus(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<d:multistatus xmlns:d="DAV:"><d:response><d:href>/carddav/v1/principals/jane.doe%40gmail.com/lists/default/</d:href><d:propstat><d:status>HTTP/1.1 200 OK</d:status><d:prop><d:current-user-principal><d:href>/carddav/v1/principals/jane.doe%40gmail.com/</d:href></d:current-user-principal></d:prop></d:propstat></d:response></d:multistatus>"#,
            ),
            ("PROPFIND", "/carddav/v1/principals/jane.doe@gmail.com/") => Reply::multistatus(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/carddav/v1/principals/jane.doe%40gmail.com/</d:href><d:propstat><d:status>HTTP/1.1 200 OK</d:status><d:prop><card:addressbook-home-set><d:href>/carddav/v1/principals/jane.doe%40gmail.com/lists/</d:href></card:addressbook-home-set><d:displayname>jane.doe@gmail.com</d:displayname></d:prop></d:propstat></d:response></d:multistatus>"#,
            ),
            _ => Reply::new(404),
        }
    })
    .await;
    let tokens = Arc::new(CountingTokens::default());
    let client = client(&server, Auth::Bearer(tokens.clone()));

    let discovery = client
        .discover_from(&server.url("/.well-known/carddav"), ServiceType::CardDav)
        .await
        .expect("discovery");
    assert_eq!(
        discovery.addressbook_home_set,
        [server.url("/carddav/v1/principals/jane.doe@gmail.com/lists/")]
    );
    assert_eq!(
        discovery.principal,
        server.url("/carddav/v1/principals/jane.doe@gmail.com/")
    );
    assert!(discovery.calendar_user_addresses.is_empty());
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 0);
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn discovery_reports_unreachable_and_unknown_services() {
    let server = MockServer::start(|_| Reply::new(404)).await;
    let client = client(&server, Auth::None);
    let err = client.discover(ServiceType::CalDav).await.unwrap_err();
    assert!(matches!(err, DavError::ServiceNotFound { .. }), "{err:?}");
    // Every candidate was probed once, with PROPFIND.
    let requests = server.requests();
    assert!(requests.len() >= 3);
    assert!(requests.iter().all(|r| r.method == "PROPFIND"));

    // Nothing listens on this port: fail fast with a network error.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port")
        .port();
    let closed = DavClient::builder(&format!("http://127.0.0.1:{port}"))
        .connect_timeout(Duration::from_secs(2))
        .build()
        .expect("client");
    let err = closed.discover(ServiceType::CalDav).await.unwrap_err();
    assert!(matches!(err, DavError::Network(_)), "{err:?}");
    assert!(err.is_transient());
}

/// Context path, principal as reported (possibly percent-encoded), principal
/// as requested, calendar home and address book home of a server layout.
type Layout = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

#[tokio::test]
async fn self_hosted_servers_are_found_without_well_known_uris() {
    let layouts: [(&str, Layout); 4] = [
        (
            "Baïkal",
            (
                "/dav.php/",
                "/dav.php/principals/jane/",
                "/dav.php/principals/jane/",
                "/dav.php/calendars/jane/",
                Some("/dav.php/addressbooks/jane/"),
            ),
        ),
        (
            "SOGo",
            (
                "/SOGo/dav/",
                "/SOGo/dav/jane/",
                "/SOGo/dav/jane/",
                "/SOGo/dav/jane/Calendar/",
                Some("/SOGo/dav/jane/Contacts/"),
            ),
        ),
        (
            "Zimbra",
            (
                "/dav/",
                "/principals/users/jane%40example.com/",
                "/principals/users/jane@example.com/",
                "/dav/jane@example.com/",
                Some("/dav/jane@example.com/"),
            ),
        ),
        (
            "Synology",
            (
                "/caldav/",
                "/caldav/jane/",
                "/caldav/jane/",
                "/caldav/jane/",
                None,
            ),
        ),
    ];
    for (name, (context, reported, principal, calendar_home, addressbook_home)) in layouts {
        let server = MockServer::start(move |req| {
            if req.method != "PROPFIND" {
                return Reply::new(405);
            }
            if req.path() == context {
                return Reply::multistatus(format!(
                    r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>{context}</D:href><D:propstat><D:prop><D:current-user-principal><D:href>{reported}</D:href></D:current-user-principal></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#
                ));
            }
            if req.path() == principal {
                let addressbooks = addressbook_home
                    .map(|home| format!("<CR:addressbook-home-set><D:href>{home}</D:href></CR:addressbook-home-set>"))
                    .unwrap_or_default();
                return Reply::multistatus(format!(
                    r#"<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CR="urn:ietf:params:xml:ns:carddav"><D:response><D:href>{reported}</D:href><D:propstat><D:prop><C:calendar-home-set><D:href>{calendar_home}</D:href></C:calendar-home-set>{addressbooks}<C:calendar-user-address-set><D:href>mailto:jane@example.com</D:href></C:calendar-user-address-set></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#
                ));
            }
            Reply::new(404)
        })
        .await;
        let client = client(&server, Auth::basic("jane", "pw"));
        let discovery = client
            .discover(ServiceType::CalDav)
            .await
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(discovery.context_url, server.url(context), "{name}");
        assert_eq!(discovery.principal, server.url(principal), "{name}");
        assert_eq!(
            discovery.calendar_home_set,
            [server.url(calendar_home)],
            "{name}"
        );
        assert_eq!(
            discovery.addressbook_home_set,
            addressbook_home
                .map(|home| server.url(home))
                .into_iter()
                .collect::<Vec<_>>(),
            "{name}"
        );
        assert_eq!(
            discovery.calendar_user_addresses,
            ["jane@example.com"],
            "{name}"
        );
        // Well-known URI and root were tried first.
        let probed: Vec<String> = server
            .requests()
            .iter()
            .map(|r| r.path().to_owned())
            .collect();
        assert_eq!(probed[..2], ["/.well-known/caldav", "/"], "{name}");
    }
}

// ------------------------------------------------------------ authentication

#[tokio::test]
async fn wrong_basic_credentials_fail_without_retrying() {
    let server = MockServer::start(|_| {
        Reply::new(401).header(
            "WWW-Authenticate",
            "Basic realm=\"Radicale - Password Required\"",
        )
    })
    .await;
    let client = client(&server, Auth::basic("jane", "wrong"));
    let err = client.list_etags("/jane/work/").await.unwrap_err();
    assert!(matches!(err, DavError::Unauthorized), "{err:?}");
    assert_eq!(server.requests().len(), 1);
}

const DIGEST_REALM: &str = "BaikalDAV";
const DIGEST_PASSWORD: &str = "s3cret";

fn md5_hex(input: &str) -> String {
    Md5::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn digest_params(header: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let rest = header.strip_prefix("Digest ").unwrap_or_default();
    for part in rest.split(", ") {
        if let Some((key, value)) = part.split_once('=') {
            params.insert(key.to_owned(), value.trim_matches('"').to_owned());
        }
    }
    params
}

/// Validates a Digest `Authorization` header like sabre/dav does.
fn digest_is_valid(method: &str, header: &str, nonce: &str) -> bool {
    let p = digest_params(header);
    let get = |key: &str| p.get(key).cloned().unwrap_or_default();
    if get("nonce") != nonce || get("realm") != DIGEST_REALM || get("qop") != "auth" {
        return false;
    }
    let ha1 = md5_hex(&format!(
        "{}:{DIGEST_REALM}:{DIGEST_PASSWORD}",
        get("username")
    ));
    let ha2 = md5_hex(&format!("{method}:{}", get("uri")));
    let expected = md5_hex(&format!(
        "{ha1}:{nonce}:{}:{}:auth:{ha2}",
        get("nc"),
        get("cnonce")
    ));
    get("response") == expected && get("opaque") == "0p4qu3"
}

#[tokio::test]
async fn digest_authentication_is_negotiated_reused_and_renewed() {
    // After two successful requests the server expires the first nonce.
    let successes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&successes);
    let server = MockServer::start(move |req| {
        let nonce = if counter.load(Ordering::SeqCst) < 2 { "nonce-one" } else { "nonce-two" };
        let challenge = |stale: bool| {
            Reply::new(401).header(
                "WWW-Authenticate",
                &format!(
                    "Digest realm=\"{DIGEST_REALM}\", qop=\"auth\", algorithm=MD5, nonce=\"{nonce}\", opaque=\"0p4qu3\"{}",
                    if stale { ", stale=TRUE" } else { "" }
                ),
            )
        };
        let Some(header) = req.header("authorization") else {
            return challenge(false);
        };
        if !header.starts_with("Digest ") {
            return challenge(false);
        }
        if !digest_is_valid(&req.method, header, nonce) {
            let stale = digest_params(header).get("nonce").is_some_and(|n| n != nonce);
            return challenge(stale);
        }
        counter.fetch_add(1, Ordering::SeqCst);
        Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav.php/calendars/jane/default/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/dav.php/calendars/jane/default/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
        )
    })
    .await;
    let client = client(&server, Auth::basic("jane", DIGEST_PASSWORD));

    for _ in 0..3 {
        let members = client
            .list_etags("/dav.php/calendars/jane/default/")
            .await
            .expect("listing");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].etag.as_deref(), Some("\"1\""));
    }
    assert_eq!(successes.load(Ordering::SeqCst), 3);

    let requests = server.requests();
    let auth: Vec<String> = requests
        .iter()
        .map(|r| r.header("authorization").unwrap_or_default().to_owned())
        .collect();
    assert_eq!(requests.len(), 5, "{auth:#?}");
    // 1: pre-emptive Basic, rejected with a Digest challenge.
    assert!(auth[0].starts_with("Basic "));
    // 2-3: Digest with the first nonce and an increasing nonce count.
    assert_eq!(digest_params(&auth[1])["nc"], "00000001");
    assert_eq!(digest_params(&auth[2])["nc"], "00000002");
    assert_eq!(digest_params(&auth[2])["nonce"], "nonce-one");
    // 4: the old nonce is now stale; 5: retried with the new one.
    assert_eq!(digest_params(&auth[3])["nonce"], "nonce-one");
    assert_eq!(digest_params(&auth[4])["nonce"], "nonce-two");
    assert_eq!(digest_params(&auth[4])["nc"], "00000001");
    assert_eq!(
        digest_params(&auth[4])["uri"],
        "/dav.php/calendars/jane/default/"
    );
}

#[tokio::test]
async fn wrong_digest_credentials_fail_after_one_digest_attempt() {
    let server = MockServer::start(|req| {
        let header = req.header("authorization").unwrap_or_default();
        if header.starts_with("Digest ") && digest_is_valid(&req.method, header, "n") {
            return Reply::new(200);
        }
        Reply::new(401).header(
            "WWW-Authenticate",
            &format!(
                "Digest realm=\"{DIGEST_REALM}\", qop=\"auth\", nonce=\"n\", opaque=\"0p4qu3\""
            ),
        )
    })
    .await;
    let client = client(&server, Auth::basic("jane", "not-the-password"));
    let err = client
        .delete("/dav.php/calendars/jane/default/a.ics", None)
        .await
        .unwrap_err();
    assert!(matches!(err, DavError::Unauthorized), "{err:?}");
    assert_eq!(server.requests().len(), 2);
}

#[derive(Default)]
struct CountingTokens {
    generation: AtomicUsize,
    invalidations: AtomicUsize,
}

#[async_trait]
impl TokenSource for CountingTokens {
    async fn access_token(&self) -> Result<String, String> {
        Ok(format!("token-{}", self.generation.load(Ordering::SeqCst)))
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn rejected_bearer_tokens_are_refreshed_once() {
    let server = MockServer::start(|req| match req.header("authorization") {
        Some("Bearer token-1") => Reply::new(204),
        _ => Reply::new(401).header("WWW-Authenticate", "Bearer error=\"invalid_token\""),
    })
    .await;
    let tokens = Arc::new(CountingTokens::default());
    let client = client(&server, Auth::Bearer(tokens.clone()));

    client
        .delete("/caldav/v2/jane/events/a.ics", None)
        .await
        .expect("delete");
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
    let auth: Vec<_> = server
        .requests()
        .iter()
        .map(|r| r.header("authorization").unwrap_or_default().to_owned())
        .collect();
    assert_eq!(auth, ["Bearer token-0", "Bearer token-1"]);

    // token-2 is rejected too: one refresh, then give up.
    client
        .delete("/caldav/v2/jane/events/a.ics", None)
        .await
        .expect("still valid");
    let server = MockServer::start(|_| Reply::new(401)).await;
    let tokens = Arc::new(CountingTokens::default());
    let client = DavClient::builder(&server.origin())
        .auth(Auth::Bearer(tokens.clone()))
        .build()
        .expect("client");
    let err = client.delete("/a.ics", None).await.unwrap_err();
    assert!(matches!(err, DavError::Unauthorized), "{err:?}");
    assert_eq!(server.requests().len(), 2);
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
}

struct FailingTokens;

#[async_trait]
impl TokenSource for FailingTokens {
    async fn access_token(&self) -> Result<String, String> {
        Err("refresh token revoked".into())
    }

    async fn invalidate(&self) {}
}

#[tokio::test]
async fn token_source_failures_are_reported() {
    let server = MockServer::start(|_| Reply::new(204)).await;
    let client = client(&server, Auth::bearer(FailingTokens));
    let err = client.delete("/a.ics", None).await.unwrap_err();
    match err {
        DavError::TokenSource(message) => assert_eq!(message, "refresh token revoked"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(server.requests().is_empty());
}

// ---------------------------------------------------------------------- sync

#[tokio::test]
async fn truncated_sync_results_are_continued_and_merged() {
    let server = MockServer::start(|req| {
        assert_eq!(req.method, "REPORT");
        if req.body.contains("<D:sync-token/>") {
            Reply::multistatus(fixture("icloud_sync_truncated.xml"))
        } else if req
            .body
            .contains("HwoQEgwAAAh4j8A2AAAAAAAAAAAAAAAAGAAgCB==")
        {
            Reply::multistatus(fixture("icloud_sync_continued.xml"))
        } else {
            Reply::new(500)
        }
    })
    .await;
    let client = client(&server, Auth::basic("jane", "pw"));

    let delta = client
        .sync_collection("/1234567890/calendars/home/", None)
        .await
        .expect("sync");
    let changed: Vec<(&str, Option<&str>)> = delta
        .changed
        .iter()
        .map(|o| (o.href.as_str(), o.etag.as_deref()))
        .collect();
    let base = server.url("/1234567890/calendars/home/");
    assert_eq!(
        changed,
        [
            (
                format!("{base}0A1B2C3D-0002.ics").as_str(),
                Some("\"lr8m9zzz\"")
            ),
            (
                format!("{base}0A1B2C3D-0003.ics").as_str(),
                Some("\"lr8m7cn5\"")
            ),
        ]
    );
    assert_eq!(delta.removed, [format!("{base}0A1B2C3D-0001.ics")]);
    assert_eq!(
        delta.sync_token.as_deref(),
        Some("HwoQEgwAAAh4kZ9xAAAAAAAAAAAAAAAAGAAgCB==")
    );
    assert!(!delta.truncated);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.header("depth") == Some("0")));
    assert!(requests[0].body.contains("<D:sync-level>1</D:sync-level>"));
}

#[tokio::test]
async fn sabre_sync_delta_normalises_hrefs() {
    let server =
        MockServer::start(|_| Reply::multistatus(fixture("nextcloud_sync_delta.xml"))).await;
    let client = client(&server, Auth::None);
    let delta = client
        .sync_collection(
            "/remote.php/dav/calendars/jane/personal/",
            Some("http://sabre.io/ns/sync/42"),
        )
        .await
        .expect("sync");
    let base = server.url("/remote.php/dav/calendars/jane/personal/");
    let hrefs: Vec<&str> = delta.changed.iter().map(|o| o.href.as_str()).collect();
    assert_eq!(
        hrefs,
        [
            format!("{base}9F3C1E2A-5B7D-4C8E-A1B2-C3D4E5F6A7B8.ics").as_str(),
            format!("{base}Meeting%20notes%20%E2%80%93%20Q3.ics").as_str(),
        ]
    );
    assert_eq!(
        delta.changed[0].etag.as_deref(),
        Some("\"6c9f0e6c4a8a3b0d2f1e5d7c8b9a0f1e\"")
    );
    assert_eq!(delta.removed, [format!("{base}deleted-event.ics")]);
    assert_eq!(
        delta.sync_token.as_deref(),
        Some("http://sabre.io/ns/sync/43")
    );
    assert!(
        server.requests()[0]
            .body
            .contains("<D:sync-token>http://sabre.io/ns/sync/42</D:sync-token>")
    );
}

#[tokio::test]
async fn stale_sync_tokens_require_a_full_resync() {
    // Radicale: 403 with the RFC 6578 precondition.
    let server = MockServer::start(|_| {
        Reply::new(403)
            .header("Content-Type", "text/xml; charset=utf-8")
            .body("<?xml version='1.0' encoding='utf-8'?>\n<error xmlns=\"DAV:\"><valid-sync-token /></error>")
    })
    .await;
    let client = client(&server, Auth::None);
    let err = client
        .sync_collection("/jane/work/", Some("http://radicale.org/ns/sync/old"))
        .await
        .unwrap_err();
    assert!(matches!(err, DavError::InvalidSyncToken), "{err:?}");
    assert!(err.requires_full_resync());

    // A bare 409 for a tokenised request means the same thing.
    let server = MockServer::start(|_| Reply::new(409)).await;
    let client = self::client(&server, Auth::None);
    let err = client
        .sync_collection("/cal/", Some("token"))
        .await
        .unwrap_err();
    assert!(matches!(err, DavError::InvalidSyncToken), "{err:?}");
}

#[tokio::test]
async fn draft_era_servers_get_depth_one_sync_reports() {
    let server = MockServer::start(|req| match req.header("depth") {
        Some("1") => Reply::multistatus(
            r#"<multistatus xmlns="DAV:"><response><href>/cal/a.ics</href><propstat><prop><getetag>abc</getetag></prop><status>HTTP/1.1 200 OK</status></propstat></response><sync-token>t1</sync-token></multistatus>"#,
        ),
        _ => Reply::new(400),
    })
    .await;
    let client = client(&server, Auth::None);
    let delta = client.sync_collection("/cal/", None).await.expect("sync");
    assert_eq!(delta.changed.len(), 1);
    // Bare entity tags are quoted so they can be used in If-Match.
    assert_eq!(delta.changed[0].etag.as_deref(), Some("\"abc\""));
    assert_eq!(delta.sync_token.as_deref(), Some("t1"));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn etag_listing_fallback_skips_the_collection_and_subcollections() {
    let server = MockServer::start(|_| {
        Reply::multistatus(
            r#"<D:multistatus xmlns:D="DAV:">
              <D:response><D:href>https://caldav.calendar.yahoo.com/dav/jdoe/Calendar/Jane_Doe/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
              <D:response><D:href>/dav/jdoe/Calendar/Jane_Doe/1.ics</D:href><D:propstat><D:prop><D:getetag>"a"</D:getetag><D:resourcetype/></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
              <D:response><D:href>/dav/jdoe/Calendar/Jane_Doe/2.ics</D:href><D:propstat><D:prop><D:getetag>"b"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
              <D:response><D:href>/dav/jdoe/Calendar/Jane_Doe/sub/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
            </D:multistatus>"#,
        )
    })
    .await;
    let client = client(&server, Auth::None);
    let members = client
        .list_etags("/dav/jdoe/Calendar/Jane_Doe/")
        .await
        .expect("listing");
    let hrefs: Vec<_> = members.iter().map(|m| m.href.clone()).collect();
    assert_eq!(
        hrefs,
        [
            server.url("/dav/jdoe/Calendar/Jane_Doe/1.ics"),
            server.url("/dav/jdoe/Calendar/Jane_Doe/2.ics")
        ]
    );
    assert_eq!(server.requests()[0].header("depth"), Some("1"));
}

// ------------------------------------------------------------------ multiget

#[tokio::test]
async fn multiget_is_batched_and_reports_missing_resources() {
    let server = MockServer::start(|req| {
        let mut body = String::from(r#"<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">"#);
        for part in req.body.split("<D:href>").skip(1) {
            let href = part.split("</D:href>").next().unwrap_or_default();
            if href.contains("skipped") {
                continue;
            }
            if href.contains("missing") {
                body.push_str(&format!(
                    "<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
                ));
            } else {
                body.push_str(&format!(
                    "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>\"{href}\"</d:getetag><cal:calendar-data>BEGIN:VCALENDAR&#13;\nX-HREF:{href}&#13;\nEND:VCALENDAR&#13;\n</cal:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                ));
            }
        }
        body.push_str("</d:multistatus>");
        Reply::multistatus(body)
    })
    .await;
    let client = client(&server, Auth::None);

    let mut hrefs: Vec<String> = (0..247).map(|i| format!("/cal/event-{i}.ics")).collect();
    hrefs.push("/cal/missing-1.ics".into());
    hrefs.push("/cal/skipped-1.ics".into());
    hrefs.push(server.url("/cal/event-0.ics")); // duplicate, other spelling
    hrefs.push("/cal/missing-2.ics".into());
    let result = client.multiget("/cal/", &hrefs).await.expect("multiget");

    assert_eq!(result.objects.len(), 247);
    // Explicit 404s first, then hrefs the server silently left out.
    assert_eq!(
        result.missing,
        [
            server.url("/cal/missing-1.ics"),
            server.url("/cal/missing-2.ics"),
            server.url("/cal/skipped-1.ics")
        ]
    );
    let first = &result.objects[0];
    assert_eq!(first.href, server.url("/cal/event-0.ics"));
    assert_eq!(first.etag.as_deref(), Some("\"/cal/event-0.ics\""));
    assert_eq!(
        first.data,
        "BEGIN:VCALENDAR\r\nX-HREF:/cal/event-0.ics\r\nEND:VCALENDAR\r\n"
    );

    let requests = server.requests();
    let batch_sizes: Vec<usize> = requests
        .iter()
        .map(|r| r.body.matches("<D:href>").count())
        .collect();
    assert_eq!(batch_sizes, [100, 100, 50]);
    for request in &requests {
        assert_eq!(request.method, "REPORT");
        assert_eq!(request.path(), "/cal/");
        assert!(request.body.contains("calendar-multiget"));
        assert!(
            request
                .header("content-type")
                .unwrap_or_default()
                .starts_with("application/xml")
        );
    }
}

#[tokio::test]
async fn icloud_multiget_cdata_payloads() {
    let server = MockServer::start(|_| Reply::multistatus(fixture("icloud_multiget.xml"))).await;
    let client = client(&server, Auth::None);
    let hrefs = [
        "/1234567890/calendars/home/0A1B2C3D-0002.ics",
        "/1234567890/calendars/home/0A1B2C3D-0003.ics",
        "/1234567890/calendars/home/0A1B2C3D-0009.ics",
    ];
    let result = client
        .multiget("/1234567890/calendars/home/", &hrefs)
        .await
        .expect("multiget");
    assert_eq!(result.objects.len(), 2);
    assert_eq!(
        result.missing,
        [server.url("/1234567890/calendars/home/0A1B2C3D-0009.ics")]
    );
    let review = &result.objects[0];
    assert_eq!(review.etag.as_deref(), Some("\"lr8m9zzz\""));
    assert!(review.data.starts_with("BEGIN:VCALENDAR\n"));
    assert!(
        review
            .data
            .contains("SUMMARY:Design review <draft> & notes\n")
    );
    assert!(review.data.ends_with("END:VCALENDAR\n"));
}

#[tokio::test]
async fn addressbook_multiget_decodes_escaped_vcards() {
    let server =
        MockServer::start(|_| Reply::multistatus(fixture("nextcloud_addressbook_multiget.xml")))
            .await;
    let client = client(&server, Auth::None);
    let base = "/remote.php/dav/addressbooks/users/jane/contacts/";
    let hrefs = [
        format!("{base}7d0a6a6e-1f0b-4b8e-9a43-2f6a1f0e3c11.vcf"),
        format!("{base}gone.vcf"),
    ];
    let result = client
        .addressbook_multiget(base, &hrefs)
        .await
        .expect("multiget");
    assert_eq!(result.objects.len(), 1);
    assert_eq!(result.missing, [server.url(&format!("{base}gone.vcf"))]);
    let card = &result.objects[0];
    assert_eq!(
        card.etag.as_deref(),
        Some("\"8e3c6a2f1d7b4e5c9a0b1c2d3e4f5a6b\"")
    );
    assert!(card.data.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"));
    assert!(card.data.contains("FN:Ayşe Yılmaz\r\n"));
    assert!(card.data.contains("NOTE:Met at the R&D summit <2024>\r\n"));
    assert!(card.data.ends_with("END:VCARD\r\n"));
    let request = &server.requests()[0];
    assert!(request.body.contains("addressbook-multiget"));
    assert!(request.body.contains("<CR:address-data/>"));
}

// -------------------------------------------------------------------- writes

#[tokio::test]
async fn puts_are_conditional_and_return_etags() {
    let server = MockServer::start(|req| match (req.method.as_str(), req.header("if-match")) {
        ("PUT", None) if req.header("if-none-match") == Some("*") => {
            if req.path().ends_with("exists.ics") {
                Reply::new(412)
            } else {
                Reply::new(201).header("ETag", "\"e1\"")
            }
        }
        ("PUT", Some("\"e1\"")) => Reply::new(204),
        ("PUT", Some(_)) => Reply::new(412),
        ("PUT", None) if req.path().ends_with("conflict.ics") => Reply::new(403).body(
            r#"<d:error xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav"><cal:no-uid-conflict><d:href>/cal/other.ics</d:href></cal:no-uid-conflict></d:error>"#,
        ),
        ("PUT", None) => Reply::new(201),
        ("PROPFIND", _) => Reply::multistatus(format!(
            r#"<multistatus xmlns="DAV:"><response><href>{}</href><propstat><prop><getetag>"e2"</getetag></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#,
            req.path()
        )),
        _ => Reply::new(500),
    })
    .await;
    let client = client(&server, Auth::None);
    let href = object_href(&server.url("/cal/"), "e1", ObjectKind::Calendar).expect("href");
    assert_eq!(href, server.url("/cal/e1.ics"));

    let etag = client
        .put(&href, SIMPLE_EVENT, Precondition::IfNoneMatch)
        .await
        .expect("create");
    assert_eq!(etag.as_deref(), Some("\"e1\""));
    // No ETag header on update: the tag is fetched with PROPFIND.
    let etag = client
        .put(&href, SIMPLE_EVENT, Precondition::IfMatch("\"e1\"".into()))
        .await
        .expect("update");
    assert_eq!(etag.as_deref(), Some("\"e2\""));

    let err = client
        .put(
            &href,
            SIMPLE_EVENT,
            Precondition::IfMatch("\"stale\"".into()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );
    let err = client
        .put("/cal/exists.ics", SIMPLE_EVENT, Precondition::IfNoneMatch)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );
    match client
        .put("/cal/conflict.ics", SIMPLE_EVENT, Precondition::None)
        .await
    {
        Err(DavError::Forbidden { condition, .. }) => {
            assert_eq!(condition.as_deref(), Some("no-uid-conflict"))
        }
        other => panic!("unexpected {other:?}"),
    }
    client
        .put(
            "/book/jane.vcf",
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Jane\r\nEND:VCARD\r\n",
            Precondition::None,
        )
        .await
        .expect("vcard");

    let requests = server.requests();
    assert_eq!(
        requests[0].header("content-type"),
        Some("text/calendar; charset=utf-8")
    );
    assert_eq!(requests[0].body, SIMPLE_EVENT);
    assert_eq!(requests[1].header("if-match"), Some("\"e1\""));
    assert_eq!(requests[2].method, "PROPFIND");
    assert_eq!(requests[2].header("depth"), Some("0"));
    let vcard = requests
        .iter()
        .find(|r| r.path() == "/book/jane.vcf")
        .expect("vcard request");
    assert_eq!(
        vcard.header("content-type"),
        Some("text/vcard; charset=utf-8")
    );
}

#[tokio::test]
async fn deletes_map_statuses() {
    let server = MockServer::start(|req| match (req.path(), req.header("if-match")) {
        ("/cal/a.ics", Some("\"1\"")) => Reply::new(204),
        ("/cal/a.ics", Some(_)) => Reply::new(412),
        ("/cal/old/", None) => Reply::new(204),
        // Partial failure: one member is locked.
        ("/cal/locked/", None) => Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/locked/a.ics</d:href><d:status>HTTP/1.1 423 Locked</d:status></d:response></d:multistatus>"#,
        ),
        // A multistatus without failures is a success.
        ("/cal/fine/", None) => Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/fine/</d:href><d:status>HTTP/1.1 204 No Content</d:status></d:response></d:multistatus>"#,
        ),
        _ => Reply::new(404),
    })
    .await;
    let client = client(&server, Auth::None);
    client
        .delete("/cal/a.ics", Some("\"1\""))
        .await
        .expect("delete");
    // Bare tags are quoted on the wire.
    client
        .delete("/cal/a.ics", Some("1"))
        .await
        .expect("delete");
    let err = client
        .delete("/cal/a.ics", Some("\"2\""))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DavError::PreconditionFailed { .. }),
        "{err:?}"
    );
    let err = client.delete("/cal/missing.ics", None).await.unwrap_err();
    assert!(matches!(err, DavError::NotFound { .. }), "{err:?}");
    match client.delete_collection("/cal/locked/").await {
        Err(DavError::Http {
            status,
            method,
            url,
            ..
        }) => {
            assert_eq!(status, 423);
            assert_eq!(method, "DELETE");
            assert_eq!(url, server.url("/cal/locked/a.ics"));
        }
        other => panic!("unexpected {other:?}"),
    }
    client
        .delete_collection("/cal/fine/")
        .await
        .expect("207 without failures");
    client
        .delete_collection("/cal/old/")
        .await
        .expect("delete collection");
    assert!(server.requests().iter().all(|r| r.method == "DELETE"));
}

#[tokio::test]
async fn get_returns_payload_and_etag() {
    let server = MockServer::start(|req| match req.path() {
        "/cal/a.ics" => Reply::new(200)
            .header("ETag", "\"g1\"")
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(SIMPLE_EVENT),
        _ => Reply::new(404),
    })
    .await;
    let client = client(&server, Auth::None);
    let object = client.get("/cal/a.ics").await.expect("get");
    assert_eq!(object.href, server.url("/cal/a.ics"));
    assert_eq!(object.etag.as_deref(), Some("\"g1\""));
    assert_eq!(object.data, SIMPLE_EVENT);
    assert!(matches!(
        client.get("/cal/b.ics").await,
        Err(DavError::NotFound { .. })
    ));
}

// --------------------------------------------------------------- collections

#[tokio::test]
async fn calendars_are_created_and_updated() {
    let server = MockServer::start(|req| match req.method.as_str() {
        "MKCALENDAR" => Reply::new(201),
        "PROPPATCH" if req.body.contains("calendar-order") => Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:x1="http://apple.com/ns/ical/"><d:response><d:href>/cal/x/</d:href>
                 <d:propstat><d:prop><x1:calendar-order/></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status></d:propstat>
                 <d:propstat><d:prop><d:displayname/></d:prop><d:status>HTTP/1.1 424 Failed Dependency</d:status></d:propstat>
               </d:response></d:multistatus>"#,
        ),
        "PROPPATCH" => Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:x1="http://apple.com/ns/ical/"><d:response><d:href>/cal/x/</d:href><d:propstat><d:prop><d:displayname/><x1:calendar-color/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
        ),
        _ => Reply::new(405),
    })
    .await;
    let client = client(&server, Auth::None);

    let href = client
        .create_calendar(
            "/calendars/jane/",
            &NewCalendar::new("Work").with_color("#0082c9"),
        )
        .await
        .expect("mkcalendar");
    assert!(href.starts_with(&server.url("/calendars/jane/")));
    assert!(href.ends_with('/'));
    let mkcalendar = &server.requests()[0];
    assert_eq!(mkcalendar.method, "MKCALENDAR");
    assert!(
        mkcalendar
            .body
            .contains("<D:displayname>Work</D:displayname>")
    );
    assert!(
        mkcalendar
            .body
            .contains("<I:calendar-color>#0082C9FF</I:calendar-color>")
    );
    assert!(mkcalendar.body.contains(r#"<C:comp name="VEVENT"/>"#));

    let patch = CalendarPatch {
        display_name: Some("Work & travel".into()),
        color: Some("#FF2968".into()),
        ..CalendarPatch::default()
    };
    client
        .update_calendar(&href, &patch)
        .await
        .expect("proppatch");
    let proppatch = &server.requests()[1];
    assert!(
        proppatch
            .body
            .contains("<D:displayname>Work &amp; travel</D:displayname>")
    );
    assert!(proppatch.body.contains("#FF2968FF"));

    let patch = CalendarPatch {
        display_name: Some("x".into()),
        order: Some(3),
        ..CalendarPatch::default()
    };
    match client.update_calendar(&href, &patch).await {
        Err(DavError::PropertyUpdate { property, status }) => {
            assert_eq!(property, "calendar-order");
            assert_eq!(status, 403);
        }
        other => panic!("unexpected {other:?}"),
    }
    // Nothing to change: no request at all.
    client
        .update_calendar(&href, &CalendarPatch::default())
        .await
        .expect("no-op");
    assert_eq!(server.requests().len(), 3);

    let err = client
        .create_calendar(
            "/calendars/jane/",
            &NewCalendar::new("Bad").with_color("teal"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DavError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn time_range_queries_return_objects() {
    let server = MockServer::start(|req| {
        assert!(req.body.contains(r#"<C:time-range start="20240101T000000Z" end="20240201T000000Z"/>"#));
        Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/cal/a.ics</d:href><d:propstat><d:prop><d:getetag>"q1"</d:getetag><cal:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR</cal:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
        )
    })
    .await;
    let client = client(&server, Auth::None);
    let start = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap();
    let objects = client
        .query_time_range("/cal/", start, end)
        .await
        .expect("query");
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].etag.as_deref(), Some("\"q1\""));
    assert_eq!(objects[0].data, "BEGIN:VCALENDAR\nEND:VCALENDAR\n");
    assert_eq!(server.requests()[0].header("depth"), Some("1"));
    assert!(matches!(
        client.query_time_range("/cal/", end, start).await,
        Err(DavError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn task_listings_ask_only_for_vtodos() {
    let server = MockServer::start(|req| {
        assert_eq!(req.method, "REPORT");
        assert!(req.body.contains(r#"<C:comp-filter name="VTODO"/>"#));
        Reply::multistatus(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/</d:href><d:propstat><d:prop><d:getetag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
               <d:response><d:href>/cal/t1.ics</d:href><d:propstat><d:prop><d:getetag>"t1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
               <d:response><d:href>/cal/t%202.ics</d:href><d:propstat><d:prop><d:getetag>W/"t2"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
        )
    })
    .await;
    let client = client(&server, Auth::None);
    let tasks = client.list_todo_etags("/cal/").await.expect("listing");
    let found: Vec<(String, Option<String>)> =
        tasks.into_iter().map(|t| (t.href, t.etag)).collect();
    assert_eq!(
        found,
        [
            (server.url("/cal/t1.ics"), Some("\"t1\"".to_owned())),
            (server.url("/cal/t%202.ics"), Some("W/\"t2\"".to_owned()))
        ]
    );
    assert_eq!(server.requests()[0].header("depth"), Some("1"));
}

// --------------------------------------------------------------- misc errors

#[tokio::test]
async fn redirect_loops_and_error_statuses() {
    let server = MockServer::start(|req| match req.path() {
        "/loop/" => Reply::redirect(302, "/loop/"),
        "/busy/" => Reply::new(429).header("Retry-After", "30"),
        "/broken/" => Reply::new(500).body("Internal error"),
        "/html/" => Reply::new(207)
            .header("Content-Type", "text/html")
            .body("<html><body>Login</body></html>"),
        _ => Reply::new(404),
    })
    .await;
    let client = client(&server, Auth::None);

    let err = client.list_etags("/loop/").await.unwrap_err();
    assert!(matches!(err, DavError::TooManyRedirects { .. }), "{err:?}");
    assert_eq!(server.requests().len(), 11);

    match client.list_etags("/busy/").await {
        Err(DavError::RateLimited { retry_after }) => assert_eq!(retry_after, Some(30)),
        other => panic!("unexpected {other:?}"),
    }
    match client.list_etags("/broken/").await {
        Err(err @ DavError::Http { status: 500, .. }) => {
            assert!(err.is_transient());
            assert_eq!(err.status(), Some(500));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        client.list_etags("/html/").await,
        Err(DavError::InvalidResponse(_))
    ));
    assert!(matches!(
        client.list_calendars("/nowhere/").await,
        Err(DavError::NotFound { .. })
    ));
}
