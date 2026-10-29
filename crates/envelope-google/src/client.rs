//! HTTP plumbing shared by every Google API wrapper in this crate.

use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use reqwest::header::{self, HeaderMap};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::OnceCell;
use url::Url;

use crate::auth::TokenSource;
use crate::calendar::{ColorPalette, Reminder};
use crate::error::{ApiError, Error, Result};

const DEFAULT_CALENDAR_BASE_URL: &str = "https://www.googleapis.com/calendar/v3";
const DEFAULT_PEOPLE_BASE_URL: &str = "https://people.googleapis.com/v1";
const DEFAULT_GMAIL_BASE_URL: &str = "https://gmail.googleapis.com/gmail/v1";
const DEFAULT_TASKS_BASE_URL: &str = "https://tasks.googleapis.com/tasks/v1";

/// Exponential-backoff policy for transient failures.
///
/// Rate-limit responses (`429`, and `403` with reason `rateLimitExceeded` or
/// `userRateLimitExceeded`), `5xx` server errors and connection failures are
/// retried after `initial_backoff · 2ⁿ` (capped at `max_backoff`) plus a
/// random jitter of up to `initial_backoff`. A `Retry-After` header is
/// honoured when it asks for a longer delay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt; `0` disables retries.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub initial_backoff: Duration,
    /// Upper bound for a single delay.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(32),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub fn none() -> Self {
        RetryPolicy {
            max_retries: 0,
            ..RetryPolicy::default()
        }
    }

    /// Delay before retry number `attempt` (zero-based).
    pub(crate) fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        let base = self
            .initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff);
        let jitter_cap = u64::try_from(self.initial_backoff.as_millis()).unwrap_or(u64::MAX);
        let jitter = match jitter_cap {
            0 => Duration::ZERO,
            cap => Duration::from_millis(random_u64() % cap),
        };
        let computed = base.saturating_add(jitter);
        match retry_after {
            Some(server) => computed.max(server.min(self.max_backoff)),
            None => computed,
        }
    }
}

/// Configuration of a [`GoogleClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// Base URL of the Calendar API (default `https://www.googleapis.com/calendar/v3`).
    pub calendar_base_url: String,
    /// Base URL of the People API (default `https://people.googleapis.com/v1`).
    pub people_base_url: String,
    /// Base URL of the Gmail API (default `https://gmail.googleapis.com/gmail/v1`).
    pub gmail_base_url: String,
    /// Base URL of the Tasks API (default `https://tasks.googleapis.com/tasks/v1`).
    pub tasks_base_url: String,
    /// Retry behaviour for transient failures.
    pub retry: RetryPolicy,
    /// `User-Agent` header. Google only compresses responses when the user
    /// agent contains `gzip`, so keep that token when overriding it.
    pub user_agent: String,
    /// Overall timeout of a single HTTP attempt.
    pub timeout: Duration,
    /// IANA time zone used to anchor floating times when writing events
    /// (Google Calendar has no floating times). Writing an event with a
    /// floating time fails with [`Error::InvalidInput`] when this is `None`.
    pub floating_time_zone: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            calendar_base_url: DEFAULT_CALENDAR_BASE_URL.to_owned(),
            people_base_url: DEFAULT_PEOPLE_BASE_URL.to_owned(),
            gmail_base_url: DEFAULT_GMAIL_BASE_URL.to_owned(),
            tasks_base_url: DEFAULT_TASKS_BASE_URL.to_owned(),
            retry: RetryPolicy::default(),
            user_agent: format!("envelope-mail/{} (gzip)", env!("CARGO_PKG_VERSION")),
            timeout: Duration::from_secs(60),
            floating_time_zone: None,
        }
    }
}

impl ClientConfig {
    /// Points all APIs at one server, using the same path layout as Google
    /// (`{root}/calendar/v3`, `{root}/v1`, `{root}/gmail/v1`,
    /// `{root}/tasks/v1`). Intended for tests against a local mock server.
    pub fn with_api_root(root: &str) -> Self {
        let root = root.trim_end_matches('/');
        ClientConfig {
            calendar_base_url: format!("{root}/calendar/v3"),
            people_base_url: format!("{root}/v1"),
            gmail_base_url: format!("{root}/gmail/v1"),
            tasks_base_url: format!("{root}/tasks/v1"),
            ..ClientConfig::default()
        }
    }
}

/// Which Google API a request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Service {
    Calendar,
    People,
    Gmail,
    Tasks,
}

/// A request description that can be replayed for retries.
#[derive(Debug, Clone)]
pub(crate) struct ApiRequest<'a> {
    method: Method,
    service: Service,
    path: Vec<&'a str>,
    query: Vec<(&'static str, String)>,
    body: Option<Bytes>,
    if_match: Option<&'a str>,
    sync: bool,
}

