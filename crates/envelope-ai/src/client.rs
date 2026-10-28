//! The provider-agnostic AI client.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::stream::{self, BoxStream, StreamExt};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue,
    USER_AGENT as USER_AGENT_HEADER,
};
use serde::Serialize;

use crate::config::{ProviderConfig, ProviderKind};
use crate::decode::StreamDecoder;
use crate::error::{AiError, Endpoint, ErrorDetails, classify};
use crate::providers::{self, BuiltRequest, ERROR_BODY_LIMIT, anthropic, gemini, openai};
use crate::quirks::{self, MAX_ADAPTATIONS, Quirks};
use crate::types::{ChatEvent, ChatMessage, ChatRequest, ModelInfo, ReasoningEffort};

const USER_AGENT: &str = concat!("Envelope/", env!("CARGO_PKG_VERSION"));

/// Attribution headers OpenRouter asks apps to send.
const OPENROUTER_REFERER: &str = "https://github.com/burakgon/envelope";
const OPENROUTER_TITLE: &str = "Envelope";

/// Largest complete (non-streamed) JSON answer accepted from a server that
/// ignored the streaming flag.
const JSON_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// How long to wait for an error body before giving up on reading it.
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// After the final event, the rest of the body is drained for at most this long
/// so the connection can be reused for the next request.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const DRAIN_LIMIT: usize = 64 * 1024;

/// Redirects followed within one host by the HTTP client created here.
const MAX_REDIRECTS: usize = 5;

/// Token budget of the connection-test probe.
const PROBE_MAX_TOKENS: u32 = 32;

/// Timeouts used by [`AiClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientOptions {
    /// Maximum time to establish a TCP/TLS connection (only applied to the HTTP
    /// client created by [`AiClient::new`] / [`AiClient::with_options`]).
    pub connect_timeout: Duration,
    /// Maximum silence while streaming: until the response starts, and between
    /// two chunks. Generous because reasoning models may think for a while.
    pub idle_timeout: Duration,
    /// Total time limit for non-streaming calls such as listing models.
    pub request_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(180),
            request_timeout: Duration::from_secs(60),
        }
    }
}

/// The result of [`AiClient::test_connection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectionReport {
    /// Time until the provider started answering.
    pub latency: Duration,
    /// The model that answered the probe (when a model is configured).
    pub model: Option<String>,
    /// Number of models listed (when no model is configured yet).
    pub models_listed: Option<usize>,
}

/// Which well-known service an OpenAI-compatible endpoint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    OpenAi,
    OpenRouter,
    Other,
}

/// Shared, immutable client state.
pub(crate) struct Inner {
    pub(crate) http: reqwest::Client,
    pub(crate) config: ProviderConfig,
    /// Normalized API root without a trailing slash.
    pub(crate) base_url: String,
    /// `host[:port]`, for error messages.
    pub(crate) host: String,
    /// Authentication, attribution and extra headers (secrets marked sensitive).
    pub(crate) headers: HeaderMap,
    pub(crate) flavor: Flavor,
    pub(crate) options: ClientOptions,
}

/// A client for one configured provider and model.
///
/// Cheap to clone; clones share the HTTP connection pool. Create one per
/// provider configuration and reuse it.
#[derive(Clone)]
pub struct AiClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for AiClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AiClient")
            .field("kind", &self.inner.config.kind)
            .field("base_url", &self.inner.base_url)
            .field("model", &self.inner.config.model)
            .finish_non_exhaustive()
    }
}

impl AiClient {
    /// Creates a client with default timeouts.
    ///
    /// # Errors
    ///
    /// [`AiError::InvalidConfig`] when the base URL or a header is malformed.
    pub fn new(config: ProviderConfig) -> Result<Self, AiError> {
        Self::with_options(config, ClientOptions::default())
    }

