use super::http::{
    capped_effort, detect_400_fallback, route_headers, AttemptState, HttpClient, HttpRequestSpec,
    HttpTransport, Parse, ProtocolAdapter,
};
use super::{ChatMessage, LedgerHandle, ProviderError, Role, ToolCall, ToolSpec, Usage};
use crate::config::{ModelRoute, ReasoningEffort, ReasoningField};
use serde_json::{json, Value};

/// OpenAI chat-completions adapter. Wire format unchanged from before the
/// transport split; the max_tokens -> max_completion_tokens fallback rides
/// `AttemptState.tokens_key`.
pub struct OpenAiChatAdapter {
    route: ModelRoute,
    api_key: String,
}

pub type OpenAiChatClient = HttpClient<OpenAiChatAdapter>;

impl OpenAiChatAdapter {
    pub fn from_route(route: ModelRoute) -> Result<Self, ProviderError> {
        let env_name = route.api_key_env.clone().unwrap_or_default();
        let api_key = std::env::var(&env_name)
            .map_err(|_| ProviderError::Other(format!("api_key_env {env_name} is not set")))?;
        Ok(Self { route, api_key })
    }

    /// Reasoning field spelling for this route (auto -> openrouter host).
    fn reasoning_field(&self) -> ReasoningField {
        match self.route.reasoning.field() {
            ReasoningField::Auto => {
                if self
                    .route
                    .base_url
                    .as_deref()
                    .unwrap_or("")
                    .contains("openrouter.ai")
                {
                    ReasoningField::Openrouter
                } else {
                    ReasoningField::Openai
                }
            }
            f => f,
        }
    }

    /// Effort `build()` emits for this attempt state.
    fn effort(&self, attempt: &AttemptState) -> ReasoningEffort {
        capped_effort(self.route.reasoning.effort(), attempt)
    }
}

impl OpenAiChatClient {
    pub fn new(
        route: ModelRoute,
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
        role: &str,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            adapter: OpenAiChatAdapter::from_route(route)?,
            transport: HttpTransport::new(ledger, max_requests, retries, role)?,
        })
    }
}

impl ProtocolAdapter for OpenAiChatAdapter {
    fn label(&self) -> String {
        format!(
            "openai-chat:{}",
            self.route.base_url.as_deref().unwrap_or("")
        )
    }

    fn model(&self) -> &str {
        &self.route.model
    }

    fn requested_reasoning(&self) -> String {
        if self.route.reasoning.enabled() {
            self.route.reasoning.effort().as_str().to_string()
        } else {
            "none".into()
        }
    }

    fn effective_reasoning(&self, attempt: &AttemptState) -> String {
        if !self.route.reasoning.enabled() || attempt.drop_reasoning {
            "none".into()
        } else {
            self.effort(attempt).as_str().to_string()
        }
    }

    fn build(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError> {
        let url = format!(
            "{}/chat/completions",
            self.route
                .base_url
                .as_deref()
                .unwrap_or("")
                .trim_end_matches('/')
        );
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| {
                let mut v = json!({
                    "role": match m.role {
                        Role::System => "system",
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => "tool",
                    }
                });
                if let Some(c) = &m.content {
                    v["content"] = json!(c);
                }
                if !m.tool_calls.is_empty() {
                    v["tool_calls"] = Value::Array(
                        m.tool_calls
                            .iter()
                            .map(|t| {
                                json!({
                                    "id": t.id,
                                    "type": "function",
                                    "function": {"name": t.name, "arguments": t.arguments.to_string()}
                                })
                            })
                            .collect(),
                    );
                }
                if let Some(id) = &m.tool_call_id {
                    v["tool_call_id"] = json!(id);
                }
                if let Some(n) = &m.name {
                    v["name"] = json!(n);
                }
                v
            })
            .collect();
        let tool_specs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {"name": t.name, "description": t.description, "parameters": t.parameters}
                })
            })
            .collect();
        let mut body = json!({
            "model": self.route.model,
            "messages": msgs,
            "tools": tool_specs,
            "tool_choice": "auto",
            attempt.tokens_key.clone(): self.route.max_output_tokens,
        });
        if !attempt.drop_temperature {
            body["temperature"] = json!(self.route.temperature);
        }
        // reasoning wire field: openai (reasoning_effort) vs openrouter
        // (reasoning object); auto picks by base_url host. The configured
        // effort is sent as requested; `attempt.reasoning_cap` lowers it
        // only after a provider 400.
        let r = &self.route.reasoning;
        if r.enabled() && !attempt.drop_reasoning {
            match self.reasoning_field() {
                ReasoningField::Openai => {
                    body["reasoning_effort"] = json!(self.effort(attempt).as_str());
                }
                ReasoningField::Openrouter => {
                    body["reasoning"] = match r.budget_tokens() {
                        Some(b) => json!({"max_tokens": b}),
                        None => json!({"effort": self.effort(attempt).as_str()}),
                    };
                }
                ReasoningField::Auto => unreachable!(),
            }
        }
        let mut headers = vec![
            (
                "authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            ("content-type".to_string(), "application/json".into()),
        ];
        headers.extend(route_headers(&self.route));
        Ok(HttpRequestSpec { url, headers, body })
    }

    fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse {
        if status == 400
            && attempt.tokens_key == "max_tokens"
            && body.contains("max_completion_tokens")
        {
            attempt.tokens_key = "max_completion_tokens".into();
            return Parse::RetrySameSlot("400: retrying with max_completion_tokens".into());
        }
        if let Some(p) = detect_400_fallback(
            status,
            body,
            attempt,
            &["reasoning", "reasoning_effort"],
            self.route.reasoning.effort(),
        ) {
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
        let msg = &parsed["choices"][0]["message"];
        if msg.is_null() {
            return Parse::Err(ProviderError::Other(format!(
                "no choices[0].message in response: {}",
                crate::text::excerpt_bytes(&parsed.to_string(), 500)
            )));
        }
        // content may be a string, null, or an array of parts
        // (e.g. [{"type":"text","text":"..."}]) — join the text parts.
        let content = match &msg["content"] {
            Value::String(s) => Some(s.clone()),
            Value::Array(parts) => {
                let text: String = parts
                    .iter()
                    .filter(|p| p["type"].as_str().unwrap_or("text") == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            _ => None,
        };
        let mut calls = Vec::new();
        if let Some(arr) = msg["tool_calls"].as_array() {
            for t in arr {
                let name = t["function"]["name"].as_str().unwrap_or("").to_string();
                let raw = t["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments = serde_json::from_str(raw).unwrap_or_else(|_| json!({"_raw": raw}));
                calls.push(ToolCall {
                    id: t["id"].as_str().unwrap_or("").to_string(),
                    name,
                    arguments,
                });
            }
        }
        if parsed["choices"][0]["finish_reason"].as_str() == Some("length")
            && content.is_none()
            && calls.is_empty()
        {
            return Parse::Err(ProviderError::Other(
                "output truncated at max_output_tokens with empty content; raise max_output_tokens or lower reasoning effort"
                    .into(),
            ));
        }
        let usage = Usage {
            prompt_tokens: parsed["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            completion_tokens: parsed["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            // response `reasoning_content`/`reasoning` fields are ignored
            // except usage accounting (completion_tokens_details)
            reasoning_tokens: parsed["usage"]["completion_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
            // OpenAI caches long prefixes automatically; this reports the hit
            cached_tokens: parsed["usage"]["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        };
        Parse::Ok(
            ChatMessage {
                role: Role::Assistant,
                content,
                tool_calls: calls,
                tool_call_id: None,
                name: None,
                provider_state: None,
            },
            usage,
        )
    }
}
