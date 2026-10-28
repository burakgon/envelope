//! Provider stream and model-list fixtures, modeled on the providers' documented
//! formats. Model ids are fake.

/// OpenAI Chat Completions stream with a usage chunk (`include_usage`).
pub const OPENAI_STREAM: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Merhaba\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" dünya — ğüşıöç 🚀\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":5,\"total_tokens\":17}}\n\n",
    "data: [DONE]\n\n",
);
pub const OPENAI_TEXT: &str = "Merhaba dünya — ğüşıöç 🚀!";

/// OpenRouter: keep-alive comments, reasoning deltas (ignored), the extra usage
/// chunk that repeats the finish reason, CRLF line endings.
pub const OPENROUTER_STREAM: &str = concat!(
    ": OPENROUTER PROCESSING\r\n\r\n",
    ": OPENROUTER PROCESSING\r\n\r\n",
    "data: {\"id\":\"gen-1\",\"provider\":\"Test\",\"model\":\"test/model\",\"object\":\"chat.completion.chunk\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\",\"reasoning\":\"Let me think.\"},\"finish_reason\":null}]}\r\n\r\n",
    "data: {\"id\":\"gen-1\",\"provider\":\"Test\",\"model\":\"test/model\",\"object\":\"chat.completion.chunk\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"finish_reason\":null}]}\r\n\r\n",
    ": OPENROUTER PROCESSING\r\n\r\n",
    "data: {\"id\":\"gen-1\",\"provider\":\"Test\",\"model\":\"test/model\",\"object\":\"chat.completion.chunk\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"lo\"},\"finish_reason\":null}]}\r\n\r\n",
    "data: {\"id\":\"gen-1\",\"provider\":\"Test\",\"model\":\"test/model\",\"object\":\"chat.completion.chunk\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":\"stop\",\"native_finish_reason\":\"stop\"}]}\r\n\r\n",
    "data: {\"id\":\"gen-1\",\"provider\":\"Test\",\"model\":\"test/model\",\"object\":\"chat.completion.chunk\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":\"stop\",\"native_finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12,\"cost\":0.0001}}\r\n\r\n",
    "data: [DONE]\r\n\r\n",
);

/// OpenRouter error after the stream started (HTTP status already 200).
pub const OPENROUTER_ERROR_STREAM: &str = concat!(
    ": OPENROUTER PROCESSING\n\n",
    "data: {\"id\":\"gen-2\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test/model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Partial\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"gen-2\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"test/model\",\"provider\":\"Test\",\"error\":{\"code\":\"server_error\",\"message\":\"Provider disconnected unexpectedly\"},\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
);

/// Anthropic Messages stream: thinking block (ignored), ping, an unknown future
/// event type, cumulative usage, cache tokens.
pub const ANTHROPIC_STREAM: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test-model\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":25,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":5,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Considering the request.\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"c2lnbmF0dXJl\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: ping\n",
    "data: {\"type\": \"ping\"}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\", Ada! Görüşürüz.\"}}\n\n",
    "event: future_event_type\n",
    "data: {\"type\":\"future_event_type\",\"detail\":\"ignored\"}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":15}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);
pub const ANTHROPIC_TEXT: &str = "Hello, Ada! Görüşürüz.";

/// Anthropic error event in the middle of a stream.
pub const ANTHROPIC_ERROR_STREAM: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test-model\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Partial\"}}\n\n",
    "event: error\n",
    "data: {\"type\": \"error\", \"error\": {\"type\": \"overloaded_error\", \"message\": \"Overloaded\"}}\n\n",
);

/// Gemini `streamGenerateContent?alt=sse`: CRLF separators, a thought part
/// (ignored), several parts per chunk, usage with thought tokens.
pub const GEMINI_STREAM: &str = concat!(
    "data: {\"candidates\": [{\"content\": {\"parts\": [{\"text\": \"Thinking about it\",\"thought\": true}],\"role\": \"model\"},\"index\": 0}],\"usageMetadata\": {\"promptTokenCount\": 9,\"totalTokenCount\": 9},\"modelVersion\": \"test-model\",\"responseId\": \"r1\"}\r\n\r\n",
    "data: {\"candidates\": [{\"content\": {\"parts\": [{\"text\": \"Hallo\"}],\"role\": \"model\"},\"index\": 0}],\"usageMetadata\": {\"promptTokenCount\": 9,\"candidatesTokenCount\": 1,\"totalTokenCount\": 10},\"modelVersion\": \"test-model\",\"responseId\": \"r1\"}\r\n\r\n",
    "data: {\"candidates\": [{\"content\": {\"parts\": [{\"text\": \" Welt, \"},{\"text\": \"wie geht's? 日本語\"}],\"role\": \"model\"},\"finishReason\": \"STOP\",\"index\": 0}],\"usageMetadata\": {\"promptTokenCount\": 9,\"candidatesTokenCount\": 6,\"thoughtsTokenCount\": 4,\"totalTokenCount\": 19},\"modelVersion\": \"test-model\",\"responseId\": \"r1\"}\r\n\r\n",
);
pub const GEMINI_TEXT: &str = "Hallo Welt, wie geht's? 日本語";

