//! The HTTP layer: client construction, authentication, redirects and the
//! mapping of HTTP statuses to [`DavError`]s.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use reqwest::header::{
    AUTHORIZATION, CONTENT_TYPE, ETAG, HeaderMap, HeaderName, HeaderValue, LOCATION, RETRY_AFTER,
    WWW_AUTHENTICATE,
};
use reqwest::{Method, StatusCode};
use tracing::{debug, warn};
use url::Url;

use crate::auth::{self, Auth, DigestChallenge, DigestRequest, DigestSession, TokenSource};
use crate::error::{DavError, Result};
use crate::href;
use crate::multistatus::{self, MultiStatus, Response};
use crate::util::truncate_for_display;

/// `User-Agent` sent unless [`DavClientBuilder::user_agent`] overrides it.
pub const DEFAULT_USER_AGENT: &str = concat!(
    "Envelope/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/burakgon/envelope)"
);

/// Default limit for a whole request, including reading the response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Default limit for establishing a connection (DNS, TCP and TLS).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 10;

/// Content type of XML request bodies.
const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";

/// A CalDAV / CardDAV client bound to one account.
///
/// The client is cheap to clone (all clones share one connection pool and
/// the negotiated authentication state) and every method can be called
/// concurrently from multiple tasks.
///
/// Hrefs accepted by its methods may be absolute URLs on any host or
/// absolute paths, which are resolved against the base URL. Hrefs returned
/// by it are always absolute and normalised (see
/// [`normalize_href`](crate::normalize_href)).
#[derive(Clone)]
pub struct DavClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base: Url,
    auth: AuthState,
}

enum AuthState {
    None,
    Password {
        username: String,
        password: String,
        /// Scheme negotiated per origin (`scheme://host:port`).
        schemes: Mutex<HashMap<String, PasswordScheme>>,
    },
    Bearer(Arc<dyn TokenSource>),
}

#[derive(Debug, Clone)]
enum PasswordScheme {
    Basic,
    Digest(DigestSession),
}

/// Which credentials accompanied a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SentAuth {
    Nothing,
    Basic,
    Digest,
    Bearer,
}

impl std::fmt::Debug for DavClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auth = match &self.inner.auth {
            AuthState::None => "none",
            AuthState::Password { .. } => "password",
            AuthState::Bearer(_) => "bearer",
        };
        f.debug_struct("DavClient")
            .field("base_url", &self.inner.base.as_str())
            .field("auth", &auth)
            .finish_non_exhaustive()
    }
}

/// Builder for [`DavClient`].
#[derive(Debug, Clone)]
pub struct DavClientBuilder {
    base_url: String,
    auth: Auth,
    accept_invalid_certs: bool,
    timeout: Duration,
    connect_timeout: Duration,
    user_agent: String,
}

impl DavClientBuilder {
    /// Sets the credentials (default: [`Auth::None`]).
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }

    /// Accepts TLS certificates that do not verify (self-signed, expired or
    /// issued for another name). Only meant for self-hosted servers the user
    /// explicitly trusts.
    pub fn accept_invalid_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_certs = accept;
        self
    }

    /// Limit for a whole request including the response body (default
    /// [`DEFAULT_TIMEOUT`]).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Limit for establishing a connection (default [`DEFAULT_CONNECT_TIMEOUT`]).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Overrides the `User-Agent` header (default [`DEFAULT_USER_AGENT`]).
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Builds the client.
    ///
    /// Fails when the base URL is unusable or the TLS backend cannot be
    /// initialised.
    pub fn build(self) -> Result<DavClient> {
        let base = href::parse_user_url(&self.base_url)?;
        let http = reqwest::Client::builder()
            .user_agent(self.user_agent)
            // Redirects are followed manually so that PROPFIND / REPORT keep
            // their method and body, and credentials are re-negotiated.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            .tls_danger_accept_invalid_certs(self.accept_invalid_certs)
            .build()
            .map_err(DavError::Network)?;
        let auth = match self.auth {
            Auth::None => AuthState::None,
            Auth::Basic { username, password } => AuthState::Password {
                username,
                password,
                schemes: Mutex::new(HashMap::new()),
            },
            Auth::Bearer(source) => AuthState::Bearer(source),
        };
        Ok(DavClient {
            inner: Arc::new(Inner { http, base, auth }),
        })
    }
}

impl DavClient {
    /// Creates a client for `base_url` with default settings.
    ///
    /// `base_url` may be a full URL (`https://cloud.example.com/remote.php/dav`)
    /// or just a host (`caldav.icloud.com`), in which case HTTPS is assumed.
    pub fn new(base_url: &str, auth: Auth) -> Result<Self> {
        Self::builder(base_url).auth(auth).build()
    }

