use revera::config::{ModelRoute, Protocol};
use revera::pipeline::common::{clamp_budget, parse_findings_checked};
use revera::provider::openai_chat::OpenAiChatClient;
use revera::provider::{ChatMessage, LedgerHandle, ModelClient};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn mock_route(url: &str) -> ModelRoute {
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    ModelRoute {
        protocol: Protocol::OpenaiChat,
        base_url: Some(url.to_string()),
        api_key_env: Some("REVERA_TEST_KEY".into()),
        model: "test-model".into(),
        max_output_tokens: 100,
        temperature: 0.0,
        extra_headers: HashMap::new(),
        session_header: None,
        script: None,
        reasoning: Default::default(),
    }
}

fn ok_response() -> serde_json::Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1234567,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "ok"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 2,
            "total_tokens": 7
        }
    })
}

#[tokio::test]
async fn matrix_rate_limit_429_exhaustion_reports_clear_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_string("Rate limit exceeded"),
        )
        .expect(3) // 1 initial + 2 retries
        .mount(&server)
        .await;

    let route = mock_route(&server.uri());
    let ledger = LedgerHandle::new();
    let client = OpenAiChatClient::new(route, ledger, 10, 2, "scout:test").unwrap();

    let err = client
        .complete(&[ChatMessage::user("analyze diff")], &[])
        .await
        .unwrap_err();

    assert!(err.to_string().contains("429") || err.to_string().contains("Rate limit"));
    server.verify().await;
}

#[tokio::test]
async fn matrix_server_503_unavailable_retried_and_recovered() {
    let server = MockServer::start().await;
    // Fails twice with 503, then succeeds with 200
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_response()))
        .mount(&server)
        .await;

    let route = mock_route(&server.uri());
    let ledger = LedgerHandle::new();
    // Allow up to 3 retries
    let client = OpenAiChatClient::new(route, ledger, 10, 3, "scout:test").unwrap();

    let completion = client
        .complete(&[ChatMessage::user("analyze diff")], &[])
        .await
        .unwrap();

    assert_eq!(completion.message.content.as_deref(), Some("ok"));
}

#[test]
fn matrix_malformed_submission_handled_truthfully() {
    // 1. Completely broken JSON string
    let bad_json = json!("not an object");
    let parsed1 = parse_findings_checked(&bad_json);
    assert!(parsed1.findings.is_empty());
    assert!(parsed1.problem.is_some());

    // 2. Object with malformed items inside findings array
    let malformed_items = json!({
        "findings": [
            { "defect_key": "valid_key", "severity": "high", "file": "src/a.rs", "start_line": 1, "title": "t", "claim": "c", "trigger": "tr", "impact": "i", "introduced_by_change": true },
            { "defect_key": 123 }, // invalid type
            { "bogus": true }      // missing required fields
        ]
    });
    let parsed2 = parse_findings_checked(&malformed_items);
    assert_eq!(parsed2.findings.len(), 1);
    assert_eq!(parsed2.dropped, 2);
    assert!(parsed2.problem.is_some());
}

#[test]
fn matrix_deadline_and_time_budget_clamp() {
    let now = Instant::now();

    // Generous deadline: budget remains intact
    let generous_deadline = now + Duration::from_secs(300);
    let budget1 = clamp_budget(25, 60, generous_deadline);
    assert_eq!(budget1.max_tool_calls, 25);
    assert_eq!(budget1.max_seconds, 60);

    // Tight deadline (5 seconds remaining): max_seconds clamped to remaining time
    let tight_deadline = now + Duration::from_secs(5);
    let budget2 = clamp_budget(25, 60, tight_deadline);
    assert!(budget2.max_seconds <= 5);
}
