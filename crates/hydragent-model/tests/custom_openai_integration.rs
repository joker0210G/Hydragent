// crates/hydragent-model/tests/custom_openai_integration.rs
//
// Integration tests for the generic OpenAI-compatible provider. These spin up
// a `wiremock` server that mimics the wire format used by OpenAI, OpenRouter,
// Together, Groq, vLLM, etc. — i.e. `POST /v1/chat/completions` returning
// `text/event-stream` chunks of `data: {json}\n\n` lines.

use std::time::Duration;

use hydragent_model::custom_openai::{CustomOpenAIClient, CustomProviderConfig};
use hydragent_model::openrouter::{ChatMessage, LLMRequest};
use hydragent_model::ModelProvider;
use tokio::sync::mpsc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_config(base_url: String) -> CustomProviderConfig {
    // The mock server only knows about /v1/chat/completions. The provider
    // concatenates `<base_url>/chat/completions`, so we point base_url at
    // `<mock>/v1`.
    let base_with_v1 = if base_url.ends_with("/v1") {
        base_url
    } else {
        format!("{}/v1", base_url.trim_end_matches('/'))
    };
    CustomProviderConfig {
        base_url: base_with_v1,
        api_key: "sk-test-key".to_string(),
        default_model: "test-model".to_string(),
        provider_label: "wiremock-test".to_string(),
        timeout: Duration::from_secs(5),
        max_retries: 0, // tests assert single-shot behaviour
    }
}

fn sse_response(chunks: &[&str]) -> ResponseTemplate {
    let body = chunks.join("\n\n") + "\n\n";
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn make_request() -> LLMRequest {
    LLMRequest {
        model: "test-model".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream: true,
        max_tokens: Some(64),
    }
}

#[tokio::test]
async fn custom_provider_streams_openai_chunks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test-key"))
        .respond_with(sse_response(&[
            r#"data: {"choices":[{"delta":{"content":"He"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"llo"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":" world"}}]}"#,
            r"data: [DONE]",
        ]))
        .mount(&server)
        .await;

    let client = CustomOpenAIClient::new(sample_config(server.uri()));
    let (tx, mut rx) = mpsc::channel(16);
    let content = client.chat_stream(&make_request(), tx).await.expect("stream");
    assert_eq!(content, "Hello world");

    // Every streamed token should also have been sent to the channel.
    let mut collected = String::new();
    while let Ok(t) = rx.try_recv() {
        collected.push_str(&t);
    }
    assert_eq!(collected, "Hello world");
}

#[tokio::test]
async fn custom_provider_sends_model_and_messages_in_body() {
    use wiremock::matchers::body_partial_json;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({
            "model": "test-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
        })))
        .respond_with(sse_response(&[r"data: [DONE]"]))
        .expect(1)
        .mount(&server)
        .await;

    let client = CustomOpenAIClient::new(sample_config(server.uri()));
    let (tx, _rx) = mpsc::channel(4);
    let _ = client.chat_stream(&make_request(), tx).await.unwrap();
}

#[tokio::test]
async fn custom_provider_falls_back_to_default_model_when_request_model_empty() {
    use wiremock::matchers::body_partial_json;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({
            "model": "gpt-4o-mini",
        })))
        .respond_with(sse_response(&[
            r#"data: {"choices":[{"delta":{"content":"ok"}}]}"#,
            r"data: [DONE]",
        ]))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = sample_config(server.uri());
    cfg.default_model = "gpt-4o-mini".to_string();

    let client = CustomOpenAIClient::new(cfg);
    let mut req = make_request();
    req.model = String::new(); // empty -> use default

    let (tx, _rx) = mpsc::channel(4);
    let content = client.chat_stream(&req, tx).await.unwrap();
    assert_eq!(content, "ok");
}

#[tokio::test]
async fn custom_provider_returns_429_as_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&server)
        .await;

    let client = CustomOpenAIClient::new(sample_config(server.uri()));
    let (tx, _rx) = mpsc::channel(4);
    let err = client.chat_stream(&make_request(), tx).await.unwrap_err();
    assert!(err.to_string().contains("429"), "unexpected error: {err}");
}

#[tokio::test]
async fn custom_provider_handles_inline_api_level_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_response(&[
            r#"data: {"error":{"message":"out of credits"}}"#,
            r"data: [DONE]",
        ]))
        .mount(&server)
        .await;

    let client = CustomOpenAIClient::new(sample_config(server.uri()));
    let (tx, _rx) = mpsc::channel(4);
    let err = client.chat_stream(&make_request(), tx).await.unwrap_err();
    assert!(err.to_string().contains("out of credits"), "got: {err}");
}

#[tokio::test]
async fn custom_provider_is_unavailable_when_key_missing() {
    let cfg = CustomProviderConfig {
        base_url: "https://api.example.com/v1".to_string(),
        api_key: "".to_string(),
        default_model: "x".to_string(),
        provider_label: "test".to_string(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
    };
    let client = CustomOpenAIClient::new(cfg);
    assert!(!client.is_available());
    assert_eq!(client.provider_name(), "test");
}
