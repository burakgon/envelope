//! The EWS client: configuration, the SOAP request loop with retries, and
//! decoding of faults and throttling responses.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, HeaderName, HeaderValue, LOCATION, RETRY_AFTER};
use tracing::{debug, warn};
use url::Url;

use crate::auth::Auth;
use crate::error::{EwsError, ResponseError, SoapFault};
use crate::soap::{self, Parsed, SoapHeaders, SoapResponse};
use crate::transport::{HttpRequest, HttpResponse, HttpSettings, LiveResponse, Transport};
use crate::version::{ExchangeVersion, ServerVersionInfo};
use crate::xml::Element;

/// `User-Agent` sent unless [`EwsClientBuilder::user_agent`] overrides it.
pub const DEFAULT_USER_AGENT: &str = concat!(
    "Envelope/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/burakgon/envelope)"
);

/// Default limit for a whole request, including reading the response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Default limit for establishing a connection (DNS, TCP and TLS).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Default number of connections used concurrently with NTLM.
pub const DEFAULT_NTLM_CONNECTIONS: usize = 4;

/// Path of the EWS endpoint on a Client Access server.
pub const EWS_PATH: &str = "/EWS/Exchange.asmx";

const SOAP_CONTENT_TYPE: &str = "text/xml; charset=utf-8";

/// How the client retries throttled and failed requests.
///
/// `ErrorServerBusy` (as SOAP fault, or for every item of a batch), HTTP
/// `429` and `503` are retried after the delay the server asks for
/// (`BackOffMilliseconds`, `Retry-After`) or with exponential backoff and
/// jitter. Connection failures are always retried; timeouts, `502`/`504`
/// and transient EWS errors only for read operations, which are safe to
/// repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of retries per request.
    pub max_retries: u32,
    /// Delay before the first retry when the server named none; doubled for
    /// every further attempt.
    pub initial_backoff: Duration,
    /// Upper bound for computed backoff delays.
    pub max_backoff: Duration,
    /// Longest server-requested delay the client waits for. When Exchange
    /// asks for more, the error is returned at once with
    /// [`EwsError::retry_after`] set so the caller can reschedule.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 4,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(60),
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

/// A SOAP request: the operation element (`<m:GetItem>…</m:GetItem>`)
/// and whether repeating it is harmless.
#[derive(Debug, Clone)]
pub(crate) struct SoapRequest {
    pub(crate) operation: &'static str,
    pub(crate) body: String,
    pub(crate) idempotent: bool,
    /// Windows id of the time zone sent as `TimeZoneContext` with this
    /// request, instead of the client's.
    pub(crate) time_zone: Option<String>,
}

impl SoapRequest {
    /// A read-only request.
    pub(crate) fn read(operation: &'static str, body: String) -> Self {
        SoapRequest {
            operation,
            body,
            idempotent: true,
            time_zone: None,
        }
    }

    /// A request that changes the mailbox.
    pub(crate) fn write(operation: &'static str, body: String) -> Self {
        SoapRequest {
            operation,
            body,
            idempotent: false,
            time_zone: None,
        }
    }

    /// Sends the request in the given time zone (a Windows id), which
    /// Exchange uses for the dates of tasks and for times without an offset.
    pub(crate) fn in_time_zone(mut self, windows_id: impl Into<String>) -> Self {
        self.time_zone = Some(windows_id.into());
        self
    }
}

struct Inner {
    url: Url,
    transport: Transport,
    retry: RetryPolicy,
    version: AtomicUsize,
    impersonate: Option<String>,
    anchor_mailbox: Option<HeaderValue>,
    time_zone_context: Option<String>,
    server_version: Mutex<Option<ServerVersionInfo>>,
    /// Set once the server rejected the `archive` distinguished folder id.
    no_archive_id: AtomicBool,
}

/// An Exchange Web Services client for one mailbox.
///
/// The client is cheap to clone; clones share connections, credentials
/// and the detected server version. Every method may be called
/// concurrently.
#[derive(Clone)]
pub struct EwsClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for EwsClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EwsClient")
            .field("url", &self.inner.url.as_str())
            .field("version", &self.version())
            .field("transport", &self.inner.transport)
            .finish_non_exhaustive()
    }
}

