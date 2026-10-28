//! End-to-end streaming against a local mock server, for every provider kind.

mod common;

use std::time::Duration;

use common::fixtures::*;
use common::{After, MockServer, Reply, client, client_with, http};
use envelope_ai::{
    AiClient, AiError, AiErrorKind, ChatEvent, ChatMessage, ChatRequest, ClientOptions,
    FinishReason, ProviderConfig, ProviderKind, ReasoningEffort, Usage,
};
use futures::StreamExt;
use serde_json::json;

type Item = Result<ChatEvent, AiError>;

async fn collect(client: &AiClient, request: ChatRequest) -> Vec<Item> {
    tokio::time::timeout(
        Duration::from_secs(10),
        client.chat_stream(request).collect(),
    )
    .await
    .expect("stream finished in time")
}

fn text(items: &[Item]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            Ok(ChatEvent::Delta(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn hello() -> ChatRequest {
    ChatRequest::new(vec![
        ChatMessage::system("You are terse."),
        ChatMessage::user("Say hello."),
    ])
}

#[tokio::test]
async fn openai_compatible_end_to_end() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENAI_STREAM).chunked(5)],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("test-key"),
        "test-model",
    );
    let request = hello().temperature(0.2).max_tokens(100).json(true);
    let items = collect(&client, request).await;

    assert_eq!(text(&items), OPENAI_TEXT);
    assert_eq!(
        items.last(),
        Some(&Ok(ChatEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: Some(Usage {
                input_tokens: Some(12),
                output_tokens: Some(5)
            })
        }))
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.header("authorization"), Some("Bearer test-key"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
    assert!(
        sent.header("user-agent")
            .is_some_and(|ua| ua.starts_with("Envelope/"))
    );
    assert_eq!(
        sent.header("http-referer"),
        None,
        "attribution only for OpenRouter"
    );
    let body = sent.json();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["max_tokens"], 100);
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "Say hello."}
        ])
    );
}

#[tokio::test]
async fn anthropic_end_to_end() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/messages",
        vec![Reply::sse(ANTHROPIC_STREAM).chunked(3)],
    );
    let client = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        Some("test-key"),
        "test-model",
    );
    let request = ChatRequest::new(vec![
        ChatMessage::system("Be kind."),
        ChatMessage::user("Hi"),
        ChatMessage::user("Who are you?"),
    ])
    .temperature(1.5)
    .effort(ReasoningEffort::Low);
    let items = collect(&client, request).await;

    assert_eq!(text(&items), ANTHROPIC_TEXT);
    assert!(matches!(
        items.last(),
        Some(Ok(ChatEvent::Done {
            finish_reason: FinishReason::Stop,
            ..
        }))
    ));
    let sent = &server.requests()[0];
    assert_eq!(sent.header("x-api-key"), Some("test-key"));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(sent.header("authorization"), None);
    let body = sent.json();
    assert_eq!(body["system"], "Be kind.");
    assert_eq!(body["max_tokens"], 8192, "max_tokens is mandatory");
    assert_eq!(body["temperature"], 1.0, "clamped to Anthropic's range");
    assert_eq!(body["output_config"], json!({"effort": "low"}));
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "Hi\n\nWho are you?"}])
    );
}

#[tokio::test]
async fn gemini_end_to_end() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1beta/models/test-model:streamGenerateContent",
        vec![Reply::sse(GEMINI_STREAM).chunked(4)],
    );
    let client = client(
        ProviderKind::Gemini,
        server.url("/v1beta"),
        Some("test-key"),
        "test-model",
    );
    let request = ChatRequest::new(vec![
        ChatMessage::system("Answer in German."),
        ChatMessage::user("Hello"),
        ChatMessage::assistant("Hallo!"),
        ChatMessage::user("How are you?"),
    ])
    .temperature(0.3)
    .max_tokens(256)
    .json(true);
    let items = collect(&client, request).await;

    assert_eq!(text(&items), GEMINI_TEXT);
    assert_eq!(
        items.last(),
        Some(&Ok(ChatEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: Some(Usage {
                input_tokens: Some(9),
                output_tokens: Some(10)
            })
        }))
    );
    let sent = &server.requests()[0];
    assert_eq!(
        sent.path,
        "/v1beta/models/test-model:streamGenerateContent?alt=sse"
    );
    assert_eq!(sent.header("x-goog-api-key"), Some("test-key"));
    let body = sent.json();
    assert_eq!(
        body["systemInstruction"],
        json!({"parts": [{"text": "Answer in German."}]})
    );
    assert_eq!(
        body["contents"],
        json!([
            {"role": "user", "parts": [{"text": "Hello"}]},
            {"role": "model", "parts": [{"text": "Hallo!"}]},
            {"role": "user", "parts": [{"text": "How are you?"}]}
        ])
    );
    assert_eq!(
        body["generationConfig"],
        json!({"temperature": 0.3, "maxOutputTokens": 256, "responseMimeType": "application/json"})
    );
}