    /// Creates a client with custom timeouts.
    ///
    /// # Errors
    ///
    /// [`AiError::InvalidConfig`] when the base URL or a header is malformed or
    /// the HTTP client cannot be initialized.
    pub fn with_options(config: ProviderConfig, options: ClientOptions) -> Result<Self, AiError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(options.connect_timeout)
            .redirect(same_host_redirects())
            .build()
            .map_err(|err| {
                AiError::InvalidConfig(format!("could not initialize the HTTP client ({err})"))
            })?;
        Self::with_http_client(config, http, options)
    }

    /// Creates a client that uses an existing `reqwest` client (for custom
    /// proxies, certificates or connection pools).
    ///
    /// # Errors
    ///
    /// [`AiError::InvalidConfig`] when the base URL or a header is malformed.
    pub fn with_http_client(
        config: ProviderConfig,
        http: reqwest::Client,
        options: ClientOptions,
    ) -> Result<Self, AiError> {
        let (base_url, host) = normalize_base_url(config.kind, &config.base_url)?;
        let flavor = flavor_of(&host);
        let headers = build_headers(&config, flavor)?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                config,
                base_url,
                host,
                headers,
                flavor,
                options,
            }),
        })
    }

    /// The configuration this client was created with.
    pub fn config(&self) -> &ProviderConfig {
        &self.inner.config
    }

    /// The normalized API root requests are sent to.
    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    /// Streams a chat completion.
    ///
    /// The stream yields [`ChatEvent::Delta`]s followed by exactly one
    /// [`ChatEvent::Done`], or ends early with a single `Err`. Nothing is sent
    /// until the stream is first polled, and dropping the stream at any point
    /// aborts the HTTP request (which stops generation and billing at providers
    /// that support cancellation).
    ///
    /// When a server rejects an optional parameter (e.g. `temperature` on a
    /// reasoning model, `max_tokens` vs `max_completion_tokens`, JSON mode), the
    /// request is transparently retried without it and the adaptation is
    /// remembered for this endpoint and model.
    pub fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> BoxStream<'static, Result<ChatEvent, AiError>> {
        let state = StreamState::Connecting {
            inner: Arc::clone(&self.inner),
            request,
        };
        stream::unfold(state, next_event).boxed()
    }

    /// Runs a chat completion and returns the whole answer text.
    ///
    /// # Errors
    ///
    /// Any [`AiError`] produced while connecting or streaming.
    pub async fn chat(&self, request: ChatRequest) -> Result<String, AiError> {
        let mut events = self.chat_stream(request);
        let mut text = String::new();
        // Run to the end of the stream (not just `Done`) so the connection is
        // drained and returned to the pool.
        while let Some(event) = events.next().await {
            if let ChatEvent::Delta(delta) = event? {
                text.push_str(&delta);
            }
        }
        Ok(text)
    }

    /// Lists the models the provider offers for chat, in the provider's order.
    ///
    /// OpenRouter and Together include pricing; most providers include context
    /// lengths. Embedding, speech and image models are filtered out. For
    /// OpenAI-compatible servers without `/models`, Ollama's `/api/tags` is tried.
    ///
    /// # Errors
    ///
    /// [`AiError::ModelListUnavailable`] when the provider has no model list,
    /// otherwise any HTTP or parsing error.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let inner = self.inner.as_ref();
        check_credentials(inner)?;
        match inner.config.kind {
            ProviderKind::OpenAiCompatible => openai::list_models(inner).await,
            ProviderKind::Anthropic => anthropic::list_models(inner).await,
            ProviderKind::Gemini => gemini::list_models(inner).await,
        }
    }

    /// Verifies that the provider is reachable and the credentials work.
    ///
    /// With a model configured, a tiny chat request is sent and the stream is
    /// dropped as soon as the model starts answering (this costs a handful of
    /// tokens). Without a model, the model list is fetched instead.
    ///
    /// # Errors
    ///
    /// The [`AiError`] that prevented the probe from succeeding.
    pub async fn test_connection(&self) -> Result<ConnectionReport, AiError> {
        let started = Instant::now();
        let model = self.inner.config.model.trim();
        if model.is_empty() {
            let models = self.list_models().await?;
            return Ok(ConnectionReport {
                latency: started.elapsed(),
                model: None,
                models_listed: Some(models.len()),
            });
        }
        let probe = ChatRequest::new(vec![ChatMessage::user("Reply with the single word: OK")])
            .max_tokens(PROBE_MAX_TOKENS)
            .effort(ReasoningEffort::Low);
        let mut events = self.chat_stream(probe);
        match events.next().await {
            Some(Err(err)) => Err(err),
            _ => Ok(ConnectionReport {
                latency: started.elapsed(),
                model: Some(model.to_owned()),
                models_listed: None,
            }),
        }
    }
}

