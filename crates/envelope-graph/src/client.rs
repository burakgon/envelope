//! The core HTTP layer: authentication, retries, error decoding and paging.

use std::collections::HashSet;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use chrono_tz::Tz;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::auth::TokenSource;
use crate::error::{ApiError, GraphError};
use crate::types::is_utc_zone;

/// Base URL of the Microsoft Graph v1.0 endpoint in the global cloud.
pub const GRAPH_V1_BASE_URL: &str = "https://graph.microsoft.com/v1.0";

/// `Prefer` value asking Outlook to return immutable item ids.
pub(crate) const PREFER_IMMUTABLE_ID: &str = "IdType=\"ImmutableId\"";

/// Characters percent-encoded inside query keys and values. Everything that
/// could change the meaning of the query string is encoded, including `+`
/// (which some servers decode as a space) and space itself (sent as `%20`).
const QUERY_COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Microsoft Graph national cloud deployments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NationalCloud {
    /// The global service (`graph.microsoft.com`).
    Global,
    /// US Government L4 / GCC High (`graph.microsoft.us`).
    UsGovL4,
    /// US Government L5 / DOD (`dod-graph.microsoft.us`).
    UsGovL5,
    /// China, operated by 21Vianet (`microsoftgraph.chinacloudapi.cn`).
    China,
}

impl NationalCloud {
    /// The v1.0 base URL of this deployment.
    pub fn base_url(self) -> &'static str {
        match self {
            NationalCloud::Global => GRAPH_V1_BASE_URL,
            NationalCloud::UsGovL4 => "https://graph.microsoft.us/v1.0",
            NationalCloud::UsGovL5 => "https://dod-graph.microsoft.us/v1.0",
            NationalCloud::China => "https://microsoftgraph.chinacloudapi.cn/v1.0",
        }
    }
}

/// How the client retries throttled and failed requests.
///
/// `429 Too Many Requests` and `503 Service Unavailable` responses are
/// retried, honoring `Retry-After` when the server sends it and using
/// exponential backoff with jitter otherwise. `504 Gateway Timeout` and
/// timeouts are retried only for requests that are safe to replay (not for
/// `POST` actions such as sending mail); connection failures always.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of retries per request (not counting the one retry
    /// after refreshing a rejected token).
    pub max_retries: u32,
    /// Delay before the first retry when the server sent no `Retry-After`;
    /// doubled for every further attempt.
    pub initial_backoff: Duration,
    /// Upper bound for computed backoff delays.
    pub max_backoff: Duration,
    /// Longest `Retry-After` the client is willing to wait. When the server
    /// asks for more, the error is returned immediately with
    /// [`ApiError::retry_after`] set so the caller can reschedule.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 4,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries (a rejected token is still refreshed once).
    pub fn none() -> Self {
        RetryPolicy {
            max_retries: 0,
            ..RetryPolicy::default()
        }
    }

    /// Backoff for the given zero-based attempt, with 50–100 % jitter.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.min(16));
        let base = self
            .initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff);
        base.mul_f64(0.5 + 0.5 * random_unit())
    }
}

/// Resolves time zone identifiers found in Graph data (IANA names, or Windows
/// names such as `Pacific Standard Time`) to [`chrono_tz::Tz`].
///
/// The resolver is only used where an exact UTC instant must be derived from a
/// local date, e.g. the `UNTIL` part of a recurrence rule. Event times keep
/// their original time zone name.
pub type TimeZoneResolver = Arc<dyn Fn(&str) -> Option<Tz> + Send + Sync>;

/// The default [`TimeZoneResolver`]: understands IANA names and UTC aliases.
/// Windows zone names are not resolved.
pub fn resolve_iana_time_zone(tzid: &str) -> Option<Tz> {
    if is_utc_zone(tzid) {
        return Some(Tz::UTC);
    }
    tzid.trim().parse::<Tz>().ok()
}

#[derive(Debug, Clone)]
enum Target {
    /// Path segments relative to the base URL.
    Path(Vec<String>),
    /// An absolute URL handed out by Graph (`@odata.nextLink`, …).
    Link(String),
}

#[derive(Debug, Clone)]
struct RequestBody {
    content_type: &'static str,
    bytes: Bytes,
}

