use revera::config::{ModelRoute, Protocol};
use revera::provider::openai_chat::OpenAiChatClient;
use revera::provider::{ChatMessage, LedgerHandle, ModelClient, ProviderError};
use serde_json::json;
use std::collections::HashMap;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn route(url: &str) -> ModelRoute {
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    ModelRoute {
        protocol: Protocol::OpenaiChat,
        base_url: Some(url.to_string()),
        api_key_env: Some("REVERA_TEST_KEY".into()),
        model: "test-model".into(),
        max_output_tokens: 100,
        temperature: 0.2,
        extra_headers: HashMap::new(),
        script: None,
    }
}

fn client(url: &str) -> OpenAiChatClient {
    OpenAiChatClient::new(route(url), LedgerHandle::new(), 100, 3).unwrap()
}

fn ok_body() -> serde_json::Value {
    json!({
        "choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function",
                "function": {"name": "submit_findings", "arguments": "{\"findings\": [], \"coverage\": \"c\"}"}}]}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2}
    })
}

#[tokio::test]
async fn success_with_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;
    let c = client(&server.uri());
    let r = c.complete(&[ChatMessage::user("hi")], &[]).await.unwrap();
    assert_eq!(r.message.tool_calls[0].name, "submit_findings");
    assert_eq!(r.usage.prompt_tokens, 10);
    assert_eq!(r.model, "test-model");
}

#[tokio::test]
async fn request_body_has_config_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"model": "test-model", "max_tokens": 100}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server.uri());
    c.complete(&[ChatMessage::user("hi")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn retries_on_429_then_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;
    let c = client(&server.uri());
    let r = c.complete(&[ChatMessage::user("hi")], &[]).await.unwrap();
    assert_eq!(r.message.tool_calls[0].name, "submit_findings");
}

#[tokio::test]
async fn server_errors_exhaust_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(4)
        .mount(&server)
        .await;
    let c = client(&server.uri());
    let e = c
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::RetryExhausted(_)), "{e}");
    server.verify().await;
}

#[tokio::test]
async fn max_completion_tokens_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"max_tokens": 100})))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("unsupported parameter: use max_completion_tokens"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"max_completion_tokens": 100})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server.uri());
    c.complete(&[ChatMessage::user("hi")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn other_400_is_fatal_with_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid something else"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server.uri());
    let e = c
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap_err();
    assert!(matches!(e, ProviderError::Http(_)), "{e}");
    assert!(e.to_string().contains("invalid something else"), "{e}");
    server.verify().await;
}

#[tokio::test]
async fn run_budget_blocks_second_attempt_on_retry() {
    // max_requests=1: the first HTTP attempt consumes the only slot; the
    // retry must fail BudgetExhausted without hitting the server again.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(route(&server.uri()), LedgerHandle::new(), 1, 3).unwrap();
    let r = c.complete(&[ChatMessage::user("hi")], &[]).await;
    assert!(matches!(r, Err(ProviderError::BudgetExhausted)), "{r:?}");
    server.verify().await;
}
