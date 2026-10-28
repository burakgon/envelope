//! Autodiscover: finding the EWS endpoint (and server version) of a
//! mailbox from its e-mail address.
//!
//! [`Autodiscover::discover`] tries, in order:
//!
//! 1. POX Autodiscover (`autodiscover.xml`, authenticated POST) at
//!    `https://autodiscover.{domain}` and `https://{domain}`;
//! 2. the HTTP redirect method: an unauthenticated `GET` of
//!    `http://autodiscover.{domain}/autodiscover/autodiscover.xml` whose
//!    redirect to an HTTPS endpoint is then queried;
//! 3. the DNS SRV record `_autodiscover._tcp.{domain}`;
//! 4. Autodiscover v2 (JSON) at Exchange Online, which also answers for
//!    hybrid deployments.
//!
//! POX answers may redirect to another address (`redirectAddr`, which
//! restarts discovery for that address) or another URL (`redirectUrl`).
//! Credentials are only ever sent over HTTPS, endpoints returned by a
//! server must be HTTPS, and redirects are capped.
//!
//! Targets learned without TLS (the HTTP redirect method and SRV records,
//! which an attacker on the network could forge) only receive credentials
//! when their host belongs to the mail domain, is an Exchange Online host,
//! or is listed in [`AutodiscoverOptions::trusted_hosts`]. Outlook asks the
//! user in these cases; an application can do the same and then add the
//! host to the trusted list.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use crate::auth::Auth;
use crate::client::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_USER_AGENT};
use crate::error::EwsError;
use crate::transport::{HttpRequest, HttpResponse, HttpSettings, Transport};
use crate::version::ExchangeVersion;
use crate::xml::{self, Element, XmlWriter};

/// Request schema of POX Autodiscover.
const REQUEST_SCHEMA: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006";
/// Response schema the client asks for.
const RESPONSE_SCHEMA: &str =
    "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";

/// Autodiscover v2 endpoint of Exchange Online.
pub const OFFICE365_AUTODISCOVER_V2: &str =
    "https://outlook.office365.com/autodiscover/autodiscover.json/v1.0/";

/// Default time limit for one Autodiscover probe.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Redirects (address and URL) followed before giving up.
const MAX_REDIRECTS: usize = 10;

/// Where Autodiscover looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutodiscoverEndpoint {
    /// A POX `autodiscover.xml` URL, queried with an authenticated POST.
    Pox(Url),
    /// An HTTP URL whose redirect (unauthenticated `GET`) names the POX
    /// endpoint to use.
    HttpRedirect(Url),
    /// The `_autodiscover._tcp` SRV record of a domain.
    Srv(String),
    /// An Autodiscover v2 (JSON) base URL such as
    /// [`OFFICE365_AUTODISCOVER_V2`]; queried without credentials.
    Json(Url),
}

/// What Autodiscover found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutodiscoverResult {
    /// The EWS endpoint to use (the external one when the server reports
    /// both).
    pub ews_url: String,
    /// EWS URL for clients inside the organization's network.
    pub internal_ews_url: Option<String>,
    /// EWS URL for clients outside the organization's network.
    pub external_ews_url: Option<String>,
    /// The schema version matching the server build, when reported.
    pub server_version: Option<ExchangeVersion>,
    /// The server build (`15.2.1544`), when reported.
    pub server_build: Option<String>,
    /// The user's display name.
    pub display_name: Option<String>,
    /// The user's primary SMTP address (may differ from the address that
    /// was looked up).
    pub smtp_address: Option<String>,
    /// Whether the mailbox is in Exchange Online.
    pub microsoft_online: bool,
    /// The endpoint that answered.
    pub source: String,
}

/// The outcome of one POX request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PoxOutcome {
    Settings(AutodiscoverResult),
    RedirectAddress(String),
    RedirectUrl(String),
    Error(String),
}