/// Builder for [`EwsClient`].
pub struct EwsClientBuilder {
    url: String,
    auth: Auth,
    version: ExchangeVersion,
    http: Option<reqwest::Client>,
    retry: RetryPolicy,
    user_agent: String,
    timeout: Duration,
    connect_timeout: Duration,
    accept_invalid_certs: bool,
    allow_insecure_http: bool,
    impersonate: Option<String>,
    anchor_mailbox: Option<String>,
    time_zone_context: Option<String>,
    ntlm_connections: usize,
}

impl fmt::Debug for EwsClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EwsClientBuilder")
            .field("url", &self.url)
            .field("auth", &self.auth)
            .field("version", &self.version)
            .field("retry", &self.retry)
            .field("user_agent", &self.user_agent)
            .field("impersonate", &self.impersonate)
            .field("anchor_mailbox", &self.anchor_mailbox)
            .finish_non_exhaustive()
    }
}

impl EwsClientBuilder {
    /// The schema version requested with `RequestServerVersion` (default
    /// [`ExchangeVersion::Exchange2013Sp1`]).
    pub fn server_version(mut self, version: ExchangeVersion) -> Self {
        self.version = version;
        self
    }

    /// Uses a preconfigured `reqwest` client (proxy, TLS roots, …). The
    /// user agent, timeouts and certificate settings of this builder are
    /// not applied to it, and it should not follow redirects. With NTLM the
    /// client serves one request at a time and must use HTTP/1.1.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Sets the retry policy.
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Overrides the `User-Agent` (default [`DEFAULT_USER_AGENT`]).
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Limit for a whole request (default [`DEFAULT_TIMEOUT`]).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Limit for establishing a connection (default
    /// [`DEFAULT_CONNECT_TIMEOUT`]).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Accepts TLS certificates that do not verify (self-signed or issued
    /// for another name), as found on many on-premises servers. Only for
    /// servers the user explicitly trusts.
    pub fn accept_invalid_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_certs = accept;
        self
    }

    /// Allows sending credentials over plain `http://` URLs to hosts other
    /// than loopback addresses. Off by default.
    pub fn allow_insecure_http(mut self, allow: bool) -> Self {
        self.allow_insecure_http = allow;
        self
    }

    /// Acts on behalf of another mailbox through Exchange impersonation
    /// (`ExchangeImpersonation` with the primary SMTP address). The
    /// account needs the ApplicationImpersonation role. Also sets the
    /// anchor mailbox unless [`anchor_mailbox`](Self::anchor_mailbox) is
    /// given.
    pub fn impersonate(mut self, primary_smtp_address: impl Into<String>) -> Self {
        self.impersonate = Some(primary_smtp_address.into());
        self
    }

    /// Sends `X-AnchorMailbox` with every request, which routes requests
    /// to the mailbox's server directly (recommended: the user's primary
    /// SMTP address).
    pub fn anchor_mailbox(mut self, smtp_address: impl Into<String>) -> Self {
        self.anchor_mailbox = Some(smtp_address.into());
        self
    }

    /// Sends a `TimeZoneContext` header with the given Windows time zone id
    /// (`W. Europe Standard Time`). Not needed for this crate's operations,
    /// which exchange times in UTC and name zones explicitly (the task
    /// operations send the zone they are given instead); requires Exchange
    /// 2010 or later.
    pub fn time_zone_context(mut self, windows_zone_id: impl Into<String>) -> Self {
        self.time_zone_context = Some(windows_zone_id.into());
        self
    }

    /// Number of connections used concurrently with NTLM (default
    /// [`DEFAULT_NTLM_CONNECTIONS`]). Each one is authenticated once.
    pub fn ntlm_connections(mut self, connections: usize) -> Self {
        self.ntlm_connections = connections.max(1);
        self
    }

