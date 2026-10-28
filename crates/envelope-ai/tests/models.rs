//! Model listing and connection tests against a local mock server.

mod common;

use common::fixtures::*;
use common::{MockServer, Reply, client};
use envelope_ai::{AiError, AiErrorKind, ModelInfo, ProviderKind};

fn ids(models: &[ModelInfo]) -> Vec<&str> {
    models.iter().map(|m| m.id.as_str()).collect()
}

#[tokio::test]
async fn openrouter_models_with_pricing_and_modalities() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/api/v1/models",
        vec![Reply::json(200, OPENROUTER_MODELS)],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/api/v1"),
        None,
        "",
    );
    let models = client.list_models().await.expect("models");

    assert_eq!(
        ids(&models),
        vec![
            "vendor/test-model-pro",
            "openrouter/auto",
            "vendor/test-small:free"
        ],
        "image-only models are filtered out"
    );
    assert_eq!(
        models[0],
        ModelInfo {
            id: "vendor/test-model-pro".into(),
            name: "Vendor: Test Model Pro".into(),
            context_length: Some(200_000),
            input_price_per_mtok: Some(3.0),
            output_price_per_mtok: Some(15.0),
        }
    );
    // Routers report "-1" (variable) prices: unknown, not negative.
    assert_eq!(models[1].input_price_per_mtok, None);
    assert_eq!(models[1].output_price_per_mtok, None);
    // Free models cost zero; a null top-level context falls back to the provider's.
    assert_eq!(models[2].input_price_per_mtok, Some(0.0));
    assert_eq!(models[2].context_length, Some(32_768));
}

#[tokio::test]
async fn openai_models_are_filtered_and_deduplicated() {
    let server = MockServer::start().await;
    server.route("GET", "/v1/models", vec![Reply::json(200, OPENAI_MODELS)]);
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("test-key"),
        "",
    );
    let models = client.list_models().await.expect("models");
    assert_eq!(ids(&models), vec!["test-model-a", "test-model-b"]);
    assert_eq!(models[0].name, "test-model-a");
    assert_eq!(models[0].context_length, None);
    let sent = &server.requests()[0];
    assert_eq!(sent.method, "GET");
    assert_eq!(sent.header("authorization"), Some("Bearer test-key"));
}

#[tokio::test]
async fn compatible_providers_metadata_variants() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/groq/v1/models",
        vec![Reply::json(200, GROQ_MODELS)],
    );
    server.route(
        "GET",
        "/mistral/v1/models",
        vec![Reply::json(200, MISTRAL_MODELS)],
    );
    server.route(
        "GET",
        "/together/v1/models",
        vec![Reply::json(200, TOGETHER_MODELS)],
    );
    let list = |prefix: &str| {
        let client = client(
            ProviderKind::OpenAiCompatible,
            server.url(prefix),
            Some("k"),
            "",
        );
        async move { client.list_models().await.expect("models") }
    };

    let groq = list("/groq/v1").await;
    assert_eq!(ids(&groq), vec!["test-fast"]);
    assert_eq!(groq[0].context_length, Some(131_072));

    let mistral = list("/mistral/v1").await;
    assert_eq!(ids(&mistral), vec!["test-chat-latest"]);
    assert_eq!(mistral[0].context_length, Some(128_000));
    assert_eq!(mistral[0].name, "test-chat-2601");

    let together = list("/together/v1").await;
    assert_eq!(
        together,
        vec![ModelInfo {
            id: "vendor/Test-Chat-8B".into(),
            name: "Test Chat 8B".into(),
            context_length: Some(131_072),
            input_price_per_mtok: Some(0.18),
            output_price_per_mtok: Some(0.18),
        }]
    );
}

#[tokio::test]
async fn anthropic_models_follow_pagination() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/v1/models",
        vec![
            Reply::json(200, ANTHROPIC_MODELS_PAGE_1),
            Reply::json(200, ANTHROPIC_MODELS_PAGE_2),
        ],
    );
    let client = client(
        ProviderKind::Anthropic,
        server.url("/v1"),
        Some("test-key"),
        "",
    );
    let models = client.list_models().await.expect("models");
    assert_eq!(
        ids(&models),
        vec!["test-model-large", "test-model-small", "test-model-legacy"]
    );
    assert_eq!(models[0].name, "Test Model Large");
    assert_eq!(models[0].context_length, Some(1_000_000));
    assert_eq!(models[2].context_length, None);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/v1/models?limit=1000");
    assert_eq!(
        requests[1].path,
        "/v1/models?limit=1000&after_id=test-model-small"
    );
    assert_eq!(requests[0].header("x-api-key"), Some("test-key"));
    assert_eq!(requests[0].header("anthropic-version"), Some("2023-06-01"));
}