/// The POX request body for `email`.
pub(crate) fn pox_request(email: &str) -> String {
    let mut w = XmlWriter::new();
    w.raw(r#"<?xml version="1.0" encoding="utf-8"?>"#)
        .start("Autodiscover", &[("xmlns", REQUEST_SCHEMA)])
        .start("Request", &[])
        .text_element("EMailAddress", email)
        .text_element("AcceptableResponseSchema", RESPONSE_SCHEMA)
        .end("Request")
        .end("Autodiscover");
    w.into_string()
}

/// Parses a POX response document.
pub(crate) fn parse_pox_response(text: &str, source: &str) -> Result<PoxOutcome, EwsError> {
    let root = xml::parse(text)?;
    if !root.is("Autodiscover") {
        return Err(EwsError::InvalidResponse(format!(
            "expected an Autodiscover document, got <{}>",
            root.name
        )));
    }
    let response = root.child("Response").ok_or_else(|| {
        EwsError::InvalidResponse("Autodiscover document without Response".into())
    })?;
    if let Some(error) = response
        .child("Error")
        .or_else(|| response.path(&["Account", "Error"]))
    {
        let code = error.child_text("ErrorCode").unwrap_or_default();
        let message = error.child_text("Message").unwrap_or_default();
        return Ok(PoxOutcome::Error(
            format!("{code} {message}").trim().to_owned(),
        ));
    }
    let account = response
        .child("Account")
        .ok_or_else(|| EwsError::InvalidResponse("Autodiscover response without Account".into()))?;
    let action = account.child_text("Action").unwrap_or_default();
    if action.eq_ignore_ascii_case("redirectAddr") {
        let address = account
            .child_text("RedirectAddr")
            .ok_or_else(|| EwsError::InvalidResponse("redirectAddr without address".into()))?;
        return Ok(PoxOutcome::RedirectAddress(address));
    }
    if action.eq_ignore_ascii_case("redirectUrl") {
        let url = account
            .child_text("RedirectUrl")
            .ok_or_else(|| EwsError::InvalidResponse("redirectUrl without URL".into()))?;
        return Ok(PoxOutcome::RedirectUrl(url));
    }

    let protocols: Vec<&Element> = account.children_named("Protocol").collect();
    let protocol_url = |kind: &str| {
        protocols
            .iter()
            .filter(|p| {
                p.child_text("Type")
                    .or_else(|| p.attr("Type").map(str::to_owned))
                    .is_some_and(|t| t.eq_ignore_ascii_case(kind))
            })
            .find_map(|p| p.child_text("EwsUrl"))
    };
    let internal = protocol_url("EXCH");
    let external = protocol_url("EXPR");
    let any = protocols.iter().find_map(|p| p.child_text("EwsUrl"));
    let Some(ews_url) = external.clone().or_else(|| internal.clone()).or(any) else {
        return Ok(PoxOutcome::Error(
            "the Autodiscover response names no EWS URL".into(),
        ));
    };
    let build = protocols
        .iter()
        .find_map(|p| p.child_text("ServerVersion"))
        .and_then(|hex| decode_server_version(&hex));
    let user = response.child("User");
    Ok(PoxOutcome::Settings(AutodiscoverResult {
        ews_url,
        internal_ews_url: internal,
        external_ews_url: external,
        server_version: build
            .and_then(|(major, minor, build)| ExchangeVersion::from_build(major, minor, build)),
        server_build: build.map(|(major, minor, build)| format!("{major}.{minor}.{build}")),
        display_name: user.and_then(|u| u.child_text("DisplayName")),
        smtp_address: user.and_then(|u| u.child_text("AutoDiscoverSMTPAddress")),
        microsoft_online: account
            .child_text("MicrosoftOnline")
            .is_some_and(|v| v.eq_ignore_ascii_case("true")),
        source: source.to_owned(),
    }))
}

/// Decodes the hexadecimal `ServerVersion` of a POX response: bits 0–3
/// are `0111`, then 6 bits major version, 6 bits minor version, one flag
/// bit and 15 bits build number.
pub fn decode_server_version(hex: &str) -> Option<(u32, u32, u32)> {
    let value = u32::from_str_radix(hex.trim(), 16).ok()?;
    if value >> 28 != 0b0111 {
        return None;
    }
    let major = (value >> 22) & 0x3F;
    let minor = (value >> 16) & 0x3F;
    let build = value & 0x7FFF;
    Some((major, minor, build))
}

/// Parses an Autodiscover v2 JSON answer and returns the EWS URL.
pub(crate) fn parse_json_response(text: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(rename = "Url", default)]
        url: Option<String>,
        #[serde(rename = "Protocol", default)]
        protocol: Option<String>,
        #[serde(rename = "ErrorCode", default)]
        error_code: Option<String>,
        #[serde(rename = "ErrorMessage", default)]
        error_message: Option<String>,
    }
    let answer: Answer =
        serde_json::from_str(text).map_err(|e| format!("not an Autodiscover v2 answer: {e}"))?;
    if let Some(code) = answer.error_code {
        return Err(format!(
            "{code}: {}",
            answer.error_message.unwrap_or_default()
        ));
    }
    match (answer.url, answer.protocol) {
        (Some(url), protocol)
            if protocol
                .as_deref()
                .is_none_or(|p| p.eq_ignore_ascii_case("EWS")) =>
        {
            Ok(url)
        }
        _ => Err("Autodiscover v2 answer without EWS URL".into()),
    }
}

