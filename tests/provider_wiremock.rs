use chrono::{Duration as ChronoDuration, Utc};
use revera::config::{ModelRoute, Protocol};
use revera::provider::openai_chat::OpenAiChatClient;
use revera::provider::{ChatMessage, LedgerHandle, ModelClient, ProviderError};
use serde_json::json;
use std::collections::HashMap;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn retry_after_supports_delta_dates_and_caps() {
    use revera::provider::http::HttpTransport;

    let now = Utc::now();
    assert_eq!(
        HttpTransport::retry_delay(Some("3"), 1, now),
        std::time::Duration::from_secs(3)
    );
    let date = (now + ChronoDuration::seconds(10)).to_rfc2822();
    let date_delay = HttpTransport::retry_delay(Some(&date), 1, now);
    assert!((9..=10).contains(&date_delay.as_secs()));
    assert_eq!(
        HttpTransport::retry_delay(Some("abc"), 1, now),
        std::time::Duration::from_millis(500)
    );
    assert_eq!(
        HttpTransport::retry_delay(Some("1e9"), 1, now),
        std::time::Duration::from_secs(60)
    );
    assert_eq!(
        HttpTransport::retry_delay(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 1, now),
        std::time::Duration::ZERO
    );
}

// REVERA_TEST_KEY is set by .cargo/config.toml, so run these through cargo test
fn route(url: &str) -> ModelRoute {
    ModelRoute {
        protocol: Protocol::OpenaiChat,
        base_url: Some(url.to_string()),
        api_key_env: Some("REVERA_TEST_KEY".into()),
        model: "test-model".into(),
        max_output_tokens: 100,
        cache: false,
        temperature: 0.2,
        extra_headers: HashMap::new(),
        session_header: None,
        script: None,
        reasoning: Default::default(),
        fallback: vec![],
    }
}

fn client(url: &str) -> OpenAiChatClient {
    OpenAiChatClient::new(route(url), LedgerHandle::new(), 100, 3, "test").unwrap()
}

#[tokio::test]
async fn request_errors_and_ledger_hide_base_url_query_values() {
    // Port 1 is privileged and unbound, so the connection is refused at once.
    let secret = "s3cr3t-value";
    let ledger = LedgerHandle::new();
    let c = OpenAiChatClient::new(
        route(&format!("http://127.0.0.1:1/v1?key={secret}")),
        ledger.clone(),
        1,
        0,
        "test",
    )
    .unwrap();
    let err = c
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap_err();
    assert!(!err.to_string().contains(secret), "{err}");

    let entries = &ledger.0.lock().unwrap().entries;
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains(secret),
        "{:?}",
        entries[0].error
    );
}

#[tokio::test]
async fn provider_error_bodies_echoing_the_url_hide_query_values() {
    let server = MockServer::start().await;
    let echoed = format!(
        "unsupported parameter in {}/v1/chat/completions?key=echoed-credential-value",
        server.uri()
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string(echoed))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = OpenAiChatClient::new(
        route(&format!("{}/v1?key=echoed-credential-value", server.uri())),
        ledger.clone(),
        2,
        0,
        "test",
    )
    .unwrap();
    let err = c
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap_err();
    assert!(
        !err.to_string().contains("echoed-credential-value"),
        "{err}"
    );
    assert!(err.to_string().contains("HTTP 400"), "{err}");
    server.verify().await;
    let entries = &ledger.0.lock().unwrap().entries;
    assert!(entries.iter().all(|e| {
        !e.error
            .as_deref()
            .unwrap_or_default()
            .contains("echoed-credential-value")
    }));
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
async fn truncated_empty_chat_reply_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"role": "assistant", "content": null}
            }],
            "usage": {}
        })))
        .mount(&server)
        .await;
    let err = client(&server.uri())
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("output truncated"));
}

#[tokio::test]
async fn truncated_chat_reply_with_content_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"role": "assistant", "content": "partial answer"}
            }],
            "usage": {}
        })))
        .mount(&server)
        .await;
    let result = client(&server.uri())
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap();
    assert_eq!(result.message.content.as_deref(), Some("partial answer"));
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
    let c = OpenAiChatClient::new(route(&server.uri()), LedgerHandle::new(), 1, 3, "test").unwrap();
    let r = c.complete(&[ChatMessage::user("hi")], &[]).await;
    assert!(matches!(r, Err(ProviderError::BudgetExhausted)), "{r:?}");
    server.verify().await;
}

// ================= M6: new protocol adapters =================

use revera::provider::anthropic::AnthropicAdapter;
use revera::provider::gemini::GeminiAdapter;
use revera::provider::http::{AttemptState, HttpClient, HttpTransport, ProtocolAdapter};
use revera::provider::openai_chat::OpenAiChatAdapter;
use revera::provider::openai_responses::{OpenAiResponsesAdapter, OpenAiResponsesClient};
use revera::provider::{Role, ToolCall};

fn route_for(url: &str, proto: Protocol, model: &str) -> ModelRoute {
    ModelRoute {
        protocol: proto,
        base_url: Some(url.to_string()),
        api_key_env: Some("REVERA_TEST_KEY".into()),
        model: model.into(),
        max_output_tokens: 100,
        cache: false,
        temperature: 0.2,
        extra_headers: HashMap::new(),
        session_header: None,
        script: None,
        reasoning: Default::default(),
        fallback: vec![],
    }
}

fn fallback_route(primary: &MockServer, backup: &MockServer) -> ModelRoute {
    let mut route = route_for(&primary.uri(), Protocol::OpenaiChat, "primary");
    route
        .fallback
        .push(route_for(&backup.uri(), Protocol::OpenaiChat, "backup"));
    route
}

fn routed_client(
    route: &ModelRoute,
    ledger: &LedgerHandle,
    role: &str,
    budget: u32,
    retries: u32,
) -> std::sync::Arc<dyn ModelClient> {
    revera::pipeline::make_client(
        route,
        role,
        None,
        ledger.clone(),
        budget,
        retries,
        "submit_findings",
    )
    .unwrap()
}

#[test]
fn fallback_doctor_reports_each_key_without_requiring_every_route() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    );
    let config = r#"
models:
  investigator:
    protocol: openai-chat
    base_url: https://primary.example/v1
    model: primary
    api_key_env: REVERA_TEST_UNSET_FALLBACK_DOCTOR
    fallback:
      - protocol: anthropic
        model: backup
        api_key_env: REVERA_TEST_KEY
      - protocol: gemini
        model: unavailable
        api_key_env: REVERA_TEST_UNSET_FALLBACK_DOCTOR
vera: {enabled: false}
"#;
    revera::fsutil::write_atomic_in(dir.path(), "revera.yaml", config.as_bytes()).unwrap();
    for available in [true, false] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_revera"));
        command
            .arg("doctor")
            .current_dir(dir.path())
            .env_remove("REVERA_TEST_UNSET_FALLBACK_DOCTOR");
        if !available {
            command.env_remove("REVERA_TEST_KEY");
        }
        let output = command.output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.success(), available, "{text}");
        assert!(
            text.contains("validator: inherited from investigator"),
            "{text}"
        );
        assert!(text.contains("investigator.fallback[0]"), "{text}");
        assert!(text.contains("investigator.fallback[1]"), "{text}");
        if available {
            assert!(
                text.contains("UNAVAILABLE (skipped; another route has a key)"),
                "{text}"
            );
            assert!(text.contains("key env REVERA_TEST_KEY set"), "{text}");
        }
    }
}