#[tokio::test]
async fn openrouter_gets_attribution_headers_and_keepalive_comments_are_ignored() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/api/v1/chat/completions",
        vec![Reply::sse(OPENROUTER_STREAM).chunked(9)],
    );
    // Resolve openrouter.ai to the mock server so host-based detection kicks in.
    let http = reqwest::Client::builder()
        .no_proxy()
        .resolve("openrouter.ai", server.addr)
        .build()
        .expect("http client");
    let base = format!("http://openrouter.ai:{}/api/v1", server.addr.port());
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        base,
        Some("or-key".into()),
        "openrouter/auto",
    );
    let client =
        AiClient::with_http_client(config, http, ClientOptions::default()).expect("client");
    let items = collect(&client, hello().effort(ReasoningEffort::Low)).await;

    assert_eq!(text(&items), "Hello");
    let sent = &server.requests()[0];
    assert_eq!(
        sent.header("http-referer"),
        Some("https://github.com/burakgon/envelope")
    );
    assert_eq!(sent.header("x-title"), Some("Envelope"));
    assert_eq!(sent.header("x-openrouter-title"), Some("Envelope"));
    assert_eq!(sent.header("authorization"), Some("Bearer or-key"));
    let body = sent.json();
    assert_eq!(body["reasoning"], json!({"effort": "low"}));
    assert!(body.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn openrouter_preset_config_resolves_and_parses() {
    let config = ProviderConfig::from_preset("openrouter", Some("k".into())).expect("preset");
    assert_eq!(config.model, "openrouter/auto");
    let client = AiClient::new(config).expect("client");
    assert_eq!(client.base_url(), "https://openrouter.ai/api/v1");
}

#[tokio::test]
async fn http_errors_are_classified() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/a/chat/completions",
        vec![Reply::json(401, OPENAI_401)],
    );
    server.route(
        "POST",
        "/b/chat/completions",
        vec![Reply::json(429, OPENAI_429).header("retry-after", "7")],
    );
    server.route(
        "POST",
        "/c/chat/completions",
        vec![Reply::json(400, OPENAI_CONTEXT_400)],
    );
    server.route(
        "POST",
        "/d/chat/completions",
        vec![Reply::json(404, OPENAI_MODEL_404)],
    );
    server.route(
        "POST",
        "/e/chat/completions",
        vec![Reply::new(502, "text/html", b"<html>bad gateway</html>")],
    );

    let openai = |prefix: &str| {
        client(
            ProviderKind::OpenAiCompatible,
            server.url(prefix),
            Some("test-key"),
            "test-model",
        )
    };

    let items = collect(&openai("/a"), hello()).await;
    assert_eq!(
        items,
        vec![Err(AiError::Unauthorized {
            message: "Incorrect API key provided: test-key. You can find your API key in your account settings.".into()
        })]
    );

    let items = collect(&openai("/b"), hello()).await;
    let [Err(err)] = &items[..] else {
        panic!("expected one error, got {items:?}");
    };
    assert_eq!(err.retry_after(), Some(Duration::from_secs(7)));
    assert!(err.is_retryable());
    assert_eq!(
        err.to_string(),
        "The AI provider's rate limit was reached. Please try again in 7 seconds."
    );

    let kind_of = |items: Vec<Item>| match &items[..] {
        [Err(err)] => err.kind(),
        other => panic!("expected one error, got {other:?}"),
    };
    assert_eq!(
        kind_of(collect(&openai("/c"), hello()).await),
        AiErrorKind::ContextTooLong
    );
    assert_eq!(
        kind_of(collect(&openai("/d"), hello()).await),
        AiErrorKind::ModelNotFound
    );
    assert_eq!(
        kind_of(collect(&openai("/e"), hello()).await),
        AiErrorKind::Overloaded
    );

    // None of these errors is retried with adapted parameters.
    assert_eq!(server.requests().len(), 5);
}