/// The domain part of an address.
fn domain_of(email: &str) -> Result<String, EwsError> {
    let (local, domain) = email
        .trim()
        .rsplit_once('@')
        .ok_or_else(|| EwsError::InvalidInput(format!("`{email}` is not an e-mail address")))?;
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(EwsError::InvalidInput(format!(
            "`{email}` is not an e-mail address"
        )));
    }
    Ok(domain)
}

/// Host suffixes of Exchange Online that redirect targets may always use.
const MICROSOFT_ONLINE_HOSTS: &[&str] = &["outlook.com", "office365.com", "office.com"];

/// Options for [`Autodiscover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutodiscoverOptions {
    /// Try the HTTP redirect method (default on).
    pub http_redirect: bool,
    /// Look up the `_autodiscover._tcp` SRV record (default on).
    pub srv: bool,
    /// Ask Exchange Online's Autodiscover v2 last (default on).
    pub office365_json: bool,
    /// Further hosts (or parent domains) that targets of the HTTP redirect
    /// method and of SRV records may point to, for example a hosting
    /// provider's domain the user confirmed.
    pub trusted_hosts: Vec<String>,
}

impl Default for AutodiscoverOptions {
    fn default() -> Self {
        AutodiscoverOptions {
            http_redirect: true,
            srv: true,
            office365_json: true,
            trusted_hosts: Vec::new(),
        }
    }
}

impl AutodiscoverOptions {
    /// Whether credentials may go to `host`, a target learned without TLS
    /// while looking up an address in `domain`.
    pub fn trusts_host(&self, host: &str, domain: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let within = |suffix: &str| {
            let suffix = suffix.trim().trim_start_matches('.').to_ascii_lowercase();
            !suffix.is_empty() && (host == suffix || host.ends_with(&format!(".{suffix}")))
        };
        within(domain)
            || MICROSOFT_ONLINE_HOSTS.iter().any(|suffix| within(suffix))
            || self.trusted_hosts.iter().any(|suffix| within(suffix))
    }
}

/// The endpoints [`Autodiscover::discover`] tries for a domain, in order.
pub fn default_endpoints(domain: &str, options: &AutodiscoverOptions) -> Vec<AutodiscoverEndpoint> {
    let mut endpoints = Vec::new();
    for host in [format!("autodiscover.{domain}"), domain.to_owned()] {
        if let Ok(url) = Url::parse(&format!("https://{host}/autodiscover/autodiscover.xml")) {
            endpoints.push(AutodiscoverEndpoint::Pox(url));
        }
    }
    if options.http_redirect
        && let Ok(url) = Url::parse(&format!(
            "http://autodiscover.{domain}/autodiscover/autodiscover.xml"
        ))
    {
        endpoints.push(AutodiscoverEndpoint::HttpRedirect(url));
    }
    if options.srv {
        endpoints.push(AutodiscoverEndpoint::Srv(domain.to_owned()));
    }
    if options.office365_json
        && let Ok(url) = Url::parse(OFFICE365_AUTODISCOVER_V2)
    {
        endpoints.push(AutodiscoverEndpoint::Json(url));
    }
    endpoints
}