/// Gemini rejected the prompt itself.
pub const GEMINI_BLOCKED_STREAM: &str = "data: {\"promptFeedback\": {\"blockReason\": \"PROHIBITED_CONTENT\"},\"usageMetadata\": {\"promptTokenCount\": 4,\"totalTokenCount\": 4},\"modelVersion\": \"test-model\"}\r\n\r\n";

/// Gemini stopped for safety before producing text.
pub const GEMINI_SAFETY_STREAM: &str = "data: {\"candidates\": [{\"content\": {\"parts\": [],\"role\": \"model\"},\"finishReason\": \"SAFETY\",\"index\": 0}],\"modelVersion\": \"test-model\"}\r\n\r\n";

pub const OPENAI_401: &str = r#"{
    "error": {
        "message": "Incorrect API key provided: test-key. You can find your API key in your account settings.",
        "type": "invalid_request_error",
        "param": null,
        "code": "invalid_api_key"
    }
}"#;

pub const OPENAI_429: &str = r#"{"error":{"message":"Rate limit reached for requests","type":"requests","param":null,"code":"rate_limit_exceeded"}}"#;

pub const OPENAI_CONTEXT_400: &str = r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your messages resulted in 9000 tokens.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#;

pub const OPENAI_MODEL_404: &str = r#"{"error":{"message":"The model `test-model` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#;

pub const OPENAI_MAX_TOKENS_400: &str = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#;

pub const OPENAI_TEMPERATURE_400: &str = r#"{"error":{"message":"Unsupported value: 'temperature' does not support 0.2 with this model. Only the default (1) value is supported.","type":"invalid_request_error","param":"temperature","code":"unsupported_value"}}"#;

pub const ANTHROPIC_529: &str = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_test"}"#;

pub const ANTHROPIC_401: &str = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_test"}"#;

pub const GEMINI_INVALID_KEY_400: &str = r#"{
  "error": {
    "code": 400,
    "message": "API key not valid. Please pass a valid API key.",
    "status": "INVALID_ARGUMENT",
    "details": [
      {"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "API_KEY_INVALID", "domain": "googleapis.com"}
    ]
  }
}"#;

pub const GEMINI_429: &str = r#"{
  "error": {
    "code": 429,
    "message": "You exceeded your current request rate. Please retry later.",
    "status": "RESOURCE_EXHAUSTED",
    "details": [
      {"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": []},
      {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "17s"}
    ]
  }
}"#;

/// A complete, non-streaming completion (servers that ignore `stream: true`).
pub const OPENAI_COMPLETION_JSON: &str = r#"{"id":"chatcmpl-9","object":"chat.completion","created":1,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"Full answer."},"finish_reason":"length"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;

pub const OPENROUTER_MODELS: &str = r#"{
  "data": [
    {
      "id": "vendor/test-model-pro",
      "canonical_slug": "vendor/test-model-pro-20260101",
      "name": "Vendor: Test Model Pro",
      "created": 1767225600,
      "context_length": 200000,
      "architecture": {"modality": "text+image->text", "input_modalities": ["text", "image"], "output_modalities": ["text"], "tokenizer": "Other"},
      "pricing": {"prompt": "0.000003", "completion": "0.000015", "request": "0", "image": "0.0048"},
      "top_provider": {"context_length": 200000, "max_completion_tokens": 64000, "is_moderated": false},
      "supported_parameters": ["max_tokens", "temperature", "response_format"]
    },
    {
      "id": "openrouter/auto",
      "name": "Auto Router",
      "context_length": 2000000,
      "architecture": {"input_modalities": ["text"], "output_modalities": ["text", "image"], "tokenizer": "Router"},
      "pricing": {"prompt": "-1", "completion": "-1"},
      "top_provider": {"context_length": null, "max_completion_tokens": null, "is_moderated": false}
    },
    {
      "id": "vendor/test-painter",
      "name": "Vendor: Test Painter",
      "context_length": 4096,
      "architecture": {"input_modalities": ["text"], "output_modalities": ["image"], "tokenizer": "Other"},
      "pricing": {"prompt": "0.000001", "completion": "0.00004"},
      "top_provider": {"context_length": 4096, "is_moderated": false}
    },
    {
      "id": "vendor/test-small:free",
      "name": "Vendor: Test Small (free)",
      "context_length": null,
      "architecture": {"input_modalities": ["text"], "output_modalities": ["text"], "tokenizer": "Other"},
      "pricing": {"prompt": "0", "completion": "0"},
      "top_provider": {"context_length": 32768, "is_moderated": false}
    }
  ],
  "total_count": 4,
  "links": {"next": null}
}"#;