#[tokio::test]
async fn anthropic_and_gemini_errors() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/messages",
        vec![Reply::json(529, ANTHROPIC_529)],
    );
    server.route(
        "POST",
        "/bad/v1/messages",
        vec![Reply::json(401, ANTHROPIC_401)],
    );
    server.route(
        "POST",
        "/v1beta/models/test-model:streamGenerateContent",
        // Gemini really answers errors on the SSE endpoint with this content type.
        vec![
            Reply::new(400, "text/event-stream", GEMINI_INVALID_KEY_400.as_bytes()),
            Reply::json(429, GEMINI_429),
        ],
    );

    let anthropic = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let items = collect(&anthropic, hello()).await;
    assert_eq!(
        items,
        vec![Err(AiError::Overloaded {
            message: "Overloaded".into()
        })]
    );
    let anthropic = client(
        ProviderKind::Anthropic,
        server.url("/bad/v1"),
        Some("k"),
        "test-model",
    );
    let items = collect(&anthropic, hello()).await;
    assert_eq!(
        items,
        vec![Err(AiError::Unauthorized {
            message: "invalid x-api-key".into()
        })]
    );

    let gemini = client(
        ProviderKind::Gemini,
        server.url("/v1beta"),
        Some("k"),
        "test-model",
    );
    let items = collect(&gemini, hello()).await;
    assert!(
        matches!(&items[..], [Err(AiError::Unauthorized { .. })]),
        "{items:?}"
    );
    let items = collect(&gemini, hello()).await;
    let [Err(err)] = &items[..] else {
        panic!("expected one error, got {items:?}");
    };
    assert_eq!(err.kind(), AiErrorKind::RateLimited);
    assert_eq!(err.retry_after(), Some(Duration::from_secs(17)));
}

#[tokio::test]
async fn errors_inside_a_started_stream_are_surfaced() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/messages",
        vec![Reply::sse(ANTHROPIC_ERROR_STREAM).chunked(7)],
    );
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENROUTER_ERROR_STREAM).chunked(7)],
    );

    let anthropic = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let items = collect(&anthropic, hello()).await;
    assert_eq!(items[0], Ok(ChatEvent::Delta("Partial".into())));
    assert!(matches!(
        items.last(),
        Some(Err(AiError::Overloaded { .. }))
    ));

    let openai = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let items = collect(&openai, hello()).await;
    assert_eq!(items.len(), 2);
    assert!(matches!(items.last(), Some(Err(e)) if e.kind() == AiErrorKind::Provider));
}

#[tokio::test]
async fn rejected_optional_parameters_are_adapted_and_remembered() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![
            Reply::json(400, OPENAI_MAX_TOKENS_400),
            Reply::json(400, OPENAI_TEMPERATURE_400),
            Reply::sse(OPENAI_STREAM),
        ],
    );
    let make = || {
        client(
            ProviderKind::OpenAiCompatible,
            server.url("/v1"),
            Some("k"),
            "adaptive-model",
        )
    };
    let request = hello().temperature(0.2).max_tokens(50);
    let items = collect(&make(), request.clone()).await;
    assert_eq!(text(&items), OPENAI_TEXT);

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let first = requests[0].json();
    assert_eq!(first["max_tokens"], 50);
    assert_eq!(first["temperature"], 0.2);
    let second = requests[1].json();
    assert!(second.get("max_tokens").is_none());
    assert_eq!(second["max_completion_tokens"], 50);
    assert_eq!(second["temperature"], 0.2);
    let third = requests[2].json();
    assert_eq!(third["max_completion_tokens"], 50);
    assert!(third.get("temperature").is_none());

    // A new client for the same endpoint and model starts with what was learned.
    let items = collect(&make(), request).await;
    assert_eq!(text(&items), OPENAI_TEXT);
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let fourth = requests[3].json();
    assert_eq!(fourth["max_completion_tokens"], 50);
    assert!(fourth.get("temperature").is_none());
}

