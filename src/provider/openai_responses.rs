use super::http::{
    detect_400_fallback, AttemptState, HttpClient, HttpRequestSpec, HttpTransport, Parse,
    ProtocolAdapter,
};
use super::{ChatMessage, LedgerHandle, ProviderError, Role, ToolCall, ToolSpec, Usage};
use crate::config::ModelRoute;
use serde_json::{json, Value};

/// OpenAI Responses API adapter (POST {base}/responses).
pub struct OpenAiResponsesAdapter {
    route: ModelRoute,
    api_key: String,
    base: String,
}

pub type OpenAiResponsesClient = HttpClient<OpenAiResponsesAdapter>;

impl OpenAiResponsesAdapter {
    pub fn from_route(route: ModelRoute) -> Result<Self, ProviderError> {
        let env_name = route.api_key_env.clone().unwrap_or_default();
        let api_key = std::env::var(&env_name)
            .map_err(|_| ProviderError::Other(format!("api_key_env {env_name} is not set")))?;
        let base = route
            .base_url
            .as_deref()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            route,
            api_key,
            base,
        })
    }
}

impl HttpClient<OpenAiResponsesAdapter> {
    pub fn new_responses(
        route: ModelRoute,
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            adapter: OpenAiResponsesAdapter::from_route(route)?,
            transport: HttpTransport::new(ledger, max_requests, retries)?,
        })
    }
}

/// Responses API `input`: system -> top-level `instructions`; user/assistant
/// text -> {role, content:[{type: input_text|output_text}]}; assistant tool
/// calls -> function_call items; tool results -> function_call_output items.
fn to_input(messages: &[ChatMessage]) -> (Vec<String>, Vec<Value>) {
    let mut instructions: Vec<String> = vec![];
    let mut out: Vec<Value> = vec![];
    for m in messages {
        match m.role {
            Role::System => {
                if let Some(c) = &m.content {
                    instructions.push(c.clone());
                }
            }
            Role::User => {
                out.push(json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": m.content.clone().unwrap_or_default()}],
                }));
            }
            Role::Assistant => {
                if let Some(c) = &m.content {
                    out.push(json!({
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": c}],
                    }));
                }
                // reasoning items go immediately before this turn's
                // function_call items (store:false echo-back)
                if let Some(Value::Array(items)) = &m.provider_state {
                    out.extend(items.iter().cloned());
                }
                for t in &m.tool_calls {
                    out.push(json!({
                        "type": "function_call",
                        "call_id": t.id,
                        "name": t.name,
                        "arguments": t.arguments.to_string(),
                    }));
                }
            }
            Role::Tool => {
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "output": m.content.clone().unwrap_or_default(),
                }));
            }
        }
    }
    (instructions, out)
}

impl ProtocolAdapter for OpenAiResponsesAdapter {
    fn label(&self) -> String {
        format!("openai-responses:{}", self.base)
    }

    fn model(&self) -> &str {
        &self.route.model
    }

    fn build(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError> {
        let (instructions, input) = to_input(messages);
        let tool_specs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false,
                })
            })
            .collect();
        let mut body = json!({
            "model": self.route.model,
            "input": input,
            "tools": tool_specs,
            "tool_choice": "auto",
            "max_output_tokens": self.route.max_output_tokens,
            "store": false,
        });
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions.join("\n\n"));
        }
        if !attempt.drop_temperature {
            body["temperature"] = json!(self.route.temperature);
        }
        let r = &self.route.reasoning;
        if r.enabled() && !attempt.drop_reasoning {
            body["reasoning"] = json!({"effort": r.effort().as_str()});
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        let mut headers = vec![
            (
                "authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            ("content-type".to_string(), "application/json".into()),
        ];
        for (k, v) in &self.route.extra_headers {
            headers.push((k.clone(), v.clone()));
        }
        Ok(HttpRequestSpec {
            url: format!("{}/responses", self.base),
            headers,
            body,
        })
    }

    fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse {
        // reasoning or temperature 400 -> drop the named field, same slot
        if let Some(p) = detect_400_fallback(status, body, attempt, &["reasoning"]) {
            return p;
        }
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
        let mut text_parts: Vec<String> = vec![];
        let mut calls = vec![];
        let mut provider_state: Vec<Value> = vec![];
        if let Some(arr) = parsed["output"].as_array() {
            for item in arr {
                match item["type"].as_str() {
                    Some("message") => {
                        if let Some(content) = item["content"].as_array() {
                            for c in content {
                                if c["type"].as_str() == Some("output_text") {
                                    if let Some(t) = c["text"].as_str() {
                                        text_parts.push(t.to_string());
                                    }
                                }
                            }
                        }
                    }
                    Some("reasoning") => {
                        provider_state.push(item.clone());
                    }
                    Some("function_call") => {
                        let raw = item["arguments"].as_str().unwrap_or("{}");
                        calls.push(ToolCall {
                            id: item["call_id"].as_str().unwrap_or("").to_string(),
                            name: item["name"].as_str().unwrap_or("").to_string(),
                            arguments: serde_json::from_str(raw)
                                .unwrap_or_else(|_| json!({"_raw": raw})),
                        });
                    }
                    _ => {}
                }
            }
        }
        if calls.is_empty() && text_parts.is_empty() {
            return Parse::Err(ProviderError::Other(format!(
                "no usable output items in response: {}",
                crate::text::excerpt_bytes(&parsed.to_string(), 500)
            )));
        }
        let usage = Usage {
            prompt_tokens: parsed["usage"]["input_tokens"].as_u64().unwrap_or(0),
            completion_tokens: parsed["usage"]["output_tokens"].as_u64().unwrap_or(0),
            reasoning_tokens: parsed["usage"]["output_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
        };
        Parse::Ok(
            ChatMessage {
                role: Role::Assistant,
                content: if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join("\n"))
                },
                tool_calls: calls,
                tool_call_id: None,
                name: None,
                provider_state: if provider_state.is_empty() {
                    None
                } else {
                    Some(Value::Array(provider_state))
                },
            },
            usage,
        )
    }
}