/// A request to Microsoft Graph.
///
/// Paths are given as segments relative to the client's base URL; every
/// segment is percent-encoded, so ids can be passed verbatim:
///
/// ```
/// use envelope_graph::GraphRequest;
/// let request = GraphRequest::get(["me", "messages", "AAMkAD=="])
///     .query("$select", "subject,isRead")
///     .prefer("outlook.body-content-type=\"text\"");
/// # let _ = request;
/// ```
#[derive(Debug, Clone)]
pub struct GraphRequest {
    method: Method,
    target: Target,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    prefer: Vec<String>,
    body: Option<RequestBody>,
}

impl GraphRequest {
    /// A request for the path made of `segments`, relative to the base URL.
    pub fn new<I, S>(method: Method, segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        GraphRequest {
            method,
            target: Target::Path(segments.into_iter().map(Into::into).collect()),
            query: Vec::new(),
            headers: Vec::new(),
            prefer: Vec::new(),
            body: None,
        }
    }

    /// A `GET` request.
    pub fn get<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        GraphRequest::new(Method::GET, segments)
    }

    /// A `POST` request.
    pub fn post<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        GraphRequest::new(Method::POST, segments)
    }

    /// A `PATCH` request.
    pub fn patch<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        GraphRequest::new(Method::PATCH, segments)
    }

    /// A `DELETE` request.
    pub fn delete<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        GraphRequest::new(Method::DELETE, segments)
    }

    /// A request for an absolute URL handed out by Graph, such as an
    /// `@odata.nextLink` or `@odata.deltaLink`. The URL must have the same
    /// origin as the client's base URL, otherwise sending fails (the access
    /// token is never sent to another host).
    pub fn link(method: Method, url: impl Into<String>) -> Self {
        GraphRequest {
            method,
            target: Target::Link(url.into()),
            query: Vec::new(),
            headers: Vec::new(),
            prefer: Vec::new(),
            body: None,
        }
    }

    /// A `GET` request for a paging link that keeps this request's headers and
    /// `Prefer` values (query parameters are already encoded in the link).
    pub fn follow(&self, url: impl Into<String>) -> Self {
        GraphRequest {
            method: Method::GET,
            target: Target::Link(url.into()),
            query: Vec::new(),
            headers: self.headers.clone(),
            prefer: self.prefer.clone(),
            body: None,
        }
    }

    /// Appends a query parameter (encoded when sent).
    pub fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    /// Adds a request header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Adds a `Prefer` preference such as `odata.maxpagesize=50` or
    /// `outlook.timezone="UTC"`. All preferences are sent in one header.
    pub fn prefer(mut self, preference: impl Into<String>) -> Self {
        self.prefer.push(preference.into());
        self
    }

    /// Sets a JSON body.
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Result<Self, GraphError> {
        let bytes = serde_json::to_vec(body)
            .map_err(|e| GraphError::InvalidInput(format!("cannot serialize request body: {e}")))?;
        self.body = Some(RequestBody {
            content_type: "application/json",
            bytes: Bytes::from(bytes),
        });
        Ok(self)
    }

    /// Sets a `text/plain` body (used for base64-encoded MIME content).
    pub fn text(mut self, body: impl Into<String>) -> Self {
        self.body = Some(RequestBody {
            content_type: "text/plain",
            bytes: Bytes::from(body.into()),
        });
        self
    }

    /// The HTTP method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The `Prefer` values attached to this request.
    pub fn preferences(&self) -> &[String] {
        &self.prefer
    }

    /// Relative URL (path and query) as used inside a JSON batch.
    pub(crate) fn relative_url(&self) -> Result<String, GraphError> {
        let segments = match &self.target {
            Target::Path(segments) => segments,
            Target::Link(_) => {
                return Err(GraphError::InvalidInput(
                    "absolute links cannot be sent inside a JSON batch".into(),
                ));
            }
        };
        let mut url = Url::parse("http://batch.invalid/")
            .map_err(|e| GraphError::InvalidInput(format!("internal URL error: {e}")))?;
        push_segments(&mut url, segments)?;
        append_query(&mut url, &self.query);
        let mut relative = url.path().to_owned();
        if let Some(query) = url.query() {
            relative.push('?');
            relative.push_str(query);
        }
        Ok(relative)
    }

    pub(crate) fn json_body(&self) -> Result<Option<serde_json::Value>, GraphError> {
        match &self.body {
            None => Ok(None),
            Some(body) if body.content_type == "application/json" => {
                serde_json::from_slice(&body.bytes)
                    .map(Some)
                    .map_err(|e| GraphError::InvalidInput(format!("invalid JSON body: {e}")))
            }
            Some(_) => Err(GraphError::InvalidInput(
                "only JSON bodies can be sent inside a JSON batch".into(),
            )),
        }
    }

    pub(crate) fn extra_headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