#[tokio::test]
async fn fallback_tries_alternatives_in_order_and_sticks_only_after_success() {
    let primary = MockServer::start().await;
    let middle = MockServer::start().await;
    let last = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&middle)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(2)
        .mount(&last)
        .await;
    let mut route = fallback_route(&primary, &middle);
    route
        .fallback
        .push(route_for(&last.uri(), Protocol::OpenaiChat, "last"));
    let ledger = LedgerHandle::new();
    let c = routed_client(&route, &ledger, "investigator", 10, 0);
    for _ in 0..2 {
        assert_eq!(
            c.complete(&[ChatMessage::user("hi")], &[])
                .await
                .unwrap()
                .model,
            "last"
        );
    }
    primary.verify().await;
    middle.verify().await;
    last.verify().await;
    let run = ledger.0.lock().unwrap();
    assert_eq!(run.fallback_events.len(), 2);
    assert_eq!(run.fallback_events[0].from_model, "primary");
    assert_eq!(run.fallback_events[0].to_model, "backup");
    assert_eq!(run.fallback_events[1].from_model, "backup");
    assert_eq!(run.fallback_events[1].to_model, "last");
    assert_eq!(
        revera::report::footer_routes(&run),
        ["investigator=primary→last@medium"]
    );
}

#[tokio::test]
async fn fallback_waits_for_same_route_request_reshaping() {
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"max_tokens":100})))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("unsupported max_tokens: use max_completion_tokens"),
        )
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"max_completion_tokens":100})))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&backup)
        .await;
    let ledger = LedgerHandle::new();
    routed_client(
        &fallback_route(&primary, &backup),
        &ledger,
        "investigator",
        10,
        0,
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    primary.verify().await;
    backup.verify().await;
    assert_eq!(ledger.request_count(), 3);
    assert_eq!(ledger.0.lock().unwrap().fallback_events.len(), 1);
}

#[tokio::test]
async fn fallback_waits_for_retries_then_is_sticky_across_recreated_role_clients() {
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .expect(3)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(3)
        .mount(&backup)
        .await;
    let route = fallback_route(&primary, &backup);
    let ledger = LedgerHandle::new();
    let c = routed_client(&route, &ledger, "validator", 20, 2);
    assert_eq!(
        c.complete(&[ChatMessage::user("first")], &[])
            .await
            .unwrap()
            .model,
        "backup"
    );
    c.complete(&[ChatMessage::user("next turn")], &[])
        .await
        .unwrap();
    let recreated = routed_client(&route, &ledger, "validator", 20, 2);
    recreated
        .complete(&[ChatMessage::user("fresh candidate")], &[])
        .await
        .unwrap();
    primary.verify().await;
    backup.verify().await;
    let run = ledger.0.lock().unwrap();
    assert_eq!(run.entries.len(), 6);
    assert_eq!(run.fallback_events.len(), 1);
    assert_eq!(
        run.fallback_events[0].reason,
        revera::provider::FallbackReason::RateLimitOrServer
    );
    assert_eq!(
        revera::report::footer_routes(&run),
        ["validator=primary→backup@medium"]
    );
}

#[tokio::test]
async fn fallback_provider_failure_classes_and_nonretryable_http_errors() {
    for (status, body, reason) in [
        (401, "bad key", Some("authentication")),
        (403, "denied", Some("authentication")),
        (404, "unknown model", Some("not_found")),
        (429, "rate limit", Some("rate_limit_or_server")),
        (200, "not JSON", Some("malformed_response")),
        (200, "{}", Some("malformed_response")),
        (
            200,
            r#"{"choices":[{"message":{"content":"   "}}]}"#,
            Some("malformed_response"),
        ),
        (400, "invalid request", None),
        (422, "invalid parameters", None),
    ] {
        let primary = MockServer::start().await;
        let backup = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .expect(1)
            .mount(&primary)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(u64::from(reason.is_some()))
            .mount(&backup)
            .await;
        let route = fallback_route(&primary, &backup);
        let ledger = LedgerHandle::new();
        let result = routed_client(&route, &ledger, "investigator", 10, 0)
            .complete(&[ChatMessage::user("hi")], &[])
            .await;
        assert_eq!(
            result.is_ok(),
            reason.is_some(),
            "status {status}: {result:?}"
        );
        primary.verify().await;
        backup.verify().await;
        let run = ledger.0.lock().unwrap();
        if let Some(reason) = reason {
            assert_eq!(run.fallback_events.len(), 1);
            assert_eq!(
                serde_json::to_value(&run.fallback_events[0]).unwrap()["reason"],
                reason
            );
        } else {
            assert!(run.fallback_events.is_empty());
        }
    }
}

#[tokio::test]
async fn fallback_does_not_bypass_shared_request_budget() {
    for (budget, retries, requests) in [(0, 0, 0), (1, 0, 1), (1, 1, 1)] {
        let primary = MockServer::start().await;
        let backup = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
            .expect(requests)
            .mount(&primary)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(0)
            .mount(&backup)
            .await;
        let route = fallback_route(&primary, &backup);
        let ledger = LedgerHandle::new();
        let result = routed_client(&route, &ledger, "investigator", budget, retries)
            .complete(&[ChatMessage::user("hi")], &[])
            .await;
        assert!(
            matches!(result, Err(ProviderError::BudgetExhausted)),
            "{result:?}"
        );
        assert!(ledger.0.lock().unwrap().fallback_events.is_empty());
        assert_eq!(ledger.request_count(), requests as u32);
        primary.verify().await;
        backup.verify().await;
    }
}

#[tokio::test]
async fn fallback_timeout_and_connection_errors_use_the_same_ledger() {
    use revera::provider::FallbackReason;
    use revera::provider::fallback::FallbackClient;
    use std::{sync::Arc, time::Duration};
    for timeout in [true, false] {
        let primary = MockServer::start().await;
        let backup = MockServer::start().await;
        if timeout {
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(ok_body())
                        .set_delay(Duration::from_millis(250)),
                )
                .expect(1)
                .mount(&primary)
                .await;
        }
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&backup)
            .await;
        let ledger = LedgerHandle::new();
        let primary_url = if timeout {
            primary.uri()
        } else {
            "http://127.0.0.1:1".into()
        };
        let mut first = OpenAiChatClient::new(
            route_for(&primary_url, Protocol::OpenaiChat, "primary"),
            ledger.clone(),
            5,
            0,
            "investigator",
        )
        .unwrap();
        // Exercise the real transport timeout without waiting its production 120 s.
        first.transport.http = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let second = OpenAiChatClient::new(
            route_for(&backup.uri(), Protocol::OpenaiChat, "backup"),
            ledger.clone(),
            5,
            0,
            "investigator",
        )
        .unwrap();
        let c = FallbackClient::new(
            vec![
                ("primary".into(), Some(Arc::new(first))),
                ("backup".into(), Some(Arc::new(second))),
            ],
            "investigator",
            ledger.clone(),
            5,
        );
        assert_eq!(
            c.complete(&[ChatMessage::user("hi")], &[])
                .await
                .unwrap()
                .model,
            "backup"
        );
        backup.verify().await;
        let run = ledger.0.lock().unwrap();
        assert_eq!(run.entries.len(), 2);
        assert_eq!(
            run.fallback_events[0].reason,
            if timeout {
                FallbackReason::Timeout
            } else {
                FallbackReason::Transport
            }
        );
    }
}

