//! OAuth 2.0 for desktop apps: authorization code + PKCE with a loopback
//! redirect (RFC 8252), token refresh and storage.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::{Error, Result};
use crate::secrets::{Secrets, oauth_key};

/// An OAuth identity provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    Google,
    Microsoft,
}

/// Client registration used for a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Microsoft tenant (`common`, `organizations`, `consumers` or a tenant id).
    pub tenant: Option<String>,
}

impl Provider {
    fn auth_url(&self, cfg: &ClientConfig) -> String {
        match self {
            Provider::Google => "https://accounts.google.com/o/oauth2/v2/auth".into(),
            Provider::Microsoft => format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
                cfg.tenant.as_deref().unwrap_or("common")
            ),
        }
    }

    fn token_url(&self, cfg: &ClientConfig) -> String {
        match self {
            Provider::Google => "https://oauth2.googleapis.com/token".into(),
            Provider::Microsoft => format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                cfg.tenant.as_deref().unwrap_or("common")
            ),
        }
    }

    /// Scopes requested for mail, calendar and contacts.
    pub fn scopes(&self) -> &'static str {
        match self {
            Provider::Google => {
                "https://mail.google.com/ https://www.googleapis.com/auth/calendar \
                 https://www.googleapis.com/auth/contacts.readonly \
                 https://www.googleapis.com/auth/contacts.other.readonly openid email profile"
            }
            Provider::Microsoft => {
                "offline_access openid email profile User.Read Mail.ReadWrite Mail.Send \
                 Calendars.ReadWrite Contacts.Read People.Read MailboxSettings.ReadWrite"
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }

    /// Built-in client registration, from build-time environment variables, which
    /// can be overridden at runtime through the environment or settings.
    pub fn default_client(&self) -> ClientConfig {
        fn pick(runtime: &str, build: Option<&'static str>) -> Option<String> {
            std::env::var(runtime)
                .ok()
                .or_else(|| build.map(str::to_string))
                .filter(|v| !v.trim().is_empty())
        }
        match self {
            Provider::Google => ClientConfig {
                client_id: pick(
                    "ENVELOPE_GOOGLE_CLIENT_ID",
                    option_env!("ENVELOPE_GOOGLE_CLIENT_ID"),
                )
                .unwrap_or_default(),
                client_secret: pick(
                    "ENVELOPE_GOOGLE_CLIENT_SECRET",
                    option_env!("ENVELOPE_GOOGLE_CLIENT_SECRET"),
                ),
                tenant: None,
            },
            Provider::Microsoft => ClientConfig {
                client_id: pick(
                    "ENVELOPE_MICROSOFT_CLIENT_ID",
                    option_env!("ENVELOPE_MICROSOFT_CLIENT_ID"),
                )
                .unwrap_or_default(),
                client_secret: None,
                tenant: pick(
                    "ENVELOPE_MICROSOFT_TENANT",
                    option_env!("ENVELOPE_MICROSOFT_TENANT"),
                ),
            },
        }
    }
}

/// Stored tokens for an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSet {
    pub provider: Provider,
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
    pub scope: Option<String>,
    #[serde(default)]
    pub client: ClientConfig,
}