/// A successful (2xx) response from Microsoft Graph.
#[derive(Debug, Clone)]
pub struct GraphResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl GraphResponse {
    /// HTTP status code.
    pub fn status(&self) -> u16 {
        self.status.as_u16()
    }

    /// Response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// A response header as text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The raw body.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// Consumes the response and returns the raw body.
    pub fn into_body(self) -> Bytes {
        self.body
    }

    /// Decodes the body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, GraphError> {
        serde_json::from_slice(&self.body).map_err(|e| GraphError::decode("response body", e))
    }
}

/// One page of a Graph collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    /// The items on this page (possibly none, even when more pages follow).
    pub items: Vec<T>,
    /// `@odata.nextLink`: absolute URL of the next page, if any.
    pub next_link: Option<String>,
}

/// The JSON envelope of a Graph collection response.
#[derive(Debug, Deserialize)]
pub(crate) struct Collection<T> {
    #[serde(default = "Vec::new")]
    pub(crate) value: Vec<T>,
    #[serde(rename = "@odata.nextLink", default)]
    pub(crate) next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink", default)]
    pub(crate) delta_link: Option<String>,
}

struct ClientInner {
    http: reqwest::Client,
    base: Url,
    tokens: Arc<dyn TokenSource>,
    retry: RetryPolicy,
    immutable_ids: bool,
    tz_resolver: TimeZoneResolver,
}

/// A Microsoft Graph v1.0 client for one signed-in user (`/me`).
///
/// The client is cheap to clone; clones share the connection pool and token
/// source.
#[derive(Clone)]
pub struct GraphClient {
    inner: Arc<ClientInner>,
}

impl fmt::Debug for GraphClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphClient")
            .field("base_url", &self.inner.base.as_str())
            .field("retry", &self.inner.retry)
            .field("immutable_ids", &self.inner.immutable_ids)
            .finish_non_exhaustive()
    }
}

/// Builder for [`GraphClient`].
pub struct GraphClientBuilder {
    tokens: Arc<dyn TokenSource>,
    base_url: String,
    http: Option<reqwest::Client>,
    retry: RetryPolicy,
    immutable_ids: bool,
    user_agent: String,
    tz_resolver: TimeZoneResolver,
}

impl fmt::Debug for GraphClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphClientBuilder")
            .field("base_url", &self.base_url)
            .field("retry", &self.retry)
            .field("immutable_ids", &self.immutable_ids)
            .field("user_agent", &self.user_agent)
            .finish_non_exhaustive()
    }
}

impl GraphClientBuilder {
    /// Overrides the base URL (default [`GRAPH_V1_BASE_URL`]); used for tests
    /// and national clouds. Include the version segment, e.g.
    /// `https://graph.microsoft.us/v1.0`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Targets a national cloud deployment.
    pub fn national_cloud(self, cloud: NationalCloud) -> Self {
        self.base_url(cloud.base_url())
    }

    /// Uses a preconfigured `reqwest` client (proxy, TLS roots, timeouts, …).
    /// The user agent configured on this builder is not applied to it.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Sets the retry policy.
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Whether to request immutable Outlook item ids
    /// (`Prefer: IdType="ImmutableId"`). Enabled by default: message, event
    /// and contact ids then survive moves between folders. Mailbox data
    /// stored under one id format must not be mixed with the other.
    pub fn immutable_ids(mut self, enabled: bool) -> Self {
        self.immutable_ids = enabled;
        self
    }