/// Finds EWS endpoints with the account's credentials.
pub struct Autodiscover {
    transport: Transport,
    options: AutodiscoverOptions,
    probe_timeout: Duration,
}

impl std::fmt::Debug for Autodiscover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Autodiscover")
            .field("options", &self.options)
            .field("probe_timeout", &self.probe_timeout)
            .finish_non_exhaustive()
    }
}

/// Builder for [`Autodiscover`].
#[derive(Debug)]
pub struct AutodiscoverBuilder {
    auth: Auth,
    options: AutodiscoverOptions,
    user_agent: String,
    probe_timeout: Duration,
    accept_invalid_certs: bool,
    http: Option<reqwest::Client>,
}

impl AutodiscoverBuilder {
    /// Which methods to try.
    pub fn options(mut self, options: AutodiscoverOptions) -> Self {
        self.options = options;
        self
    }

    /// Overrides the `User-Agent`.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Time limit for each probe (default [`DEFAULT_PROBE_TIMEOUT`]).
    pub fn probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Accepts TLS certificates that do not verify (self-signed servers).
    pub fn accept_invalid_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_certs = accept;
        self
    }

    /// Uses a preconfigured `reqwest` client (it must not follow redirects).
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Builds the discoverer.
    pub fn build(self) -> Result<Autodiscover, EwsError> {
        let transport = Transport::new(
            self.auth,
            HttpSettings {
                user_agent: self.user_agent,
                timeout: self.probe_timeout,
                connect_timeout: DEFAULT_CONNECT_TIMEOUT.min(self.probe_timeout),
                accept_invalid_certs: self.accept_invalid_certs,
                allow_insecure_http: false,
                ntlm_connections: 1,
            },
            self.http,
        )?;
        Ok(Autodiscover {
            transport,
            options: self.options,
            probe_timeout: self.probe_timeout,
        })
    }
}

impl Autodiscover {
    /// Starts building a discoverer that authenticates with `auth`.
    pub fn builder(auth: Auth) -> AutodiscoverBuilder {
        AutodiscoverBuilder {
            auth,
            options: AutodiscoverOptions::default(),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            accept_invalid_certs: false,
            http: None,
        }
    }

    /// A discoverer with default settings.
    pub fn new(auth: Auth) -> Result<Self, EwsError> {
        Autodiscover::builder(auth).build()
    }

    /// Finds the EWS endpoint of `email` with the default endpoints (see
    /// the [module documentation](self)).
    pub async fn discover(&self, email: &str) -> Result<AutodiscoverResult, EwsError> {
        let options = self.options.clone();
        self.discover_with(email, move |domain| default_endpoints(domain, &options))
            .await
    }

