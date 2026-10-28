//! Shared helpers for the integration tests: a mock Graph server, a token
//! source that records refreshes, and fixture loading.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use envelope_graph::{GraphClient, RetryPolicy, TokenSource};
use serde_json::Value;
use wiremock::{MockServer, Request};

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

/// A short retry policy so throttling tests run quickly.
pub fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(20),
        max_retry_after: Duration::from_secs(5),
    }
}

/// The base URL the client uses against the mock server.
pub fn base(server: &MockServer) -> String {
    format!("{}/v1.0", server.uri())
}

/// Starts a mock server and a client pointed at it.
pub async fn setup() -> (MockServer, GraphClient, Arc<CountingTokens>) {
    let server = MockServer::start().await;
    let tokens = Arc::new(CountingTokens::default());
    let client = GraphClient::builder(tokens.clone())
        .base_url(base(&server))
        .retry_policy(fast_retry())
        .build()
        .expect("client");
    (server, client, tokens)
}

/// Loads `tests/fixtures/{name}` with `{{base}}` replaced by the mock
/// server's base URL.
pub fn fixture(server: &MockServer, name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text.replace("{{base}}", &base(server)))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Loads a fixture that needs no server substitution.
pub fn static_fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Matches requests whose `Prefer` header contains `needle`.
pub fn prefer_contains(needle: &'static str) -> impl Fn(&Request) -> bool + Send + Sync {
    move |request: &Request| {
        request
            .headers
            .get_all("prefer")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.contains(needle))
    }
}

/// Matches requests whose JSON body equals `expected`.
pub fn json_body(expected: Value) -> impl Fn(&Request) -> bool + Send + Sync {
    move |request: &Request| request.body_json::<Value>().ok().as_ref() == Some(&expected)
}

/// All requests the server received.
pub async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}