/// States of a chat stream. Everything lives inside the stream value, so
/// dropping the stream drops the in-flight request or response body.
enum StreamState {
    Connecting {
        inner: Arc<Inner>,
        request: ChatRequest,
    },
    Streaming(Box<ActiveStream>),
    Replaying(VecDeque<Result<ChatEvent, AiError>>),
    Finished,
}

/// An open SSE response being decoded.
struct ActiveStream {
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    decoder: StreamDecoder,
    pending: VecDeque<Result<ChatEvent, AiError>>,
    idle_timeout: Duration,
    host: String,
}

async fn next_event(mut state: StreamState) -> Option<(Result<ChatEvent, AiError>, StreamState)> {
    loop {
        state = match state {
            StreamState::Finished => return None,
            StreamState::Replaying(mut items) => {
                let item = items.pop_front()?;
                return Some((item, StreamState::Replaying(items)));
            }
            StreamState::Connecting { inner, request } => match open(&inner, &request).await {
                Ok(Opened::Streaming { response, model }) => {
                    StreamState::Streaming(Box::new(ActiveStream {
                        body: response.bytes_stream().boxed(),
                        decoder: StreamDecoder::with_model(inner.config.kind, &model),
                        pending: VecDeque::new(),
                        idle_timeout: inner.options.idle_timeout,
                        host: inner.host.clone(),
                    }))
                }
                Ok(Opened::Complete(items)) => StreamState::Replaying(items.into()),
                Err(err) => return Some((Err(err), StreamState::Finished)),
            },
            StreamState::Streaming(mut active) => {
                if let Some(item) = active.pending.pop_front() {
                    return Some((item, StreamState::Streaming(active)));
                }
                if active.decoder.is_finished() {
                    drain(active.body).await;
                    return None;
                }
                match tokio::time::timeout(active.idle_timeout, active.body.next()).await {
                    Err(_) => return Some((Err(AiError::Timeout), StreamState::Finished)),
                    Ok(Some(Ok(chunk))) => {
                        let events = active.decoder.feed(&chunk);
                        active.pending.extend(events);
                    }
                    Ok(Some(Err(err))) => {
                        let err = AiError::from_transport(err, &active.host);
                        return Some((Err(err), StreamState::Finished));
                    }
                    Ok(None) => {
                        let events = active.decoder.finish();
                        active.pending.extend(events);
                    }
                }
                StreamState::Streaming(active)
            }
        };
    }
}

