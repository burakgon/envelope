//! Shared helpers for the integration tests: a mock EWS endpoint, fixture
//! loading, request inspection and a token source that records refreshes.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use envelope_ews::{Auth, EwsClient, RetryPolicy, TokenSource};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Path of the EWS endpoint on the mock server.
pub const EWS_PATH: &str = "/EWS/Exchange.asmx";

/// Loads `tests/fixtures/{name}`.
pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A short retry policy so throttling tests run quickly.
pub fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(20),
        max_retry_after: Duration::from_secs(2),
    }
}

/// The EWS URL of the mock server.
pub fn ews_url(server: &MockServer) -> String {
    format!("{}{EWS_PATH}", server.uri())
}

/// A client with Basic credentials pointed at the mock server.
pub async fn setup() -> (MockServer, EwsClient) {
    let server = MockServer::start().await;
    let client = EwsClient::builder(ews_url(&server), Auth::basic("CORP\\jane", "secret"))
        .retry_policy(fast_retry())
        .anchor_mailbox("jane@example.com")
        .build()
        .expect("client");
    (server, client)
}

/// A `200 OK` SOAP response with the given fixture.
pub fn soap_ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/xml; charset=utf-8")
        .set_body_string(fixture(name))
}

/// A `500` SOAP fault response with the given body.
pub fn soap_fault(body: String) -> ResponseTemplate {
    ResponseTemplate::new(500)
        .insert_header("content-type", "text/xml; charset=utf-8")
        .set_body_string(body)
}

/// Mounts a response for POSTs whose body contains `needle` (typically the
/// operation element such as `<m:GetItem>`).
pub async fn on(server: &MockServer, needle: &'static str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(EWS_PATH))
        .and(body_string_contains(needle))
        .respond_with(response)
        .mount(server)
        .await;
}

/// All requests the server received.
pub async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

/// The bodies of all requests, as text.
pub async fn bodies(server: &MockServer) -> Vec<String> {
    requests(server)
        .await
        .into_iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}

/// A header of a request as text.
pub fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|v| v.to_str().ok())
}

/// Hands out `token-1`, then `token-2` after an invalidation, and so on.
#[derive(Default)]
pub struct CountingTokens {
    invalidations: AtomicUsize,
    issued: AtomicUsize,
}

impl CountingTokens {
    pub fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::SeqCst)
    }

    pub fn issued(&self) -> usize {
        self.issued.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TokenSource for CountingTokens {
    async fn access_token(&self) -> Result<String, String> {
        self.issued.fetch_add(1, Ordering::SeqCst);
        Ok(format!("token-{}", self.invalidations() + 1))
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}