#[tokio::test]
async fn fallback_stays_within_outer_deadline_and_cancellation() {
    use std::time::Duration;
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_body())
                .set_delay(Duration::from_millis(250)),
        )
        .expect(2)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(0)
        .mount(&backup)
        .await;
    let ledger = LedgerHandle::new();
    let c = routed_client(
        &fallback_route(&primary, &backup),
        &ledger,
        "investigator",
        10,
        0,
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            c.complete(&[ChatMessage::user("deadline")], &[])
        )
        .await
        .is_err()
    );
    let task = tokio::spawn(async move { c.complete(&[ChatMessage::user("cancel")], &[]).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(ledger.0.lock().unwrap().fallback_events.is_empty());
    primary.verify().await;
    backup.verify().await;
}

#[tokio::test]
async fn fallback_skips_missing_keys_including_primary_and_isolates_roles() {
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(2)
        .mount(&backup)
        .await;
    let mut route = fallback_route(&primary, &backup);
    let mut missing = route_for(&primary.uri(), Protocol::OpenaiChat, "missing-backup");
    missing.api_key_env = Some("REVERA_TEST_UNSET_FALLBACK_KEY_2F4D7".into());
    assert!(!revera::config::env_is_set(
        missing.api_key_env.as_deref().unwrap()
    ));
    route.fallback.insert(0, missing);
    let ledger = LedgerHandle::new();
    routed_client(&route, &ledger, "investigator", 10, 0)
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap();
    route.api_key_env = Some("REVERA_TEST_UNSET_FALLBACK_KEY_2F4D7".into());
    routed_client(&route, &ledger, "validator", 10, 0)
        .complete(&[ChatMessage::user("fresh")], &[])
        .await
        .unwrap();
    primary.verify().await;
    backup.verify().await;
    let run = ledger.0.lock().unwrap();
    assert_eq!(run.fallback_events.len(), 2);
    assert_eq!(
        run.fallback_events[1].reason,
        revera::provider::FallbackReason::MissingKey
    );
    assert_eq!(run.unavailable_routes.len(), 3);
}

#[tokio::test]
async fn fallback_cross_protocol_mid_conversation_replays_full_transcript_and_scopes_state() {
    for primary_protocol in [Protocol::OpenaiChat, Protocol::OpenaiResponses] {
        let primary = MockServer::start().await;
        let backup = MockServer::start().await;
        let first_reply = if primary_protocol == Protocol::OpenaiChat {
            json!({"choices":[{"message":{"content":"Looking up callers", "tool_calls":[{"id":"call-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}}]}}]})
        } else {
            json!({"output":[{"type":"reasoning","encrypted_content":"PRIMARY-OPAQUE"},{"type":"message","content":[{"type":"output_text","text":"Looking up callers"}]},{"type":"function_call","call_id":"call-1","name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}]})
        };
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(first_reply))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&primary)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .with_priority(2)
            .expect(1)
            .mount(&primary)
            .await;
        Mock::given(method("POST")).and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"content":[{"type":"thinking","thinking":"backup thought","signature":"BACKUP-OPAQUE"},{"type":"text","text":"Continue"}]})))
            .expect(2).mount(&backup).await;
        let mut route = fallback_route(&primary, &backup);
        route.protocol = primary_protocol;
        route.fallback[0].protocol = Protocol::Anthropic;
        let ledger = LedgerHandle::new();
        let c = routed_client(&route, &ledger, "investigator", 10, 0);
        let mut transcript = vec![
            ChatMessage::system("Review system"),
            ChatMessage::user("Review user"),
        ];
        transcript.push(c.complete(&transcript, &[]).await.unwrap().message);
        transcript.push(ChatMessage::tool("call-1", "read_file", "FILE CONTENT"));
        transcript.push(c.complete(&transcript, &[]).await.unwrap().message);
        transcript.push(ChatMessage::user("Continue the review"));
        c.complete(&transcript, &[]).await.unwrap();
        primary.verify().await;
        backup.verify().await;
        let requests = backup.received_requests().await.unwrap();
        for request in &requests {
            let body: serde_json::Value = request.body_json().unwrap();
            let text = body.to_string();
            assert!(
                text.contains("Review system") && text.contains("Review user"),
                "{text}"
            );
            assert!(
                text.contains("Looking up callers") && text.contains("FILE CONTENT"),
                "{text}"
            );
            assert!(
                text.contains("read_file") && text.contains("src/lib.rs"),
                "{text}"
            );
            assert!(
                !text.contains("PRIMARY-OPAQUE") && !text.contains("revera_route"),
                "{text}"
            );
            assert_eq!(
                body["messages"][1]["content"][1],
                json!({"type":"tool_use","id":"call-1","name":"read_file","input":{"path":"src/lib.rs"}})
            );
            assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call-1");
        }
        let next = requests[1]
            .body_json::<serde_json::Value>()
            .unwrap()
            .to_string();
        assert!(next.contains("BACKUP-OPAQUE"));
    }
}

#[tokio::test]
async fn fallback_position_is_per_chain_for_roles_with_several_routes() {
    let a_primary = MockServer::start().await;
    let a_backup = MockServer::start().await;
    let b_primary = MockServer::start().await;
    let b_backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&a_primary)
        .await;
    for (server, calls) in [(&a_backup, 1), (&b_primary, 1), (&b_backup, 0)] {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(calls)
            .mount(server)
            .await;
    }
    let ledger = LedgerHandle::new();
    for route in [
        fallback_route(&a_primary, &a_backup),
        fallback_route(&b_primary, &b_backup),
    ] {
        routed_client(&route, &ledger, "workers", 10, 0)
            .complete(&[ChatMessage::user("question")], &[])
            .await
            .unwrap();
    }
    for server in [&a_primary, &a_backup, &b_primary, &b_backup] {
        server.verify().await;
    }
}

#[tokio::test]
async fn fallback_to_gemini_signs_tool_calls_from_other_models() {
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":"Reading", "tool_calls":[{"id":"call-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}}]}}]})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(2)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"candidates":[{"content":{"parts":[{"text":"Continue"}]}}]})),
        )
        .expect(1)
        .mount(&backup)
        .await;
    let mut route = fallback_route(&primary, &backup);
    route.fallback[0].protocol = Protocol::Gemini;
    route.fallback[0].model = "gemini-3-pro".into();
    let ledger = LedgerHandle::new();
    let c = routed_client(&route, &ledger, "investigator", 10, 0);
    let mut transcript = vec![ChatMessage::user("Review user")];
    transcript.push(c.complete(&transcript, &[]).await.unwrap().message);
    transcript.push(ChatMessage::tool("call-1", "read_file", "FILE CONTENT"));
    c.complete(&transcript, &[]).await.unwrap();
    backup.verify().await;
    let body: serde_json::Value = backup.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    let call = body["contents"][1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part.get("functionCall").is_some())
        .cloned()
        .unwrap();
    assert_eq!(call["functionCall"]["name"], "read_file");
    assert_eq!(call["thoughtSignature"], "skip_thought_signature_validator");
}

#[tokio::test]
async fn fallback_to_responses_sends_full_input_not_previous_response_id() {
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":"Done"}]}]}),
        ))
        .expect(1)
        .mount(&backup)
        .await;
    let mut route = fallback_route(&primary, &backup);
    route.fallback[0].protocol = Protocol::OpenaiResponses;
    let mut assistant = ChatMessage::user("assistant history");
    assistant.role = Role::Assistant;
    assistant.provider_state = Some(json!([{"type":"thinking","signature":"FOREIGN-STATE"}]));
    let ledger = LedgerHandle::new();
    routed_client(&route, &ledger, "investigator", 10, 0)
        .complete(
            &[
                ChatMessage::system("system history"),
                ChatMessage::user("user history"),
                assistant,
                ChatMessage::user("continue"),
            ],
            &[],
        )
        .await
        .unwrap();
    let body: serde_json::Value = backup.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(body["instructions"], "system history");
    assert_eq!(body["input"].as_array().unwrap().len(), 3);
    assert_eq!(body["input"][1]["content"][0]["text"], "assistant history");
    assert_eq!(body["store"], false);
    assert!(body.get("previous_response_id").is_none());
    assert!(!body.to_string().contains("FOREIGN-STATE"));
}

#[tokio::test]
async fn fallback_reports_switches_and_summary_without_secrets() {
    use revera::report::{Summary, footer_routes, ledger_report, summary_markdown};
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    let key = std::env::var("REVERA_TEST_SECRET_REDACT").unwrap();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!(
            "bad key {key}; https://provider.example/v1?token=private-query"
        )))
        .expect(1)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&backup)
        .await;
    let mut route = fallback_route(&primary, &backup);
    route.api_key_env = Some("REVERA_TEST_SECRET_REDACT".into());
    route.extra_headers.insert("x-secret".into(), key.clone());
    let ledger = LedgerHandle::new();
    routed_client(&route, &ledger, "investigator", 10, 0)
        .complete(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap();
    let run = ledger.0.lock().unwrap();
    let value = serde_json::to_value(ledger_report(&run, 1)).unwrap();
    assert_eq!(
        value["fallback_events"],
        json!([{"role":"investigator","from_model":"primary","to_model":"backup","reason":"authentication"}])
    );
    assert_eq!(value["unavailable_routes"], json!([]));
    assert_eq!(value["by_route"].as_array().unwrap().len(), 2);
    let routes = footer_routes(&run);
    let summary = summary_markdown(&Summary {
        strategy: "baseline",
        routes: &routes,
        ..Default::default()
    });
    assert!(summary.contains("investigator=primary→backup@medium"));
    assert!(!summary.contains("http"));
    for output in [value.to_string(), summary, format!("{:?}", run.entries)] {
        assert!(
            !output.contains(&key) && !output.contains("private-query"),
            "{output}"
        );
    }
}