    /// Starts building a client for `base_url` (see [`DavClient::new`]).
    pub fn builder(base_url: &str) -> DavClientBuilder {
        DavClientBuilder {
            base_url: base_url.to_owned(),
            auth: Auth::None,
            accept_invalid_certs: false,
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }

    /// The normalised base URL.
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// Resolves an href supplied by the caller (absolute URL or path).
    pub(crate) fn resolve_input(&self, href: &str) -> Result<Url> {
        href::resolve(&self.inner.base, href)
    }

    /// Sends a request, handling authentication challenges and redirects.
    ///
    /// Returns the final response whatever its status, except for `401`
    /// (mapped to [`DavError::Unauthorized`] once retries are exhausted).
    pub(crate) async fn send(&self, request: DavRequest) -> Result<HttpResponse> {
        let DavRequest {
            method,
            url: original_url,
            headers,
            body,
        } = request;
        let mut url = original_url.clone();
        let mut redirects = 0usize;
        let mut auth_retried = false;

        loop {
            let (authorization, sent) = self.authorization(&method, &url, body.as_deref()).await?;
            let mut builder = self
                .inner
                .http
                .request(method.clone(), url.clone())
                .headers(headers.clone());
            if let Some(value) = authorization {
                builder = builder.header(AUTHORIZATION, value);
            }
            if let Some(body) = &body {
                builder = builder.body(body.clone());
            }

            let started = Instant::now();
            let response = builder.send().await.map_err(DavError::from_reqwest)?;
            let status = response.status();
            debug!(
                method = %method,
                url = %url,
                status = status.as_u16(),
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "DAV request"
            );

            if status == StatusCode::UNAUTHORIZED {
                let response_headers = response.headers().clone();
                drain(response).await;
                if !auth_retried
                    && self
                        .handle_unauthorized(&url, &response_headers, sent)
                        .await
                {
                    auth_retried = true;
                    continue;
                }
                return Err(DavError::Unauthorized);
            }

            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| value.trim().to_owned());
                if let Some(location) = location {
                    drain(response).await;
                    redirects += 1;
                    if redirects > MAX_REDIRECTS {
                        return Err(DavError::TooManyRedirects {
                            url: original_url.to_string(),
                        });
                    }
                    let next = url
                        .join(&location)
                        .map_err(|err| DavError::invalid_url(location.clone(), err))?;
                    if !matches!(next.scheme(), "http" | "https") {
                        return Err(DavError::invalid_url(
                            location,
                            "redirect to a non-HTTP URL",
                        ));
                    }
                    debug!(from = %url, to = %next, status = status.as_u16(), "following redirect");
                    url = next;
                    auth_retried = false;
                    continue;
                }
            }

            let response_headers = response.headers().clone();
            let text = response.text().await.map_err(DavError::from_reqwest)?;
            return Ok(HttpResponse {
                method,
                status,
                headers: response_headers,
                url,
                body: text,
            });
        }
    }

    /// Computes the `Authorization` header for a request.
    async fn authorization(
        &self,
        method: &Method,
        url: &Url,
        body: Option<&[u8]>,
    ) -> Result<(Option<HeaderValue>, SentAuth)> {
        match &self.inner.auth {
            AuthState::None => Ok((None, SentAuth::Nothing)),
            AuthState::Bearer(source) => {
                // Never expose a token on an unencrypted connection.
                if !href::is_secure_or_local(url) {
                    return Ok((None, SentAuth::Nothing));
                }
                let token = source.access_token().await.map_err(DavError::TokenSource)?;
                let value = sensitive_header(&format!("Bearer {}", token.trim()))?;
                Ok((Some(value), SentAuth::Bearer))
            }
            AuthState::Password {
                username,
                password,
                schemes,
            } => {
                let origin = origin_key(url);
                let mut schemes = lock(schemes);
                match schemes.get_mut(&origin) {
                    Some(PasswordScheme::Digest(session)) => {
                        let uri = request_target(url);
                        let cnonce = auth::new_cnonce();
                        let header = session.authorization(&DigestRequest {
                            username,
                            password,
                            method: method.as_str(),
                            uri: &uri,
                            body: body.unwrap_or_default(),
                            cnonce: &cnonce,
                        });
                        Ok((Some(sensitive_header(&header)?), SentAuth::Digest))
                    }
                    Some(PasswordScheme::Basic) => Ok((
                        Some(sensitive_header(&auth::basic_header(username, password))?),
                        SentAuth::Basic,
                    )),
                    // Unknown server: send Basic pre-emptively only where it
                    // cannot be sniffed; elsewhere wait for the challenge so a
                    // Digest-only server never sees the cleartext password.
                    None if href::is_secure_or_local(url) => Ok((
                        Some(sensitive_header(&auth::basic_header(username, password))?),
                        SentAuth::Basic,
                    )),
                    None => Ok((None, SentAuth::Nothing)),
                }
            }
        }
    }

    /// Reacts to a `401` response. Returns `true` when retrying the request
    /// with updated credentials may succeed.
    async fn handle_unauthorized(&self, url: &Url, headers: &HeaderMap, sent: SentAuth) -> bool {
        match &self.inner.auth {
            AuthState::None => false,
            AuthState::Bearer(source) => {
                if sent == SentAuth::Bearer {
                    debug!(url = %url, "bearer token rejected; refreshing it");
                    source.invalidate().await;
                    true
                } else {
                    false
                }
            }
            AuthState::Password { schemes, .. } => {
                let challenges = auth::parse_challenges(
                    headers
                        .get_all(WWW_AUTHENTICATE)
                        .iter()
                        .filter_map(|value| value.to_str().ok()),
                );
                let origin = origin_key(url);
                let mut schemes = lock(schemes);
                if let Some(digest) = DigestChallenge::select(&challenges) {
                    let nonce_unchanged = matches!(
                        schemes.get(&origin),
                        Some(PasswordScheme::Digest(session)) if session.challenge.nonce == digest.nonce
                    );
                    if sent == SentAuth::Digest && !digest.stale && nonce_unchanged {
                        // A fresh nonce was rejected: wrong credentials.
                        return false;
                    }
                    debug!(url = %url, algorithm = ?digest.algorithm, "using HTTP Digest authentication");
                    schemes.insert(origin, PasswordScheme::Digest(DigestSession::new(digest)));
                    return true;
                }
                if sent != SentAuth::Basic && challenges.iter().any(|c| c.scheme == "basic") {
                    schemes.insert(origin, PasswordScheme::Basic);
                    return true;
                }
                false
            }
        }
    }

    /// `PROPFIND` returning the parsed multistatus.
    pub(crate) async fn propfind(
        &self,
        url: &Url,
        depth: Depth,
        body: String,
    ) -> Result<MultiStatusReply> {
        let request = DavRequest::new(dav_method("PROPFIND")?, url.clone())
            .depth(depth)
            .xml(body);
        let request = if depth == Depth::One {
            // RFC 8144: omit 404 propstats for the many members of a collection.
            request.header(
                HeaderName::from_static("prefer"),
                HeaderValue::from_static("return=minimal"),
            )
        } else {
            request
        };
        self.send(request).await?.into_multistatus()
    }

    /// `REPORT` returning the raw response (callers interpret error statuses).
    pub(crate) async fn report_raw(
        &self,
        url: &Url,
        depth: Depth,
        body: String,
    ) -> Result<HttpResponse> {
        let request = DavRequest::new(dav_method("REPORT")?, url.clone())
            .depth(depth)
            .xml(body);
        self.send(request).await
    }

    /// `REPORT` returning the parsed multistatus.
    pub(crate) async fn report(
        &self,
        url: &Url,
        depth: Depth,
        body: String,
    ) -> Result<MultiStatusReply> {
        self.report_raw(url, depth, body).await?.into_multistatus()
    }
}

