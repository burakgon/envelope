//! Adding Exchange accounts: finding the server with Autodiscover, checking
//! the sign-in by reading the inbox, and messages that say what went wrong.
//! Also the credential-free check provider discovery uses to suggest
//! Exchange for a domain.

use std::time::Duration;

use envelope_ews::{
    self as ews, Autodiscover, AutodiscoverEndpoint, AutodiscoverOptions, AutodiscoverResult,
    DistinguishedFolder, EwsClient, EwsError, ExchangeVersion, RetryPolicy,
};

use super::{Scheme, certificate_problem, ews_error, ntlm_not_offered};
use crate::db::mail;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::*;
use crate::secrets;

/// Time limit for each Autodiscover probe while adding an account.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What it takes to sign in to a mailbox that is being added.
#[derive(Clone)]
pub(super) struct Credentials {
    pub(super) email: String,
    pub(super) username: String,
    pub(super) password: String,
    pub(super) auth: ExchangeAuth,
    pub(super) accept_invalid_certs: bool,
}

/// A mailbox that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) url: String,
    pub(super) version: Option<ExchangeVersion>,
    pub(super) display_name: Option<String>,
}

fn host_of(url: &str) -> String {
    ews::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}

fn is_dns_failure(e: &EwsError) -> bool {
    let EwsError::Transport(inner) = e else {
        return false;
    };
    let mut text = inner.to_string();
    let mut source = std::error::Error::source(inner);
    while let Some(s) = source {
        text.push(' ');
        text.push_str(&s.to_string());
        source = s.source();
    }
    let text = text.to_ascii_lowercase();
    text.contains("dns error")
        || text.contains("failed to lookup address")
        || text.contains("name or service not known")
        || text.contains("no such host")
}

/// A sign-in or connection failure while adding an account, explained.
pub(super) fn explain(e: EwsError, url: &str) -> Error {
    let host = host_of(url);
    if ntlm_not_offered(&e) {
        return Error::Auth(format!(
            "{host} does not offer a sign-in method Envelope supports ({e}). Ask your \
             administrator whether NTLM or Basic authentication is turned on for Exchange Web \
             Services."
        ));
    }
    if e.is_auth_error() {
        return Error::Auth(
            "Exchange did not accept the user name or password. If your organization signs in \
             with DOMAIN\\user, enter that under Advanced."
                .into(),
        );
    }
    if let Some(problem) = certificate_problem(&e) {
        return Error::Invalid(format!(
            "The security certificate of {host} is not trusted ({problem}). If this is your \
             organization's own server, turn on \u{201c}Accept self-signed certificates\u{201d} \
             under Advanced and try again."
        ));
    }
    match &e {
        _ if is_dns_failure(&e) => Error::Network(format!(
            "{host} could not be found. Check the server address."
        )),
        EwsError::Timeout(_) => Error::Network(format!("{host} did not answer in time.")),
        EwsError::Transport(_) => Error::Network(format!("Could not connect to {host}: {e}")),
        EwsError::Forbidden { .. } => Error::Invalid(format!(
            "{host} refused access to Exchange Web Services. EWS may be turned off for your \
             account; ask your administrator."
        )),
        EwsError::Http { status: 404, .. } => Error::Invalid(format!(
            "There is no Exchange Web Services endpoint at {url}. The address usually ends in \
             /EWS/Exchange.asmx."
        )),
        EwsError::Http { status, .. } if *status < 500 => Error::Invalid(format!(
            "{host} answered with HTTP status {status}. Is this the address of an Exchange \
             server?"
        )),
        EwsError::InvalidResponse(_) | EwsError::Xml(_) => Error::Invalid(format!(
            "{url} did not answer like an Exchange server. Check the server address."
        )),
        _ => ews_error(e),
    }
}