#[tokio::test]
async fn fallback_does_not_handle_agent_terminal_protocol_failures() {
    use revera::agent::{AgentBudget, StopReason, run_agent};
    use std::sync::Arc;
    let primary = MockServer::start().await;
    let backup = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"choices":[{"message":{"content":"I will not submit a terminal call"}}]}),
        ))
        .expect(2)
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(0)
        .mount(&backup)
        .await;
    let root = std::env::current_dir().unwrap();
    let toolbox = revera::tools::ToolBox::new(
        root.clone(),
        Arc::new(revera::diff::DiffSet::default()),
        Arc::new(revera::vera::VeraClient::disabled(&root)),
        12000,
    );
    let ledger = LedgerHandle::new();
    let c = routed_client(
        &fallback_route(&primary, &backup),
        &ledger,
        "investigator",
        10,
        0,
    );
    let terminal = revera::provider::ToolSpec {
        name: "submit_findings".into(),
        description: "done".into(),
        parameters: json!({"type":"object"}),
    };
    let result = run_agent(
        c.as_ref(),
        "system",
        "review",
        &toolbox,
        &terminal,
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.stopped, StopReason::NoTerminalCall);
    assert!(ledger.0.lock().unwrap().fallback_events.is_empty());
    primary.verify().await;
    backup.verify().await;
}

#[test]
fn adapter_labels_hide_base_url_query_values() {
    let base = "https://h.example/v1?key=s3cr3t-value";
    let labels = [
        OpenAiChatAdapter::from_route(route_for(base, Protocol::OpenaiChat, "m"))
            .unwrap()
            .label(),
        OpenAiResponsesAdapter::from_route(route_for(base, Protocol::OpenaiResponses, "m"))
            .unwrap()
            .label(),
        AnthropicAdapter::from_route(route_for(base, Protocol::Anthropic, "m"))
            .unwrap()
            .label(),
        GeminiAdapter::from_route(route_for(base, Protocol::Gemini, "m"))
            .unwrap()
            .label(),
    ];

    for label in labels {
        assert!(!label.contains("s3cr3t-value"), "{label}");
        assert!(label.contains("key"), "{label}");
    }
}

fn convo() -> (Vec<ChatMessage>, Vec<revera::provider::ToolSpec>) {
    let msgs = vec![
        ChatMessage::system("you review"),
        ChatMessage::user("check this"),
        ChatMessage {
            role: Role::Assistant,
            content: Some("looking".into()),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "a.rs"}),
            }],
            tool_call_id: None,
            name: None,
            provider_state: None,
        },
        ChatMessage::tool("call_1", "read_file", "file contents"),
    ];
    let tools = vec![revera::provider::ToolSpec {
        name: "read_file".into(),
        description: "read a file".into(),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }];
    (msgs, tools)
}

// ---------- openai-responses ----------

#[tokio::test]
async fn openai_responses_request_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_partial_json(json!({
            "model": "m-resp",
            "tool_choice": "auto",
            "max_output_tokens": 100,
            "store": false,
            "temperature": 0.2,
            "instructions": "you review",
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "check this"}]},
                {"role": "assistant", "content": [{"type": "output_text", "text": "looking"}]},
                {"type": "function_call", "call_id": "call_1", "name": "read_file",
                 "arguments": "{\"path\":\"a.rs\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "file contents"}
            ],
            "tools": [{"type": "function", "name": "read_file", "strict": false}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "done"}]}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 3}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_for(
            &server.uri(),
            Protocol::OpenaiResponses,
            "m-resp",
        ))
        .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    let (msgs, tools) = convo();
    let r = c.complete(&msgs, &tools).await.unwrap();
    assert_eq!(r.message.content.as_deref(), Some("done"));
    assert_eq!(r.usage.prompt_tokens, 5);
    server.verify().await;
}

#[tokio::test]
async fn openai_responses_parses_function_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "let me look"}]},
                {"type": "function_call", "call_id": "fc_9", "name": "vera_search",
                 "arguments": "{\"query\":\"discount\"}"}
            ],
            "usage": {"input_tokens": 4, "output_tokens": 7}
        })))
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_for(
            &server.uri(),
            Protocol::OpenaiResponses,
            "m",
        ))
        .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    let r = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(r.message.tool_calls[0].id, "fc_9");
    assert_eq!(r.message.tool_calls[0].name, "vera_search");
    assert_eq!(
        r.message.tool_calls[0].arguments,
        json!({"query": "discount"})
    );
}

#[tokio::test]
async fn openai_responses_retries_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_for(
            &server.uri(),
            Protocol::OpenaiResponses,
            "m",
        ))
        .unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    let r = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(r.message.content.as_deref(), Some("ok"));
    assert_eq!(ledger.request_count(), 2, "429 + 200 = two ledger entries");
    server.verify().await;
}

// ---------- anthropic ----------

#[tokio::test]
async fn anthropic_request_shape_and_tool_result_merge() {
    let server = MockServer::start().await;
    // two consecutive tool results must merge into one user message
    let mut msgs = convo().0;
    msgs.push(ChatMessage::tool(
        "call_2",
        "list_changed_files",
        "a.rs b.rs",
    ));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(wiremock::matchers::header("x-api-key", "sk-test"))
        .and(wiremock::matchers::header(
            "anthropic-version",
            "2023-06-01",
        ))
        .and(body_partial_json(json!({
            "model": "claude-x",
            "system": "you review",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "check this"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "looking"},
                    {"type": "tool_use", "id": "call_1", "name": "read_file",
                     "input": {"path": "a.rs"}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "file contents"},
                    {"type": "tool_result", "tool_use_id": "call_2", "content": "a.rs b.rs"}]}
            ],
            "tools": [{"name": "read_file", "input_schema": {"type": "object"}}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "all good"}],
            "usage": {"input_tokens": 9, "output_tokens": 4}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: AnthropicAdapter::from_route(route_for(
            &server.uri(),
            Protocol::Anthropic,
            "claude-x",
        ))
        .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    let (.., tools) = convo();
    let r = c.complete(&msgs, &tools).await.unwrap();
    assert_eq!(r.message.content.as_deref(), Some("all good"));
    assert_eq!(r.usage.prompt_tokens, 9);
    server.verify().await;
}

#[tokio::test]
async fn anthropic_parses_tool_use() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "tu_1", "name": "vera_grep",
                 "input": {"pattern": "discount"}}
            ],
            "usage": {"input_tokens": 3, "output_tokens": 6}
        })))
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: AnthropicAdapter::from_route(route_for(
            &server.uri(),
            Protocol::Anthropic,
            "claude-x",
        ))
        .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    let r = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(r.message.tool_calls[0].name, "vera_grep");
    assert_eq!(r.message.content.as_deref(), Some("checking"));
}

#[tokio::test]
async fn anthropic_retries_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = HttpClient {
        adapter: AnthropicAdapter::from_route(route_for(&server.uri(), Protocol::Anthropic, "m"))
            .unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(ledger.request_count(), 2);
    server.verify().await;
}

// ---------- gemini ----------