impl<'a> ApiRequest<'a> {
    /// A request for `path` (individual, unencoded segments) relative to the
    /// service's base URL.
    pub(crate) fn new(method: Method, service: Service, path: &[&'a str]) -> Self {
        ApiRequest {
            method,
            service,
            path: path.to_vec(),
            query: Vec::new(),
            body: None,
            if_match: None,
            sync: false,
        }
    }

    /// A `GET` request.
    pub(crate) fn get(service: Service, path: &[&'a str]) -> Self {
        Self::new(Method::GET, service, path)
    }

    /// Appends a query parameter.
    pub(crate) fn query(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.query.push((key, value.into()));
        self
    }

    /// Appends a query parameter when `value` is `Some`.
    pub(crate) fn query_opt(self, key: &'static str, value: Option<impl Into<String>>) -> Self {
        match value {
            Some(value) => self.query(key, value),
            None => self,
        }
    }

    /// Appends a query parameter when `condition` holds.
    pub(crate) fn query_if(self, condition: bool, key: &'static str, value: &str) -> Self {
        if condition {
            self.query(key, value)
        } else {
            self
        }
    }

    /// Sets a JSON request body.
    pub(crate) fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Result<Self> {
        let bytes = serde_json::to_vec(body)
            .map_err(|e| Error::InvalidInput(format!("could not serialize request body: {e}")))?;
        self.body = Some(Bytes::from(bytes));
        Ok(self)
    }

    /// Sends `If-Match` with the given etag.
    pub(crate) fn if_match(mut self, etag: Option<&'a str>) -> Self {
        self.if_match = etag;
        self
    }

    /// Marks a list request that uses sync tokens, so that `410 Gone` is
    /// reported as [`Error::SyncTokenExpired`].
    pub(crate) fn sync_request(mut self) -> Self {
        self.sync = true;
        self
    }

    fn is_idempotent(&self) -> bool {
        self.method != Method::POST
    }
}

/// Client for the Google Calendar, People, Gmail settings and Tasks APIs.
///
/// Cheap to clone (all clones share one connection pool, token source and
/// caches). Every public async method returns a `Send` future.
#[derive(Clone)]
pub struct GoogleClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    tokens: Arc<dyn TokenSource>,
    calendar_base: Url,
    people_base: Url,
    gmail_base: Url,
    tasks_base: Url,
    retry: RetryPolicy,
    floating_time_zone: Option<String>,
    palette: OnceCell<Arc<ColorPalette>>,
    default_reminders: Mutex<HashMap<String, Vec<Reminder>>>,
    contacts_search_warm: AtomicBool,
    other_contacts_search_warm: AtomicBool,
}

impl fmt::Debug for GoogleClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleClient")
            .field("calendar_base", &self.inner.calendar_base.as_str())
            .field("people_base", &self.inner.people_base.as_str())
            .field("gmail_base", &self.inner.gmail_base.as_str())
            .field("tasks_base", &self.inner.tasks_base.as_str())
            .field("retry", &self.inner.retry)
            .finish_non_exhaustive()
    }
}

impl GoogleClient {
    /// Creates a client talking to Google's production endpoints.
    pub fn new(tokens: Arc<dyn TokenSource>) -> Result<Self> {
        Self::with_config(tokens, ClientConfig::default())
    }

    /// Creates a client with a custom configuration and its own HTTP client.
    pub fn with_config(tokens: Arc<dyn TokenSource>, config: ClientConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(Duration::from_secs(15))
            .timeout(config.timeout)
            .build()?;
        Self::with_http_client(http, tokens, config)
    }

    /// Creates a client on top of an existing `reqwest::Client` (to share a
    /// connection pool). `config.user_agent` and `config.timeout` are not
    /// applied to a client supplied this way.
    pub fn with_http_client(
        http: reqwest::Client,
        tokens: Arc<dyn TokenSource>,
        config: ClientConfig,
    ) -> Result<Self> {
        Ok(GoogleClient {
            inner: Arc::new(Inner {
                http,
                tokens,
                calendar_base: parse_base_url(&config.calendar_base_url)?,
                people_base: parse_base_url(&config.people_base_url)?,
                gmail_base: parse_base_url(&config.gmail_base_url)?,
                tasks_base: parse_base_url(&config.tasks_base_url)?,
                retry: config.retry,
                floating_time_zone: config.floating_time_zone,
                palette: OnceCell::new(),
                default_reminders: Mutex::new(HashMap::new()),
                contacts_search_warm: AtomicBool::new(false),
                other_contacts_search_warm: AtomicBool::new(false),
            }),
        })
    }