/// A client for checking a sign-in: quick to give up, so the person adding
/// the account learns soon what is wrong.
fn probe_client(
    creds: &Credentials,
    url: &str,
    scheme: Scheme,
    version: Option<ExchangeVersion>,
) -> Result<EwsClient> {
    let auth = match scheme {
        Scheme::Ntlm => ews::Auth::ntlm(creds.username.clone(), creds.password.clone()),
        Scheme::Basic => ews::Auth::basic(creds.username.clone(), creds.password.clone()),
    };
    let mut builder = EwsClient::builder(url.to_string(), auth)
        .accept_invalid_certs(creds.accept_invalid_certs)
        .anchor_mailbox(creds.email.clone())
        .timeout(Duration::from_secs(60))
        .retry_policy(RetryPolicy {
            max_retries: 1,
            ..RetryPolicy::default()
        });
    if let Some(version) = version {
        builder = builder.server_version(version);
    }
    builder.build().map_err(|e| explain(e, url))
}

/// Checks the sign-in at `url` by reading the inbox. NTLM goes first (the
/// default of on-premises servers); automatic sign-in falls back to Basic
/// when the server does not offer NTLM. The schema version Autodiscover
/// reported is tried first, then older ones should the server refuse it.
/// Returns the normalized URL and the schema version that worked.
pub(super) async fn verify(
    creds: &Credentials,
    url: &str,
    version: Option<ExchangeVersion>,
) -> Result<(String, Option<ExchangeVersion>)> {
    let schemes: &[Scheme] = match creds.auth {
        ExchangeAuth::Auto => &[Scheme::Ntlm, Scheme::Basic],
        ExchangeAuth::Ntlm => &[Scheme::Ntlm],
        ExchangeAuth::Basic => &[Scheme::Basic],
    };
    let mut versions: Vec<ExchangeVersion> = version.into_iter().collect();
    for fallback in [
        ExchangeVersion::Exchange2013Sp1,
        ExchangeVersion::Exchange2013,
        ExchangeVersion::Exchange2010Sp2,
    ] {
        if !versions.contains(&fallback) {
            versions.push(fallback);
        }
    }
    'schemes: for (i, scheme) in schemes.iter().enumerate() {
        for (j, version) in versions.iter().enumerate() {
            let client = probe_client(creds, url, *scheme, Some(*version))?;
            match client.get_folder(DistinguishedFolder::Inbox).await {
                Ok(_) => return Ok((client.url().to_string(), Some(*version))),
                Err(e) if i + 1 < schemes.len() && ntlm_not_offered(&e) => continue 'schemes,
                // An older server does not know the schema version asked for.
                Err(e)
                    if j + 1 < versions.len()
                        && e.response_code()
                            == Some(&ews::ResponseCode::ErrorInvalidServerVersion) => {}
                Err(e) => return Err(explain(e, url)),
            }
        }
    }
    Err(Error::Auth(
        "Exchange did not accept any sign-in method Envelope supports".into(),
    ))
}