#[tokio::test]
async fn gemini_request_shape_and_schema_strip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gem-x:generateContent"))
        .and(wiremock::matchers::header("x-goog-api-key", "sk-test"))
        .and(body_partial_json(json!({
            "systemInstruction": {"parts": [{"text": "you review"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "check this"}]},
                {"role": "model", "parts": [
                    {"text": "looking"},
                    {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}}}]},
                {"role": "user", "parts": [
                    {"functionResponse": {"name": "read_file",
                        "response": {"content": {"text": "file contents"}}}}]}
            ],
            "tools": [{"functionDeclarations": [{
                "name": "read_file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            }]}],
            "generationConfig": {"temperature": 0.2, "maxOutputTokens": 100}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model",
                "parts": [{"text": "fine"}]}}],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: GeminiAdapter::from_route(route_for(&server.uri(), Protocol::Gemini, "gem-x"))
            .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    // schema includes a key Gemini rejects; stripping must remove it
    let (mut msgs, mut tools) = convo();
    tools[0].parameters["additionalProperties"] = json!(false);
    tools[0].parameters["$schema"] = json!("http://json-schema.org/draft-07/schema#");
    // tool result carries synthesized id "read_file-0" -> functionResponse name
    msgs[3].tool_call_id = Some("read_file-0".into());
    msgs[3].name = Some("read_file".into());
    let r = c.complete(&msgs, &tools).await.unwrap();
    assert_eq!(r.message.content.as_deref(), Some("fine"));
    server.verify().await;
}

#[tokio::test]
async fn gemini_parses_function_call_synth_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [
                {"text": "hmm"},
                {"functionCall": {"name": "vera_references", "args": {"symbol": "f"}}}
            ]}}],
            "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 5}
        })))
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: GeminiAdapter::from_route(route_for(&server.uri(), Protocol::Gemini, "gem-x"))
            .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    let r = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(r.message.tool_calls[0].id, "vera_references-0");
    assert_eq!(r.message.tool_calls[0].name, "vera_references");
}

#[tokio::test]
async fn gemini_retries_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = HttpClient {
        adapter: GeminiAdapter::from_route(route_for(&server.uri(), Protocol::Gemini, "gem-x"))
            .unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(ledger.request_count(), 2);
    server.verify().await;
}

// ================= M7: per-route reasoning =================

use revera::config::{Reasoning, ReasoningEffort, ReasoningField, ReasoningSpec};

fn route_reasoning(url: &str, proto: Protocol, model: &str, r: Reasoning) -> ModelRoute {
    let mut rt = route_for(url, proto, model);
    rt.reasoning = r;
    rt
}

fn spec(effort: ReasoningEffort, budget: Option<u64>, field: ReasoningField) -> Reasoning {
    Reasoning::Spec(ReasoningSpec {
        effort,
        budget_tokens: budget,
        field,
    })
}

// ---------- openai-chat reasoning ----------

#[tokio::test]
async fn openai_chat_reasoning_effort_high_emitted() {
    let server = MockServer::start().await;
    // field: auto + non-openrouter host -> "reasoning_effort"
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "high"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "test-model",
            spec(ReasoningEffort::High, None, ReasoningField::Auto),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("hi")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn openai_chat_reasoning_openrouter_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning": {"effort": "high"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "m",
            spec(ReasoningEffort::High, None, ReasoningField::Openrouter),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn openai_chat_reasoning_budget_becomes_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"reasoning": {"max_tokens": 4096}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "m",
            spec(ReasoningEffort::Low, Some(4096), ReasoningField::Openrouter),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn openai_chat_reasoning_none_emits_nothing() {
    let server = MockServer::start().await;
    // wiremock has no "field absent" matcher; capture via a handler that
    // fails when the field is present by matching exact-ish body? Use a
    // response that requires no reasoning field via a negative check on
    // request journal instead.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "m",
            spec(ReasoningEffort::None, None, ReasoningField::Auto),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(body.get("reasoning").is_none(), "{body}");
    assert!(body.get("reasoning_effort").is_none(), "{body}");
}

#[tokio::test]
async fn openai_chat_reasoning_400_retries_without() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "high"})))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("unrecognized field: reasoning_effort"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "m",
            spec(ReasoningEffort::High, None, ReasoningField::Auto),
        ),
        ledger.clone(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    assert_eq!(ledger.request_count(), 2);
    let reqs = server.received_requests().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(body2.get("reasoning_effort").is_none(), "{body2}");
    assert!(body2.get("reasoning").is_none(), "{body2}");
    assert!(ledger.totals().0 == 2);
}

#[tokio::test]
async fn openai_chat_xhigh_passes_through_for_non_gpt5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "xhigh"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "test-model",
            spec(ReasoningEffort::Xhigh, None, ReasoningField::Openai),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn openai_chat_max_passes_through_on_gpt5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "max"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "gpt-5.6-terra",
            spec(ReasoningEffort::Max, None, ReasoningField::Openai),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn openai_chat_max_passes_through_for_non_gpt5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "max"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "glm-5.3",
            spec(ReasoningEffort::Max, None, ReasoningField::Openai),
        ),
        LedgerHandle::new(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn openai_chat_400_steps_effort_down_to_high() {
    let server = MockServer::start().await;
    // first attempt: requested "max" verbatim -> 400 mentions reasoning_effort
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "max"})))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("unrecognized field: reasoning_effort"),
        )
        .expect(1)
        .mount(&server)
        .await;
    // same-slot retry: capped at "high" -> success
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "high"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "glm-5.3",
            spec(ReasoningEffort::Max, None, ReasoningField::Openai),
        ),
        ledger.clone(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    let entries = &ledger.0.lock().unwrap().entries;
    let last = entries.last().unwrap();
    assert_eq!(last.requested_reasoning, "max");
    assert_eq!(last.effective_reasoning, "high");
    assert_eq!(last.role, "test");
}

#[tokio::test]
async fn openai_chat_second_400_drops_reasoning() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "max"})))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("unrecognized field: reasoning_effort"),
        )
        .expect(1)
        .mount(&server)
        .await;
    // capped retry also rejected -> next retry carries no reasoning field
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning_effort": "high"})))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("unrecognized field: reasoning_effort"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = OpenAiChatClient::new(
        route_reasoning(
            &server.uri(),
            Protocol::OpenaiChat,
            "glm-5.3",
            spec(ReasoningEffort::Max, None, ReasoningField::Openai),
        ),
        ledger.clone(),
        10,
        3,
        "test",
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3);
    let body3: serde_json::Value = serde_json::from_slice(&reqs[2].body).unwrap();
    assert!(body3.get("reasoning_effort").is_none(), "{body3}");
    assert!(body3.get("reasoning").is_none(), "{body3}");
    let entries = &ledger.0.lock().unwrap().entries;
    let last = entries.last().unwrap();
    assert_eq!(last.requested_reasoning, "max");
    assert_eq!(last.effective_reasoning, "none");
}

#[tokio::test]
async fn session_header_and_user_agent_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;
    let mut rt = route(&server.uri());
    rt.session_header = Some("x-opencode-session".into());
    let c = OpenAiChatClient::new(rt, LedgerHandle::new(), 10, 3, "test").unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let session = reqs[0]
        .headers
        .get("x-opencode-session")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert_eq!(session.len(), 36, "{session}");
    let ua = reqs[0]
        .headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert!(ua.starts_with("revera/"), "{ua}");
}

// ---------- openai-responses reasoning + provider_state ----------

fn resp_adapter(url: &str, r: Reasoning) -> HttpClient<OpenAiResponsesAdapter> {
    HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_reasoning(
            url,
            Protocol::OpenaiResponses,
            "m-resp",
            r,
        ))
        .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    }
}

#[tokio::test]
async fn responses_reasoning_emits_effort_and_include() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "reasoning": {"effort": "high"},
            "include": ["reasoning.encrypted_content"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    resp_adapter(
        &server.uri(),
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn responses_max_passes_through_on_gpt5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning": {"effort": "max"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_reasoning(
            &server.uri(),
            Protocol::OpenaiResponses,
            "gpt-5.6-terra",
            spec(ReasoningEffort::Max, None, ReasoningField::Auto),
        ))
        .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn responses_max_passes_through_for_non_gpt5() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning": {"effort": "max"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    resp_adapter(
        &server.uri(),
        spec(ReasoningEffort::Max, None, ReasoningField::Auto),
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn responses_reasoning_none_emits_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    resp_adapter(
        &server.uri(),
        spec(ReasoningEffort::None, None, ReasoningField::Auto),
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(body.get("reasoning").is_none(), "{body}");
    assert!(body.get("include").is_none(), "{body}");
}

