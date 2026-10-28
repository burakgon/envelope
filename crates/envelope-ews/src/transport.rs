//! HTTP exchanges with authentication: HTTP Basic, OAuth bearer tokens
//! (refreshed once after a `401`) and NTLM.
//!
//! NTLM authenticates a *connection*, not a request: the NEGOTIATE,
//! CHALLENGE and AUTHENTICATE legs must travel over the same TCP
//! connection, which then stays authenticated for later requests. The
//! transport therefore keeps a few NTLM "slots", each owning an HTTP/1.1
//! client limited to a single pooled connection and serving one request
//! at a time. A slot remembers that its connection is authenticated and
//! repeats the handshake when the server answers `401` (the connection was
//! closed and a new one opened).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use reqwest::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue, WWW_AUTHENTICATE,
};
use reqwest::{Method, StatusCode};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::debug;
use url::Url;

use crate::auth::{self, Auth, TokenSource};
use crate::error::EwsError;
use crate::ntlm;

/// Settings for the HTTP clients the transport creates.
#[derive(Debug, Clone)]
pub(crate) struct HttpSettings {
    pub(crate) user_agent: String,
    pub(crate) timeout: Duration,
    pub(crate) connect_timeout: Duration,
    pub(crate) accept_invalid_certs: bool,
    pub(crate) allow_insecure_http: bool,
    pub(crate) ntlm_connections: usize,
}

/// One HTTP request.
#[derive(Debug, Clone)]
pub(crate) struct HttpRequest {
    pub(crate) method: Method,
    pub(crate) url: Url,
    pub(crate) headers: HeaderMap,
    pub(crate) content_type: Option<&'static str>,
    pub(crate) body: Bytes,
    /// Overrides the default request timeout (long-running streams).
    pub(crate) timeout: Option<Duration>,
}

impl HttpRequest {
    pub(crate) fn post(url: Url, content_type: &'static str, body: impl Into<Bytes>) -> Self {
        HttpRequest {
            method: Method::POST,
            url,
            headers: HeaderMap::new(),
            content_type: Some(content_type),
            body: body.into(),
            timeout: None,
        }
    }

    pub(crate) fn get(url: Url) -> Self {
        HttpRequest {
            method: Method::GET,
            url,
            headers: HeaderMap::new(),
            content_type: None,
            body: Bytes::new(),
            timeout: None,
        }
    }
}

/// A response whose body has been read.
#[derive(Debug, Clone)]
pub(crate) struct HttpResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl HttpResponse {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// A response whose body has not been read yet, together with the NTLM
/// slot that must stay reserved while the body is consumed.
pub(crate) struct LiveResponse {
    pub(crate) response: reqwest::Response,
    pub(crate) _slot: Option<OwnedMutexGuard<NtlmSlot>>,
}

enum Credentials {
    Basic(HeaderValue),
    Ntlm {
        user: String,
        domain: String,
        password: String,
        workstation: String,
    },
    Bearer(Arc<dyn TokenSource>),
}

/// The authenticated HTTP layer shared by EWS and Autodiscover requests.
pub(crate) struct Transport {
    credentials: Credentials,
    /// Client for Basic, bearer and unauthenticated requests.
    shared: reqwest::Client,
    ntlm: Option<NtlmPool>,
    settings: HttpSettings,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = match &self.credentials {
            Credentials::Basic(_) => "basic",
            Credentials::Ntlm { .. } => "ntlm",
            Credentials::Bearer(_) => "bearer",
        };
        f.debug_struct("Transport")
            .field("auth", &scheme)
            .finish_non_exhaustive()
    }
}

impl Transport {
    /// Builds the transport. A caller-supplied client is used for every
    /// request; with NTLM it then serves one request at a time.
    pub(crate) fn new(
        auth: Auth,
        settings: HttpSettings,
        custom: Option<reqwest::Client>,
    ) -> Result<Self, EwsError> {
        let shared = match &custom {
            Some(client) => client.clone(),
            None => build_client(&settings, false)?,
        };
        let (credentials, ntlm) = match auth {
            Auth::Basic { username, password } => {
                let value = sensitive_header(&auth::basic_header(&username, &password))?;
                (Credentials::Basic(value), None)
            }
            Auth::Bearer(source) => (Credentials::Bearer(source), None),
            Auth::Ntlm {
                username,
                password,
                domain,
            } => {
                let (user, domain) = auth::split_ntlm_user(&username, domain.as_deref());
                let pool = match custom {
                    Some(client) => NtlmPool::with_client(client),
                    None => NtlmPool::new(settings.ntlm_connections.max(1)),
                };
                (
                    Credentials::Ntlm {
                        user,
                        domain,
                        password,
                        workstation: workstation_name(),
                    },
                    Some(pool),
                )
            }
        };
        Ok(Transport {
            credentials,
            shared,
            ntlm,
            settings,
        })
    }