fn domain_of(email: &str) -> String {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// The places Autodiscover looks for a domain (see [`ews::autodiscover`]).
fn default_endpoints(domain: &str) -> Vec<AutodiscoverEndpoint> {
    ews::autodiscover::default_endpoints(domain, &AutodiscoverOptions::default())
}

/// Finds the mailbox's EWS endpoint with the account's credentials, asking
/// the endpoints `endpoints` names for a domain.
pub(super) async fn autodiscover<F>(
    creds: &Credentials,
    endpoints: &F,
) -> Result<AutodiscoverResult>
where
    F: Fn(&str) -> Vec<AutodiscoverEndpoint> + Sync,
{
    let schemes: &[Scheme] = match creds.auth {
        ExchangeAuth::Basic => &[Scheme::Basic],
        ExchangeAuth::Ntlm => &[Scheme::Ntlm],
        ExchangeAuth::Auto => &[Scheme::Ntlm, Scheme::Basic],
    };
    let domain = domain_of(&creds.email);
    let mut last = None;
    for (i, scheme) in schemes.iter().enumerate() {
        let more_schemes = i + 1 < schemes.len();
        let auth = match scheme {
            Scheme::Ntlm => ews::Auth::ntlm(creds.username.clone(), creds.password.clone()),
            Scheme::Basic => ews::Auth::basic(creds.username.clone(), creds.password.clone()),
        };
        let discoverer = Autodiscover::builder(auth)
            .accept_invalid_certs(creds.accept_invalid_certs)
            .probe_timeout(PROBE_TIMEOUT)
            .build()
            .map_err(ews_error)?;
        match discoverer.discover_with(&creds.email, endpoints).await {
            Ok(found) => return Ok(found),
            Err(EwsError::AutodiscoverFailed {
                attempts,
                unauthorized,
                ..
            }) => {
                tracing::info!(
                    "Autodiscover for {domain} failed ({}): {}",
                    scheme_name(*scheme),
                    attempts.join("; ")
                );
                let ntlm_missing = attempts.iter().any(|a| {
                    let a = a.to_ascii_lowercase();
                    a.contains("does not offer ntlm")
                        || a.contains("did not send an ntlm challenge")
                });
                if ntlm_missing && *scheme == Scheme::Ntlm {
                    if more_schemes {
                        continue;
                    }
                    return Err(Error::Auth(format!(
                        "The Exchange server for {domain} does not offer NTLM sign-in. Try \
                         Basic authentication, or ask your administrator."
                    )));
                }
                if unauthorized {
                    return Err(Error::Auth(
                        "Exchange did not accept the user name or password. If your \
                         organization signs in with DOMAIN\\user, enter that under Advanced."
                            .into(),
                    ));
                }
                if let Some(problem) = attempts
                    .iter()
                    .find(|a| a.to_ascii_lowercase().contains("certificate"))
                    && !creds.accept_invalid_certs
                {
                    last = Some(Error::Invalid(format!(
                        "The server's security certificate is not trusted ({problem}). If \
                         this is your organization's own server, turn on \u{201c}Accept \
                         self-signed certificates\u{201d} under Advanced and try again."
                    )));
                    continue;
                }
                last = Some(Error::NotFound(format!(
                    "Envelope could not find the Exchange server for {domain} automatically. \
                     Open Advanced and enter the server address (for example mail.{domain})."
                )));
            }
            Err(e) => return Err(explain(e, &format!("https://autodiscover.{domain}"))),
        }
    }
    Err(last.unwrap_or_else(|| {
        Error::NotFound(format!(
            "Envelope could not find the Exchange server for {domain} automatically."
        ))
    }))
}

fn scheme_name(scheme: Scheme) -> &'static str {
    match scheme {
        Scheme::Ntlm => "NTLM",
        Scheme::Basic => "Basic",
    }
}

/// Finds (unless given) and checks the EWS endpoint of a mailbox.
pub(super) async fn connect(creds: &Credentials, url: Option<&str>) -> Result<Found> {
    connect_with(creds, url, &default_endpoints).await
}

/// [`connect`], with Autodiscover asking the given endpoints.
pub(super) async fn connect_with<F>(
    creds: &Credentials,
    url: Option<&str>,
    endpoints: &F,
) -> Result<Found>
where
    F: Fn(&str) -> Vec<AutodiscoverEndpoint> + Sync,
{
    let (urls, version, display_name) = match url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => {
            let mut urls = vec![url.to_string()];
            // People paste the Outlook on the web address; EWS lives next to it.
            if let Some(standard) = standard_endpoint(url)
                && !urls.contains(&standard)
            {
                urls.push(standard);
            }
            (urls, None, None)
        }
        None => {
            let found = autodiscover(creds, endpoints).await?;
            if found.microsoft_online {
                return Err(Error::Invalid(
                    "This mailbox is hosted by Microsoft 365. Add it with the Microsoft option \
                     instead."
                        .into(),
                ));
            }
            let mut urls = vec![found.ews_url.clone()];
            for alternative in [&found.external_ews_url, &found.internal_ews_url]
                .into_iter()
                .flatten()
            {
                if !urls.contains(alternative) {
                    urls.push(alternative.clone());
                }
            }
            (urls, found.server_version, found.display_name)
        }
    };
    let mut failure = None;
    for url in &urls {
        match verify(creds, url, version).await {
            Ok((url, version)) => {
                return Ok(Found {
                    url,
                    version,
                    display_name: display_name.clone().filter(|n| !n.trim().is_empty()),
                });
            }
            Err(e) if e.is_auth() => return Err(e),
            // Unreachable, or not EWS: the next address may be.
            Err(e) => {
                failure.get_or_insert(e);
            }
        }
    }
    Err(failure.unwrap_or_else(|| Error::NotFound("no Exchange server address".into())))
}

