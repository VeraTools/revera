use super::http::{
    detect_400_fallback, AttemptState, HttpClient, HttpRequestSpec, HttpTransport, Parse,
    ProtocolAdapter,
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
                // per-part thoughtSignatures must ride back on the echoed
                // text/functionCall parts they were returned on
                let sigs = m.provider_state.as_ref();
                let text_sig = sigs.and_then(|v| v["text"].as_str()).map(|v| v.to_string());
                if let Some(c) = &m.content {
                    let mut p = json!({"text": c});
                    if let Some(sig) = text_sig {
                        p["thoughtSignature"] = json!(sig);
                    }
                    parts.push(p);
                }
                for (i, t) in m.tool_calls.iter().enumerate() {
                    let mut p = json!({
                        "functionCall": {"name": t.name, "args": t.arguments}
                    });
                    if let Some(sig) = sigs
                        .and_then(|v| v["calls"][i].as_str())
                        .map(|v| v.to_string())
                    {
                        p["thoughtSignature"] = json!(sig);
                    }
                    parts.push(p);
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
        attempt: &mut AttemptState,
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
        let mut gen = json!({
            "temperature": self.route.temperature,
            "maxOutputTokens": self.route.max_output_tokens,
        });
        let r = &self.route.reasoning;
        if !attempt.drop_reasoning {
            if self.route.model.starts_with("gemini-3") {
                // gemini-3 takes a thinking level, not a token budget
                if r.enabled() {
                    let level = match r.effort() {
                        crate::config::ReasoningEffort::Minimal
                        | crate::config::ReasoningEffort::Low => "low",
                        _ => "high",
                    };
                    gen["thinkingConfig"] = json!({"thinkingLevel": level});
                }
            } else if r.enabled() {
                gen["thinkingConfig"] = json!({
                    "thinkingBudget": r.effective_budget(),
                    "includeThoughts": false,
                });
            } else if !self.route.model.starts_with("gemini-2.5-pro") {
                // 2.5-pro cannot disable thinking; others get budget 0
                gen["thinkingConfig"] = json!({"thinkingBudget": 0, "includeThoughts": false});
            }
        }
        let mut body = json!({
            "contents": contents,
            "tools": [{"functionDeclarations": decls}],
            "generationConfig": gen,
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

    fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse {
        if let Some(p) = detect_400_fallback(status, body, attempt, &["thinking"]) {
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
        let mut text_sig: Option<String> = None;
        let mut call_sigs: Vec<Value> = vec![];
        let mut idx = 0u32;
        if let Some(parts) = parsed["candidates"][0]["content"]["parts"].as_array() {
            for p in parts {
                let sig = p["thoughtSignature"].as_str().map(|v| v.to_string());
                if let Some(t) = p["text"].as_str() {
                    text_parts.push(t.to_string());
                    if let Some(sv) = &sig {
                        text_sig = Some(sv.clone());
                    }
                }
                if let Some(fc) = p.get("functionCall") {
                    let name = fc["name"].as_str().unwrap_or("").to_string();
                    calls.push(ToolCall {
                        id: format!("{name}-{idx}"),
                        name,
                        arguments: fc["args"].clone(),
                    });
                    call_sigs.push(match sig {
                        Some(s) => json!(s),
                        None => Value::Null,
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
            reasoning_tokens: u["thoughtsTokenCount"].as_u64().unwrap_or(0),
        };
        let provider_state = if text_sig.is_some() || call_sigs.iter().any(|s| !s.is_null()) {
            Some(json!({"text": text_sig, "calls": call_sigs}))
        } else {
            None
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
                provider_state,
            },
            usage,
        )
    }
}