/// Reads and discards a response body so the connection can be reused.
async fn drain(response: reqwest::Response) {
    let _ = response.bytes().await;
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn origin_key(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// The request target as sent on the request line (`path?query`).
fn request_target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

fn sensitive_header(value: &str) -> Result<HeaderValue> {
    let mut header = HeaderValue::from_bytes(value.as_bytes()).map_err(|_| {
        DavError::InvalidInput("credentials contain characters not allowed in HTTP headers".into())
    })?;
    header.set_sensitive(true);
    Ok(header)
}

/// Builds an extension method such as `PROPFIND`.
pub(crate) fn dav_method(name: &'static str) -> Result<Method> {
    Method::from_bytes(name.as_bytes())
        .map_err(|_| DavError::InvalidInput(format!("invalid HTTP method {name}")))
}

/// Value of the `Depth` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Depth {
    Zero,
    One,
}

impl Depth {
    fn header_value(self) -> HeaderValue {
        match self {
            Depth::Zero => HeaderValue::from_static("0"),
            Depth::One => HeaderValue::from_static("1"),
        }
    }
}

/// A request to send with [`DavClient::send`].
#[derive(Debug)]
pub(crate) struct DavRequest {
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Option<Bytes>,
}

impl DavRequest {
    pub(crate) fn new(method: Method, url: Url) -> Self {
        DavRequest {
            method,
            url,
            headers: HeaderMap::new(),
            body: None,
        }
    }

    pub(crate) fn depth(self, depth: Depth) -> Self {
        self.header(HeaderName::from_static("depth"), depth.header_value())
    }

    pub(crate) fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    pub(crate) fn body(mut self, content_type: &'static str, body: impl Into<Bytes>) -> Self {
        self.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        self.body = Some(body.into());
        self
    }

    pub(crate) fn xml(self, body: String) -> Self {
        self.body(XML_CONTENT_TYPE, body)
    }
}

/// A response whose body has been read.
#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub(crate) method: Method,
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    /// The URL that produced the response (after redirects).
    pub(crate) url: Url,
    pub(crate) body: String,
}