    /// Finds the EWS endpoint of `email`, trying the endpoints `endpoints`
    /// returns for a domain (called again when a server redirects to an
    /// address in another domain).
    pub async fn discover_with<F>(
        &self,
        email: &str,
        endpoints: F,
    ) -> Result<AutodiscoverResult, EwsError>
    where
        F: Fn(&str) -> Vec<AutodiscoverEndpoint>,
    {
        let mut address = email.trim().to_owned();
        let mut attempts = Vec::new();
        let mut unauthorized = false;
        let mut redirects = 0usize;
        let mut visited_addresses = vec![address.to_ascii_lowercase()];
        'address: loop {
            let domain = domain_of(&address)?;
            for endpoint in endpoints(&domain) {
                let outcome = match &endpoint {
                    AutodiscoverEndpoint::Pox(url) => {
                        self.pox_chain(
                            &address,
                            url.clone(),
                            &mut redirects,
                            &mut attempts,
                            &mut unauthorized,
                        )
                        .await
                    }
                    AutodiscoverEndpoint::HttpRedirect(url) => {
                        match self.redirect_target(url).await {
                            Ok(Some(target)) if self.trusted(&target, &domain) => {
                                self.pox_chain(
                                    &address,
                                    target,
                                    &mut redirects,
                                    &mut attempts,
                                    &mut unauthorized,
                                )
                                .await
                            }
                            Ok(Some(target)) => {
                                attempts.push(format!(
                                    "{url}: redirect to untrusted host {}",
                                    target.host_str().unwrap_or_default()
                                ));
                                None
                            }
                            Ok(None) => {
                                attempts.push(format!("{url}: no redirect"));
                                None
                            }
                            Err(err) => {
                                attempts.push(format!("{url}: {err}"));
                                None
                            }
                        }
                    }
                    AutodiscoverEndpoint::Srv(srv_domain) => {
                        let mut found = None;
                        for url in srv_endpoints(srv_domain).await {
                            if !self.trusted(&url, &domain) {
                                attempts.push(format!(
                                    "SRV _autodiscover._tcp.{srv_domain}: untrusted host {}",
                                    url.host_str().unwrap_or_default()
                                ));
                                continue;
                            }
                            found = self
                                .pox_chain(
                                    &address,
                                    url,
                                    &mut redirects,
                                    &mut attempts,
                                    &mut unauthorized,
                                )
                                .await;
                            if found.is_some() {
                                break;
                            }
                        }
                        if found.is_none() {
                            attempts.push(format!(
                                "SRV _autodiscover._tcp.{srv_domain}: no usable record"
                            ));
                        }
                        found
                    }
                    AutodiscoverEndpoint::Json(base) => {
                        match self.json(&address, base, &mut redirects).await {
                            Ok(result) => Some(PoxOutcome::Settings(result)),
                            Err(err) => {
                                attempts.push(format!("{base}: {err}"));
                                None
                            }
                        }
                    }
                };
                match outcome {
                    Some(PoxOutcome::Settings(result)) => return Ok(result),
                    Some(PoxOutcome::RedirectAddress(next)) => {
                        redirects += 1;
                        let key = next.trim().to_ascii_lowercase();
                        if redirects > MAX_REDIRECTS || visited_addresses.contains(&key) {
                            return Err(EwsError::TooManyRedirects {
                                url: format!("mailto:{email}"),
                            });
                        }
                        debug!(from = %address, to = %next, "Autodiscover redirects to another address");
                        visited_addresses.push(key);
                        address = next.trim().to_owned();
                        continue 'address;
                    }
                    Some(PoxOutcome::RedirectUrl(_) | PoxOutcome::Error(_)) | None => {}
                }
            }
            return Err(EwsError::AutodiscoverFailed {
                email: email.trim().to_owned(),
                attempts,
                unauthorized,
            });
        }
    }

    /// Queries a POX endpoint, following `redirectUrl` answers and HTTP
    /// redirects. Returns settings or an address redirect; failures are
    /// recorded in `attempts`.
    async fn pox_chain(
        &self,
        email: &str,
        start: Url,
        redirects: &mut usize,
        attempts: &mut Vec<String>,
        unauthorized: &mut bool,
    ) -> Option<PoxOutcome> {
        let mut url = start;
        loop {
            if !is_acceptable_endpoint(&url) {
                attempts.push(format!("{url}: refusing a non-HTTPS endpoint"));
                return None;
            }
            match self.pox(email, &url).await {
                Ok(PoxOutcome::RedirectUrl(next)) => {
                    *redirects += 1;
                    if *redirects > MAX_REDIRECTS {
                        attempts.push(format!("{url}: too many redirects"));
                        return None;
                    }
                    match url.join(&next) {
                        Ok(next) => {
                            debug!(from = %url, to = %next, "Autodiscover redirects to another URL");
                            url = next;
                        }
                        Err(err) => {
                            attempts.push(format!("{url}: invalid redirect `{next}`: {err}"));
                            return None;
                        }
                    }
                }
                Ok(PoxOutcome::Settings(result)) => {
                    if is_acceptable_endpoint_str(&result.ews_url) {
                        return Some(PoxOutcome::Settings(result));
                    }
                    attempts.push(format!("{url}: unusable EWS URL `{}`", result.ews_url));
                    return None;
                }
                Ok(PoxOutcome::Error(message)) => {
                    attempts.push(format!("{url}: {message}"));
                    return None;
                }
                Ok(redirect @ PoxOutcome::RedirectAddress(_)) => return Some(redirect),
                Err(err) => {
                    *unauthorized |= err.is_auth_error();
                    attempts.push(format!("{url}: {err}"));
                    return None;
                }
            }
        }
    }

    fn trusted(&self, url: &Url, domain: &str) -> bool {
        // Loopback endpoints only appear in tests and local setups.
        let loopback = matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        loopback
            || url
                .host_str()
                .is_some_and(|host| self.options.trusts_host(host, domain))
    }

    /// One POX request.
    async fn pox(&self, email: &str, url: &Url) -> Result<PoxOutcome, EwsError> {
        let mut request =
            HttpRequest::post(url.clone(), "text/xml; charset=utf-8", pox_request(email));
        request.timeout = Some(self.probe_timeout);
        let response = self.transport.send(&request).await?;
        if response.status.is_redirection() {
            let location = location(&response)
                .ok_or_else(|| EwsError::InvalidResponse("redirect without Location".into()))?;
            return Ok(PoxOutcome::RedirectUrl(location));
        }
        if !response.status.is_success() {
            return Err(EwsError::Http {
                status: response.status.as_u16(),
                url: url.to_string(),
                body: String::new(),
                retry_after: None,
            });
        }
        parse_pox_response(&String::from_utf8_lossy(&response.body), url.as_str())
    }

    /// The HTTPS target an HTTP redirect probe points to.
    async fn redirect_target(&self, url: &Url) -> Result<Option<Url>, EwsError> {
        let mut request = HttpRequest::get(url.clone());
        request.timeout = Some(self.probe_timeout);
        let response = self.transport.send_unauthenticated(&request).await?;
        if !response.status.is_redirection() {
            return Ok(None);
        }
        let Some(location) = location(&response) else {
            return Ok(None);
        };
        let target = url.join(&location).map_err(|e| {
            EwsError::InvalidResponse(format!("invalid redirect `{location}`: {e}"))
        })?;
        // Only an HTTPS autodiscover.xml endpoint is acceptable: the next
        // request carries credentials.
        let looks_right = target
            .path()
            .to_ascii_lowercase()
            .ends_with("/autodiscover/autodiscover.xml");
        Ok((is_acceptable_endpoint(&target) && looks_right).then_some(target))
    }

    /// Autodiscover v2: an unauthenticated JSON `GET`.
    async fn json(
        &self,
        email: &str,
        base: &Url,
        redirects: &mut usize,
    ) -> Result<AutodiscoverResult, EwsError> {
        let mut url = base.clone();
        url.path_segments_mut()
            .map_err(|()| EwsError::InvalidInput(format!("invalid Autodiscover v2 URL {base}")))?
            .pop_if_empty()
            .push(email);
        url.query_pairs_mut().append_pair("Protocol", "EWS");
        loop {
            let mut request = HttpRequest::get(url.clone());
            request.timeout = Some(self.probe_timeout);
            request.headers.insert(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
            let response = self.transport.send_unauthenticated(&request).await?;
            if response.status.is_redirection() {
                *redirects += 1;
                let next = location(&response)
                    .and_then(|l| url.join(&l).ok())
                    .filter(is_acceptable_endpoint)
                    .ok_or_else(|| {
                        EwsError::InvalidResponse("unusable Autodiscover v2 redirect".into())
                    })?;
                if *redirects > MAX_REDIRECTS {
                    return Err(EwsError::TooManyRedirects {
                        url: base.to_string(),
                    });
                }
                url = next;
                continue;
            }
            let text = String::from_utf8_lossy(&response.body);
            let ews_url = parse_json_response(&text).map_err(|message| {
                EwsError::InvalidResponse(format!("HTTP {}: {message}", response.status.as_u16()))
            })?;
            if !is_acceptable_endpoint_str(&ews_url) {
                return Err(EwsError::InvalidResponse(format!(
                    "unusable EWS URL `{ews_url}`"
                )));
            }
            return Ok(AutodiscoverResult {
                microsoft_online: url
                    .host_str()
                    .is_some_and(|h| h.ends_with(".office365.com") || h.ends_with(".outlook.com")),
                ews_url,
                internal_ews_url: None,
                external_ews_url: None,
                server_version: None,
                server_build: None,
                display_name: None,
                smtp_address: None,
                source: url.to_string(),
            });
        }
    }
}

