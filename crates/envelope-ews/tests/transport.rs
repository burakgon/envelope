//! The SOAP transport against a mock server: headers, retries for
//! throttling, faults, and Basic, bearer and NTLM authentication.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use common::{
    CountingTokens, EWS_PATH, bodies, ews_url, fast_retry, fixture, header, on, requests, setup,
    soap_fault, soap_ok,
};
use envelope_ews::{
    Auth, DistinguishedFolder, EwsClient, EwsError, ExchangeVersion, ItemId, ResponseCode,
    RetryPolicy, StaticToken,
};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn sends_soap_headers_and_records_the_server_version() {
    let (server, client) = setup().await;
    on(&server, "<m:CreateFolder>", soap_ok("create_folder.xml")).await;

    let id = client
        .create_folder(
            DistinguishedFolder::Inbox,
            "Receipts",
            envelope_ews::FOLDER_CLASS_MAIL,
        )
        .await
        .expect("folder created");
    assert_eq!(id.id, "new-folder-id");
    assert_eq!(id.change_key.as_deref(), Some("AQAAABYAAAB-new"));

    let received = requests(&server).await;
    assert_eq!(received.len(), 1);
    let request = &received[0];
    assert_eq!(
        header(request, "content-type"),
        Some("text/xml; charset=utf-8")
    );
    assert_eq!(header(request, "x-anchormailbox"), Some("jane@example.com"));
    assert_eq!(
        header(request, "authorization"),
        Some("Basic Q09SUFxqYW5lOnNlY3JldA==")
    );
    assert!(header(request, "user-agent").is_some_and(|ua| ua.starts_with("Envelope/")));
    assert!(header(request, "client-request-id").is_some());
    let body = String::from_utf8_lossy(&request.body);
    assert!(body.contains(r#"<t:RequestServerVersion Version="Exchange2013_SP1"/>"#));
    assert!(body.contains("<t:DisplayName>Receipts</t:DisplayName>"));

    let info = client
        .server_version_info()
        .expect("server version recorded");
    assert_eq!(info.to_string(), "15.2.1544.4");
    assert_eq!(info.exchange_version(), Some(ExchangeVersion::Exchange2016));
}

#[tokio::test]
async fn retries_server_busy_faults_after_the_requested_back_off() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .respond_with(soap_fault(
            fixture("fault_server_busy.xml").replace("{{backoff}}", "30"),
        ))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    on(&server, "<m:DeleteItem", soap_ok("delete_item.xml")).await;

    let started = std::time::Instant::now();
    let results = client
        .delete_items(
            &[ItemId::new("a")],
            envelope_ews::DeleteType::MoveToDeletedItems,
        )
        .await
        .expect("succeeds after throttling");
    assert!(results[0].is_ok());
    assert_eq!(requests(&server).await.len(), 3);
    assert!(started.elapsed() >= Duration::from_millis(60));
}

#[tokio::test]
async fn returns_long_back_offs_to_the_caller() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:GetFolder>",
        soap_fault(fixture("fault_server_busy.xml").replace("{{backoff}}", "297749")),
    )
    .await;
    let err = client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .expect_err("throttled");
    assert!(err.is_throttled());
    assert!(err.is_transient());
    assert_eq!(err.response_code(), Some(&ResponseCode::ErrorServerBusy));
    assert_eq!(err.retry_after(), Some(Duration::from_millis(297_749)));
    assert_eq!(
        requests(&server).await.len(),
        1,
        "no retry beyond max_retry_after"
    );
}