#[tokio::test]
async fn unexplained_rejection_retries_once_without_optional_parameters() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![
            Reply::json(422, r#"{"detail": "Invalid request"}"#),
            Reply::sse(OPENAI_STREAM),
        ],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        None,
        "minimal-model",
    );
    let items = collect(&client, hello().json(true).effort(ReasoningEffort::Low)).await;
    assert_eq!(text(&items), OPENAI_TEXT);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("authorization"),
        None,
        "no key, no header"
    );
    let retry = requests[1].json();
    for field in [
        "response_format",
        "reasoning_effort",
        "stream_options",
        "temperature",
    ] {
        assert!(retry.get(field).is_none(), "{field} should be stripped");
    }
}

#[tokio::test]
async fn servers_that_ignore_streaming_still_work() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::json(200, OPENAI_COMPLETION_JSON)],
    );
    server.route(
        "POST",
        "/err/v1/chat/completions",
        vec![Reply::json(
            200,
            r#"{"error": {"code": 402, "message": "Insufficient credits"}}"#,
        )],
    );
    let plain = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let items = collect(&plain, hello()).await;
    assert_eq!(
        items,
        vec![
            Ok(ChatEvent::Delta("Full answer.".into())),
            Ok(ChatEvent::Done {
                finish_reason: FinishReason::Length,
                usage: Some(Usage {
                    input_tokens: Some(3),
                    output_tokens: Some(2)
                })
            })
        ]
    );
    let failing = client(
        ProviderKind::OpenAiCompatible,
        server.url("/err/v1"),
        Some("k"),
        "test-model",
    );
    let items = collect(&failing, hello()).await;
    assert!(
        matches!(&items[..], [Err(AiError::QuotaExceeded { .. })]),
        "{items:?}"
    );
}

#[tokio::test]
async fn dropping_the_stream_cancels_the_response() {
    let server = MockServer::start().await;
    let first_event = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Once upon\"},\"finish_reason\":null}]}\n\n";
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(first_event).then(After::KeepAlive)],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let mut stream = client.chat_stream(hello());
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
    assert_eq!(
        first.ok().flatten(),
        Some(Ok(ChatEvent::Delta("Once upon".into())))
    );
    drop(stream);
    assert!(
        server.wait_for_disconnect(Duration::from_secs(5)).await,
        "the server should see the connection close"
    );
}

#[tokio::test]
async fn dropping_the_stream_before_the_response_cancels_the_request() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENAI_STREAM).head_delay(Duration::from_secs(30))],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let mut stream = client.chat_stream(hello());
    let pending = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
    assert!(pending.is_err(), "no response yet");
    assert_eq!(server.requests().len(), 1, "the request was sent");
    drop(stream);
    assert!(server.wait_for_disconnect(Duration::from_secs(5)).await);
}

#[tokio::test]
async fn a_stalled_stream_times_out() {
    let server = MockServer::start().await;
    let first_event = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n";
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(first_event).then(After::Stall)],
    );
    let options = ClientOptions {
        idle_timeout: Duration::from_millis(300),
        ..ClientOptions::default()
    };
    let client = client_with(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "test-model",
        options,
    );
    let items = collect(&client, hello()).await;
    assert_eq!(
        items,
        vec![Ok(ChatEvent::Delta("Hi".into())), Err(AiError::Timeout)]
    );
}

#[tokio::test]
async fn chat_collects_the_whole_answer() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/messages",
        vec![Reply::sse(ANTHROPIC_STREAM).chunked(64)],
    );
    let client = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let answer = client.chat(hello()).await;
    assert_eq!(answer.as_deref(), Ok(ANTHROPIC_TEXT));
}

#[tokio::test]
async fn request_level_model_override() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENAI_STREAM)],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        None,
        "test-model",
    );
    let _ = collect(&client, hello().model("other-model")).await;
    assert_eq!(server.requests()[0].json()["model"], "other-model");
}

