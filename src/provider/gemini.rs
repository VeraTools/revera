use super::http::{
    AttemptState, HttpClient, HttpRequestSpec, HttpTransport, Parse, ProtocolAdapter,
};
use super::{ChatMessage, LedgerHandle, ProviderError, Role, ToolCall, ToolSpec, Usage};
use crate::config::ModelRoute;
use serde_json::{json, Value};

/// Google AI (Gemini) generateContent adapter.
pub struct GeminiAdapter {
    route: ModelRoute,
    api_key: String,
    base: String,
}

pub type GeminiClient = HttpClient<GeminiAdapter>;

impl GeminiAdapter {
    pub fn from_route(route: ModelRoute) -> Result<Self, ProviderError> {
        let env_name = route.api_key_env.clone().unwrap_or_default();
        let api_key = std::env::var(&env_name)
            .map_err(|_| ProviderError::Other(format!("api_key_env {env_name} is not set")))?;
        let base = route
            .base_url
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com")
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            route,
            api_key,
            base,
        })
    }
}

impl HttpClient<GeminiAdapter> {
    pub fn new_gemini(
        route: ModelRoute,
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            adapter: GeminiAdapter::from_route(route)?,
            transport: HttpTransport::new(ledger, max_requests, retries)?,
        })
    }
}

/// Gemini rejects a handful of JSON-schema keywords; strip them recursively.
fn strip_schema(v: &mut Value) {
    const BAD: [&str; 4] = ["additionalProperties", "$schema", "default", "examples"];
    match v {
        Value::Object(map) => {
            for k in BAD {
                map.remove(k);
            }
            for (_, vv) in map.iter_mut() {
                strip_schema(vv);
            }
        }
        Value::Array(a) => {
            for vv in a {
                strip_schema(vv);
            }
        }
        _ => {}
    }
}

fn contents(messages: &[ChatMessage]) -> (Vec<String>, Vec<Value>) {
    let mut system: Vec<String> = vec![];
    let mut out: Vec<Value> = vec![];
    let push = |role: &str, parts: Vec<Value>, out: &mut Vec<Value>| {
        if let Some(last) = out.last_mut() {
            if last["role"] == role {
                if let Some(a) = last["parts"].as_array_mut() {
                    a.extend(parts);
                    return;
                }
            }
        }
        out.push(json!({"role": role, "parts": parts}));
    };
    for m in messages {
        match m.role {
            Role::System => {
                if let Some(c) = &m.content {
                    system.push(c.clone());
                }
            }
            Role::User => {
                push(
                    "user",
                    vec![json!({"text": m.content.clone().unwrap_or_default()})],
                    &mut out,
                );
            }
            Role::Assistant => {
                let mut parts: Vec<Value> = vec![];
                if let Some(c) = &m.content {
                    parts.push(json!({"text": c}));
                }
                for t in &m.tool_calls {
                    parts.push(json!({
                        "functionCall": {"name": t.name, "args": t.arguments}
                    }));
                }
                push("model", parts, &mut out);
            }
            Role::Tool => {
                // tool result; Gemini has no call ids — the tool name was
                // preserved in ChatMessage.name (synthesized id "<name>-<idx>"
                // maps back by stripping the trailing -<idx>)
                let name = m
                    .name
                    .clone()
                    .or_else(|| {
                        m.tool_call_id
                            .as_deref()
                            .and_then(|id| id.rsplit_once('-').map(|(n, _)| n.to_string()))
                    })
                    .unwrap_or_default();
                let output = m.content.clone().unwrap_or_default();
                let response = serde_json::from_str::<Value>(&output)
                    .unwrap_or_else(|_| json!({"text": output}));
                push(
                    "user",
                    vec![
                        json!({"functionResponse": {"name": name, "response": {"content": response}}}),
                    ],
                    &mut out,
                );
            }
        }
    }
    (system, out)
}

impl ProtocolAdapter for GeminiAdapter {
    fn label(&self) -> String {
        format!("gemini:{}", self.base)
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
        let (system, contents) = contents(messages);
        let decls: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut params = t.parameters.clone();
                strip_schema(&mut params);
                json!({"name": t.name, "description": t.description, "parameters": params})
            })
            .collect();
        let mut body = json!({
            "contents": contents,
            "tools": [{"functionDeclarations": decls}],
            "generationConfig": {
                "temperature": self.route.temperature,
                "maxOutputTokens": self.route.max_output_tokens,
            },
        });
        if !system.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": system.join("\n\n")}]});
        }
        let mut headers = vec![
            ("x-goog-api-key".to_string(), self.api_key.clone()),
            ("content-type".to_string(), "application/json".into()),
        ];
        for (k, v) in &self.route.extra_headers {
            headers.push((k.clone(), v.clone()));
        }
        Ok(HttpRequestSpec {
            url: format!(
                "{}/v1beta/models/{}:generateContent",
                self.base, self.route.model
            ),
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
        let mut text_parts: Vec<String> = vec![];
        let mut calls = vec![];
        let mut idx = 0u32;
        if let Some(parts) = parsed["candidates"][0]["content"]["parts"].as_array() {
            for p in parts {
                if let Some(t) = p["text"].as_str() {
                    text_parts.push(t.to_string());
                }
                if let Some(fc) = p.get("functionCall") {
                    let name = fc["name"].as_str().unwrap_or("").to_string();
                    calls.push(ToolCall {
                        id: format!("{name}-{idx}"),
                        name,
                        arguments: fc["args"].clone(),
                    });
                    idx += 1;
                }
            }
        }
        if calls.is_empty() && text_parts.is_empty() {
            return Parse::Err(ProviderError::Other(format!(
                "no candidates[0].content.parts in response: {}",
                crate::text::excerpt_bytes(&parsed.to_string(), 500)
            )));
        }
        let u = &parsed["usageMetadata"];
        let usage = Usage {
            prompt_tokens: u["promptTokenCount"].as_u64().unwrap_or(0),
            completion_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0),
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
            },
            usage,
        )
    }
}
