//! Autodiscover flows against a mock server: POX settings and redirects,
//! the HTTP redirect method, Autodiscover v2 and failure reporting.

mod common;

use common::{fixture, header, requests};
use envelope_ews::{Auth, Autodiscover, AutodiscoverEndpoint, EwsError, ExchangeVersion, Url};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const POX_PATH: &str = "/autodiscover/autodiscover.xml";

fn settings(internal: &str, external: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/xml; charset=utf-8")
        .set_body_string(
            fixture("autodiscover_settings.xml")
                .replace("{{internal}}", internal)
                .replace("{{external}}", external),
        )
}

fn xml(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/xml; charset=utf-8")
        .set_body_string(fixture(name))
}

fn url(server: &MockServer, path: &str) -> Url {
    Url::parse(&format!("{}{path}", server.uri())).unwrap()
}

fn discoverer() -> Autodiscover {
    Autodiscover::new(Auth::basic("CORP\\jane", "secret")).unwrap()
}

#[tokio::test]
async fn pox_settings_prefer_the_external_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(POX_PATH))
        .respond_with(settings(
            "https://mail.internal.example.com/EWS/Exchange.asmx",
            "https://mail.example.com/EWS/Exchange.asmx",
        ))
        .mount(&server)
        .await;
    let endpoint = url(&server, POX_PATH);
    let result = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![AutodiscoverEndpoint::Pox(endpoint.clone())]
        })
        .await
        .unwrap();
    assert_eq!(result.ews_url, "https://mail.example.com/EWS/Exchange.asmx");
    assert_eq!(
        result.internal_ews_url.as_deref(),
        Some("https://mail.internal.example.com/EWS/Exchange.asmx")
    );
    assert_eq!(result.server_version, Some(ExchangeVersion::Exchange2016));
    assert_eq!(result.server_build.as_deref(), Some("15.2.3080"));
    assert_eq!(result.display_name.as_deref(), Some("Jane Doe"));
    assert_eq!(result.smtp_address.as_deref(), Some("jane.doe@example.com"));
    assert!(!result.microsoft_online);
    assert_eq!(result.source, endpoint.as_str());

    let received = requests(&server).await;
    assert_eq!(
        header(&received[0], "authorization"),
        Some("Basic Q09SUFxqYW5lOnNlY3JldA==")
    );
    let body = String::from_utf8_lossy(&received[0].body);
    assert!(body.contains("<EMailAddress>jane@example.com</EMailAddress>"));
}

#[tokio::test]
async fn address_redirects_restart_discovery_in_the_new_domain() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/first/autodiscover/autodiscover.xml"))
        .respond_with(xml("autodiscover_redirect_addr.xml"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/second/autodiscover/autodiscover.xml"))
        .respond_with(settings(
            "https://a.example.net/EWS/Exchange.asmx",
            "https://b.example.net/EWS/Exchange.asmx",
        ))
        .mount(&server)
        .await;
    let first = url(&server, "/first/autodiscover/autodiscover.xml");
    let second = url(&server, "/second/autodiscover/autodiscover.xml");
    let result = discoverer()
        .discover_with("jane@example.com", |domain| match domain {
            "example.com" => vec![AutodiscoverEndpoint::Pox(first.clone())],
            "corp.example.net" => vec![AutodiscoverEndpoint::Pox(second.clone())],
            other => panic!("unexpected domain {other}"),
        })
        .await
        .unwrap();
    assert_eq!(result.ews_url, "https://b.example.net/EWS/Exchange.asmx");
    let received = requests(&server).await;
    assert!(
        String::from_utf8_lossy(&received[1].body)
            .contains("<EMailAddress>jane@corp.example.net</EMailAddress>")
    );
}

#[tokio::test]
async fn address_redirect_loops_are_detected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(xml("autodiscover_redirect_addr.xml"))
        .mount(&server)
        .await;
    let endpoint = url(&server, POX_PATH);
    let err = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![AutodiscoverEndpoint::Pox(endpoint.clone())]
        })
        .await
        .unwrap_err();
    assert!(matches!(err, EwsError::TooManyRedirects { .. }), "{err:?}");
}

#[tokio::test]
async fn url_redirects_are_followed() {
    let server = MockServer::start().await;
    let target = url(&server, "/other/autodiscover/autodiscover.xml");
    Mock::given(method("POST"))
        .and(path(POX_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            fixture("autodiscover_redirect_url.xml").replace("{{url}}", target.as_str()),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/other/autodiscover/autodiscover.xml"))
        .respond_with(settings(
            "https://x.example.com/EWS/Exchange.asmx",
            "https://y.example.com/EWS/Exchange.asmx",
        ))
        .mount(&server)
        .await;
    let endpoint = url(&server, POX_PATH);
    let result = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![AutodiscoverEndpoint::Pox(endpoint.clone())]
        })
        .await
        .unwrap();
    assert_eq!(result.ews_url, "https://y.example.com/EWS/Exchange.asmx");
    assert_eq!(result.source, target.as_str());
}