#[tokio::test]
async fn responses_reasoning_400_retries_without() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"reasoning": {"effort": "high"}})))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("unsupported parameter: reasoning"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_reasoning(
            &server.uri(),
            Protocol::OpenaiResponses,
            "m",
            spec(ReasoningEffort::High, None, ReasoningField::Auto),
        ))
        .unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    assert_eq!(ledger.request_count(), 2);
    let reqs = server.received_requests().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(body2.get("reasoning").is_none(), "{body2}");
}

#[tokio::test]
async fn responses_reasoning_items_echo_verbatim_before_calls() {
    let server = MockServer::start().await;
    // first turn: response contains a reasoning item + function_call
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "BLOB",
                 "summary": []},
                {"type": "function_call", "call_id": "fc_1", "name": "read_file",
                 "arguments": "{}"}
            ],
            "usage": {"input_tokens": 3, "output_tokens": 5,
                      "output_tokens_details": {"reasoning_tokens": 42}}
        })))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // second turn: reasoning item must appear immediately before the
    // function_call item in input
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "x"}]},
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "BLOB",
                 "summary": []},
                {"type": "function_call", "call_id": "fc_1", "name": "read_file",
                 "arguments": "{}"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"done"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = resp_adapter(
        &server.uri(),
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    );
    let r1 = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(r1.usage.reasoning_tokens, 42);
    let st = r1.message.provider_state.as_ref().expect("provider_state");
    assert!(st.as_array().unwrap()[0]["type"] == "reasoning");
    let mut msgs = vec![ChatMessage::user("x")];
    msgs.push(r1.message);
    c.complete(&msgs, &[]).await.unwrap();
    server.verify().await;
}

// ---------- anthropic thinking + provider_state ----------

fn anth_adapter(url: &str, r: Reasoning, max_out: u32) -> HttpClient<AnthropicAdapter> {
    let mut rt = route_reasoning(url, Protocol::Anthropic, "claude-x", r);
    rt.max_output_tokens = max_out;
    HttpClient {
        adapter: AnthropicAdapter::from_route(rt).unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    }
}

#[tokio::test]
async fn anthropic_thinking_emitted_and_temp_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "thinking": {"type": "enabled", "budget_tokens": 16384},
            "max_tokens": 20000
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    // max_output_tokens 20000: budget 16384 leaves 3616 > 1024 headroom
    anth_adapter(
        &server.uri(),
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
        20000,
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(body.get("temperature").is_none(), "{body}");
}

#[tokio::test]
async fn anthropic_thinking_budget_clamps_and_raises_max() {
    let server = MockServer::start().await;
    // max_output_tokens 4000 < B+1024 → clamp B to 2976, raise max_tokens
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "thinking": {"type": "enabled", "budget_tokens": 2976},
            "max_tokens": 6976
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    anth_adapter(
        &server.uri(),
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
        4000,
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn anthropic_thinking_none_emits_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    anth_adapter(
        &server.uri(),
        spec(ReasoningEffort::None, None, ReasoningField::Auto),
        4000,
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(body.get("thinking").is_none(), "{body}");
    assert_eq!(body["temperature"], json!(0.2), "{body}");
}

#[tokio::test]
async fn anthropic_thinking_400_retries_without() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"thinking": {"type": "enabled"}})))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid field: thinking"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let mut rt = route_reasoning(
        &server.uri(),
        Protocol::Anthropic,
        "claude-x",
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    );
    rt.max_output_tokens = 20000;
    let c = HttpClient {
        adapter: AnthropicAdapter::from_route(rt).unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    assert_eq!(ledger.request_count(), 2);
    let reqs = server.received_requests().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(body2.get("thinking").is_none(), "{body2}");
}

#[tokio::test]
async fn anthropic_thinking_blocks_echo_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "SIG"},
                {"type": "tool_use", "id": "tu_1", "name": "read_file",
                 "input": {"path": "a.rs"}}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 9}
        })))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // echo: thinking block must lead the assistant message content
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "x"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "SIG"},
                    {"type": "tool_use", "id": "tu_1", "name": "read_file",
                     "input": {"path": "a.rs"}}]}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"done"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = anth_adapter(
        &server.uri(),
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
        20000,
    );
    let r1 = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    let st = r1
        .message
        .provider_state
        .as_ref()
        .expect("thinking provider_state");
    assert_eq!(st[0]["signature"], json!("SIG"));
    let msgs = vec![ChatMessage::user("x"), r1.message];
    c.complete(&msgs, &[]).await.unwrap();
    server.verify().await;
}

// ---------- gemini thinkingConfig + thoughtSignature ----------

fn gem_adapter(url: &str, model: &str, r: Reasoning) -> HttpClient<GeminiAdapter> {
    HttpClient {
        adapter: GeminiAdapter::from_route(route_reasoning(url, Protocol::Gemini, model, r))
            .unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    }
}

#[tokio::test]
async fn gemini_thinking_budget_emitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "generationConfig": {
                "thinkingConfig": {"thinkingBudget": 16384, "includeThoughts": false}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    {
        let mut rt = route_reasoning(
            &server.uri(),
            Protocol::Gemini,
            "gemini-2.5-flash",
            spec(ReasoningEffort::High, None, ReasoningField::Auto),
        );
        rt.max_output_tokens = 20000;
        let c = HttpClient {
            adapter: GeminiAdapter::from_route(rt).unwrap(),
            transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
        };
        c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    }
    server.verify().await;
}

#[tokio::test]
async fn gemini_3_uses_thinking_level() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "generationConfig": {"thinkingConfig": {"thinkingLevel": "high"}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    gem_adapter(
        &server.uri(),
        "gemini-3-pro",
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn gemini_none_budget_zero_on_flash_nothing_on_pro() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:generateContent"))
        .and(body_partial_json(json!({
            "generationConfig": {"thinkingConfig": {"thinkingBudget": 0}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-pro:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let none = spec(ReasoningEffort::None, None, ReasoningField::Auto);
    gem_adapter(&server.uri(), "gemini-2.5-flash", none.clone())
        .complete(&[ChatMessage::user("x")], &[])
        .await
        .unwrap();
    gem_adapter(&server.uri(), "gemini-2.5-pro", none)
        .complete(&[ChatMessage::user("x")], &[])
        .await
        .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let pro: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(
        pro["generationConfig"].get("thinkingConfig").is_none(),
        "{pro}"
    );
}

#[tokio::test]
async fn gemini_thinking_400_retries_without() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "generationConfig": {"thinkingConfig": {"thinkingBudget": 16384}}
        })))
        .respond_with(ResponseTemplate::new(400).set_body_string("unknown field thinkingConfig"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let mut rt = route_reasoning(
        &server.uri(),
        Protocol::Gemini,
        "gemini-2.5-flash",
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    );
    rt.max_output_tokens = 20000;
    let c = HttpClient {
        adapter: GeminiAdapter::from_route(rt).unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    assert_eq!(ledger.request_count(), 2);
    let reqs = server.received_requests().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(
        body2["generationConfig"].get("thinkingConfig").is_none(),
        "{body2}"
    );
}

#[tokio::test]
async fn gemini_thought_signature_round_trips() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [
                {"text": "hmm", "thoughtSignature": "TS1"},
                {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}},
                 "thoughtSignature": "TS2"}
            ]}}],
            "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 5,
                              "thoughtsTokenCount": 17}
        })))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "contents": [
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [
                    {"text": "hmm", "thoughtSignature": "TS1"},
                    {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}},
                     "thoughtSignature": "TS2"}]}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = gem_adapter(
        &server.uri(),
        "gemini-2.5-flash",
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    );
    let r1 = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert_eq!(r1.usage.reasoning_tokens, 17);
    assert!(r1.message.provider_state.is_some());
    let msgs = vec![ChatMessage::user("x"), r1.message];
    c.complete(&msgs, &[]).await.unwrap();
    server.verify().await;
}

// ================= review fixes (PR #3) =================

