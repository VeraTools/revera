use super::{
    ChatMessage, Completion, LedgerEntry, LedgerHandle, ModelClient, ProviderError, Role, ToolCall,
    ToolSpec, Usage,
};
use crate::config::ModelRoute;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct OpenAiChatClient {
    route: ModelRoute,
    api_key: String,
    http: reqwest::Client,
    ledger: LedgerHandle,
    max_requests: u32,
    retries: u32,
}

impl OpenAiChatClient {
    pub fn new(
        route: ModelRoute,
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
    ) -> Result<Self, ProviderError> {
        let env_name = route.api_key_env.clone().unwrap_or_default();
        let api_key = std::env::var(&env_name)
            .map_err(|_| ProviderError::Other(format!("api_key_env {env_name} is not set")))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(Self {
            route,
            api_key,
            http,
            ledger,
            max_requests,
            retries,
        })
    }

    fn url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.route
                .base_url
                .as_deref()
                .unwrap_or("")
                .trim_end_matches('/')
        )
    }

    fn build_body(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        max_tokens_key: &str,
    ) -> Value {
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
        json!({
            "model": self.route.model,
            "messages": msgs,
            "tools": tool_specs,
            "tool_choice": "auto",
            "temperature": self.route.temperature,
            max_tokens_key: self.route.max_output_tokens,
        })
    }

    fn parse_message(v: &Value) -> Result<ChatMessage, ProviderError> {
        let msg = &v["choices"][0]["message"];
        if msg.is_null() {
            return Err(ProviderError::Other(format!(
                "no choices[0].message in response: {}",
                &v.to_string()[..v.to_string().len().min(500)]
            )));
        }
        let content = msg["content"].as_str().map(|s| s.to_string());
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
        Ok(ChatMessage {
            role: Role::Assistant,
            content,
            tool_calls: calls,
            tool_call_id: None,
            name: None,
        })
    }
}

#[async_trait::async_trait]
impl ModelClient for OpenAiChatClient {
    fn route_label(&self) -> String {
        format!(
            "openai-chat:{}",
            self.route.base_url.as_deref().unwrap_or("")
        )
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        if self.ledger.request_count() >= self.max_requests {
            return Err(ProviderError::BudgetExhausted);
        }
        let mut attempts = 0u32;
        let mut retries_used = 0u32;
        let mut tokens_key = "max_tokens";
        let start = Instant::now();
        loop {
            if self.ledger.request_count() >= self.max_requests {
                return Err(ProviderError::BudgetExhausted);
            }
            attempts += 1;
            let body = self.build_body(messages, tools, tokens_key);
            let mut req = self
                .http
                .post(self.url())
                .bearer_auth(&self.api_key)
                .json(&body);
            for (k, v) in &self.route.extra_headers {
                req = req.header(k, v);
            }
            let result = req.send().await;
            match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<f64>().ok());
                    let text = resp.text().await.unwrap_or_default();
                    if status == 400
                        && tokens_key == "max_tokens"
                        && text.contains("max_completion_tokens")
                    {
                        tokens_key = "max_completion_tokens";
                        continue;
                    }
                    if status == 429 || status >= 500 {
                        if attempts <= self.retries {
                            retries_used += 1;
                            let backoff =
                                retry_after.unwrap_or(0.5 * 2f64.powi(retries_used as i32 - 1));
                            let jitter = (std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.subsec_nanos())
                                .unwrap_or(0)
                                % 250) as f64
                                / 1000.0;
                            tokio::time::sleep(Duration::from_secs_f64(backoff + jitter)).await;
                            continue;
                        }
                        let err = ProviderError::RetryExhausted(format!(
                            "HTTP {status}: {}",
                            &text[..text.len().min(400)]
                        ));
                        self.ledger.record(LedgerEntry {
                            route: self.route_label(),
                            model: self.route.model.clone(),
                            latency_ms: start.elapsed().as_millis() as u64,
                            retries: retries_used,
                            error: Some(err.to_string()),
                            ..Default::default()
                        });
                        return Err(err);
                    }
                    if status >= 400 {
                        let err = ProviderError::Http(format!(
                            "HTTP {status}: {}",
                            &text[..text.len().min(400)]
                        ));
                        self.ledger.record(LedgerEntry {
                            route: self.route_label(),
                            model: self.route.model.clone(),
                            latency_ms: start.elapsed().as_millis() as u64,
                            retries: retries_used,
                            error: Some(err.to_string()),
                            ..Default::default()
                        });
                        return Err(err);
                    }
                    let parsed: Value = serde_json::from_str(&text).map_err(|e| {
                        ProviderError::Other(format!(
                            "bad JSON response: {e}: {}",
                            &text[..text.len().min(300)]
                        ))
                    })?;
                    let message = Self::parse_message(&parsed)?;
                    let usage = Usage {
                        prompt_tokens: parsed["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                        completion_tokens: parsed["usage"]["completion_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                    };
                    let latency = start.elapsed().as_millis() as u64;
                    self.ledger.record(LedgerEntry {
                        route: self.route_label(),
                        model: self.route.model.clone(),
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        latency_ms: latency,
                        retries: retries_used,
                        error: None,
                    });
                    tracing::debug!(
                        route = self.route_label(),
                        model = self.route.model,
                        prompt_tokens = usage.prompt_tokens,
                        latency_ms = latency,
                        "model request"
                    );
                    return Ok(Completion {
                        message,
                        usage,
                        model: self.route.model.clone(),
                        latency_ms: latency,
                    });
                }
                Err(e) => {
                    let transient = e.is_timeout() || e.is_connect();
                    if transient && attempts <= self.retries {
                        retries_used += 1;
                        let backoff = 0.5 * 2f64.powi(retries_used as i32 - 1);
                        let jitter = (std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos())
                            .unwrap_or(0)
                            % 250) as f64
                            / 1000.0;
                        tokio::time::sleep(Duration::from_secs_f64(backoff + jitter)).await;
                        continue;
                    }
                    let err = ProviderError::Http(e.to_string());
                    self.ledger.record(LedgerEntry {
                        route: self.route_label(),
                        model: self.route.model.clone(),
                        latency_ms: start.elapsed().as_millis() as u64,
                        retries: retries_used,
                        error: Some(err.to_string()),
                        ..Default::default()
                    });
                    return Err(err);
                }
            }
        }
    }
}