    /// Sends a request and decodes the JSON response.
    pub(crate) async fn send_json<T: DeserializeOwned>(
        &self,
        request: &ApiRequest<'_>,
        context: &'static str,
    ) -> Result<T> {
        let body = self.execute(request).await?;
        serde_json::from_slice(&body).map_err(|source| Error::Decode { context, source })
    }

    /// Sends a request with token refresh and retries; returns the body of a
    /// successful response.
    pub(crate) async fn execute(&self, request: &ApiRequest<'_>) -> Result<Bytes> {
        self.execute_dated(request).await.map(|(body, _)| body)
    }

    /// Like [`execute`](Self::execute), and also returns when Google sent the
    /// successful response by its own clock (the `Date` header), for
    /// requests whose time serves as a watermark (`updatedMin`).
    pub(crate) async fn execute_dated(
        &self,
        request: &ApiRequest<'_>,
    ) -> Result<(Bytes, Option<DateTime<Utc>>)> {
        let url = self.url(request)?;
        let retry = &self.inner.retry;
        let mut attempt = 0u32;
        let mut token_refreshed = false;
        loop {
            let token = self
                .inner
                .tokens
                .access_token()
                .await
                .map_err(Error::Auth)?;
            let started = Instant::now();
            let outcome = match self.build(request, url.clone(), &token).send().await {
                Ok(response) => {
                    let status = response.status();
                    let retry_after = retry_after(response.headers());
                    let date = server_date(response.headers());
                    response
                        .bytes()
                        .await
                        .map(|body| (status, retry_after, date, body))
                }
                Err(err) => Err(err),
            };
            let (status, retry_after, date, body) = match outcome {
                Ok(parts) => parts,
                Err(err) => {
                    if attempt < retry.max_retries && retryable_transport(&err, request) {
                        let delay = retry.delay(attempt, None);
                        tracing::warn!(
                            error = %err,
                            path = url.path(),
                            attempt = attempt + 1,
                            ?delay,
                            "transport error talking to Google; retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::Http(err));
                }
            };
            tracing::debug!(
                method = %request.method,
                path = url.path(),
                status = status.as_u16(),
                elapsed = ?started.elapsed(),
                "Google API request"
            );
            if status.is_success() {
                return Ok((body, date));
            }
            let error = ApiError::from_response(status.as_u16(), &body);
            if status == StatusCode::UNAUTHORIZED && !token_refreshed {
                tracing::debug!(path = url.path(), "access token rejected; refreshing once");
                self.inner.tokens.invalidate().await;
                token_refreshed = true;
                continue;
            }
            if attempt < retry.max_retries && error.is_retryable() {
                let delay = retry.delay(attempt, retry_after);
                tracing::warn!(
                    status = status.as_u16(),
                    reason = error.reason().unwrap_or_default(),
                    path = url.path(),
                    attempt = attempt + 1,
                    ?delay,
                    "Google API request failed transiently; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            return Err(Error::from_api(error, request.sync));
        }
    }

    fn build(&self, request: &ApiRequest<'_>, url: Url, token: &str) -> reqwest::RequestBuilder {
        let mut builder = self
            .inner
            .http
            .request(request.method.clone(), url)
            .bearer_auth(token)
            .header(header::ACCEPT, "application/json");
        if let Some(etag) = request.if_match {
            builder = builder.header(header::IF_MATCH, etag);
        }
        match &request.body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                .body(body.clone()),
            // Google's front end answers bodiless POSTs without a
            // Content-Length header with `411 Length Required`.
            None if request.method != Method::GET && request.method != Method::DELETE => {
                builder.header(header::CONTENT_LENGTH, "0")
            }
            None => builder,
        }
    }

    fn url(&self, request: &ApiRequest<'_>) -> Result<Url> {
        let mut url = match request.service {
            Service::Calendar => self.inner.calendar_base.clone(),
            Service::People => self.inner.people_base.clone(),
            Service::Gmail => self.inner.gmail_base.clone(),
            Service::Tasks => self.inner.tasks_base.clone(),
        };
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::InvalidInput("API base URL cannot carry a path".to_owned()))?;
            segments.pop_if_empty();
            for segment in &request.path {
                segments.push(segment);
            }
        }
        if !request.query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in &request.query {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }

    /// IANA zone used for floating times on writes.
    pub(crate) fn floating_time_zone(&self) -> Option<&str> {
        self.inner.floating_time_zone.as_deref()
    }

    /// The lazily fetched color palette cache.
    pub(crate) fn palette_cell(&self) -> &OnceCell<Arc<ColorPalette>> {
        &self.inner.palette
    }

    /// Remembers a calendar's default reminders (from `calendarList` or an
    /// `events.list` page) for mapping write responses.
    pub(crate) fn remember_default_reminders(&self, calendar_id: &str, reminders: &[Reminder]) {
        let mut cache = self
            .inner
            .default_reminders
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.insert(calendar_id.to_owned(), reminders.to_vec());
    }

    /// Previously seen default reminders of a calendar.
    pub(crate) fn cached_default_reminders(&self, calendar_id: &str) -> Option<Vec<Reminder>> {
        let cache = self
            .inner
            .default_reminders
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.get(calendar_id).cloned()
    }

    /// Warm-up flags for the People search endpoints.
    pub(crate) fn search_warm_flag(&self, other_contacts: bool) -> &AtomicBool {
        if other_contacts {
            &self.inner.other_contacts_search_warm
        } else {
            &self.inner.contacts_search_warm
        }
    }
}

fn parse_base_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw)
        .map_err(|e| Error::InvalidInput(format!("invalid API base URL {raw:?}: {e}")))?;
    if url.cannot_be_a_base() {
        return Err(Error::InvalidInput(format!(
            "API base URL {raw:?} cannot carry a path"
        )));
    }
    Ok(url)
}