/// `https://host/EWS/Exchange.asmx` for an address on the same server
/// that is not the EWS endpoint itself (`https://host/owa/`).
fn standard_endpoint(url: &str) -> Option<String> {
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let mut parsed = ews::Url::parse(&with_scheme).ok()?;
    let path = parsed.path().trim_end_matches('/');
    if path.is_empty() || path.eq_ignore_ascii_case(ews::EWS_PATH) {
        return None;
    }
    parsed.set_path(ews::EWS_PATH);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

/// The settings of an account that signed in with `creds` at `found`.
pub(super) fn account_config(creds: &Credentials, found: &Found) -> AccountConfig {
    AccountConfig {
        auth: AuthMethod::Password,
        accept_invalid_certs: creds.accept_invalid_certs,
        exchange: Some(ExchangeConfig {
            ews_url: found.url.clone(),
            // The e-mail address is the default login name.
            username: if creds.username.eq_ignore_ascii_case(&creds.email) {
                String::new()
            } else {
                creds.username.clone()
            },
            auth: creds.auth,
            version: found.version.map(|v| v.as_str().to_string()),
        }),
        ..AccountConfig::default()
    }
}

impl Engine {
    /// Adds an on-premises Exchange account: finds the server with
    /// Autodiscover unless a URL is given, checks the sign-in by reading the
    /// inbox and starts mail and calendar sync. Adding an Exchange account
    /// again updates its settings and password.
    /// Signs in to the EWS server of changed Exchange settings, so that a
    /// mistake is caught before they replace working ones.
    pub(crate) async fn verify_exchange_settings(
        &self,
        email: &str,
        config: &AccountConfig,
        password: String,
    ) -> Result<()> {
        let exchange = config.exchange.clone().unwrap_or_default();
        let creds = Credentials {
            email: email.to_string(),
            username: Some(exchange.username.trim())
                .filter(|u| !u.is_empty())
                .unwrap_or(email)
                .to_string(),
            password,
            auth: exchange.auth,
            accept_invalid_certs: config.accept_invalid_certs,
        };
        connect(&creds, Some(exchange.ews_url.as_str())).await?;
        Ok(())
    }

    pub async fn add_exchange_account(&self, input: NewExchangeAccount) -> Result<Account> {
        let email = input.email.trim().to_string();
        if !email.contains('@') {
            return Err(Error::Invalid("Enter a valid e-mail address".into()));
        }
        if input.password.is_empty() {
            return Err(Error::Invalid("Enter your password".into()));
        }
        let username = input
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .unwrap_or(&email)
            .to_string();
        let creds = Credentials {
            email: email.clone(),
            username,
            password: input.password.clone(),
            auth: input.auth,
            accept_invalid_certs: input.accept_invalid_certs,
        };
        let found = connect(&creds, input.ews_url.as_deref()).await?;
        let config = account_config(&creds, &found);
        let existing = self
            .db
            .read(mail::list_accounts)?
            .into_iter()
            .find(|a| a.kind == AccountKind::Exchange && a.email.eq_ignore_ascii_case(&email));
        if let Some(existing) = existing {
            let patch = crate::engine::AccountPatch {
                config: Some(AccountConfig {
                    body_sync_days: existing.config.body_sync_days,
                    ..config
                }),
                password: Some(input.password.clone()),
                enabled: Some(true),
                ..Default::default()
            };
            return self.update_account(existing.id, patch).await;
        }
        let name = Some(input.name.trim())
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or(found.display_name)
            .unwrap_or_else(|| email.split('@').next().unwrap_or(&email).to_string());
        let color = self.next_color()?;
        let id = self.db.write(|tx| {
            mail::insert_account(
                tx,
                AccountKind::Exchange,
                "exchange",
                &email,
                &name,
                &color,
                &config,
            )
        })?;
        if let Err(e) = self
            .secrets
            .set(&secrets::password_key(id), &input.password)
            .await
        {
            // Without its password the account cannot work: take it back.
            let _ = self.db.write(|tx| {
                tx.execute("DELETE FROM accounts WHERE id = ?", [id])?;
                Ok(())
            });
            return Err(e);
        }
        self.start_worker(id);
        self.calendar_wake.notify_one();
        self.tasks_wake.notify_one();
        self.emit(EngineEvent::AccountsChanged);
        self.account(id)
    }
}

// ---------------------------------------------------------------------------
// Provider discovery

/// What a mail domain's Autodiscover host says about it, learned without
/// credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExchangeHint {
    /// Nothing suggests Exchange.
    None,
    /// An on-premises Exchange server answers Autodiscover.
    OnPremises,
    /// Autodiscover points at Microsoft 365.
    Microsoft365,
}

