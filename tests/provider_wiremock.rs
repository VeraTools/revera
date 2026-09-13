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
        reasoning: Default::default(),
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
    let c = OpenAiChatClient::new(route(&server.uri()), LedgerHandle::new(), 1, 3).unwrap();
    let r = c.complete(&[ChatMessage::user("hi")], &[]).await;
    assert!(matches!(r, Err(ProviderError::BudgetExhausted)), "{r:?}");
    server.verify().await;
}

// ================= M6: new protocol adapters =================

use revera::provider::anthropic::AnthropicAdapter;
use revera::provider::gemini::GeminiAdapter;
use revera::provider::http::{HttpClient, HttpTransport};
use revera::provider::openai_responses::OpenAiResponsesAdapter;
use revera::provider::{Role, ToolCall};

fn route_for(url: &str, proto: Protocol, model: &str) -> ModelRoute {
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    ModelRoute {
        protocol: proto,
        base_url: Some(url.to_string()),
        api_key_env: Some("REVERA_TEST_KEY".into()),
        model: model.into(),
        max_output_tokens: 100,
        temperature: 0.2,
        extra_headers: HashMap::new(),
        script: None,
        reasoning: Default::default(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
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
async fn openai_chat_xhigh_clamps_to_high_for_non_gpt5() {
    let server = MockServer::start().await;
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
            spec(ReasoningEffort::Xhigh, None, ReasoningField::Openai),
        ),
        LedgerHandle::new(),
        10,
        3,
    )
    .unwrap();
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
            transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(LedgerHandle::new(), 10, 3).unwrap(),
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
        transport: HttpTransport::new(ledger.clone(), 10, 3).unwrap(),
    };
    c.complete(&[ChatMessage::user("x")], &[]).await.unwrap();
    server.verify().await;
    let entries = ledger.0.lock().unwrap().entries.clone();
    assert_eq!(entries[0].reasoning_tokens, 7);
    assert_eq!(ledger.totals().3, 7);
}
