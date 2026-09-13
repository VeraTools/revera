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