/// Result of an interactive sign-in.
#[derive(Debug, Clone)]
pub struct Authorized {
    pub tokens: TokenSet,
    pub email: String,
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn random_token(len: usize) -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..len).map(|_| rng.random()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

const DONE_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Envelope</title>
<style>body{margin:0;height:100vh;display:grid;place-items:center;font:16px/1.5 system-ui,sans-serif;background:#0f1117;color:#e7e9ee}
.card{text-align:center;padding:48px 56px;border-radius:20px;background:#171a22;box-shadow:0 20px 60px rgba(0,0,0,.4)}
.check{width:56px;height:56px;border-radius:50%;margin:0 auto 20px;display:grid;place-items:center;background:linear-gradient(135deg,#6d5efc,#b05cf6);font-size:28px}
h1{font-size:22px;margin:0 0 8px}p{margin:0;color:#9aa0ad}</style></head>
<body><div class="card"><div class="check">✓</div><h1>You're signed in</h1><p>You can close this tab and return to Envelope.</p></div></body></html>"#;

const FAIL_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Envelope</title>
<style>body{margin:0;height:100vh;display:grid;place-items:center;font:16px/1.5 system-ui,sans-serif;background:#0f1117;color:#e7e9ee}
.card{text-align:center;padding:48px 56px;border-radius:20px;background:#171a22}h1{font-size:22px;margin:0 0 8px}p{margin:0;color:#9aa0ad}</style></head>
<body><div class="card"><h1>Sign-in was not completed</h1><p>Return to Envelope and try again.</p></div></body></html>"#;

/// Runs the interactive authorization flow. `open` must open the URL in the
/// user's browser.
pub async fn authorize(
    provider: Provider,
    client: ClientConfig,
    login_hint: Option<&str>,
    open: impl FnOnce(String) -> Result<()>,
) -> Result<Authorized> {
    if client.client_id.is_empty() {
        return Err(Error::Auth(format!(
            "No {} OAuth client is configured. See docs/OAUTH.md to set one up, or use an app password.",
            provider.name()
        )));
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect = match provider {
        Provider::Google => format!("http://127.0.0.1:{port}"),
        Provider::Microsoft => format!("http://localhost:{port}"),
    };
    // Microsoft redirects to "localhost", which browsers may resolve to ::1.
    let listener6 = if provider == Provider::Microsoft {
        TcpListener::bind(("::1", port)).await.ok()
    } else {
        None
    };

    let verifier = random_token(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let mut url =
        url::Url::parse(&provider.auth_url(&client)).map_err(|e| Error::Other(e.to_string()))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("response_type", "code")
            .append_pair("scope", provider.scopes())
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        match provider {
            Provider::Google => {
                q.append_pair("access_type", "offline")
                    .append_pair("prompt", "consent");
            }
            Provider::Microsoft => {
                q.append_pair("prompt", "select_account");
            }
        }
        if let Some(hint) = login_hint.filter(|h| !h.is_empty()) {
            q.append_pair("login_hint", hint);
        }
    }
    open(url.to_string())?;

    let code = tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let accepted = match &listener6 {
                Some(l6) => tokio::select! {
                    r = listener.accept() => r,
                    r = l6.accept() => r,
                },
                None => listener.accept().await,
            };
            let (mut sock, _) = accepted?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await?;
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let parsed = url::Url::parse(&format!("http://localhost{path}")).ok();
            let params: HashMap<String, String> = parsed
                .map(|u| u.query_pairs().into_owned().collect())
                .unwrap_or_default();
            if params.is_empty() {
                // Favicon or other noise.
                let _ = sock
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }
            let ok = params.get("state") == Some(&state) && params.contains_key("code");
            let page = if ok { DONE_PAGE } else { FAIL_PAGE };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
            if let Some(err) = params.get("error") {
                let desc = params.get("error_description").cloned().unwrap_or_default();
                return Err(Error::Auth(format!("{err}: {desc}")));
            }
            if !ok {
                return Err(Error::Auth("invalid OAuth response (state mismatch)".into()));
            }
            return Ok(params["code"].clone());
        }
    })
    .await
    .map_err(|_| Error::Auth("sign-in timed out".into()))??;

    let mut form = vec![
        ("client_id", client.client_id.clone()),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code".to_string()),
        ("redirect_uri", redirect),
    ];
    if let Some(secret) = &client.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let resp = token_request(&provider.token_url(&client), &form).await?;
    let claims = resp
        .id_token
        .as_deref()
        .and_then(jwt_claims)
        .unwrap_or_default();
    let email = claims
        .get("email")
        .or_else(|| claims.get("preferred_username"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let name = claims
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let tokens = TokenSet {
        provider,
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        expires_at: chrono::Utc::now().timestamp() + resp.expires_in.unwrap_or(3600),
        scope: resp.scope,
        client,
    };
    let email = match email {
        Some(e) => e,
        None => fetch_email(&tokens).await?,
    };
    Ok(Authorized {
        tokens,
        email,
        name,
    })
}

async fn fetch_email(tokens: &TokenSet) -> Result<String> {
    let url = match tokens.provider {
        Provider::Google => "https://openidconnect.googleapis.com/v1/userinfo",
        Provider::Microsoft => "https://graph.microsoft.com/v1.0/me",
    };
    let v: serde_json::Value = http()
        .get(url)
        .bearer_auth(&tokens.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    v.get("email")
        .or_else(|| v.get("mail"))
        .or_else(|| v.get("userPrincipalName"))
        .and_then(|e| e.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::Auth("could not determine the account e-mail address".into()))
}

fn jwt_claims(token: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn http() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

async fn token_request(url: &str, form: &[(&str, String)]) -> Result<TokenResponse> {
    let resp = http().post(url).form(form).send().await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        let msg = serde_json::from_slice::<ErrorResponse>(&body)
            .map(|e| format!("{}: {}", e.error, e.error_description.unwrap_or_default()))
            .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
        // An unavailable identity provider is not a rejected sign-in.
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::Network(format!("{status}: {msg}")));
        }
        return Err(Error::Auth(msg));
    }
    Ok(serde_json::from_slice(&body)?)
}

/// Hands out valid access tokens, refreshing (once, even under concurrency)
/// when they are about to expire.
pub struct TokenManager {
    secrets: Arc<Secrets>,
    locks: parking_lot::Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl TokenManager {
    pub fn new(secrets: Arc<Secrets>) -> TokenManager {
        TokenManager {
            secrets,
            locks: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub async fn store(&self, account_id: i64, tokens: &TokenSet) -> Result<()> {
        self.secrets
            .set(&oauth_key(account_id), &serde_json::to_string(tokens)?)
            .await
    }

    async fn load(&self, account_id: i64) -> Result<TokenSet> {
        let raw = self
            .secrets
            .get(&oauth_key(account_id))
            .await?
            .ok_or_else(|| Error::Auth("not signed in".into()))?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Returns a valid access token, refreshing it when needed. With `force`,
    /// refreshes even if the cached token looks valid (after a 401).
    pub async fn access_token(&self, account_id: i64, force: bool) -> Result<String> {
        let lock = self
            .locks
            .lock()
            .entry(account_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        let mut tokens = self.load(account_id).await?;
        let now = chrono::Utc::now().timestamp();
        if !force && tokens.expires_at - 120 > now {
            return Ok(tokens.access_token);
        }
        let refresh = tokens
            .refresh_token
            .clone()
            .ok_or_else(|| Error::Auth("session expired, please sign in again".into()))?;
        let client = if tokens.client.client_id.is_empty() {
            tokens.provider.default_client()
        } else {
            tokens.client.clone()
        };
        let mut form = vec![
            ("client_id", client.client_id.clone()),
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh),
        ];
        if let Some(secret) = &client.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        if tokens.provider == Provider::Microsoft {
            form.push(("scope", tokens.provider.scopes().to_string()));
        }
        let resp = token_request(&tokens.provider.token_url(&client), &form)
            .await
            .map_err(|e| match e {
                Error::Auth(m) if m.contains("invalid_grant") => {
                    Error::Auth("access was revoked or expired, please sign in again".into())
                }
                other => other,
            })?;
        tokens.access_token = resp.access_token;
        if let Some(r) = resp.refresh_token {
            tokens.refresh_token = Some(r);
        }
        tokens.expires_at = now + resp.expires_in.unwrap_or(3600);
        if resp.scope.is_some() {
            tokens.scope = resp.scope;
        }
        self.store(account_id, &tokens).await?;
        Ok(tokens.access_token)
    }
}