#[tokio::test]
async fn local_validation_errors_send_nothing() {
    let server = MockServer::start().await;
    let no_key = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        None,
        "test-model",
    );
    assert_eq!(
        collect(&no_key, hello()).await,
        vec![Err(AiError::MissingApiKey)]
    );
    let gemini = client(
        ProviderKind::Gemini,
        server.url("/v1beta"),
        Some(" "),
        "test-model",
    );
    assert_eq!(
        collect(&gemini, hello()).await,
        vec![Err(AiError::MissingApiKey)]
    );
    let no_model = client(ProviderKind::OpenAiCompatible, server.url("/v1"), None, " ");
    assert!(matches!(
        &collect(&no_model, hello()).await[..],
        [Err(AiError::InvalidConfig(_))]
    ));
    let no_user = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let only_system = ChatRequest::new(vec![ChatMessage::system("x")]);
    assert!(matches!(
        &collect(&no_user, only_system).await[..],
        [Err(AiError::InvalidRequest(_))]
    ));
    let bad_gemini_model = client(
        ProviderKind::Gemini,
        server.url("/v1beta"),
        Some("k"),
        "a:b?c",
    );
    assert!(matches!(
        &collect(&bad_gemini_model, hello()).await[..],
        [Err(AiError::InvalidConfig(_))]
    ));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn unreachable_server_is_a_network_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        format!("http://127.0.0.1:{port}/v1"),
        None,
        "test-model",
    );
    let client =
        AiClient::with_http_client(config, http(), ClientOptions::default()).expect("client");
    let items = collect(&client, hello()).await;
    let [Err(AiError::Network { message })] = &items[..] else {
        panic!("expected a network error, got {items:?}");
    };
    assert!(message.contains(&format!("127.0.0.1:{port}")), "{message}");
}

#[test]
fn public_futures_and_streams_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    fn assert_shareable<T: Send + Sync + Clone + 'static>() {}
    assert_shareable::<AiClient>();
    assert_shareable::<envelope_ai::AiError>();
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        "http://localhost:1/v1",
        None,
        "m",
    );
    let client =
        AiClient::with_http_client(config, http(), ClientOptions::default()).expect("client");
    assert_send(&client.chat_stream(hello()));
    assert_send(&client.chat(hello()));
    assert_send(&client.list_models());
    assert_send(&client.test_connection());
}

#[tokio::test]
async fn html_answers_point_at_the_base_url() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/chat/completions",
        vec![Reply::new(
            200,
            "text/html; charset=utf-8",
            b"<!doctype html><title>Welcome</title>",
        )],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url(""),
        None,
        "test-model",
    );
    let items = collect(&client, hello()).await;
    let [Err(AiError::InvalidResponse { message })] = &items[..] else {
        panic!("expected an invalid-response error, got {items:?}");
    };
    assert!(message.contains("check the base URL"), "{message}");
}

#[tokio::test]
async fn redirects_are_only_followed_within_the_same_host() {
    if ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return; // The client built by `with_options` honors system proxies.
    }
    let other_host = MockServer::start().await;
    other_host.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENAI_STREAM)],
    );
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENAI_STREAM)],
    );
    server.route(
        "POST",
        "/moved/chat/completions",
        vec![Reply::new(307, "text/plain", b"").header("location", "/v1/chat/completions")],
    );
    let elsewhere = format!(
        "http://localhost:{}/v1/chat/completions",
        other_host.addr.port()
    );
    server.route(
        "POST",
        "/away/chat/completions",
        vec![Reply::new(307, "text/plain", b"").header("location", &elsewhere)],
    );
    let make = |prefix: &str| {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            server.url(prefix),
            Some("secret-key".into()),
            "test-model",
        );
        AiClient::with_options(config, ClientOptions::default()).expect("client")
    };

    let items = collect(&make("/moved"), hello()).await;
    assert_eq!(text(&items), OPENAI_TEXT, "same-host redirect is followed");

    let items = collect(&make("/away"), hello()).await;
    assert!(
        matches!(
            &items[..],
            [Err(AiError::Provider {
                status: Some(307),
                ..
            })]
        ),
        "{items:?}"
    );
    assert!(
        other_host.requests().is_empty(),
        "the key never reaches another host"
    );
}