    /// Builds the client. Fails when the URL is unusable or the TLS backend
    /// cannot be initialised.
    pub fn build(self) -> Result<EwsClient, EwsError> {
        let url = normalize_ews_url(&self.url)?;
        let anchor = self.anchor_mailbox.or_else(|| self.impersonate.clone());
        let anchor_mailbox = anchor
            .map(|address| {
                HeaderValue::from_str(address.trim()).map_err(|_| {
                    EwsError::InvalidInput(format!("invalid anchor mailbox `{address}`"))
                })
            })
            .transpose()?;
        let transport = Transport::new(
            self.auth,
            HttpSettings {
                user_agent: self.user_agent,
                timeout: self.timeout,
                connect_timeout: self.connect_timeout,
                accept_invalid_certs: self.accept_invalid_certs,
                allow_insecure_http: self.allow_insecure_http,
                ntlm_connections: self.ntlm_connections,
            },
            self.http,
        )?;
        Ok(EwsClient {
            inner: Arc::new(Inner {
                url,
                transport,
                retry: self.retry,
                version: AtomicUsize::new(version_index(self.version)),
                impersonate: self.impersonate,
                anchor_mailbox,
                time_zone_context: self.time_zone_context,
                server_version: Mutex::new(None),
                no_archive_id: AtomicBool::new(false),
            }),
        })
    }
}

/// Turns user input into an EWS endpoint URL: a scheme defaults to
/// `https`, and a bare host gets the standard [`EWS_PATH`].
pub(crate) fn normalize_ews_url(input: &str) -> Result<Url, EwsError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(EwsError::InvalidInput("empty EWS URL".into()));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|e| EwsError::InvalidInput(format!("invalid EWS URL `{trimmed}`: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(EwsError::InvalidInput(format!(
            "EWS URL `{trimmed}` must be an http(s) URL with a host"
        )));
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path(EWS_PATH);
    }
    url.set_fragment(None);
    Ok(url)
}

impl EwsClient {
    /// Starts building a client for the EWS endpoint `url`
    /// (`https://mail.example.com/EWS/Exchange.asmx`; a bare host gets the
    /// standard path).
    pub fn builder(url: impl Into<String>, auth: Auth) -> EwsClientBuilder {
        EwsClientBuilder {
            url: url.into(),
            auth,
            version: ExchangeVersion::default(),
            http: None,
            retry: RetryPolicy::default(),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            accept_invalid_certs: false,
            allow_insecure_http: false,
            impersonate: None,
            anchor_mailbox: None,
            time_zone_context: None,
            ntlm_connections: DEFAULT_NTLM_CONNECTIONS,
        }
    }

    /// A client with default settings.
    pub fn new(url: impl Into<String>, auth: Auth) -> Result<Self, EwsError> {
        EwsClient::builder(url, auth).build()
    }

    /// The EWS endpoint.
    pub fn url(&self) -> &Url {
        &self.inner.url
    }

    /// The schema version requests are written against.
    pub fn version(&self) -> ExchangeVersion {
        let index = self.inner.version.load(Ordering::Relaxed);
        ExchangeVersion::ALL.get(index).copied().unwrap_or_default()
    }

    /// Changes the requested schema version for this client and its clones,
    /// for example to the version reported by [`server_version_info`](Self::server_version_info).
    pub fn set_version(&self, version: ExchangeVersion) {
        self.inner
            .version
            .store(version_index(version), Ordering::Relaxed);
    }