/// Reads what is left of a finished stream (usually just the end of the chunked
/// encoding) so the connection can go back to the pool.
async fn drain(mut body: BoxStream<'static, reqwest::Result<Bytes>>) {
    let read_rest = async {
        let mut read = 0;
        while let Some(Ok(chunk)) = body.next().await {
            read += chunk.len();
            if read > DRAIN_LIMIT {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, read_rest).await;
}

enum Opened {
    /// An SSE response to decode incrementally.
    Streaming {
        response: reqwest::Response,
        model: String,
    },
    /// A server that ignored `stream: true` answered with complete JSON.
    Complete(Vec<Result<ChatEvent, AiError>>),
}

/// Sends the chat request, adapting optional parameters on 400/422 rejections.
async fn open(inner: &Inner, request: &ChatRequest) -> Result<Opened, AiError> {
    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(inner.config.model.trim())
        .to_owned();
    if model.is_empty() {
        return Err(AiError::InvalidConfig("no model is selected".to_owned()));
    }
    check_credentials(inner)?;
    let kind = inner.config.kind;
    let cache_key = quirks::cache_key(kind, &inner.base_url, &model);
    let learned = quirks::load(&cache_key);
    let mut current = learned;
    tracing::debug!(
        provider = kind.display_name(),
        host = %inner.host,
        model = %model,
        "opening chat stream"
    );
    for attempt in 0..=MAX_ADAPTATIONS {
        let built = build_chat(inner, request, &model, &current)?;
        let send = inner
            .http
            .post(&built.url)
            .headers(inner.headers.clone())
            .header(ACCEPT, "text/event-stream")
            .json(&built.body)
            .send();
        let response = match tokio::time::timeout(inner.options.idle_timeout, send).await {
            Err(_) => return Err(AiError::Timeout),
            Ok(Err(err)) => return Err(AiError::from_transport(err, &inner.host)),
            Ok(Ok(response)) => response,
        };
        let status = response.status();
        if status.is_success() {
            if current != learned {
                quirks::store(&cache_key, current);
            }
            return accept(inner, response, model).await;
        }
        let headers = response.headers().clone();
        let body = tokio::time::timeout(
            ERROR_BODY_TIMEOUT,
            providers::read_limited(response, ERROR_BODY_LIMIT),
        )
        .await
        .map(|(body, _)| body)
        .unwrap_or_default();
        let details =
            ErrorDetails::from_http(status.as_u16(), &headers, &String::from_utf8_lossy(&body));
        let error = classify(details.clone(), &model, Endpoint::Chat);
        if attempt < MAX_ADAPTATIONS
            && matches!(error, AiError::Provider { .. })
            && quirks::adapt(&mut current, kind, &details, &built.sent)
        {
            tracing::info!(
                status = status.as_u16(),
                "provider rejected an optional parameter; retrying without it"
            );
            continue;
        }
        tracing::debug!(status = status.as_u16(), kind = ?error.kind(), "chat request failed");
        return Err(error);
    }
    Err(AiError::invalid_response(
        "the provider rejected every variant of the request",
    ))
}

fn build_chat(
    inner: &Inner,
    request: &ChatRequest,
    model: &str,
    quirks: &Quirks,
) -> Result<BuiltRequest, AiError> {
    match inner.config.kind {
        ProviderKind::OpenAiCompatible => openai::build_chat(inner, request, model, quirks),
        ProviderKind::Anthropic => anthropic::build_chat(inner, request, model, quirks),
        ProviderKind::Gemini => gemini::build_chat(inner, request, model, quirks),
    }
}

/// Decides how to read a successful response.
async fn accept(
    inner: &Inner,
    response: reqwest::Response,
    model: String,
) -> Result<Opened, AiError> {
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if content_type.starts_with("text/html") {
        return Err(AiError::invalid_response(format!(
            "{} answered with a web page instead of an API response; check the base URL",
            inner.host
        )));
    }
    if !content_type.starts_with("application/json") {
        return Ok(Opened::Streaming { response, model });
    }
    let read = providers::read_limited(response, JSON_BODY_LIMIT);
    let (body, complete) = tokio::time::timeout(inner.options.idle_timeout, read)
        .await
        .map_err(|_| AiError::Timeout)?;
    if !complete {
        return Err(AiError::Network {
            message: format!(
                "the response from {} was interrupted or too large",
                inner.host
            ),
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(|err| {
        AiError::invalid_response(format!("the response is not valid JSON ({err})"))
    })?;
    let mut decoder = StreamDecoder::with_model(inner.config.kind, &model);
    Ok(Opened::Complete(decoder.decode_json(&value)))
}

/// Follows redirects only within the same host: API keys travel in custom
/// headers (`x-api-key`, `x-goog-api-key`) that HTTP clients do not strip when a
/// redirect leads to another host.
fn same_host_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_host = attempt
            .previous()
            .first()
            .is_some_and(|first| first.host_str() == attempt.url().host_str());
        if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if same_host {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Anthropic and Gemini always need a key; fail fast instead of sending a
/// request that can only be rejected.
fn check_credentials(inner: &Inner) -> Result<(), AiError> {
    let needs_key = matches!(
        inner.config.kind,
        ProviderKind::Anthropic | ProviderKind::Gemini
    );
    let has_auth_header = inner.config.extra_headers.keys().any(|name| {
        let name = name.trim().to_ascii_lowercase();
        ["authorization", "x-api-key", "x-goog-api-key", "api-key"].contains(&name.as_str())
    });
    if needs_key && inner.config.key().is_none() && !has_auth_header {
        return Err(AiError::MissingApiKey);
    }
    Ok(())
}

/// Validates and normalizes the base URL. Returns `(base_url, host[:port])`.
fn normalize_base_url(kind: ProviderKind, raw: &str) -> Result<(String, String), AiError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AiError::InvalidConfig("the base URL is empty".to_owned()));
    }
    let url = reqwest::Url::parse(trimmed).map_err(|err| {
        AiError::InvalidConfig(format!(
            "\u{201c}{trimmed}\u{201d} is not a valid URL ({err})"
        ))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AiError::InvalidConfig(
            "the base URL must start with http:// or https://".to_owned(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AiError::InvalidConfig(
            "the base URL must not contain a query string or fragment".to_owned(),
        ));
    }
    let host = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => {
            return Err(AiError::InvalidConfig(
                "the base URL has no host name".to_owned(),
            ));
        }
    };
    let mut base = trimmed.to_owned();
    // Accept endpoint URLs pasted from documentation.
    for suffix in ["/chat/completions", "/completions", "/messages", "/models"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.trim_end_matches('/').to_owned();
            break;
        }
    }
    // A bare host gets the protocol's standard API path.
    if url.path().trim_matches('/').is_empty() {
        match kind {
            ProviderKind::Anthropic => base.push_str("/v1"),
            ProviderKind::Gemini => base.push_str("/v1beta"),
            ProviderKind::OpenAiCompatible => {}
        }
    }
    Ok((base, host))
}

fn flavor_of(host: &str) -> Flavor {
    let name = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    if name == "api.openai.com" {
        Flavor::OpenAi
    } else if name == "openrouter.ai" || name.ends_with(".openrouter.ai") {
        Flavor::OpenRouter
    } else {
        Flavor::Other
    }
}

fn secret_header(value: &str) -> Result<HeaderValue, AiError> {
    let mut header = HeaderValue::from_str(value).map_err(|_| {
        AiError::InvalidConfig("the API key contains characters that are not allowed".to_owned())
    })?;
    header.set_sensitive(true);
    Ok(header)
}

fn build_headers(config: &ProviderConfig, flavor: Flavor) -> Result<HeaderMap, AiError> {
    let mut headers = HeaderMap::new();
    // Sent per request so it is present even with an app-supplied HTTP client.
    headers.insert(USER_AGENT_HEADER, HeaderValue::from_static(USER_AGENT));
    let key = config.key();
    match config.kind {
        ProviderKind::OpenAiCompatible => {
            if let Some(key) = key {
                headers.insert(AUTHORIZATION, secret_header(&format!("Bearer {key}"))?);
            }
        }
        ProviderKind::Anthropic => {
            if let Some(key) = key {
                headers.insert(HeaderName::from_static("x-api-key"), secret_header(key)?);
            }
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static(anthropic::API_VERSION),
            );
        }
        ProviderKind::Gemini => {
            if let Some(key) = key {
                headers.insert(
                    HeaderName::from_static("x-goog-api-key"),
                    secret_header(key)?,
                );
            }
        }
    }
    if flavor == Flavor::OpenRouter {
        headers.insert(
            HeaderName::from_static("http-referer"),
            HeaderValue::from_static(OPENROUTER_REFERER),
        );
        headers.insert(
            HeaderName::from_static("x-title"),
            HeaderValue::from_static(OPENROUTER_TITLE),
        );
        headers.insert(
            HeaderName::from_static("x-openrouter-title"),
            HeaderValue::from_static(OPENROUTER_TITLE),
        );
    }
    for (name, value) in &config.extra_headers {
        let header_name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|_| {
            AiError::InvalidConfig(format!(
                "\u{201c}{name}\u{201d} is not a valid HTTP header name"
            ))
        })?;
        let mut header_value = HeaderValue::from_str(value.trim()).map_err(|_| {
            AiError::InvalidConfig(format!(
                "the value of header \u{201c}{name}\u{201d} contains characters that are not allowed"
            ))
        })?;
        header_value.set_sensitive(true);
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalization() {
        let ok = |kind, raw: &str| normalize_base_url(kind, raw).map(|(base, _)| base);
        assert_eq!(
            ok(
                ProviderKind::OpenAiCompatible,
                " https://api.example.com/v1/ "
            ),
            Ok("https://api.example.com/v1".to_owned())
        );
        assert_eq!(
            ok(
                ProviderKind::OpenAiCompatible,
                "http://localhost:11434/v1/chat/completions"
            ),
            Ok("http://localhost:11434/v1".to_owned())
        );
        assert_eq!(
            ok(ProviderKind::Anthropic, "https://api.anthropic.com"),
            Ok("https://api.anthropic.com/v1".to_owned())
        );
        assert_eq!(
            ok(
                ProviderKind::Gemini,
                "https://generativelanguage.googleapis.com/"
            ),
            Ok("https://generativelanguage.googleapis.com/v1beta".to_owned())
        );
        for bad in [
            "",
            "localhost:11434",
            "ftp://x.example",
            "https://x.example/v1?a=b",
        ] {
            assert!(
                matches!(
                    normalize_base_url(ProviderKind::OpenAiCompatible, bad),
                    Err(AiError::InvalidConfig(_))
                ),
                "{bad}"
            );
        }
        let (_, host) =
            normalize_base_url(ProviderKind::OpenAiCompatible, "http://127.0.0.1:8080/v1")
                .unwrap_or_default();
        assert_eq!(host, "127.0.0.1:8080");
    }

    #[test]
    fn flavors() {
        assert_eq!(flavor_of("api.openai.com"), Flavor::OpenAi);
        assert_eq!(flavor_of("openrouter.ai"), Flavor::OpenRouter);
        assert_eq!(flavor_of("openrouter.ai:8443"), Flavor::OpenRouter);
        assert_eq!(flavor_of("localhost:11434"), Flavor::Other);
    }

    #[test]
    fn invalid_key_characters_are_config_errors() {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            "https://api.example.com/v1",
            Some("bad\nkey".into()),
            "test-model",
        );
        assert!(matches!(
            build_headers(&config, Flavor::Other),
            Err(AiError::InvalidConfig(_))
        ));
    }

    #[test]
    fn keys_are_trimmed_and_sensitive() {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            "https://api.example.com/v1",
            Some("  sk-test \n".into()),
            "test-model",
        );
        let headers = build_headers(&config, Flavor::Other).unwrap_or_default();
        let auth = headers.get(AUTHORIZATION);
        assert_eq!(auth.and_then(|v| v.to_str().ok()), Some("Bearer sk-test"));
        assert!(auth.is_some_and(HeaderValue::is_sensitive));
    }
}