#[tokio::test]
async fn retries_503_and_429_with_retry_after() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;
    on(
        &server,
        "<m:MarkAllItemsAsRead>",
        soap_ok("mark_all_read.xml"),
    )
    .await;
    client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .expect("succeeds on the third attempt");
    assert_eq!(requests(&server).await.len(), 3);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let server = MockServer::start().await;
    let client = EwsClient::builder(ews_url(&server), Auth::basic("u", "p"))
        .retry_policy(RetryPolicy {
            max_retries: 1,
            ..fast_retry()
        })
        .build()
        .unwrap();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let err = client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(503));
    assert!(err.is_transient());
    assert!(
        !err.is_throttled(),
        "503 without Retry-After is an outage, not throttling"
    );
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn whole_batch_throttling_is_retried() {
    let (server, client) = setup().await;
    let busy = r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:GetItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Error"><m:MessageText>busy</m:MessageText><m:ResponseCode>ErrorServerBusy</m:ResponseCode><m:MessageXml><t:Value Name="BackOffMilliseconds">10</t:Value></m:MessageXml></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse></s:Body></s:Envelope>"#;
    Mock::given(method("POST"))
        .and(body_string_contains("<m:GetItem>"))
        .respond_with(ResponseTemplate::new(200).set_body_string(busy))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    on(&server, "<m:GetItem>", soap_ok("get_item_messages.xml")).await;
    let ids = [ItemId::new("1"), ItemId::new("2"), ItemId::new("3")];
    let results = client
        .get_messages(&ids, &Default::default())
        .await
        .expect("retried");
    assert_eq!(results.len(), 3);
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn schema_faults_are_typed_and_not_retried() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:GetFolder>",
        soap_fault(fixture("fault_schema_validation.xml")),
    )
    .await;
    let err = client
        .get_folder(DistinguishedFolder::Archive)
        .await
        .unwrap_err();
    let EwsError::Fault(fault) = &err else {
        panic!("expected a fault, got {err:?}");
    };
    assert_eq!(fault.code, Some(ResponseCode::ErrorSchemaValidation));
    assert_eq!(fault.http_status, 500);
    assert!(
        fault
            .details
            .iter()
            .any(|(k, v)| k == "LineNumber" && v == "1")
    );
    assert!(!err.is_transient());
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn non_soap_responses_are_reported() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:GetFolder>",
        ResponseTemplate::new(200)
            .set_body_string("<html><body>Outlook Web App sign-in</body></html>"),
    )
    .await;
    let err = client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .unwrap_err();
    assert!(matches!(err, EwsError::InvalidResponse(_)), "{err:?}");

    let (server, client) = setup().await;
    on(
        &server,
        "<m:GetFolder>",
        ResponseTemplate::new(302)
            .insert_header("location", "https://mail.example.com/owa/auth/logon.aspx"),
    )
    .await;
    match client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .unwrap_err()
    {
        EwsError::Http { status, body, .. } => {
            assert_eq!(status, 302);
            assert!(body.contains("logon.aspx"));
        }
        other => panic!("unexpected {other:?}"),
    }

    let (server, client) = setup().await;
    on(&server, "<m:GetFolder>", ResponseTemplate::new(403)).await;
    assert!(matches!(
        client
            .get_folder(DistinguishedFolder::Inbox)
            .await
            .unwrap_err(),
        EwsError::Forbidden { .. }
    ));
}

#[tokio::test]
async fn basic_auth_rejection_names_the_offered_schemes() {
    let (server, client) = setup().await;
    on(
        &server,
        "<m:GetFolder>",
        ResponseTemplate::new(401)
            .append_header("www-authenticate", "Negotiate")
            .append_header("www-authenticate", "NTLM"),
    )
    .await;
    let err = client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .unwrap_err();
    assert!(err.is_unauthorized());
    assert!(err.is_auth_error());
    assert!(err.to_string().contains("negotiate, ntlm"), "{err}");
}