fn location(response: &HttpResponse) -> Option<String> {
    response
        .header(reqwest::header::LOCATION.as_str())
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
}

/// HTTPS endpoints, or HTTP to a loopback address (tests).
fn is_acceptable_endpoint(url: &Url) -> bool {
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => {
            matches!(
                url.host(),
                Some(url::Host::Ipv4(ip)) if ip.is_loopback()
            ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
        }
        _ => false,
    }
}

fn is_acceptable_endpoint_str(url: &str) -> bool {
    Url::parse(url.trim()).is_ok_and(|u| is_acceptable_endpoint(&u))
}

/// POX endpoints named by the domain's `_autodiscover._tcp` SRV records,
/// by priority and weight.
async fn srv_endpoints(domain: &str) -> Vec<Url> {
    let Ok(builder) = hickory_resolver::TokioResolver::builder_tokio() else {
        return Vec::new();
    };
    let Ok(resolver) = builder.build() else {
        return Vec::new();
    };
    let name = format!("_autodiscover._tcp.{domain}.");
    let Ok(Ok(lookup)) =
        tokio::time::timeout(Duration::from_secs(5), resolver.srv_lookup(name)).await
    else {
        return Vec::new();
    };
    let mut records: Vec<(u16, u16, String, u16)> = lookup
        .answers()
        .iter()
        .filter_map(|record| match &record.data {
            hickory_resolver::proto::rr::RData::SRV(srv) => {
                let target = srv.target.to_string().trim_end_matches('.').to_owned();
                (!target.is_empty()).then_some((srv.priority, srv.weight, target, srv.port))
            }
            _ => None,
        })
        .collect();
    records.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    records
        .into_iter()
        .filter_map(|(_, _, target, port)| srv_url(&target, port))
        .collect()
}