impl HttpResponse {
    /// The `ETag` header, normalised.
    pub(crate) fn etag(&self) -> Option<String> {
        self.headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_etag)
    }

    /// The precondition named in a `DAV:error` body.
    pub(crate) fn error_condition(&self) -> Option<multistatus::ErrorCondition> {
        multistatus::parse_error_condition(&self.body)
    }

    /// Converts an unsuccessful response into the matching error.
    pub(crate) fn into_error(self) -> DavError {
        let url = self.url.to_string();
        match self.status.as_u16() {
            401 => DavError::Unauthorized,
            403 => DavError::Forbidden {
                url,
                condition: self.error_condition().map(|c| c.name),
            },
            404 | 410 => DavError::NotFound { url },
            409 => DavError::Conflict {
                url,
                condition: self.error_condition().map(|c| c.name),
            },
            412 => DavError::PreconditionFailed { url },
            429 => DavError::RateLimited {
                retry_after: retry_after(&self.headers),
            },
            503 if self.headers.contains_key(RETRY_AFTER) => DavError::RateLimited {
                retry_after: retry_after(&self.headers),
            },
            status => DavError::Http {
                status,
                method: self.method.to_string(),
                url,
                body: truncate_for_display(self.body.trim(), 512),
            },
        }
    }

    /// The error for a member of a multistatus reply that reported `status`.
    pub(crate) fn member_error(method: &Method, url: &Url, status: u16) -> DavError {
        HttpResponse {
            method: method.clone(),
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            headers: HeaderMap::new(),
            url: url.clone(),
            body: String::new(),
        }
        .into_error()
    }

    /// Parses a `207 Multi-Status` reply, or converts any other status into
    /// an error.
    pub(crate) fn into_multistatus(self) -> Result<MultiStatusReply> {
        let is_multistatus = self.status == StatusCode::MULTI_STATUS
            // A few servers answer PROPFIND with 200 and a multistatus body.
            || (self.status == StatusCode::OK && self.body.contains("multistatus"));
        if !is_multistatus {
            return Err(self.into_error());
        }
        let multistatus = multistatus::parse_multistatus(&self.body)?;
        Ok(MultiStatusReply {
            url: self.url,
            multistatus,
        })
    }
}

/// A parsed multistatus together with the URL it was obtained from, which is
/// the base for resolving its hrefs.
#[derive(Debug)]
pub(crate) struct MultiStatusReply {
    pub(crate) url: Url,
    pub(crate) multistatus: MultiStatus,
}

impl MultiStatusReply {
    /// Responses paired with their resolved, normalised URLs. Responses whose
    /// href cannot be resolved are skipped with a warning.
    pub(crate) fn entries(&self) -> Vec<(Url, &Response)> {
        self.multistatus
            .responses
            .iter()
            .filter_map(|response| {
                let raw = response.href()?;
                match href::resolve(&self.url, raw) {
                    Ok(url) => Some((url, response)),
                    Err(err) => {
                        warn!(href = raw, error = %err, "ignoring unusable href in multistatus");
                        None
                    }
                }
            })
            .collect()
    }

    /// The response describing the request URL itself, if present.
    pub(crate) fn own_entry(&self) -> Option<(Url, &Response)> {
        let entries = self.entries();
        let single = entries.len() == 1;
        entries
            .into_iter()
            .find(|(url, _)| single || href::same_resource(url, &self.url))
    }
}

/// Normalises an entity tag: surrounding whitespace is removed and a bare
/// value is wrapped in double quotes (some servers omit them in
/// `DAV:getetag`), so tags from headers and properties compare equal.
pub(crate) fn normalize_etag(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('"') || trimmed.starts_with("W/\"") {
        Some(trimmed.to_owned())
    } else {
        Some(format!("\"{trimmed}\""))
    }
}