fn retryable_transport(err: &reqwest::Error, request: &ApiRequest<'_>) -> bool {
    // A connection that could not be established never reached Google; other
    // failures are only safe to repeat for idempotent methods.
    err.is_connect()
        || (request.is_idempotent() && (err.is_timeout() || err.is_request() || err.is_body()))
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// The `Date` header, an HTTP date such as `Wed, 21 Oct 2015 07:28:00 GMT`.
fn server_date(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let value = headers.get(header::DATE)?.to_str().ok()?;
    DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn random_u64() -> u64 {
    // Each `RandomState` is seeded with fresh per-process randomness, which is
    // plenty for backoff jitter and avoids a dependency on a RNG crate.
    RandomState::new().hash_one(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = RetryPolicy {
            max_retries: 10,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(1_000),
        };
        let first = policy.delay(0, None);
        assert!(first >= Duration::from_millis(100) && first < Duration::from_millis(200));
        let third = policy.delay(2, None);
        assert!(third >= Duration::from_millis(400) && third < Duration::from_millis(500));
        let huge = policy.delay(40, None);
        assert!(huge >= Duration::from_millis(1_000) && huge < Duration::from_millis(1_100));
    }

    #[test]
    fn retry_after_extends_the_delay_up_to_the_cap() {
        let policy = RetryPolicy {
            max_retries: 3,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_secs(5),
        };
        assert_eq!(
            policy.delay(0, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(
            policy.delay(0, Some(Duration::from_secs(60))),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn zero_backoff_has_no_jitter() {
        let policy = RetryPolicy {
            max_retries: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        };
        assert_eq!(policy.delay(3, None), Duration::ZERO);
    }

    #[test]
    fn urls_encode_path_segments() {
        let client = GoogleClient::with_config(
            Arc::new(crate::StaticToken::new("t")),
            ClientConfig::with_api_root("http://127.0.0.1:9/"),
        )
        .expect("client");
        let request = ApiRequest::get(
            Service::Calendar,
            &[
                "calendars",
                "en.usa#holiday@group.v.calendar.google.com",
                "events",
            ],
        )
        .query("timeMin", "2024-01-01T00:00:00+03:00")
        .query("q", "a b");
        let url = client.url(&request).expect("url");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:9/calendar/v3/calendars/en.usa%23holiday@group.v.calendar.google.com/events?timeMin=2024-01-01T00%3A00%3A00%2B03%3A00&q=a+b"
        );
        let people = ApiRequest::get(Service::People, &["people:searchContacts"]);
        assert_eq!(
            client.url(&people).expect("url").as_str(),
            "http://127.0.0.1:9/v1/people:searchContacts"
        );
    }

    #[test]
    fn reads_the_server_date() {
        let mut headers = HeaderMap::new();
        assert_eq!(server_date(&headers), None);
        headers.insert(
            header::DATE,
            header::HeaderValue::from_static("Wed, 28 Oct 2026 10:15:00 GMT"),
        );
        assert_eq!(
            server_date(&headers).map(|d| d.to_rfc3339()),
            Some("2026-10-28T10:15:00+00:00".to_owned())
        );
        headers.insert(header::DATE, header::HeaderValue::from_static("soon"));
        assert_eq!(server_date(&headers), None);
    }

    #[test]
    fn rejects_unusable_base_urls() {
        let config = ClientConfig {
            calendar_base_url: "mailto:someone@example.com".to_owned(),
            ..ClientConfig::default()
        };
        let err = GoogleClient::with_config(Arc::new(crate::StaticToken::new("t")), config)
            .expect_err("must fail");
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