/// The POX URL for an SRV target.
fn srv_url(target: &str, port: u16) -> Option<Url> {
    let authority = if port == 443 || port == 0 {
        target.to_owned()
    } else {
        format!("{target}:{port}")
    };
    Url::parse(&format!(
        "https://{authority}/autodiscover/autodiscover.xml"
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body() {
        let body = pox_request("jane&co@example.com");
        let root = xml::parse(&body).unwrap();
        assert!(root.is("Autodiscover"));
        assert_eq!(
            root.path(&["Request", "EMailAddress"])
                .and_then(Element::trimmed_text)
                .as_deref(),
            Some("jane&co@example.com")
        );
        assert!(body.contains(RESPONSE_SCHEMA));
        assert!(body.contains(&format!("xmlns=\"{REQUEST_SCHEMA}\"")));
    }

    #[test]
    fn server_versions() {
        assert_eq!(decode_server_version("73C18880"), Some((15, 1, 2176)));
        assert_eq!(decode_server_version("73C28C08"), Some((15, 2, 3080)));
        assert_eq!(decode_server_version("738180DA"), Some((14, 1, 218)));
        assert_eq!(decode_server_version("12345678"), None);
        assert_eq!(decode_server_version("zz"), None);
    }

    #[test]
    fn endpoints_in_order() {
        let endpoints = default_endpoints("example.com", &AutodiscoverOptions::default());
        let described: Vec<String> = endpoints
            .iter()
            .map(|e| match e {
                AutodiscoverEndpoint::Pox(u) => format!("pox {u}"),
                AutodiscoverEndpoint::HttpRedirect(u) => format!("redirect {u}"),
                AutodiscoverEndpoint::Srv(d) => format!("srv {d}"),
                AutodiscoverEndpoint::Json(u) => format!("json {u}"),
            })
            .collect();
        assert_eq!(
            described,
            [
                "pox https://autodiscover.example.com/autodiscover/autodiscover.xml",
                "pox https://example.com/autodiscover/autodiscover.xml",
                "redirect http://autodiscover.example.com/autodiscover/autodiscover.xml",
                "srv example.com",
                "json https://outlook.office365.com/autodiscover/autodiscover.json/v1.0/",
            ]
        );
        let minimal = default_endpoints(
            "example.com",
            &AutodiscoverOptions {
                http_redirect: false,
                srv: false,
                office365_json: false,
                trusted_hosts: Vec::new(),
            },
        );
        assert_eq!(minimal.len(), 2);
    }

    #[test]
    fn trust_for_unauthenticated_redirect_targets() {
        let options = AutodiscoverOptions {
            trusted_hosts: vec!["hoster.example.net".into()],
            ..AutodiscoverOptions::default()
        };
        assert!(options.trusts_host("mail.example.com", "example.com"));
        assert!(options.trusts_host("EXAMPLE.COM.", "example.com"));
        assert!(options.trusts_host("autodiscover-s.outlook.com", "example.com"));
        assert!(options.trusts_host("ex1.hoster.example.net", "example.com"));
        assert!(!options.trusts_host("example.com.evil.test", "example.com"));
        assert!(!options.trusts_host("notexample.com", "example.com"));
        assert!(
            !AutodiscoverOptions::default().trusts_host("ex1.hoster.example.net", "example.com")
        );
    }

    #[test]
    fn domains_and_endpoint_checks() {
        assert_eq!(domain_of(" Jane@Example.COM ").unwrap(), "example.com");
        assert!(domain_of("jane").is_err());
        assert!(domain_of("@example.com").is_err());
        assert!(domain_of("jane@localhost").is_err());
        assert!(is_acceptable_endpoint_str(
            "https://mail.example.com/EWS/Exchange.asmx"
        ));
        assert!(!is_acceptable_endpoint_str(
            "http://mail.example.com/EWS/Exchange.asmx"
        ));
        assert!(is_acceptable_endpoint_str(
            "http://127.0.0.1:8080/EWS/Exchange.asmx"
        ));
        assert!(!is_acceptable_endpoint_str("ftp://mail.example.com/"));
        assert_eq!(
            srv_url("mail.example.com", 443)
                .map(|u| u.to_string())
                .as_deref(),
            Some("https://mail.example.com/autodiscover/autodiscover.xml")
        );
        assert_eq!(
            srv_url("mail.example.com", 8443)
                .map(|u| u.to_string())
                .as_deref(),
            Some("https://mail.example.com:8443/autodiscover/autodiscover.xml")
        );
    }

    #[test]
    fn json_answers() {
        assert_eq!(
            parse_json_response(
                r#"{"Protocol":"EWS","Url":"https://outlook.office365.com/EWS/Exchange.asmx"}"#
            )
            .as_deref(),
            Ok("https://outlook.office365.com/EWS/Exchange.asmx")
        );
        assert!(
            parse_json_response(r#"{"ErrorCode":"InvalidUser","ErrorMessage":"Invalid user"}"#)
                .unwrap_err()
                .contains("InvalidUser")
        );
        assert!(parse_json_response(r#"{"Protocol":"ActiveSync","Url":"https://x"}"#).is_err());
        assert!(parse_json_response("<html/>").is_err());
    }
}