pub const OPENAI_MODELS: &str = r#"{
  "object": "list",
  "data": [
    {"id": "test-model-a", "object": "model", "created": 1, "owned_by": "system"},
    {"id": "text-embedding-test", "object": "model", "created": 1, "owned_by": "system"},
    {"id": "whisper-test", "object": "model", "created": 1, "owned_by": "system"},
    {"id": "test-model-b", "object": "model", "created": 2, "owned_by": "system"},
    {"id": "tts-test-hd", "object": "model", "created": 1, "owned_by": "system"},
    {"id": "test-model-a", "object": "model", "created": 1, "owned_by": "system"}
  ]
}"#;

/// Groq-style list: `context_window` and inactive models.
pub const GROQ_MODELS: &str = r#"{"object":"list","data":[
  {"id":"test-fast","object":"model","created":1,"owned_by":"Test","active":true,"context_window":131072,"public_apps":null,"max_completion_tokens":32768},
  {"id":"test-retired","object":"model","created":1,"owned_by":"Test","active":false,"context_window":8192}
]}"#;

/// Mistral-style list: `max_context_length`, `capabilities.completion_chat`.
pub const MISTRAL_MODELS: &str = r#"{"object":"list","data":[
  {"id":"test-chat-latest","object":"model","created":1,"owned_by":"test","capabilities":{"completion_chat":true,"completion_fim":false,"function_calling":true,"vision":false},"name":"test-chat-2601","description":"Chat","max_context_length":128000,"aliases":["test-chat-2601"]},
  {"id":"test-ocr","object":"model","created":1,"owned_by":"test","capabilities":{"completion_chat":false},"max_context_length":32000}
]}"#;

/// Together-style list: a bare array, `type`, pricing per million tokens.
pub const TOGETHER_MODELS: &str = r#"[
  {"id":"vendor/Test-Chat-8B","object":"model","created":1,"type":"chat","display_name":"Test Chat 8B","organization":"Vendor","context_length":131072,"pricing":{"hourly":0,"input":0.18,"output":0.18,"base":0,"finetune":0}},
  {"id":"vendor/test-retriever","object":"model","created":1,"type":"embedding","display_name":"Test Retriever","context_length":512,"pricing":{"input":0.008,"output":0}}
]"#;

pub const ANTHROPIC_MODELS_PAGE_1: &str = r#"{
  "data": [
    {"type": "model", "id": "test-model-large", "display_name": "Test Model Large", "created_at": "2026-09-01T00:00:00Z", "max_input_tokens": 1000000, "max_tokens": 128000, "lifecycle": "active"},
    {"type": "model", "id": "test-model-small", "display_name": "Test Model Small", "created_at": "2026-06-01T00:00:00Z", "max_input_tokens": 200000, "max_tokens": 64000, "lifecycle": "active"}
  ],
  "has_more": true,
  "first_id": "test-model-large",
  "last_id": "test-model-small"
}"#;

pub const ANTHROPIC_MODELS_PAGE_2: &str = r#"{
  "data": [
    {"type": "model", "id": "test-model-legacy", "display_name": "Test Model Legacy", "created_at": "2025-01-01T00:00:00Z", "lifecycle": "deprecated"}
  ],
  "has_more": false,
  "first_id": "test-model-legacy",
  "last_id": "test-model-legacy"
}"#;

pub const GEMINI_MODELS_PAGE_1: &str = r#"{
  "models": [
    {"name": "models/test-flash", "baseModelId": "test-flash", "version": "001", "displayName": "Test Flash", "inputTokenLimit": 1048576, "outputTokenLimit": 65536, "supportedGenerationMethods": ["generateContent", "countTokens", "createCachedContent"], "thinking": true},
    {"name": "models/test-embedding-001", "displayName": "Test Embedding", "inputTokenLimit": 2048, "supportedGenerationMethods": ["embedContent"]}
  ],
  "nextPageToken": "page-2"
}"#;

pub const GEMINI_MODELS_PAGE_2: &str = r#"{
  "models": [
    {"name": "models/test-pro", "displayName": "Test Pro", "inputTokenLimit": 2097152, "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/test-answer", "displayName": "Test Answer", "supportedGenerationMethods": ["generateAnswer"]}
  ]
}"#;

pub const OLLAMA_TAGS: &str = r#"{
  "models": [
    {"name": "test-llm:latest", "model": "test-llm:latest", "modified_at": "2026-10-03T23:34:03.409490317-07:00", "size": 9608350245, "digest": "c6eb", "details": {"format": "gguf", "family": "test", "families": ["test"], "parameter_size": "8.0B", "quantization_level": "Q4_K_M"}},
    {"name": "nomic-embed-text:latest", "model": "nomic-embed-text:latest", "details": {"family": "nomic-bert"}}
  ]
}"#;