/// Value for `If-Match`: the entity tag in quoted form.
pub(crate) fn if_match_value(etag: &str) -> Result<HeaderValue> {
    let etag = normalize_etag(etag)
        .ok_or_else(|| DavError::InvalidInput("empty ETag for If-Match".into()))?;
    HeaderValue::from_str(&etag)
        .map_err(|_| DavError::InvalidInput(format!("ETag {etag} is not a valid header value")))
}

/// Parses `Retry-After` (delta seconds or an HTTP date).
fn retry_after(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let delta = date.with_timezone(&chrono::Utc) - chrono::Utc::now();
    Some(u64::try_from(delta.num_seconds()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_normalisation() {
        assert_eq!(normalize_etag(" \"abc\" ").as_deref(), Some("\"abc\""));
        assert_eq!(normalize_etag("abc").as_deref(), Some("\"abc\""));
        assert_eq!(normalize_etag("W/\"abc\"").as_deref(), Some("W/\"abc\""));
        assert_eq!(normalize_etag("  "), None);
        assert_eq!(
            if_match_value("abc").unwrap(),
            HeaderValue::from_static("\"abc\"")
        );
        assert!(if_match_value("").is_err());
    }

    #[test]
    fn retry_after_parsing() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, HeaderValue::from_static("120"));
        assert_eq!(retry_after(&headers), Some(120));
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&headers), Some(0));
        headers.insert(RETRY_AFTER, HeaderValue::from_static("soon"));
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn status_mapping() {
        let response = |status: u16, body: &str| HttpResponse {
            method: Method::PUT,
            status: StatusCode::from_u16(status).unwrap(),
            headers: HeaderMap::new(),
            url: Url::parse("https://dav.example.com/cal/a.ics").unwrap(),
            body: body.to_owned(),
        };
        assert!(matches!(
            response(412, "").into_error(),
            DavError::PreconditionFailed { .. }
        ));
        assert!(matches!(
            response(404, "").into_error(),
            DavError::NotFound { .. }
        ));
        assert!(matches!(
            response(410, "").into_error(),
            DavError::NotFound { .. }
        ));
        assert!(matches!(
            response(429, "").into_error(),
            DavError::RateLimited { retry_after: None }
        ));
        match response(
            403,
            r#"<d:error xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><c:no-uid-conflict/></d:error>"#,
        )
        .into_error()
        {
            DavError::Forbidden { condition, .. } => assert_eq!(condition.as_deref(), Some("no-uid-conflict")),
            other => panic!("unexpected {other:?}"),
        }
        match response(500, &"x".repeat(2000)).into_error() {
            DavError::Http {
                status,
                method,
                body,
                ..
            } => {
                assert_eq!(status, 500);
                assert_eq!(method, "PUT");
                assert!(body.len() < 600);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            response(200, "<html/>").into_multistatus(),
            Err(DavError::Http { status: 200, .. })
        ));
        assert!(matches!(
            response(207, "<html/>").into_multistatus(),
            Err(DavError::InvalidResponse(_))
        ));
    }

    #[test]
    fn builder_validates_the_base_url() {
        assert!(DavClient::new("caldav.icloud.com", Auth::None).is_ok());
        assert!(matches!(
            DavClient::new("", Auth::None),
            Err(DavError::InvalidUrl { .. })
        ));
        assert!(matches!(
            DavClient::new("ftp://example.com", Auth::None),
            Err(DavError::InvalidUrl { .. })
        ));
        let client = DavClient::builder("https://Cloud.Example.com/remote.php/dav/")
            .auth(Auth::basic("jane", "secret"))
            .accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .user_agent("test-agent")
            .build()
            .unwrap();
        assert_eq!(
            client.base_url().as_str(),
            "https://cloud.example.com/remote.php/dav/"
        );
        let debug = format!("{client:?}");
        assert!(debug.contains("password"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn default_user_agent_names_the_project() {
        assert!(DEFAULT_USER_AGENT.starts_with("Envelope/"));
        assert!(DEFAULT_USER_AGENT.ends_with("(+https://github.com/burakgon/envelope)"));
    }

    #[test]
    fn request_targets_include_queries() {
        assert_eq!(
            request_target(&Url::parse("https://h/a/b?x=1").unwrap()),
            "/a/b?x=1"
        );
        assert_eq!(request_target(&Url::parse("https://h/").unwrap()), "/");
    }
}