/// Host suffixes of Microsoft 365's Autodiscover service.
const MICROSOFT_365_HOSTS: &[&str] = &["outlook.com", "office365.com", "office.com"];

/// Checks `autodiscover.<domain>`: a CNAME into Microsoft 365, or an
/// Exchange server answering the unauthenticated Autodiscover URL (asking
/// for Windows sign-in, or with Exchange's own headers). Sends no
/// credentials.
pub(crate) async fn exchange_hint(domain: &str) -> ExchangeHint {
    let host = format!("autodiscover.{domain}");
    if let Some(target) = cname(&host).await
        && MICROSOFT_365_HOSTS
            .iter()
            .any(|h| target == *h || target.ends_with(&format!(".{h}")))
    {
        return ExchangeHint::Microsoft365;
    }
    if probe_url(&format!("https://{host}/autodiscover/autodiscover.xml")).await {
        ExchangeHint::OnPremises
    } else {
        ExchangeHint::None
    }
}

async fn cname(host: &str) -> Option<String> {
    use hickory_resolver::proto::rr::{RData, RecordType};
    let resolver = hickory_resolver::TokioResolver::builder_tokio()
        .ok()?
        .build()
        .ok()?;
    let lookup = tokio::time::timeout(
        Duration::from_secs(3),
        resolver.lookup(host, RecordType::CNAME),
    )
    .await
    .ok()?
    .ok()?;
    lookup
        .answers()
        .iter()
        .find_map(|record| match &record.data {
            RData::CNAME(target) => Some(
                target
                    .to_string()
                    .trim_end_matches('.')
                    .to_ascii_lowercase(),
            ),
            _ => None,
        })
}

/// Whether an unauthenticated request to an Autodiscover URL is answered by
/// Exchange.
pub(super) async fn probe_url(url: &str) -> bool {
    // Only headers are read and nothing is sent, so a self-signed
    // certificate (common on-premises) does no harm here.
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
        .build()
    else {
        return false;
    };
    match client.get(url).send().await {
        Ok(response) => looks_like_exchange(response.status().as_u16(), response.headers()),
        Err(_) => false,
    }
}

/// Exchange asks for Windows sign-in (NTLM or Negotiate) on Autodiscover
/// and labels its responses with its own headers; hosting control panels
/// that answer Autodiscover for IMAP do neither.
pub(super) fn looks_like_exchange(status: u16, headers: &reqwest::header::HeaderMap) -> bool {
    const EXCHANGE_HEADERS: &[&str] = &[
        "x-feserver",
        "x-beserver",
        "x-owa-version",
        "x-calculatedbetarget",
    ];
    if EXCHANGE_HEADERS.iter().any(|h| headers.contains_key(*h)) {
        return true;
    }
    status == 401
        && headers
            .get_all(reqwest::header::WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| {
                let v = v.trim_start().to_ascii_lowercase();
                v.starts_with("ntlm") || v.starts_with("negotiate")
            })
}
