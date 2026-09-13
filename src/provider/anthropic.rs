use super::http::{
    AttemptState, HttpClient, HttpRequestSpec, HttpTransport, Parse, ProtocolAdapter,
};
use super::{ChatMessage, LedgerHandle, ProviderError, Role, ToolCall, ToolSpec, Usage};
use crate::config::ModelRoute;
use serde_json::{json, Value};

/// Anthropic Messages API adapter (POST {base}/v1/messages).
pub struct AnthropicAdapter {
    route: ModelRoute,
    api_key: String,
    base: String,
}

pub type AnthropicClient = HttpClient<AnthropicAdapter>;

impl AnthropicAdapter {
    pub fn from_route(route: ModelRoute) -> Result<Self, ProviderError> {
        let env_name = route.api_key_env.clone().unwrap_or_default();
        let api_key = std::env::var(&env_name)
            .map_err(|_| ProviderError::Other(format!("api_key_env {env_name} is not set")))?;
        // default base; strip a trailing /v1 if the user already supplied it
        let base = route
            .base_url
            .as_deref()
            .unwrap_or("https://api.anthropic.com")
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .to_string();
        Ok(Self {
            route,
            api_key,
            base,
        })
    }
}

impl HttpClient<AnthropicAdapter> {
    pub fn new_anthropic(
        route: ModelRoute,
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            adapter: AnthropicAdapter::from_route(route)?,
            transport: HttpTransport::new(ledger, max_requests, retries)?,
        })
    }
}

/// Anthropic requires strictly alternating user/assistant roles; merge
/// consecutive same-role content into blocks. Consecutive tool results become
/// one user message of tool_result blocks.
fn to_blocks(messages: &[ChatMessage]) -> (Vec<String>, Vec<Value>) {
    let mut system: Vec<String> = vec![];
    let mut out: Vec<Value> = vec![];
    for m in messages {
        match m.role {
            Role::System => {
                if let Some(c) = &m.content {
                    system.push(c.clone());
                }
            }
            Role::Assistant => {
                let mut parts: Vec<Value> = vec![];
                if let Some(c) = &m.content {
                    parts.push(json!({"type": "text", "text": c}));
                }
                for t in &m.tool_calls {
                    parts.push(json!({
                        "type": "tool_use",
                        "id": t.id,
                        "name": t.name,
                        "input": t.arguments,
                    }));
                }
                push_role(&mut out, "assistant", parts);
            }
            Role::Tool => {
                // tool result -> user message with tool_result block;
                // consecutive results merge into the same user message
                let blk = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id,
                    "content": m.content.clone().unwrap_or_default(),
                });
                if let Some(last) = out.last_mut() {
                    if last["role"] == "user"
                        && last["content"]
                            .as_array()
                            .map(|a| a.iter().all(|b| b["type"] == "tool_result"))
                            .unwrap_or(false)
                    {
                        last["content"].as_array_mut().unwrap().push(blk);
                        continue;
                    }
                }
                out.push(json!({"role": "user", "content": [blk]}));
            }
            Role::User => {
                let parts = vec![json!({
                    "type": "text",
                    "text": m.content.clone().unwrap_or_default(),
                })];
                push_role(&mut out, "user", parts);
            }
        }
    }
    (system, out)
}

/// Append `parts` under `role`, merging into the previous message when the
/// roles match (keeps strict alternation).
fn push_role(out: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = out.last_mut() {
        if last["role"] == role {
            if let Some(a) = last["content"].as_array_mut() {
                a.extend(parts);
                return;
            }
        }
    }
    out.push(json!({"role": role, "content": parts}));
}

impl ProtocolAdapter for AnthropicAdapter {
    fn label(&self) -> String {
        format!("anthropic:{}", self.base)
    }

    fn model(&self) -> &str {
        &self.route.model
    }

    fn build(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        _attempt: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError> {
        let (system, msgs) = to_blocks(messages);
        let tool_specs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        let mut body = json!({
            "model": self.route.model,
            "messages": msgs,
            "tools": tool_specs,
            "tool_choice": {"type": "auto"},
            "max_tokens": self.route.max_output_tokens,
            "temperature": self.route.temperature,
        });
        if !system.is_empty() {
            body["system"] = json!(system.join("\n\n"));
        }
        let mut headers = vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".into()),
            ("content-type".to_string(), "application/json".into()),
        ];
        for (k, v) in &self.route.extra_headers {
            headers.push((k.clone(), v.clone()));
        }
        Ok(HttpRequestSpec {
            url: format!("{}/v1/messages", self.base),
            headers,
            body,
        })
    }

    fn parse(&self, status: u16, body: &str, _attempt: &mut AttemptState) -> Parse {
        if status >= 400 {
            return Parse::Err(ProviderError::Http(format!(
                "HTTP {status}: {}",
                crate::text::excerpt_bytes(body, 400)
            )));
        }
        let parsed: Value = match serde_json::from_str(body) {
            Ok(p) => p,
            Err(e) => {
                return Parse::Err(ProviderError::Other(format!(
                    "bad JSON response: {e}: {}",
                    crate::text::excerpt_bytes(body, 300)
                )));
            }
        };
        let mut content_parts: Vec<String> = vec![];
        let mut calls = vec![];
        if let Some(arr) = parsed["content"].as_array() {
            for b in arr {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str() {
                            content_parts.push(t.to_string());
                        }
                    }
                    Some("tool_use") => {
                        calls.push(ToolCall {
                            id: b["id"].as_str().unwrap_or("").to_string(),
                            name: b["name"].as_str().unwrap_or("").to_string(),
                            arguments: b["input"].clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        let usage = Usage {
            prompt_tokens: parsed["usage"]["input_tokens"].as_u64().unwrap_or(0),
            completion_tokens: parsed["usage"]["output_tokens"].as_u64().unwrap_or(0),
        };
        Parse::Ok(
            ChatMessage {
                role: Role::Assistant,
                content: if content_parts.is_empty() {
                    None
                } else {
                    Some(content_parts.join("\n"))
                },
                tool_calls: calls,
                tool_call_id: None,
                name: None,
            },
            usage,
        )
    }
}