    /// The server build reported by the most recent response, if any.
    pub fn server_version_info(&self) -> Option<ServerVersionInfo> {
        self.inner
            .server_version
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether the server is known not to accept the `archive`
    /// distinguished folder id.
    pub(crate) fn archive_id_unsupported(&self) -> bool {
        self.inner.no_archive_id.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_archive_id_unsupported(&self) {
        self.inner.no_archive_id.store(true, Ordering::Relaxed);
    }

    /// The impersonated mailbox, if any.
    pub fn impersonated_mailbox(&self) -> Option<&str> {
        self.inner.impersonate.as_deref()
    }

    /// Sends an operation element (for example `<m:GetUserOofSettingsRequest>`
    /// in the `m:`/`t:` namespaces) that this crate does not wrap and
    /// returns the response envelope as text. Faults, HTTP errors and
    /// throttling are handled like for every other request; per-message
    /// response classes are left to the caller. `read_only` requests are
    /// retried more liberally.
    pub async fn execute_raw(
        &self,
        operation_xml: &str,
        read_only: bool,
    ) -> Result<String, EwsError> {
        let request = SoapRequest {
            operation: "Raw",
            body: operation_xml.to_owned(),
            idempotent: read_only,
            time_zone: None,
        };
        let (_, text) = self.call_inner(&request).await?;
        Ok(text)
    }

    /// Sends a request and returns the operation response element.
    pub(crate) async fn call(&self, request: &SoapRequest) -> Result<Element, EwsError> {
        Ok(self.call_inner(request).await?.0.body)
    }

    async fn call_inner(&self, request: &SoapRequest) -> Result<(SoapResponse, String), EwsError> {
        let retry = &self.inner.retry;
        let http_request = self.http_request(request)?;
        let mut attempt: u32 = 0;
        loop {
            let outcome = self.inner.transport.send(&http_request).await;
            let failure = match outcome {
                Ok(response) => match self.interpret(request, response) {
                    Ok(done) => return Ok(done),
                    Err(failure) => failure,
                },
                Err(error) => Failure {
                    retryable: match &error {
                        EwsError::Transport(e) => e.is_connect(),
                        EwsError::Timeout(_) => request.idempotent,
                        _ => false,
                    },
                    delay: None,
                    error,
                },
            };
            if !failure.retryable || attempt >= retry.max_retries {
                return Err(failure.error);
            }
            let delay = failure.delay.unwrap_or_else(|| retry.backoff(attempt));
            if delay > retry.max_retry_after {
                return Err(failure.error);
            }
            warn!(
                operation = request.operation,
                attempt,
                ?delay,
                error = %failure.error,
                "EWS request failed; retrying"
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// Opens a long-running request (streaming notifications) and returns
    /// the live response after checking its status.
    pub(crate) async fn call_streaming(
        &self,
        request: &SoapRequest,
        timeout: Duration,
    ) -> Result<LiveResponse, EwsError> {
        let mut http_request = self.http_request(request)?;
        http_request.timeout = Some(timeout);
        let live = self.inner.transport.send_live(&http_request).await?;
        let status = live.response.status();
        if status.is_success() {
            return Ok(live);
        }
        let headers = live.response.headers().clone();
        let body = live
            .response
            .bytes()
            .await
            .map_err(EwsError::from_reqwest)?;
        let response = HttpResponse {
            status,
            headers,
            body,
        };
        match self.interpret(request, response) {
            Ok(_) => Err(EwsError::InvalidResponse(format!(
                "unexpected HTTP status {status} for a streaming request"
            ))),
            Err(failure) => Err(failure.error),
        }
    }

    fn http_request(&self, request: &SoapRequest) -> Result<HttpRequest, EwsError> {
        let version = self.version();
        let time_zone = if version.has_2010_features() {
            request
                .time_zone
                .as_deref()
                .or(self.inner.time_zone_context.as_deref())
        } else {
            None
        };
        let envelope = soap::envelope(
            &SoapHeaders {
                version: version.as_str(),
                impersonate: self.inner.impersonate.as_deref(),
                time_zone,
            },
            &request.body,
        );
        let mut http_request =
            HttpRequest::post(self.inner.url.clone(), SOAP_CONTENT_TYPE, envelope);
        let headers = &mut http_request.headers;
        headers.insert(ACCEPT, HeaderValue::from_static("text/xml"));
        if let Some(anchor) = &self.inner.anchor_mailbox {
            headers.insert(HeaderName::from_static("x-anchormailbox"), anchor.clone());
        }
        let request_id = HeaderValue::from_str(&new_request_id())
            .map_err(|e| EwsError::InvalidInput(format!("request id: {e}")))?;
        headers.insert(HeaderName::from_static("client-request-id"), request_id);
        headers.insert(
            HeaderName::from_static("return-client-request-id"),
            HeaderValue::from_static("true"),
        );
        Ok(http_request)
    }

    /// Classifies an HTTP response: a usable SOAP response, or a failure
    /// with retry advice.
    fn interpret(
        &self,
        request: &SoapRequest,
        response: HttpResponse,
    ) -> Result<(SoapResponse, String), Failure> {
        let status = response.status;
        let url = self.inner.url.to_string();
        debug!(
            operation = request.operation,
            status = status.as_u16(),
            bytes = response.body.len(),
            "EWS response"
        );
        let text = String::from_utf8_lossy(&response.body).into_owned();
        if status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR {
            match soap::parse_envelope(status.as_u16(), &text) {
                Ok(Parsed::Response(parsed)) if status == StatusCode::OK => {
                    if let Some(info) = &parsed.server_version {
                        *self
                            .inner
                            .server_version
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = Some(info.clone());
                    }
                    if let Some(busy) = soap::all_busy(&parsed.body) {
                        return Err(Failure::busy(busy.back_off, busy));
                    }
                    return Ok((parsed, text));
                }
                Ok(Parsed::Fault(fault)) => return Err(Failure::from_fault(request, fault)),
                Ok(Parsed::Response(_)) | Err(_) => {
                    if status == StatusCode::OK {
                        return Err(Failure::fatal(EwsError::InvalidResponse(format!(
                            "{} did not return a SOAP response: {}",
                            request.operation,
                            snippet(&text)
                        ))));
                    }
                }
            }
        }
        let retry_after = response
            .header(RETRY_AFTER.as_str())
            .and_then(|v| parse_retry_after(v, chrono::Utc::now()));
        let error = match status.as_u16() {
            403 => EwsError::Forbidden { url },
            300..=399 => EwsError::Http {
                status: status.as_u16(),
                body: match response.header(LOCATION.as_str()) {
                    Some(location) => format!("redirected to {location}"),
                    None => snippet(&text),
                },
                url,
                retry_after,
            },
            _ => EwsError::Http {
                status: status.as_u16(),
                url,
                body: snippet(&text),
                retry_after,
            },
        };
        let retryable = match status.as_u16() {
            429 | 503 => true,
            502 | 504 => request.idempotent,
            _ => false,
        };
        Err(Failure {
            error,
            retryable,
            delay: retry_after,
        })
    }
}

/// A failed attempt and whether to try again.
struct Failure {
    error: EwsError,
    retryable: bool,
    /// Delay requested by the server.
    delay: Option<Duration>,
}

impl Failure {
    fn fatal(error: EwsError) -> Self {
        Failure {
            error,
            retryable: false,
            delay: None,
        }
    }

    fn busy(delay: Option<Duration>, error: ResponseError) -> Self {
        Failure {
            error: error.into(),
            retryable: true,
            delay,
        }
    }

    fn from_fault(request: &SoapRequest, fault: SoapFault) -> Self {
        let (retryable, delay) = match &fault.code {
            // Throttled requests were not executed: always safe to repeat.
            Some(code) if code.is_throttling() => (true, fault.back_off),
            Some(code) if code.is_transient() => (request.idempotent, fault.back_off),
            _ => (false, None),
        };
        Failure {
            error: fault.into(),
            retryable,
            delay,
        }
    }
}

fn version_index(version: ExchangeVersion) -> usize {
    ExchangeVersion::ALL
        .iter()
        .position(|v| *v == version)
        .unwrap_or_default()
}

fn snippet(text: &str) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(512).collect();
    if out.len() < trimmed.len() {
        out.push('…');
    }
    out
}

/// Parses a `Retry-After` header: either delay-seconds or an HTTP date.
pub(crate) fn parse_retry_after(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let delta = at.with_timezone(&chrono::Utc) - now;
    Some(delta.to_std().unwrap_or(Duration::ZERO))
}

fn random_u64() -> u64 {
    getrandom::u64().unwrap_or_else(|_| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or_default()
    })
}

fn random_unit() -> f64 {
    // 53 random bits mapped onto [0, 1).
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// A random RFC 4122 version 4 identifier for `client-request-id`.
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
    use chrono::TimeZone;

    #[test]
    fn normalizes_ews_urls() {
        let url = |s: &str| normalize_ews_url(s).map(|u| u.to_string());
        assert_eq!(
            url("mail.example.com").unwrap(),
            "https://mail.example.com/EWS/Exchange.asmx"
        );
        assert_eq!(
            url("https://mail.example.com/").unwrap(),
            "https://mail.example.com/EWS/Exchange.asmx"
        );
        assert_eq!(
            url(" https://mail.example.com/ews/exchange.asmx#x ").unwrap(),
            "https://mail.example.com/ews/exchange.asmx"
        );
        assert_eq!(
            url("http://127.0.0.1:8080/custom/path").unwrap(),
            "http://127.0.0.1:8080/custom/path"
        );
        assert!(url("").is_err());
        assert!(url("ftp://mail.example.com").is_err());
        assert!(url("https://").is_err());
    }

    #[test]
    fn builder_and_version_switching() {
        let client = EwsClient::builder("mail.example.com", Auth::basic("jane", "secret"))
            .server_version(ExchangeVersion::Exchange2010Sp2)
            .impersonate("room@example.com")
            .build()
            .unwrap();
        assert_eq!(client.version(), ExchangeVersion::Exchange2010Sp2);
        let clone = client.clone();
        clone.set_version(ExchangeVersion::Exchange2016);
        assert_eq!(client.version(), ExchangeVersion::Exchange2016);
        assert_eq!(client.impersonated_mailbox(), Some("room@example.com"));
        assert_eq!(
            client
                .inner
                .anchor_mailbox
                .as_ref()
                .and_then(|v| v.to_str().ok()),
            Some("room@example.com")
        );
        assert!(client.server_version_info().is_none());
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(
            EwsClient::builder("mail.example.com", Auth::basic("a", "b"))
                .anchor_mailbox("bad\nvalue")
                .build()
                .is_err()
        );
    }

    #[test]
    fn requests_carry_headers() {
        let client = EwsClient::builder(
            "https://mail.example.com/EWS/Exchange.asmx",
            Auth::basic("a", "b"),
        )
        .anchor_mailbox("jane@example.com")
        .time_zone_context("UTC")
        .build()
        .unwrap();
        let request = client
            .http_request(&SoapRequest::read("GetFolder", "<m:GetFolder/>".into()))
            .unwrap();
        assert_eq!(request.content_type, Some(SOAP_CONTENT_TYPE));
        assert_eq!(
            request
                .headers
                .get("x-anchormailbox")
                .and_then(|v| v.to_str().ok()),
            Some("jane@example.com")
        );
        assert!(request.headers.contains_key("client-request-id"));
        let body = String::from_utf8_lossy(&request.body).into_owned();
        assert!(body.contains(r#"<t:RequestServerVersion Version="Exchange2013_SP1"/>"#));
        assert!(body.contains(r#"<t:TimeZoneDefinition Id="UTC"/>"#));
        // A request can name its own zone.
        let request = client
            .http_request(
                &SoapRequest::read("GetItem", "<m:GetItem/>".into())
                    .in_time_zone("Turkey Standard Time"),
            )
            .unwrap();
        let body = String::from_utf8_lossy(&request.body).into_owned();
        assert!(body.contains(r#"<t:TimeZoneDefinition Id="Turkey Standard Time"/>"#));
        assert!(!body.contains(r#"Id="UTC""#));
        // Exchange 2007 does not know TimeZoneContext.
        client.set_version(ExchangeVersion::Exchange2007Sp1);
        let request = client
            .http_request(&SoapRequest::read("GetFolder", "<m:GetFolder/>".into()))
            .unwrap();
        assert!(!String::from_utf8_lossy(&request.body).contains("TimeZoneContext"));
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
        assert_eq!(RetryPolicy::none().max_retries, 0);
    }

    #[test]
    fn retry_after_parsing() {
        let now = Utc
            .with_ymd_and_hms(2015, 10, 21, 7, 27, 30)
            .single()
            .unwrap();
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after("soon", now), None);
    }

    #[test]
    fn request_ids_look_like_uuids() {
        let id = new_request_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(id, new_request_id());
    }

    #[test]
    fn snippets_are_bounded() {
        assert_eq!(snippet("  short  "), "short");
        let long = snippet(&"x".repeat(2000));
        assert!(long.chars().count() <= 513);
        assert!(long.ends_with('…'));
    }

    use chrono::Utc;
}