#[tokio::test]
async fn bearer_tokens_are_refreshed_once() {
    let server = MockServer::start().await;
    let tokens = Arc::new(CountingTokens::default());
    let client = EwsClient::builder(ews_url(&server), Auth::Bearer(tokens.clone()))
        .retry_policy(fast_retry())
        .build()
        .unwrap();
    Mock::given(method("POST"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer token-1",
        ))
        .respond_with(ResponseTemplate::new(401).insert_header(
            "www-authenticate",
            r#"Bearer error="invalid_token", error_description="The token has expired""#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer token-2",
        ))
        .respond_with(soap_ok("mark_all_read.xml"))
        .mount(&server)
        .await;
    client
        .mark_all_read(DistinguishedFolder::Inbox, false)
        .await
        .expect("succeeds with the refreshed token");
    assert_eq!(tokens.invalidations(), 1);
    assert_eq!(tokens.issued(), 2);

    // A static token cannot be refreshed: the second 401 is final.
    let server = MockServer::start().await;
    let client = EwsClient::builder(ews_url(&server), Auth::bearer(StaticToken::new("t")))
        .build()
        .unwrap();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).insert_header(
            "www-authenticate",
            r#"Bearer error="invalid_token", error_description="The token has expired""#,
        ))
        .mount(&server)
        .await;
    let err = client
        .mark_all_read(DistinguishedFolder::Inbox, false)
        .await
        .unwrap_err();
    assert!(err.is_unauthorized());
    assert!(err.to_string().contains("The token has expired"));
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn token_source_failures_are_auth_errors() {
    struct Broken;
    #[async_trait::async_trait]
    impl envelope_ews::TokenSource for Broken {
        async fn access_token(&self) -> Result<String, String> {
            Err("refresh token revoked".into())
        }
        async fn invalidate(&self) {}
    }
    let server = MockServer::start().await;
    let client = EwsClient::new(ews_url(&server), Auth::bearer(Broken)).unwrap();
    let err = client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .unwrap_err();
    assert!(matches!(err, EwsError::TokenSource(ref m) if m.contains("revoked")));
    assert!(err.is_auth_error());
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn refuses_credentials_over_plain_http_to_remote_hosts() {
    let client = EwsClient::new(
        "http://mail.example.com/EWS/Exchange.asmx",
        Auth::basic("u", "p"),
    )
    .unwrap();
    let err = client
        .get_folder(DistinguishedFolder::Inbox)
        .await
        .unwrap_err();
    assert!(matches!(err, EwsError::InvalidInput(_)), "{err:?}");
}

// --- NTLM ------------------------------------------------------------------

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn av(id: u16, value: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// A CHALLENGE message for domain CORP with a server timestamp.
fn challenge_message() -> Vec<u8> {
    let target = utf16("CORP");
    let mut info = av(2, &utf16("CORP"));
    info.extend(av(1, &utf16("EXCH01")));
    info.extend(av(4, &utf16("corp.example.com")));
    info.extend(av(7, &0x01da_9b5c_0000_0000u64.to_le_bytes()));
    info.extend(av(0, &[]));
    let flags: u32 = 0xe28a_8215 | 0x0001_0000;
    let mut message = b"NTLMSSP\0".to_vec();
    message.extend(2u32.to_le_bytes());
    message.extend((target.len() as u16).to_le_bytes());
    message.extend((target.len() as u16).to_le_bytes());
    message.extend(56u32.to_le_bytes());
    message.extend(flags.to_le_bytes());
    message.extend([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
    message.extend([0u8; 8]);
    message.extend((info.len() as u16).to_le_bytes());
    message.extend((info.len() as u16).to_le_bytes());
    message.extend((56 + target.len() as u32).to_le_bytes());
    message.extend([10, 0, 0x5d, 0x58, 0, 0, 0, 15]);
    message.extend(target);
    message.extend(info);
    message
}

fn field(message: &[u8], at: usize) -> Vec<u8> {
    let len = u16::from_le_bytes([message[at], message[at + 1]]) as usize;
    let offset = u32::from_le_bytes([
        message[at + 4],
        message[at + 5],
        message[at + 6],
        message[at + 7],
    ]) as usize;
    message[offset..offset + len].to_vec()
}

fn from_utf16(bytes: &[u8]) -> String {
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    String::from_utf16_lossy(&units)
}

/// What the fake NTLM server saw.
#[derive(Default)]
struct NtlmLog {
    /// (scheme, message type, body length) per authorization header.
    legs: Mutex<Vec<(String, u32, usize)>>,
    /// User and domain of AUTHENTICATE messages.
    logons: Mutex<Vec<(String, String, String)>>,
    /// Whether the "connection" is authenticated.
    authenticated: AtomicBool,
    /// Requests without authorization header.
    anonymous: AtomicUsize,
}

/// A responder that plays the server side of the NTLM handshake. With
/// `negotiate_only` the server offers only the `Negotiate` scheme.
fn ntlm_server(
    log: Arc<NtlmLog>,
    negotiate_only: bool,
    password_ok: bool,
) -> impl Fn(&Request) -> ResponseTemplate {
    let offer = if negotiate_only { "Negotiate" } else { "NTLM" };
    move |request: &Request| {
        let unauthorized = || {
            let mut response =
                ResponseTemplate::new(401).append_header("www-authenticate", "Negotiate");
            if !negotiate_only {
                response = response.append_header("www-authenticate", "NTLM");
            }
            response
        };
        let Some(auth) = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
        else {
            log.anonymous.fetch_add(1, Ordering::SeqCst);
            return if log.authenticated.load(Ordering::SeqCst) {
                soap_ok("mark_all_read.xml")
            } else {
                unauthorized()
            };
        };
        let (scheme, token) = auth.split_once(' ').unwrap();
        if !scheme.eq_ignore_ascii_case(offer) {
            return unauthorized();
        }
        let message = base64::engine::general_purpose::STANDARD
            .decode(token)
            .unwrap();
        assert_eq!(&message[..8], b"NTLMSSP\0");
        let kind = u32::from_le_bytes([message[8], message[9], message[10], message[11]]);
        log.legs
            .lock()
            .unwrap()
            .push((scheme.to_owned(), kind, request.body.len()));
        match kind {
            1 => ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    "{offer} {}",
                    base64::engine::general_purpose::STANDARD.encode(challenge_message())
                ),
            ),
            3 => {
                let domain = from_utf16(&field(&message, 28));
                let user = from_utf16(&field(&message, 36));
                let workstation = from_utf16(&field(&message, 44));
                log.logons.lock().unwrap().push((user, domain, workstation));
                // The NTLMv2 response embeds the server timestamp.
                let nt = field(&message, 20);
                assert_eq!(&nt[24..32], &0x01da_9b5c_0000_0000u64.to_le_bytes());
                if password_ok {
                    log.authenticated.store(true, Ordering::SeqCst);
                    soap_ok("mark_all_read.xml")
                } else {
                    unauthorized()
                }
            }
            _ => unauthorized(),
        }
    }
}

async fn ntlm_client(server: &MockServer, username: &str) -> EwsClient {
    EwsClient::builder(ews_url(server), Auth::ntlm(username, "Secret!"))
        .retry_policy(fast_retry())
        .ntlm_connections(1)
        .build()
        .unwrap()
}

#[tokio::test]
async fn ntlm_handshake_then_authenticated_connection() {
    let server = MockServer::start().await;
    let log = Arc::new(NtlmLog::default());
    Mock::given(method("POST"))
        .respond_with(ntlm_server(log.clone(), false, true))
        .mount(&server)
        .await;
    let client = ntlm_client(&server, "CORP\\jane").await;

    client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .expect("first request");
    client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .expect("second request");

    let legs = log.legs.lock().unwrap().clone();
    assert_eq!(legs.len(), 2, "one handshake: {legs:?}");
    assert_eq!(
        legs[0],
        ("NTLM".to_owned(), 1, 0),
        "negotiate leg without body"
    );
    assert_eq!(legs[1].1, 3);
    assert!(legs[1].2 > 0, "the authenticate leg carries the SOAP body");
    let logons = log.logons.lock().unwrap().clone();
    assert_eq!(logons[0].0, "jane");
    assert_eq!(logons[0].1, "CORP");
    // The second request rode the authenticated connection.
    assert_eq!(log.anonymous.load(Ordering::SeqCst), 1);
    let bodies = bodies(&server).await;
    assert!(bodies[2].contains("<m:MarkAllItemsAsRead>"));
}

#[tokio::test]
async fn ntlm_redoes_the_handshake_when_the_connection_was_reset() {
    let server = MockServer::start().await;
    let log = Arc::new(NtlmLog::default());
    Mock::given(method("POST"))
        .respond_with(ntlm_server(log.clone(), false, true))
        .mount(&server)
        .await;
    let client = ntlm_client(&server, "jane@corp.example.com").await;
    client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .unwrap();
    // The server forgets the connection (idle timeout).
    log.authenticated.store(false, Ordering::SeqCst);
    client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .unwrap();
    let legs: Vec<u32> = log.legs.lock().unwrap().iter().map(|l| l.1).collect();
    assert_eq!(legs, [1, 3, 1, 3]);
    // UPN logons keep the whole name and an empty domain.
    let logons = log.logons.lock().unwrap().clone();
    assert_eq!(logons[0].0, "jane@corp.example.com");
    assert_eq!(logons[0].1, "");
}

#[tokio::test]
async fn ntlm_tokens_travel_in_negotiate_when_only_that_is_offered() {
    let server = MockServer::start().await;
    let log = Arc::new(NtlmLog::default());
    Mock::given(method("POST"))
        .respond_with(ntlm_server(log.clone(), true, true))
        .mount(&server)
        .await;
    let client = ntlm_client(&server, "jane").await;
    client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .expect("authenticated");
    let legs = log.legs.lock().unwrap().clone();
    assert!(
        legs.iter().all(|(scheme, _, _)| scheme == "Negotiate"),
        "{legs:?}"
    );
    // A bare user name logs on to the server's domain.
    assert_eq!(log.logons.lock().unwrap()[0].1, "CORP");
}

#[tokio::test]
async fn ntlm_rejected_credentials() {
    let server = MockServer::start().await;
    let log = Arc::new(NtlmLog::default());
    Mock::given(method("POST"))
        .respond_with(ntlm_server(log.clone(), false, false))
        .mount(&server)
        .await;
    let client = ntlm_client(&server, "CORP\\jane").await;
    let err = client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .unwrap_err();
    assert!(err.is_unauthorized(), "{err:?}");
    assert!(err.to_string().contains("NTLM credentials were rejected"));
}

#[tokio::test]
async fn ntlm_requires_a_scheme_the_client_speaks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).insert_header("www-authenticate", "Basic realm=\"mail\""),
        )
        .mount(&server)
        .await;
    let client = ntlm_client(&server, "CORP\\jane").await;
    let err = client
        .mark_all_read(DistinguishedFolder::Inbox, true)
        .await
        .unwrap_err();
    assert!(err.is_unauthorized());
    assert!(err.to_string().contains("offered: basic"), "{err}");
}

#[tokio::test]
async fn concurrent_requests_share_the_ntlm_slots() {
    let server = MockServer::start().await;
    let log = Arc::new(NtlmLog::default());
    Mock::given(method("POST"))
        .respond_with(ntlm_server(log.clone(), false, true))
        .mount(&server)
        .await;
    let client = EwsClient::builder(ews_url(&server), Auth::ntlm("CORP\\jane", "Secret!"))
        .ntlm_connections(2)
        .build()
        .unwrap();
    let calls = (0..6).map(|_| client.mark_all_read(DistinguishedFolder::Inbox, true));
    for result in futures::future::join_all(calls).await {
        result.expect("every request succeeds");
    }
}