#[tokio::test]
async fn gemini_thinking_budget_reconciles_max_output() {
    let server = MockServer::start().await;
    // max_output 4000 < B+1024 → clamp B to 2976, raise maxOutputTokens
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "generationConfig": {
                "maxOutputTokens": 6976,
                "thinkingConfig": {"thinkingBudget": 2976, "includeThoughts": false}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut rt = route_reasoning(
        &server.uri(),
        Protocol::Gemini,
        "gemini-2.5-flash",
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    );
    rt.max_output_tokens = 4000;
    let c = HttpClient {
        adapter: GeminiAdapter::from_route(rt).unwrap(),
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn gemini_provider_state_rebuilds_parts_in_order() {
    let server = MockServer::start().await;
    // first turn: two signed text parts + a signed call
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [
                {"text": "first thought", "thoughtSignature": "SA"},
                {"text": "second thought", "thoughtSignature": "SB"},
                {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}},
                 "thoughtSignature": "SC"}
            ]}}],
            "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 5}
        })))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // echo: three separate parts, signatures in order
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "contents": [
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [
                    {"text": "first thought", "thoughtSignature": "SA"},
                    {"text": "second thought", "thoughtSignature": "SB"},
                    {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}},
                     "thoughtSignature": "SC"}]}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = gem_adapter(
        &server.uri(),
        "gemini-2.5-flash",
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
    );
    let r1 = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    let st = r1.message.provider_state.as_ref().expect("provider_state");
    assert_eq!(st.as_array().unwrap().len(), 3, "ordered per-part state");
    let msgs = vec![ChatMessage::user("x"), r1.message];
    c.complete(&msgs, &[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn anthropic_400_thinking_drops_provider_state_blocks() {
    let server = MockServer::start().await;
    // request 1 (fresh): ok with thinking block
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "SIG"},
                {"type": "text", "text": "looking"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 5}
        })))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // request 2 (echoes the thinking block): 400 mentions thinking
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "thinking": {"type": "enabled"}
        })))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid field: thinking"))
        .expect(1)
        .mount(&server)
        .await;
    // request 3 (same-slot retry): no thinking anywhere
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"done"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = anth_adapter(
        &server.uri(),
        spec(ReasoningEffort::High, None, ReasoningField::Auto),
        20000,
    );
    let r1 = c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    assert!(r1.message.provider_state.is_some());
    let msgs = vec![ChatMessage::user("x"), r1.message];
    let r2 = c.complete(&msgs, &[]).await.unwrap();
    assert_eq!(r2.message.content.as_deref(), Some("done"));
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3);
    let body3: serde_json::Value = serde_json::from_slice(&reqs[2].body).unwrap();
    assert!(body3.get("thinking").is_none(), "{body3}");
    let asst = &body3["messages"][1];
    assert!(
        !asst["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == "thinking" || b["type"] == "redacted_thinking"),
        "{asst}"
    );
}

#[tokio::test]
async fn anthropic_temperature_400_retries_without() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"temperature": 0.2})))
        .respond_with(ResponseTemplate::new(400).set_body_string("unsupported: temperature"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    anth_adapter(
        &server.uri(),
        spec(ReasoningEffort::None, None, ReasoningField::Auto),
        4000,
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(body2.get("temperature").is_none(), "{body2}");
}

#[tokio::test]
async fn gemini_temperature_400_retries_without() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"generationConfig": {"temperature": 0.2}}),
        ))
        .respond_with(ResponseTemplate::new(400).set_body_string("unsupported: temperature"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    gem_adapter(
        &server.uri(),
        "gemini-2.5-pro", // 2.5-pro with effort none → no thinkingConfig, cleanest check
        spec(ReasoningEffort::None, None, ReasoningField::Auto),
    )
    .complete(&[ChatMessage::user("x")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let body2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(
        body2["generationConfig"].get("temperature").is_none(),
        "{body2}"
    );
}

#[tokio::test]
async fn ledger_entry_records_reasoning_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 9,
                      "output_tokens_details": {"reasoning_tokens": 7}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    let c = HttpClient {
        adapter: OpenAiResponsesAdapter::from_route(route_reasoning(
            &server.uri(),
            Protocol::OpenaiResponses,
            "m",
            spec(ReasoningEffort::High, None, ReasoningField::Auto),
        ))
        .unwrap(),
        transport: HttpTransport::new(ledger.clone(), 10, 3, "test").unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    let entries = ledger.0.lock().unwrap().entries.clone();
    assert_eq!(entries[0].reasoning_tokens, 7);
    assert_eq!(ledger.totals().3, 7);
}

fn cache_route_for(url: &str, proto: Protocol, model: &str, cache: bool) -> ModelRoute {
    let mut rt = route_for(url, proto, model);
    rt.cache = cache;
    rt
}

fn cache_key() -> Option<String> {
    Some("revera-0123456789abcdef".into())
}

fn cache_client(
    url: &str,
    proto: Protocol,
    model: &str,
    cache: bool,
    ledger: LedgerHandle,
) -> Box<dyn ModelClient> {
    match proto {
        Protocol::OpenaiChat => Box::new(
            OpenAiChatClient::new_with_cache_key(
                cache_route_for(url, proto, model, cache),
                ledger,
                10,
                0,
                "cache-test",
                cache_key(),
            )
            .unwrap(),
        ),
        Protocol::OpenaiResponses => Box::new(
            OpenAiResponsesClient::new_responses_with_cache_key(
                cache_route_for(url, proto, model, cache),
                ledger,
                10,
                0,
                "cache-test",
                cache_key(),
            )
            .unwrap(),
        ),
        Protocol::Anthropic => Box::new(HttpClient {
            adapter: AnthropicAdapter::from_route(cache_route_for(url, proto, model, cache))
                .unwrap(),
            transport: HttpTransport::new(ledger, 10, 0, "cache-test").unwrap(),
        }),
        Protocol::Gemini => Box::new(HttpClient {
            adapter: GeminiAdapter::from_route(cache_route_for(url, proto, model, cache)).unwrap(),
            transport: HttpTransport::new(ledger, 10, 0, "cache-test").unwrap(),
        }),
        Protocol::Scripted => unreachable!(),
    }
}

fn count_cache_controls(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(count_cache_controls).sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter().map(count_cache_controls).sum(),
        _ => 0,
    }
}

#[tokio::test]
async fn openai_chat_cache_hint_is_keyed_and_disabled_when_configured_off() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(2)
        .mount(&server)
        .await;
    let msgs = [ChatMessage::user("hi")];
    cache_client(
        &server.uri(),
        Protocol::OpenaiChat,
        "m",
        true,
        LedgerHandle::new(),
    )
    .complete(&msgs, &[])
    .await
    .unwrap();
    cache_client(
        &server.uri(),
        Protocol::OpenaiChat,
        "m",
        false,
        LedgerHandle::new(),
    )
    .complete(&msgs, &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let enabled: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let disabled: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(enabled["prompt_cache_key"], "revera-0123456789abcdef");
    assert!(disabled.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn openai_responses_cache_hint_is_keyed_and_disabled_when_configured_off() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(2)
        .mount(&server)
        .await;
    let msgs = [ChatMessage::user("hi")];
    cache_client(
        &server.uri(),
        Protocol::OpenaiResponses,
        "m",
        true,
        LedgerHandle::new(),
    )
    .complete(&msgs, &[])
    .await
    .unwrap();
    cache_client(
        &server.uri(),
        Protocol::OpenaiResponses,
        "m",
        false,
        LedgerHandle::new(),
    )
    .complete(&msgs, &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let enabled: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let disabled: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(enabled["prompt_cache_key"], "revera-0123456789abcdef");
    assert!(disabled.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn anthropic_cache_hints_use_at_most_four_breakpoints_and_disable_cleanly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(2)
        .mount(&server)
        .await;
    let (mut msgs, tools) = convo();
    msgs[2].provider_state = Some(json!([
        {"type": "thinking", "thinking": "private", "signature": "sig"},
        {"type": "redacted_thinking", "data": "opaque"}
    ]));
    cache_client(
        &server.uri(),
        Protocol::Anthropic,
        "claude",
        true,
        LedgerHandle::new(),
    )
    .complete(&msgs, &tools)
    .await
    .unwrap();
    cache_client(
        &server.uri(),
        Protocol::Anthropic,
        "claude",
        false,
        LedgerHandle::new(),
    )
    .complete(&msgs, &tools)
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let enabled: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let disabled: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(enabled["system"].is_array(), "{enabled}");
    assert_eq!(enabled["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(enabled["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(count_cache_controls(&enabled), 4, "{enabled}");
    assert!(
        enabled["messages"][1]["content"][0]
            .get("cache_control")
            .is_none(),
        "{enabled}"
    );
    assert_eq!(
        enabled["messages"][1]["content"][3]["cache_control"]["type"], "ephemeral",
        "{enabled}"
    );
    assert_eq!(disabled["system"], "you review", "{disabled}");
    assert_eq!(count_cache_controls(&disabled), 0, "{disabled}");
}

#[tokio::test]
async fn gemini_uses_only_implicit_caching_with_cache_enabled_or_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        })))
        .expect(2)
        .mount(&server)
        .await;
    let msgs = [ChatMessage::user("hi")];
    cache_client(
        &server.uri(),
        Protocol::Gemini,
        "gemini-2.5-pro",
        true,
        LedgerHandle::new(),
    )
    .complete(&msgs, &[])
    .await
    .unwrap();
    cache_client(
        &server.uri(),
        Protocol::Gemini,
        "gemini-2.5-pro",
        false,
        LedgerHandle::new(),
    )
    .complete(&msgs, &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    for req in reqs {
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert!(body.get("prompt_cache_key").is_none(), "{body}");
        assert_eq!(count_cache_controls(&body), 0, "{body}");
    }
}

#[tokio::test]
async fn openai_chat_retries_without_rejected_cache_key_and_records_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"prompt_cache_key": "revera-0123456789abcdef"}),
        ))
        .respond_with(ResponseTemplate::new(400).set_body_string("unsupported prompt_cache_key"))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    cache_client(
        &server.uri(),
        Protocol::OpenaiChat,
        "m",
        true,
        ledger.clone(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(second.get("prompt_cache_key").is_none(), "{second}");
    let entries = ledger.0.lock().unwrap().entries.clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].error.as_deref(),
        Some("400: retrying without prompt cache hint")
    );
}