    /// Sets the `User-Agent` of the internally created HTTP client.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Installs a [`TimeZoneResolver`], typically one that also maps Windows
    /// time zone names to IANA zones (such as the TZID resolver of
    /// `envelope-ical`). The default only knows IANA names; series masters
    /// in Windows zones then get their `UNTIL` from the UTC offset of the
    /// series start, and all-day exceptions east of UTC+12 may be dated a
    /// day early.
    pub fn time_zone_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&str) -> Option<Tz> + Send + Sync + 'static,
    {
        self.tz_resolver = Arc::new(resolver);
        self
    }

    /// Builds the client.
    pub fn build(self) -> Result<GraphClient, GraphError> {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        let mut base = Url::parse(trimmed)
            .map_err(|e| GraphError::InvalidInput(format!("invalid base URL `{trimmed}`: {e}")))?;
        if base.cannot_be_a_base() || !matches!(base.scheme(), "http" | "https") {
            return Err(GraphError::InvalidInput(format!(
                "base URL `{trimmed}` must be an http(s) URL"
            )));
        }
        base.set_query(None);
        base.set_fragment(None);
        let http = match self.http {
            Some(http) => http,
            None => reqwest::Client::builder()
                .user_agent(self.user_agent)
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(120))
                .build()?,
        };
        Ok(GraphClient {
            inner: Arc::new(ClientInner {
                http,
                base,
                tokens: self.tokens,
                retry: self.retry,
                immutable_ids: self.immutable_ids,
                tz_resolver: self.tz_resolver,
            }),
        })
    }
}

impl GraphClient {
    /// Starts building a client that authenticates with `tokens`.
    pub fn builder(tokens: Arc<dyn TokenSource>) -> GraphClientBuilder {
        GraphClientBuilder {
            tokens,
            base_url: GRAPH_V1_BASE_URL.to_owned(),
            http: None,
            retry: RetryPolicy::default(),
            immutable_ids: true,
            user_agent: concat!("Envelope/", env!("CARGO_PKG_VERSION"), " (envelope-graph)")
                .to_owned(),
            tz_resolver: Arc::new(resolve_iana_time_zone),
        }
    }

    /// A client for the global cloud with default settings.
    pub fn new(tokens: Arc<dyn TokenSource>) -> Result<Self, GraphError> {
        GraphClient::builder(tokens).build()
    }

    /// The base URL requests are resolved against.
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// Whether immutable item ids are requested.
    pub fn uses_immutable_ids(&self) -> bool {
        self.inner.immutable_ids
    }

    /// Resolves a time zone name with the configured [`TimeZoneResolver`].
    pub fn resolve_time_zone(&self, tzid: &str) -> Option<Tz> {
        (self.inner.tz_resolver)(tzid)
    }

    pub(crate) fn retry_policy(&self) -> &RetryPolicy {
        &self.inner.retry
    }