#[tokio::test]
async fn gemini_models_follow_page_tokens_and_filter_methods() {
    let server = MockServer::start().await;
    server.route(
        "GET",
        "/v1beta/models",
        vec![
            Reply::json(200, GEMINI_MODELS_PAGE_1),
            Reply::json(200, GEMINI_MODELS_PAGE_2),
        ],
    );
    let client = client(
        ProviderKind::Gemini,
        server.url("/v1beta"),
        Some("test-key"),
        "",
    );
    let models = client.list_models().await.expect("models");
    assert_eq!(
        models,
        vec![
            ModelInfo {
                id: "test-flash".into(),
                name: "Test Flash".into(),
                context_length: Some(1_048_576),
                input_price_per_mtok: None,
                output_price_per_mtok: None,
            },
            ModelInfo {
                id: "test-pro".into(),
                name: "Test Pro".into(),
                context_length: Some(2_097_152),
                input_price_per_mtok: None,
                output_price_per_mtok: None,
            },
        ]
    );
    let requests = server.requests();
    assert_eq!(requests[0].path, "/v1beta/models?pageSize=1000");
    assert_eq!(
        requests[1].path,
        "/v1beta/models?pageSize=1000&pageToken=page-2"
    );
    assert_eq!(requests[0].header("x-goog-api-key"), Some("test-key"));
}

#[tokio::test]
async fn ollama_falls_back_to_api_tags() {
    let server = MockServer::start().await;
    server.route("GET", "/api/tags", vec![Reply::json(200, OLLAMA_TAGS)]);
    let client = client(ProviderKind::OpenAiCompatible, server.url("/v1"), None, "");
    let models = client.list_models().await.expect("models");
    assert_eq!(ids(&models), vec!["test-llm:latest"]);
    let paths: Vec<String> = server.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, vec!["/v1/models", "/api/tags"]);
}

#[tokio::test]
async fn providers_without_a_model_list() {
    let server = MockServer::start().await;
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url(""),
        Some("k"),
        "",
    );
    assert_eq!(
        client.list_models().await,
        Err(AiError::ModelListUnavailable)
    );
}

#[tokio::test]
async fn model_list_errors_are_classified() {
    let server = MockServer::start().await;
    server.route("GET", "/v1/models", vec![Reply::json(401, OPENAI_401)]);
    server.route(
        "GET",
        "/bad/v1/models",
        vec![Reply::json(200, "{\"unexpected\": true}")],
    );
    let client = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "",
    );
    let err = client.list_models().await.expect_err("401");
    assert_eq!(err.kind(), AiErrorKind::Unauthorized);
    let client = client_for(&server, "/bad/v1");
    let err = client.list_models().await.expect_err("bad shape");
    assert_eq!(err.kind(), AiErrorKind::InvalidResponse);
    let no_key = common::client(ProviderKind::Gemini, server.url("/v1beta"), None, "");
    assert_eq!(no_key.list_models().await, Err(AiError::MissingApiKey));
}

fn client_for(server: &MockServer, prefix: &str) -> envelope_ai::AiClient {
    client(
        ProviderKind::OpenAiCompatible,
        server.url(prefix),
        Some("k"),
        "",
    )
}

#[tokio::test]
async fn test_connection_with_and_without_a_model() {
    let server = MockServer::start().await;
    server.route(
        "POST",
        "/v1/chat/completions",
        vec![Reply::sse(OPENAI_STREAM)],
    );
    server.route("GET", "/v1/models", vec![Reply::json(200, OPENAI_MODELS)]);

    let with_model = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "test-model",
    );
    let report = with_model
        .test_connection()
        .await
        .expect("connection works");
    assert_eq!(report.model.as_deref(), Some("test-model"));
    assert_eq!(report.models_listed, None);
    let probe = server.requests()[0].json();
    assert_eq!(probe["max_tokens"], 32);
    assert_eq!(probe["reasoning_effort"], "low");

    let without_model = client(
        ProviderKind::OpenAiCompatible,
        server.url("/v1"),
        Some("k"),
        "",
    );
    let report = without_model
        .test_connection()
        .await
        .expect("connection works");
    assert_eq!(report.model, None);
    assert_eq!(report.models_listed, Some(2));

    let broken = client(
        ProviderKind::OpenAiCompatible,
        server.url("/nothing/v1"),
        Some("k"),
        "m",
    );
    let err = broken.test_connection().await.expect_err("404");
    assert_eq!(err.kind(), AiErrorKind::Provider);
    assert!(err.to_string().contains("base URL"), "{err}");
}