#[tokio::test]
async fn openai_responses_retries_without_rejected_cache_key_and_records_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"prompt_cache_key": "revera-0123456789abcdef"}),
        ))
        .respond_with(ResponseTemplate::new(400).set_body_string("unsupported prompt_cache_key"))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    cache_client(
        &server.uri(),
        Protocol::OpenaiResponses,
        "m",
        true,
        ledger.clone(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(second.get("prompt_cache_key").is_none(), "{second}");
    let entries = ledger.0.lock().unwrap().entries.clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].error.as_deref(),
        Some("400: retrying without prompt cache hint")
    );
}

#[tokio::test]
async fn anthropic_retries_without_rejected_cache_control_and_records_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "system": [{"cache_control": {"type": "ephemeral"}}]
        })))
        .respond_with(ResponseTemplate::new(400).set_body_string("unsupported cache_control"))
        .expect(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let ledger = LedgerHandle::new();
    cache_client(
        &server.uri(),
        Protocol::Anthropic,
        "claude",
        true,
        ledger.clone(),
    )
    .complete(
        &[ChatMessage::system("system"), ChatMessage::user("hi")],
        &[],
    )
    .await
    .unwrap();
    server.verify().await;
    let reqs = server.received_requests().await.unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(second["system"], "system", "{second}");
    assert_eq!(count_cache_controls(&second), 0, "{second}");
    let entries = ledger.0.lock().unwrap().entries.clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].error.as_deref(),
        Some("400: retrying without prompt cache hint")
    );
}

#[tokio::test]
async fn provider_usage_parses_cached_tokens_for_all_protocols() {
    let chat_server = MockServer::start().await;
    let mut chat_body = ok_body();
    chat_body["usage"] = json!({
        "prompt_tokens": 12,
        "prompt_tokens_details": {"cached_tokens": 5},
        "completion_tokens": 2
    });
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_body))
        .mount(&chat_server)
        .await;
    let chat = cache_client(
        &chat_server.uri(),
        Protocol::OpenaiChat,
        "m",
        false,
        LedgerHandle::new(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    assert_eq!(chat.usage.prompt_tokens, 12);
    assert_eq!(chat.usage.cached_prompt_tokens, 5);
    assert_eq!(chat.usage.cache_write_tokens, 0);

    let responses_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage": {
                "input_tokens": 20,
                "input_tokens_details": {"cached_tokens": 8},
                "output_tokens": 2
            }
        })))
        .mount(&responses_server)
        .await;
    let responses = cache_client(
        &responses_server.uri(),
        Protocol::OpenaiResponses,
        "m",
        false,
        LedgerHandle::new(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    assert_eq!(responses.usage.prompt_tokens, 20);
    assert_eq!(responses.usage.cached_prompt_tokens, 8);

    let anthropic_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type":"text","text":"ok"}],
            "usage": {
                "input_tokens": 3,
                "cache_read_input_tokens": 4,
                "cache_creation_input_tokens": 2,
                "output_tokens": 1
            }
        })))
        .mount(&anthropic_server)
        .await;
    let anthropic = cache_client(
        &anthropic_server.uri(),
        Protocol::Anthropic,
        "claude",
        false,
        LedgerHandle::new(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    assert_eq!(anthropic.usage.prompt_tokens, 9);
    assert_eq!(anthropic.usage.cached_prompt_tokens, 4);
    assert_eq!(anthropic.usage.cache_write_tokens, 2);

    let gemini_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role":"model","parts":[{"text":"ok"}]}}],
            "usageMetadata": {
                "promptTokenCount": 13,
                "cachedContentTokenCount": 5,
                "candidatesTokenCount": 1
            }
        })))
        .mount(&gemini_server)
        .await;
    let gemini = cache_client(
        &gemini_server.uri(),
        Protocol::Gemini,
        "gemini-2.5-pro",
        true,
        LedgerHandle::new(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    assert_eq!(gemini.usage.prompt_tokens, 13);
    assert_eq!(gemini.usage.cached_prompt_tokens, 5);
    assert_eq!(gemini.usage.cache_write_tokens, 0);
}

#[tokio::test]
async fn openai_chat_parses_deepseek_cache_hit_fallback() {
    let server = MockServer::start().await;
    let mut body = ok_body();
    body["usage"] = json!({
        "prompt_tokens": 11,
        "prompt_cache_hit_tokens": 4,
        "completion_tokens": 2
    });
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let result = cache_client(
        &server.uri(),
        Protocol::OpenaiChat,
        "m",
        false,
        LedgerHandle::new(),
    )
    .complete(&[ChatMessage::user("hi")], &[])
    .await
    .unwrap();
    assert_eq!(result.usage.prompt_tokens, 11);
    assert_eq!(result.usage.cached_prompt_tokens, 4);
}

#[test]
fn cache_hint_request_is_deterministic_and_turns_are_append_only() {
    let adapter = OpenAiChatAdapter::from_route(cache_route_for(
        "https://api.example.com/v1",
        Protocol::OpenaiChat,
        "m",
        true,
    ))
    .unwrap()
    .with_cache_key(cache_key());
    let (msgs, tools) = convo();
    let first = adapter
        .build(&msgs, &tools, &mut AttemptState::default())
        .unwrap();
    let repeat = adapter
        .build(&msgs, &tools, &mut AttemptState::default())
        .unwrap();
    assert_eq!(
        serde_json::to_string(&first.body).unwrap(),
        serde_json::to_string(&repeat.body).unwrap()
    );
    let mut turn_two = msgs.clone();
    turn_two.push(ChatMessage::user("next turn"));
    let second = adapter
        .build(&turn_two, &tools, &mut AttemptState::default())
        .unwrap();
    let first_messages = first.body["messages"].as_array().unwrap();
    let second_messages = second.body["messages"].as_array().unwrap();
    assert_eq!(
        &second_messages[..first_messages.len()],
        first_messages.as_slice()
    );
}