    /// Sends a request, refreshing a rejected token once and retrying
    /// throttled or failed attempts according to the [`RetryPolicy`].
    ///
    /// Returns the response for 2xx statuses and [`GraphError::Api`] otherwise.
    pub async fn execute(&self, request: &GraphRequest) -> Result<GraphResponse, GraphError> {
        let url = self.resolve_url(request)?;
        let headers = self.build_headers(request)?;
        let client_request_id = new_request_id();
        let retry = &self.inner.retry;
        let mut attempt: u32 = 0;
        let mut refreshed = false;
        loop {
            let token = self
                .inner
                .tokens
                .access_token()
                .await
                .map_err(GraphError::Auth)?;
            let mut builder = self
                .inner
                .http
                .request(request.method.clone(), url.clone())
                .headers(headers.clone())
                .header("client-request-id", client_request_id.as_str())
                .bearer_auth(token);
            if let Some(body) = &request.body {
                builder = builder
                    .header(CONTENT_TYPE, body.content_type)
                    .body(body.bytes.clone());
            } else if matches!(request.method, Method::POST | Method::PUT | Method::PATCH) {
                // Graph answers body-less POSTs (`/send`, `/permanentDelete`)
                // with 411 unless `Content-Length: 0` is sent explicitly;
                // hyper omits it for empty bodies.
                builder = builder
                    .header(CONTENT_LENGTH, HeaderValue::from_static("0"))
                    .body(Bytes::new());
            }
            let response = match builder.send().await {
                Ok(response) => response,
                Err(err) => {
                    if attempt < retry.max_retries && is_retryable_transport(&err, &request.method)
                    {
                        let delay = retry.backoff(attempt);
                        tracing::warn!(
                            method = %request.method,
                            path = url.path(),
                            attempt,
                            ?delay,
                            error = %err,
                            "Graph request failed; retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(GraphError::Transport(err));
                }
            };
            let status = response.status();
            let response_headers = response.headers().clone();
            tracing::debug!(
                method = %request.method,
                path = url.path(),
                status = status.as_u16(),
                attempt,
                "Graph request"
            );
            if status.is_success() {
                match response.bytes().await {
                    Ok(body) => {
                        return Ok(GraphResponse {
                            status,
                            headers: response_headers,
                            body,
                        });
                    }
                    Err(err) => {
                        if attempt < retry.max_retries && is_replayable(&request.method) {
                            tokio::time::sleep(retry.backoff(attempt)).await;
                            attempt += 1;
                            continue;
                        }
                        return Err(GraphError::Transport(err));
                    }
                }
            }
            let body = response.bytes().await.unwrap_or_default();
            if status == StatusCode::NOT_FOUND && attempt > 0 && request.method == Method::DELETE {
                // An earlier attempt that timed out or got a 5xx may have
                // deleted the item already; the requested state holds.
                return Ok(GraphResponse {
                    status: StatusCode::NO_CONTENT,
                    headers: response_headers,
                    body: Bytes::new(),
                });
            }
            let error = ApiError::from_response(status.as_u16(), &response_headers, &body);
            if status == StatusCode::UNAUTHORIZED && !refreshed {
                tracing::debug!(path = url.path(), "access token rejected; refreshing");
                self.inner.tokens.invalidate().await;
                refreshed = true;
                continue;
            }
            if is_retryable_status(status, &request.method) && attempt < retry.max_retries {
                let delay = error.retry_after.unwrap_or_else(|| retry.backoff(attempt));
                if delay <= retry.max_retry_after {
                    tracing::warn!(
                        method = %request.method,
                        path = url.path(),
                        status = status.as_u16(),
                        attempt,
                        ?delay,
                        "Graph throttled or unavailable; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
            }
            return Err(GraphError::Api(Box::new(error)));
        }
    }

    /// Sends a request and decodes the JSON response body.
    pub async fn send_json<T: DeserializeOwned>(
        &self,
        request: &GraphRequest,
    ) -> Result<T, GraphError> {
        self.execute(request).await?.json()
    }

    /// PUTs one byte range of a file to a pre-authenticated upload URL (an
    /// attachment upload session). No `Authorization` header is sent: the URL
    /// carries its own credentials.
    pub(crate) async fn put_range(
        &self,
        url: &str,
        data: Bytes,
        start: usize,
        total: usize,
    ) -> Result<(), GraphError> {
        let end = start + data.len();
        let response = self
            .inner
            .http
            .put(url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, data.len())
            .header(
                reqwest::header::CONTENT_RANGE,
                format!("bytes {start}-{}/{total}", end.saturating_sub(1)),
            )
            .body(data)
            .send()
            .await?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let headers = response.headers().clone();
        let body = response.bytes().await.unwrap_or_default();
        Err(GraphError::Api(Box::new(ApiError::from_response(
            status, &headers, &body,
        ))))
    }

    /// Sends a request whose response body is irrelevant.
    pub async fn send_empty(&self, request: &GraphRequest) -> Result<(), GraphError> {
        self.execute(request).await.map(drop)
    }

    /// Fetches one page of a collection.
    pub async fn get_page<T: DeserializeOwned>(
        &self,
        request: &GraphRequest,
    ) -> Result<Page<T>, GraphError> {
        let collection: Collection<T> = self.send_json(request).await?;
        Ok(Page {
            items: collection.value,
            next_link: collection.next_link,
        })
    }

    /// Fetches a collection, following `@odata.nextLink` until the end or
    /// until `limit` items were collected. Follow-up requests carry the same
    /// headers and `Prefer` values as `request`.
    pub async fn get_all<T: DeserializeOwned>(
        &self,
        request: &GraphRequest,
        limit: Option<usize>,
    ) -> Result<Vec<T>, GraphError> {
        let mut items = Vec::new();
        let mut page: Page<T> = self.get_page(request).await?;
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            items.extend(page.items);
            if let Some(limit) = limit
                && items.len() >= limit
            {
                items.truncate(limit);
                break;
            }
            match page.next_link {
                Some(link) if !seen.insert(link.clone()) => {
                    return Err(GraphError::Decode(
                        "paging did not advance (repeated @odata.nextLink)".into(),
                    ));
                }
                Some(link) => page = self.get_page(&request.follow(link)).await?,
                None => break,
            }
        }
        Ok(items)
    }

    fn resolve_url(&self, request: &GraphRequest) -> Result<Url, GraphError> {
        let mut url = match &request.target {
            Target::Path(segments) => {
                let mut url = self.inner.base.clone();
                push_segments(&mut url, segments)?;
                url
            }
            Target::Link(link) => {
                let url = Url::parse(link)
                    .map_err(|e| GraphError::InvalidInput(format!("invalid link `{link}`: {e}")))?;
                if url.origin() != self.inner.base.origin() {
                    return Err(GraphError::InvalidInput(format!(
                        "refusing to send credentials to foreign origin {}",
                        url.origin().ascii_serialization()
                    )));
                }
                url
            }
        };
        append_query(&mut url, &request.query);
        Ok(url)
    }

    fn build_headers(&self, request: &GraphRequest) -> Result<HeaderMap, GraphError> {
        let mut headers = HeaderMap::new();
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                GraphError::InvalidInput(format!("invalid header name `{name}`: {e}"))
            })?;
            let value = HeaderValue::from_str(value)
                .map_err(|e| GraphError::InvalidInput(format!("invalid header value: {e}")))?;
            headers.append(name, value);
        }
        headers
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static("application/json"));
        if let Some(prefer) = self.prefer_header(&request.prefer) {
            let value = HeaderValue::from_str(&prefer)
                .map_err(|e| GraphError::InvalidInput(format!("invalid Prefer value: {e}")))?;
            headers.insert("prefer", value);
        }
        Ok(headers)
    }

    /// The combined `Prefer` header for a request, including the immutable
    /// id preference when enabled.
    pub(crate) fn prefer_header(&self, preferences: &[String]) -> Option<String> {
        let mut all: Vec<&str> = Vec::with_capacity(preferences.len() + 1);
        if self.inner.immutable_ids
            && !preferences
                .iter()
                .any(|p| p.to_ascii_lowercase().starts_with("idtype"))
        {
            all.push(PREFER_IMMUTABLE_ID);
        }
        all.extend(preferences.iter().map(String::as_str));
        if all.is_empty() {
            None
        } else {
            Some(all.join(", "))
        }
    }
}