    /// Sends an authenticated request and reads the response body.
    ///
    /// Returns the response for every status except `401`, which becomes
    /// [`EwsError::Unauthorized`] once the automatic retries are exhausted.
    pub(crate) async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, EwsError> {
        let live = self.send_live(request).await?;
        let status = live.response.status();
        let headers = live.response.headers().clone();
        let body = live
            .response
            .bytes()
            .await
            .map_err(EwsError::from_reqwest)?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    /// Sends an authenticated request and returns the response before its
    /// body is read (for streaming notifications).
    pub(crate) async fn send_live(&self, request: &HttpRequest) -> Result<LiveResponse, EwsError> {
        self.check_secure(&request.url)?;
        match &self.credentials {
            Credentials::Basic(value) => {
                let response = self
                    .raw(&self.shared, request, Some(value.clone()), false)
                    .await?;
                if response.status() == StatusCode::UNAUTHORIZED {
                    let detail = offered_schemes(response.headers());
                    drain(response).await;
                    return Err(EwsError::Unauthorized { detail });
                }
                Ok(LiveResponse {
                    response,
                    _slot: None,
                })
            }
            Credentials::Bearer(source) => self.send_bearer(source.as_ref(), request).await,
            Credentials::Ntlm { .. } => self.send_ntlm(request).await,
        }
    }

    /// Sends a request without credentials (Autodiscover redirect probes).
    pub(crate) async fn send_unauthenticated(
        &self,
        request: &HttpRequest,
    ) -> Result<HttpResponse, EwsError> {
        let response = self.raw(&self.shared, request, None, false).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await.map_err(EwsError::from_reqwest)?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    fn check_secure(&self, url: &Url) -> Result<(), EwsError> {
        if url.scheme() == "https" || self.settings.allow_insecure_http || is_loopback(url) {
            Ok(())
        } else {
            Err(EwsError::InvalidInput(format!(
                "refusing to send credentials over plain HTTP to {}",
                url.host_str().unwrap_or_default()
            )))
        }
    }

    async fn send_bearer(
        &self,
        source: &dyn TokenSource,
        request: &HttpRequest,
    ) -> Result<LiveResponse, EwsError> {
        let mut refreshed = false;
        loop {
            let token = source.access_token().await.map_err(EwsError::TokenSource)?;
            let value = sensitive_header(&format!("Bearer {}", token.trim()))?;
            let response = self.raw(&self.shared, request, Some(value), false).await?;
            if response.status() != StatusCode::UNAUTHORIZED {
                return Ok(LiveResponse {
                    response,
                    _slot: None,
                });
            }
            let detail = bearer_error(response.headers());
            drain(response).await;
            if refreshed {
                return Err(EwsError::Unauthorized { detail });
            }
            debug!(url = %request.url, "access token rejected; refreshing");
            source.invalidate().await;
            refreshed = true;
        }
    }

    async fn send_ntlm(&self, request: &HttpRequest) -> Result<LiveResponse, EwsError> {
        let Credentials::Ntlm {
            user,
            domain,
            password,
            workstation,
        } = &self.credentials
        else {
            return Err(EwsError::Ntlm("NTLM credentials missing".into()));
        };
        let Some(pool) = &self.ntlm else {
            return Err(EwsError::Ntlm("NTLM connection pool missing".into()));
        };
        let mut slot = pool.acquire().await;
        let http = match &slot.http {
            Some(http) => http.clone(),
            None => {
                let http = build_client(&self.settings, true)?;
                slot.http = Some(http.clone());
                http
            }
        };

        let origin = request.url.origin().ascii_serialization();
        if slot.authenticated.as_deref() == Some(origin.as_str()) {
            let response = self.raw(&http, request, None, false).await?;
            if response.status() != StatusCode::UNAUTHORIZED {
                return Ok(LiveResponse {
                    response,
                    _slot: Some(slot),
                });
            }
            debug!(url = %request.url, "NTLM connection no longer authenticated");
            drain(response).await;
            slot.authenticated = None;
        }

        let mut use_negotiate = pool.use_negotiate.load(Ordering::Relaxed);
        let mut switched = false;
        loop {
            let scheme = if use_negotiate { "Negotiate" } else { "NTLM" };
            let negotiate = ntlm::negotiate_message();
            let header = sensitive_header(&format!("{scheme} {}", b64(&negotiate)))?;
            let started = Instant::now();
            let response = self.raw(&http, request, Some(header), true).await?;
            if response.status() != StatusCode::UNAUTHORIZED {
                // The server did not ask for authentication after all (the
                // connection was authenticated, or the URL is anonymous):
                // the empty probe was answered, now send the real request.
                drain(response).await;
                slot.authenticated = Some(origin.clone());
                let response = self.raw(&http, request, None, false).await?;
                if response.status() == StatusCode::UNAUTHORIZED {
                    drain(response).await;
                    slot.authenticated = None;
                    return Err(EwsError::Unauthorized { detail: None });
                }
                return Ok(LiveResponse {
                    response,
                    _slot: Some(slot),
                });
            }
            let certificate = response
                .extensions()
                .get::<reqwest::tls::TlsInfo>()
                .and_then(|info| info.peer_certificate())
                .map(<[u8]>::to_vec);
            let headers = response.headers().clone();
            drain(response).await;

            let Some(token) = challenge_token(&headers, scheme) else {
                let offered = offered_scheme_names(&headers);
                if !switched && !use_negotiate && offered.iter().any(|s| s == "negotiate") {
                    // Only `Negotiate` is offered: send raw NTLM tokens in it.
                    use_negotiate = true;
                    switched = true;
                    continue;
                }
                if !switched && use_negotiate && offered.iter().any(|s| s == "ntlm") {
                    use_negotiate = false;
                    switched = true;
                    continue;
                }
                let speaks_ntlm = offered.iter().any(|s| s == "ntlm" || s == "negotiate");
                return Err(EwsError::Unauthorized {
                    detail: Some(if offered.is_empty() {
                        "the server did not send an NTLM challenge".to_owned()
                    } else if speaks_ntlm {
                        format!(
                            "the server did not answer the NTLM negotiation (offered: {})",
                            offered.join(", ")
                        )
                    } else {
                        format!(
                            "the server does not offer NTLM (offered: {})",
                            offered.join(", ")
                        )
                    }),
                });
            };
            pool.use_negotiate.store(use_negotiate, Ordering::Relaxed);
            let challenge = base64::engine::general_purpose::STANDARD
                .decode(token.trim())
                .map_err(|e| EwsError::Ntlm(format!("challenge is not base64: {e}")))
                .and_then(|bytes| {
                    ntlm::parse_challenge(&bytes).map_err(|e| EwsError::Ntlm(e.to_string()))
                })?;
            let host = request.url.host_str().unwrap_or_default().to_owned();
            let spn = format!("HTTP/{host}");
            let channel_bindings = certificate.as_deref().map(ntlm::tls_channel_bindings);
            let client_challenge =
                ntlm::random_client_challenge().map_err(|e| EwsError::Ntlm(e.to_string()))?;
            let authenticate = ntlm::authenticate_message(
                &challenge,
                &ntlm::AuthenticateInput {
                    user,
                    domain,
                    password,
                    workstation,
                    negotiate: &negotiate,
                    client_challenge,
                    now: ntlm::filetime_now(),
                    channel_bindings,
                    target_spn: Some(&spn),
                },
            )
            .map_err(|e| EwsError::Ntlm(e.to_string()))?;
            let header = sensitive_header(&format!("{scheme} {}", b64(&authenticate)))?;
            let response = self.raw(&http, request, Some(header), false).await?;
            debug!(
                url = %request.url,
                status = response.status().as_u16(),
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "NTLM handshake"
            );
            if response.status() == StatusCode::UNAUTHORIZED {
                drain(response).await;
                return Err(EwsError::Unauthorized {
                    detail: Some("NTLM credentials were rejected".into()),
                });
            }
            slot.authenticated = Some(origin);
            return Ok(LiveResponse {
                response,
                _slot: Some(slot),
            });
        }
    }

    /// One HTTP exchange; `empty_body` sends `Content-Length: 0` instead of
    /// the body (NTLM negotiation leg).
    async fn raw(
        &self,
        http: &reqwest::Client,
        request: &HttpRequest,
        authorization: Option<HeaderValue>,
        empty_body: bool,
    ) -> Result<reqwest::Response, EwsError> {
        let mut builder = http
            .request(request.method.clone(), request.url.clone())
            .headers(request.headers.clone())
            .timeout(request.timeout.unwrap_or(self.settings.timeout));
        if let Some(value) = authorization {
            builder = builder.header(AUTHORIZATION, value);
        }
        if empty_body {
            if request.method != Method::GET {
                builder = builder
                    .header(CONTENT_LENGTH, HeaderValue::from_static("0"))
                    .body(Bytes::new());
            }
        } else if request.method != Method::GET {
            if let Some(content_type) = request.content_type {
                builder = builder.header(CONTENT_TYPE, content_type);
            }
            builder = builder.body(request.body.clone());
        }
        let started = Instant::now();
        let response = builder.send().await.map_err(EwsError::from_reqwest)?;
        debug!(
            method = %request.method,
            url = %request.url,
            status = response.status().as_u16(),
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "HTTP exchange"
        );
        Ok(response)
    }
}

/// A connection reserved for NTLM.
#[derive(Debug, Default)]
pub(crate) struct NtlmSlot {
    http: Option<reqwest::Client>,
    /// Origin (`https://host:port`) the connection is authenticated to.
    authenticated: Option<String>,
}

struct NtlmPool {
    slots: Vec<Arc<Mutex<NtlmSlot>>>,
    next: AtomicUsize,
    /// Whether the server only offers the `Negotiate` scheme.
    use_negotiate: AtomicBool,
}

impl NtlmPool {
    fn new(size: usize) -> Self {
        NtlmPool {
            slots: (0..size)
                .map(|_| Arc::new(Mutex::new(NtlmSlot::default())))
                .collect(),
            next: AtomicUsize::new(0),
            use_negotiate: AtomicBool::new(false),
        }
    }

    fn with_client(client: reqwest::Client) -> Self {
        NtlmPool {
            slots: vec![Arc::new(Mutex::new(NtlmSlot {
                http: Some(client),
                authenticated: None,
            }))],
            next: AtomicUsize::new(0),
            use_negotiate: AtomicBool::new(false),
        }
    }

    /// A free slot, or the next one in turn when all are busy.
    async fn acquire(&self) -> OwnedMutexGuard<NtlmSlot> {
        for slot in &self.slots {
            if let Ok(guard) = Arc::clone(slot).try_lock_owned() {
                return guard;
            }
        }
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        Arc::clone(&self.slots[index]).lock_owned().await
    }
}

fn build_client(settings: &HttpSettings, for_ntlm: bool) -> Result<reqwest::Client, EwsError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(settings.user_agent.clone())
        // Redirects are handled explicitly where they are expected
        // (Autodiscover); credentials never follow them silently.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(settings.connect_timeout)
        .tls_danger_accept_invalid_certs(settings.accept_invalid_certs);
    if for_ntlm {
        // NTLM is bound to one HTTP/1.1 connection: IIS refuses Windows
        // authentication over HTTP/2.
        builder = builder
            .http1_only()
            .pool_max_idle_per_host(1)
            .tls_info(true);
    }
    builder.build().map_err(EwsError::Transport)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn sensitive_header(value: &str) -> Result<HeaderValue, EwsError> {
    let mut header = HeaderValue::from_bytes(value.as_bytes()).map_err(|_| {
        EwsError::InvalidInput("credentials contain characters not allowed in HTTP headers".into())
    })?;
    header.set_sensitive(true);
    Ok(header)
}

/// The token of a `WWW-Authenticate: <scheme> <token>` challenge.
fn challenge_token(headers: &HeaderMap, scheme: &str) -> Option<String> {
    headers
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|challenge| {
            let challenge = challenge.trim();
            let (name, token) = challenge.split_once(' ')?;
            (name.eq_ignore_ascii_case(scheme) && !token.trim().is_empty())
                .then(|| token.trim().to_owned())
        })
}

/// Lower-cased names of the schemes offered in `WWW-Authenticate`.
fn offered_scheme_names(headers: &HeaderMap) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for value in headers
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
    {
        // Scheme names are the tokens that start a challenge; parameters
        // contain `=`.
        for part in value.split(',') {
            let first = part.split_whitespace().next().unwrap_or_default();
            if !first.is_empty() && !first.contains('=') {
                let name = first.to_ascii_lowercase();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn offered_schemes(headers: &HeaderMap) -> Option<String> {
    let names = offered_scheme_names(headers);
    (!names.is_empty()).then(|| format!("server offered: {}", names.join(", ")))
}

/// The `error_description` (or `error`) of a bearer challenge.
fn bearer_error(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.trim_start().to_ascii_lowercase().starts_with("bearer"))?;
    let param = |name: &str| {
        let key = format!("{name}=\"");
        let start = value.find(&key)? + key.len();
        let end = value[start..].find('"')? + start;
        Some(value[start..end].to_owned())
    };
    param("error_description").or_else(|| param("error"))
}

/// Reads and discards a response body so the connection can be reused.
async fn drain(response: reqwest::Response) {
    let _ = response.bytes().await;
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// The NetBIOS-style name of this machine, sent as NTLM workstation.
fn workstation_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|name| {
            name.trim()
                .split('.')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(15)
                .collect::<String>()
                .to_ascii_uppercase()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(WWW_AUTHENTICATE, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn finds_challenge_tokens_and_schemes() {
        let map = headers(&[
            "Negotiate",
            "NTLM TlRMTVNTUAACAAAA",
            "Basic realm=\"mail.example.com\"",
        ]);
        assert_eq!(
            challenge_token(&map, "NTLM").as_deref(),
            Some("TlRMTVNTUAACAAAA")
        );
        assert_eq!(challenge_token(&map, "Negotiate"), None);
        assert_eq!(offered_scheme_names(&map), ["negotiate", "ntlm", "basic"]);
        let combined = headers(&["Negotiate, NTLM"]);
        assert_eq!(offered_scheme_names(&combined), ["negotiate", "ntlm"]);
        assert_eq!(
            offered_schemes(&combined).as_deref(),
            Some("server offered: negotiate, ntlm")
        );
        assert_eq!(offered_schemes(&HeaderMap::new()), None);
    }

    #[test]
    fn reads_bearer_errors() {
        let map = headers(&[
            r#"Bearer client_id="00000002-0000-0ff1-ce00-000000000000", error="invalid_token", error_description="The token has expired""#,
        ]);
        assert_eq!(bearer_error(&map).as_deref(), Some("The token has expired"));
        let map = headers(&[r#"Bearer error="invalid_token""#]);
        assert_eq!(bearer_error(&map).as_deref(), Some("invalid_token"));
        assert_eq!(bearer_error(&headers(&["Basic realm=\"x\""])), None);
    }

    #[test]
    fn loopback_detection() {
        for url in [
            "http://127.0.0.1:8080/EWS",
            "http://[::1]/EWS",
            "http://localhost/EWS",
        ] {
            assert!(is_loopback(&Url::parse(url).unwrap()), "{url}");
        }
        assert!(!is_loopback(
            &Url::parse("http://mail.example.com/EWS").unwrap()
        ));
    }

    #[test]
    fn refuses_plain_http_with_credentials() {
        let settings = HttpSettings {
            user_agent: "test".into(),
            timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(5),
            accept_invalid_certs: false,
            allow_insecure_http: false,
            ntlm_connections: 2,
        };
        let transport = Transport::new(Auth::basic("u", "p"), settings.clone(), None).unwrap();
        assert!(matches!(
            transport
                .check_secure(&Url::parse("http://mail.example.com/EWS/Exchange.asmx").unwrap()),
            Err(EwsError::InvalidInput(_))
        ));
        assert!(
            transport
                .check_secure(&Url::parse("https://mail.example.com/EWS/Exchange.asmx").unwrap())
                .is_ok()
        );
        let lax = Transport::new(
            Auth::basic("u", "p"),
            HttpSettings {
                allow_insecure_http: true,
                ..settings
            },
            None,
        )
        .unwrap();
        assert!(
            lax.check_secure(&Url::parse("http://mail.example.com/EWS/Exchange.asmx").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn workstation_names_are_netbios_like() {
        let name = workstation_name();
        assert!(name.len() <= 15);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