#[tokio::test]
async fn http_redirect_method_is_unauthenticated() {
    let server = MockServer::start().await;
    let target = url(&server, "/target/autodiscover/autodiscover.xml");
    Mock::given(method("POST"))
        .and(path(POX_PATH))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/redirect/autodiscover/autodiscover.xml"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/target/autodiscover/autodiscover.xml"))
        .respond_with(settings(
            "https://in.example.com/EWS/Exchange.asmx",
            "https://out.example.com/EWS/Exchange.asmx",
        ))
        .mount(&server)
        .await;
    let pox = url(&server, POX_PATH);
    let redirect = url(&server, "/redirect/autodiscover/autodiscover.xml");
    let result = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![
                AutodiscoverEndpoint::Pox(pox.clone()),
                AutodiscoverEndpoint::HttpRedirect(redirect.clone()),
            ]
        })
        .await
        .unwrap();
    assert_eq!(result.ews_url, "https://out.example.com/EWS/Exchange.asmx");
    let received = requests(&server).await;
    let probe = received
        .iter()
        .find(|r| r.method.as_str() == "GET")
        .unwrap();
    assert_eq!(header(probe, "authorization"), None);
    let target_request = received.last().unwrap();
    assert!(header(target_request, "authorization").is_some());
}

#[tokio::test]
async fn autodiscover_v2_json_follows_redirects_without_credentials() {
    let server = MockServer::start().await;
    let second = url(
        &server,
        "/second/autodiscover/autodiscover.json/v1.0/jane@example.com?Protocol=EWS",
    );
    Mock::given(method("GET"))
        .and(path(
            "/autodiscover/autodiscover.json/v1.0/jane@example.com",
        ))
        .and(query_param("Protocol", "EWS"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", second.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/second/autodiscover/autodiscover.json/v1.0/jane@example.com",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"Protocol":"EWS","Url":"https://outlook.office365.com/EWS/Exchange.asmx"}"#,
        ))
        .mount(&server)
        .await;
    let base = url(&server, "/autodiscover/autodiscover.json/v1.0/");
    let result = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![AutodiscoverEndpoint::Json(base.clone())]
        })
        .await
        .unwrap();
    assert_eq!(
        result.ews_url,
        "https://outlook.office365.com/EWS/Exchange.asmx"
    );
    assert_eq!(result.server_version, None);
    for request in requests(&server).await {
        assert_eq!(header(&request, "authorization"), None);
    }
}

#[tokio::test]
async fn failures_are_collected_per_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/a/autodiscover/autodiscover.xml"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/b/autodiscover/autodiscover.xml"))
        .respond_with(xml("autodiscover_error.xml"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/c/autodiscover/autodiscover.xml"))
        .respond_with(settings(
            "http://insecure.example.com/EWS/Exchange.asmx",
            "http://insecure.example.com/EWS/Exchange.asmx",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"ErrorCode":"InvalidUser","ErrorMessage":"The user is not valid"}"#,
        ))
        .mount(&server)
        .await;
    let endpoints = vec![
        AutodiscoverEndpoint::Pox(url(&server, "/a/autodiscover/autodiscover.xml")),
        AutodiscoverEndpoint::Pox(url(&server, "/b/autodiscover/autodiscover.xml")),
        AutodiscoverEndpoint::Pox(url(&server, "/c/autodiscover/autodiscover.xml")),
        AutodiscoverEndpoint::Json(url(&server, "/autodiscover/autodiscover.json/v1.0/")),
    ];
    let err = discoverer()
        .discover_with("jane@example.com", |_| endpoints.clone())
        .await
        .unwrap_err();
    let EwsError::AutodiscoverFailed {
        email,
        attempts,
        unauthorized,
    } = &err
    else {
        panic!("unexpected {err:?}");
    };
    assert!(!unauthorized);
    assert!(!err.is_auth_error());
    assert_eq!(email, "jane@example.com");
    assert_eq!(attempts.len(), 4, "{attempts:?}");
    assert!(attempts[0].contains("404"));
    assert!(attempts[1].contains("The email address can't be found."));
    assert!(attempts[2].contains("unusable EWS URL"));
    assert!(attempts[3].contains("InvalidUser"));
}

#[tokio::test]
async fn redirects_to_unrelated_hosts_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "https://autodiscover.attacker.test/autodiscover/autodiscover.xml",
        ))
        .mount(&server)
        .await;
    let redirect = url(&server, "/redirect/autodiscover/autodiscover.xml");
    let err = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![AutodiscoverEndpoint::HttpRedirect(redirect.clone())]
        })
        .await
        .unwrap_err();
    let EwsError::AutodiscoverFailed { attempts, .. } = &err else {
        panic!("unexpected {err:?}");
    };
    assert!(
        attempts[0].contains("untrusted host autodiscover.attacker.test"),
        "{attempts:?}"
    );
    // Only the unauthenticated probe was sent.
    let received = requests(&server).await;
    assert_eq!(received.len(), 1);
    assert_eq!(header(&received[0], "authorization"), None);
}

#[tokio::test]
async fn rejected_credentials_are_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("www-authenticate", "Basic realm=\"autodiscover\""),
        )
        .mount(&server)
        .await;
    let endpoint = url(&server, POX_PATH);
    let err = discoverer()
        .discover_with("jane@example.com", |_| {
            vec![AutodiscoverEndpoint::Pox(endpoint.clone())]
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            EwsError::AutodiscoverFailed {
                unauthorized: true,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.is_auth_error());
}

#[tokio::test]
async fn rejects_addresses_without_domain() {
    let err = discoverer().discover("jane").await.unwrap_err();
    assert!(matches!(err, EwsError::InvalidInput(_)));
}