fn push_segments(url: &mut Url, segments: &[String]) -> Result<(), GraphError> {
    let mut path = url
        .path_segments_mut()
        .map_err(|()| GraphError::InvalidInput("base URL cannot have a path".into()))?;
    path.pop_if_empty();
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(GraphError::InvalidInput(format!(
                "invalid path segment `{segment}` (missing id?)"
            )));
        }
        path.push(segment);
    }
    Ok(())
}

fn append_query(url: &mut Url, pairs: &[(String, String)]) {
    if pairs.is_empty() {
        return;
    }
    let encoded = pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                utf8_percent_encode(k, QUERY_COMPONENT),
                utf8_percent_encode(v, QUERY_COMPONENT)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let query = match url.query() {
        Some(existing) if !existing.is_empty() => format!("{existing}&{encoded}"),
        _ => encoded,
    };
    url.set_query(Some(&query));
}

/// `429` and `503` mean the request was turned away before processing and
/// may always be retried; a `504` may come after the work was done, so it is
/// only retried for requests that are safe to replay.
fn is_retryable_status(status: StatusCode, method: &Method) -> bool {
    match status {
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => true,
        StatusCode::GATEWAY_TIMEOUT => is_replayable(method),
        _ => false,
    }
}

/// Methods whose repetition has no additional effect. Graph `PATCH` requests
/// set properties, so replaying one is harmless; `POST` actions (sending
/// mail, moving, copying) are not replayed after a possible success.
fn is_replayable(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::PUT | Method::DELETE | Method::PATCH
    )
}

fn is_retryable_transport(err: &reqwest::Error, method: &Method) -> bool {
    err.is_connect() || (err.is_timeout() && is_replayable(method))
}

/// Process-local randomness without an extra dependency: `RandomState` is
/// seeded randomly per instance.
fn random_u64() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    hasher.write_u128(nanos);
    hasher.finish()
}

fn random_unit() -> f64 {
    // 53 random bits mapped onto [0, 1).
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// A random RFC 4122 version 4 identifier for `client-request-id` and
/// `transactionId`.
pub(crate) fn new_request_id() -> String {
    let high = random_u64();
    let low = random_u64();
    let high = (high & 0xFFFF_FFFF_FFFF_0FFF) | 0x0000_0000_0000_4000;
    let low = (low & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        high >> 32,
        (high >> 16) & 0xFFFF,
        high & 0xFFFF,
        low >> 48,
        low & 0xFFFF_FFFF_FFFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::StaticToken;

    fn client(base: &str) -> GraphClient {
        GraphClient::builder(Arc::new(StaticToken::new("t")))
            .base_url(base)
            .build()
            .unwrap_or_else(|e| panic!("client: {e}"))
    }

    #[test]
    fn encodes_path_segments_and_query() {
        let c = client("https://graph.microsoft.com/v1.0/");
        let req = GraphRequest::get(["me", "messages", "AAMk/a+b="])
            .query("$filter", "receivedDateTime ge 2024-01-01T00:00:00Z")
            .query("$search", "\"a&b\"");
        let url = c.resolve_url(&req).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(url.path(), "/v1.0/me/messages/AAMk%2Fa+b=");
        let query = url.query().unwrap_or_default();
        assert!(query.contains("$filter=receivedDateTime%20ge%202024-01-01T00:00:00Z"));
        assert!(query.contains("$search=%22a%26b%22"));
    }

    #[test]
    fn rejects_foreign_links_and_empty_segments() {
        let c = client("https://graph.microsoft.com/v1.0");
        let foreign = GraphRequest::link(Method::GET, "https://evil.example/v1.0/me");
        assert!(matches!(
            c.resolve_url(&foreign),
            Err(GraphError::InvalidInput(_))
        ));
        let same = GraphRequest::link(
            Method::GET,
            "https://graph.microsoft.com/v1.0/me/messages?$skiptoken=x",
        );
        assert!(c.resolve_url(&same).is_ok());
        for bad in ["", ".", ".."] {
            let request = GraphRequest::get(["me", "messages", bad]);
            assert!(matches!(
                c.resolve_url(&request),
                Err(GraphError::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn batch_relative_urls() {
        let req = GraphRequest::get(["me", "mailFolders", "inbox"]).query("$select", "id");
        assert_eq!(
            req.relative_url().ok().as_deref(),
            Some("/me/mailFolders/inbox?$select=id")
        );
    }

    #[test]
    fn combines_prefer_values() {
        let c = client("https://graph.microsoft.com/v1.0");
        assert_eq!(
            c.prefer_header(&["odata.maxpagesize=10".into()]).as_deref(),
            Some("IdType=\"ImmutableId\", odata.maxpagesize=10")
        );
        let plain = GraphClient::builder(Arc::new(StaticToken::new("t")))
            .immutable_ids(false)
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(plain.prefer_header(&[]), None);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = RetryPolicy {
            max_retries: 5,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(400),
            max_retry_after: Duration::from_secs(1),
        };
        assert!(policy.backoff(0) <= Duration::from_millis(100));
        assert!(policy.backoff(0) >= Duration::from_millis(50));
        assert!(policy.backoff(10) <= Duration::from_millis(400));
        assert!(policy.backoff(10) >= Duration::from_millis(200));
    }

    #[test]
    fn request_ids_look_like_uuids() {
        let id = new_request_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(id, new_request_id());
    }

    #[test]
    fn rejects_bad_base_urls() {
        let err = GraphClient::builder(Arc::new(StaticToken::new("t")))
            .base_url("mailto:someone@example.com")
            .build();
        assert!(err.is_err());
    }
}
