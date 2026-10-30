use super::http::{
    AttemptState, HttpClient, HttpRequestSpec, HttpTransport, Parse, ProtocolAdapter,
    detect_400_fallback, route_headers,
};
use super::{ChatMessage, LedgerHandle, ProviderError, Role, ToolCall, ToolSpec, Usage};
use crate::config::{ModelRoute, ReasoningEffort};
use serde_json::{Value, json};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThinkingMode {
    Adaptive,
    Budget,
}

/// The thinking mode a 400 response says the model does not support, if any.
fn rejected_thinking_mode(body: &str) -> Option<ThinkingMode> {
    // `"thinking.type.enabled" is not supported ... Use "thinking.type.adaptive"`,
    // or a gateway that rewrote the request: `thinking.adaptive.budget_tokens`
    if body.contains("thinking.type.enabled") || body.contains("thinking.adaptive.") {
        return Some(ThinkingMode::Budget);
    }
    // `adaptive thinking is not supported on this model` and similar; a
    // rejected effort level is left to the effort step-down instead
    let lower = body.to_ascii_lowercase();
    (lower.contains("adaptive")
        && (lower.contains("not supported") || lower.contains("unsupported"))
        && !lower.contains("effort"))
    .then_some(ThinkingMode::Adaptive)
}

/// Anthropic Messages API adapter (POST {base}/v1/messages).
pub struct AnthropicAdapter {
    route: ModelRoute,
    api_key: String,
    base: String,
    /// A rejected preferred mode can switch once, for this client's lifetime.
    learned_thinking_mode: OnceLock<ThinkingMode>,
}

pub type AnthropicClient = HttpClient<AnthropicAdapter>;

impl AnthropicAdapter {
    pub fn from_route(route: ModelRoute) -> Result<Self, ProviderError> {
        let env_name = route.api_key_env.clone().unwrap_or_default();
        let api_key =
            crate::redact::secret_env(&env_name).ok_or(ProviderError::MissingKey(env_name))?;
        Ok(Self::with_api_key(route, api_key))
    }

    pub fn with_api_key(route: ModelRoute, api_key: String) -> Self {
        crate::redact::register(&api_key);
        // default base; strip a trailing /v1 if the user already supplied it
        let base = route
            .base_url
            .as_deref()
            .unwrap_or("https://api.anthropic.com")
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .to_string();
        Self {
            route,
            api_key,
            base,
            learned_thinking_mode: OnceLock::new(),
        }
    }
}

impl HttpClient<AnthropicAdapter> {
    pub fn new_anthropic(
        route: ModelRoute,
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
        role: &str,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            adapter: AnthropicAdapter::from_route(route)?,
            transport: HttpTransport::new(ledger, max_requests, retries, role)?,
        })
    }
}

/// Anthropic requires strictly alternating user/assistant roles; merge
/// consecutive same-role content into blocks. Consecutive tool results become
/// one user message of tool_result blocks.
fn to_blocks(messages: &[ChatMessage], drop_reasoning: bool) -> (Vec<String>, Vec<Value>) {
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
                // verbatim thinking/redacted_thinking blocks lead the
                // assistant message (required for echo-back); skipped once
                // a 400 has dropped thinking for this attempt
                if !drop_reasoning && let Some(Value::Array(items)) = &m.provider_state {
                    parts.extend(items.iter().cloned());
                }
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
                if let Some(last) = out.last_mut()
                    && last["role"] == "user"
                    && last["content"]
                        .as_array()
                        .map(|a| a.iter().all(|b| b["type"] == "tool_result"))
                        .unwrap_or(false)
                {
                    last["content"].as_array_mut().unwrap().push(blk);
                    continue;
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
    if let Some(last) = out.last_mut()
        && last["role"] == role
        && let Some(a) = last["content"].as_array_mut()
    {
        a.extend(parts);
        return;
    }
    out.push(json!({"role": role, "content": parts}));
}

impl AnthropicAdapter {
    fn thinking_mode(&self) -> ThinkingMode {
        self.learned_thinking_mode
            .get()
            .copied()
            .unwrap_or_else(|| {
                if self.route.reasoning.budget_tokens().is_some() {
                    ThinkingMode::Budget
                } else {
                    ThinkingMode::Adaptive
                }
            })
    }

    /// Effort as sent: adaptive thinking has no `minimal`, so it sends `low`.
    fn sent_effort(&self, attempt: &AttemptState) -> &'static str {
        let effort = super::http::capped_effort(self.route.reasoning.effort(), attempt);
        match (self.thinking_mode(), effort) {
            (ThinkingMode::Adaptive, ReasoningEffort::Minimal) => "low",
            _ => effort.as_str(),
        }
    }

    /// Thinking budget `build()` emits for this attempt state. Effort is
    /// mapped to a budget (capped by `attempt.reasoning_cap` like the
    /// openai adapters); explicit budget_tokens still wins.
    fn budget(&self, attempt: &AttemptState) -> u64 {
        let eff = super::http::capped_effort(self.route.reasoning.effort(), attempt);
        self.route
            .reasoning
            .budget_tokens()
            .unwrap_or_else(|| eff.budget())
    }
}

impl ProtocolAdapter for AnthropicAdapter {
    fn label(&self) -> String {
        format!("anthropic:{}", crate::config::identity_url(&self.base))
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
            self.sent_effort(attempt).to_string()
        }
    }

    fn build(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError> {
        let (system, mut msgs) = to_blocks(messages, attempt.drop_reasoning);
        let mut tool_specs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        let cache_hint = self.route.cache && !attempt.drop_cache_hint;
        if cache_hint {
            if let Some(last) = tool_specs.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
            let len = msgs.len();
            for message in msgs.iter_mut().skip(len.saturating_sub(2)) {
                if let Some(blocks) = message["content"].as_array_mut()
                    && let Some(block) = blocks.iter_mut().rev().find(|block| {
                        !matches!(
                            block["type"].as_str(),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                {
                    block["cache_control"] = json!({"type": "ephemeral"});
                }
            }
        }
        let max_out = self.route.max_output_tokens as u64;
        let r = &self.route.reasoning;
        let thinking_on = r.enabled() && !attempt.drop_reasoning;
        let mut max_tokens = max_out;
        let mut body = json!({
            "model": self.route.model,
            "messages": msgs,
            "tools": tool_specs,
            "tool_choice": {"type": "auto"},
        });
        if thinking_on {
            match self.thinking_mode() {
                ThinkingMode::Adaptive => {
                    body["thinking"] = json!({"type": "adaptive"});
                    body["output_config"] = json!({"effort": self.sent_effort(attempt)});
                    // max_output_tokens caps thinking plus the answer.
                }
                ThinkingMode::Budget => {
                    // thinking budget counts toward max_tokens; raise max_tokens
                    // when the budget leaves too little headroom for the answer
                    let b = self.budget(attempt).min(max_out.saturating_sub(1024));
                    if max_out <= b + 1024 {
                        max_tokens = b + max_out;
                    }
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": b});
                }
            }
        }
        if !thinking_on && !attempt.drop_temperature {
            // temperature is rejected when extended thinking is enabled
            body["temperature"] = json!(self.route.temperature);
        }
        body["max_tokens"] = json!(max_tokens);
        if !system.is_empty() {
            if cache_hint {
                let mut system_blocks: Vec<Value> = system
                    .into_iter()
                    .map(|text| json!({"type": "text", "text": text}))
                    .collect();
                if let Some(last) = system_blocks.last_mut() {
                    last["cache_control"] = json!({"type": "ephemeral"});
                }
                body["system"] = Value::Array(system_blocks);
            } else {
                body["system"] = json!(system.join("\n\n"));
            }
        }
        let mut headers = vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".into()),
            ("content-type".to_string(), "application/json".into()),
        ];
        headers.extend(route_headers(&self.route));
        Ok(HttpRequestSpec {
            url: format!("{}/v1/messages", self.base),
            headers,
            body,
        })
    }

    fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse {
        if status == 400
            && self.route.reasoning.enabled()
            && !attempt.drop_reasoning
            && !attempt.thinking_mode_retried
            && let Some(rejected) = rejected_thinking_mode(body)
        {
            let target = match rejected {
                ThinkingMode::Adaptive => ThinkingMode::Budget,
                ThinkingMode::Budget => ThinkingMode::Adaptive,
            };
            if self.thinking_mode() == rejected {
                // Switch at most once per client; a second rejection falls through.
                let _ = self.learned_thinking_mode.set(target);
            }
            // Also retries a request built before a concurrent call switched.
            if self.thinking_mode() == target {
                attempt.thinking_mode_retried = true;
                return Parse::RetrySameSlot(
                    match target {
                        ThinkingMode::Budget => "400: model does not support adaptive thinking; retrying with a thinking budget",
                        ThinkingMode::Adaptive => "400: model does not support thinking budgets; retrying with adaptive thinking",
                    }
                    .into(),
                );
            }
        }
        if status == 400
            && self.route.cache
            && !attempt.drop_cache_hint
            && body.contains("cache_control")
        {
            attempt.drop_cache_hint = true;
            return Parse::RetrySameSlot("400: retrying without prompt cache hint".into());
        }
        if let Some(p) = detect_400_fallback(
            status,
            body,
            attempt,
            &["thinking", "output_config", "effort"],
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
        let mut content_parts: Vec<String> = vec![];
        let mut calls = vec![];
        let mut provider_state: Vec<Value> = vec![];
        if let Some(arr) = parsed["content"].as_array() {
            for b in arr {
                match b["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = b["text"].as_str() {
                            content_parts.push(t.to_string());
                        }
                    }
                    Some("thinking") | Some("redacted_thinking") => {
                        provider_state.push(b.clone());
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
        if parsed["stop_reason"].as_str() == Some("max_tokens")
            && content_parts.is_empty()
            && calls.is_empty()
        {
            return Parse::Err(ProviderError::Other(
                "output truncated at max_tokens with empty content; raise max_tokens or lower reasoning effort"
                    .into(),
            ));
        }
        let usage = Usage {
            prompt_tokens: parsed["usage"]["input_tokens"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(
                    parsed["usage"]["cache_read_input_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                )
                .saturating_add(
                    parsed["usage"]["cache_creation_input_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                ),
            completion_tokens: parsed["usage"]["output_tokens"].as_u64().unwrap_or(0),
            cached_prompt_tokens: parsed["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0),
            cache_write_tokens: parsed["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or(0),
            // anthropic reports thinking under output_tokens; no split
            reasoning_tokens: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(reasoning: &str) -> AnthropicAdapter {
        let route: ModelRoute = serde_saphyr::from_str(&format!(
            "protocol: anthropic\nmodel: claude-test\napi_key_env: ANTHROPIC_TEST_KEY\nreasoning: {reasoning}\n"
        ))
        .unwrap();
        AnthropicAdapter::with_api_key(route, "anthropic-unit-test-key".into())
    }

    #[test]
    fn classifies_thinking_mode_rejections() {
        for body in [
            "adaptive thinking is not supported on this model",
            "thinking.type.adaptive is not supported for this model",
            "Adaptive thinking unsupported",
        ] {
            assert_eq!(
                rejected_thinking_mode(body),
                Some(ThinkingMode::Adaptive),
                "{body}"
            );
        }
        for body in [
            r#""thinking.type.enabled" is not supported for this model. Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior."#,
            "thinking.adaptive.budget_tokens: Extra inputs are not permitted",
        ] {
            assert_eq!(
                rejected_thinking_mode(body),
                Some(ThinkingMode::Budget),
                "{body}"
            );
        }
        for body in [
            "output_config.effort: xhigh is not supported with adaptive thinking on this model",
            "invalid field: thinking",
            "temperature may only be set to 1 when thinking is enabled or in adaptive mode",
        ] {
            assert_eq!(rejected_thinking_mode(body), None, "{body}");
        }
    }

    #[test]
    fn a_request_built_before_a_concurrent_switch_retries_in_the_learned_mode() {
        let adapter = adapter("max");
        // Another call already learned that this model needs a budget.
        adapter
            .learned_thinking_mode
            .set(ThinkingMode::Budget)
            .unwrap();
        let mut attempt = AttemptState::default();
        let parsed = adapter.parse(
            400,
            "adaptive thinking is not supported on this model",
            &mut attempt,
        );
        assert!(matches!(parsed, Parse::RetrySameSlot(_)));
        assert!(!attempt.drop_reasoning);
        assert_eq!(attempt.reasoning_cap, None);
    }

    #[test]
    fn a_second_mode_rejection_in_one_request_falls_back_normally() {
        let adapter = adapter("high");
        let mut attempt = AttemptState::default();
        let body = "adaptive thinking is not supported on this model";
        assert!(matches!(
            adapter.parse(400, body, &mut attempt),
            Parse::RetrySameSlot(_)
        ));
        assert_eq!(adapter.thinking_mode(), ThinkingMode::Budget);
        // The budget request is rejected the other way: no flip back, normal fallback.
        let body = r#""thinking.type.enabled" is not supported for this model"#;
        assert!(matches!(
            adapter.parse(400, body, &mut attempt),
            Parse::RetrySameSlot(_)
        ));
        assert!(attempt.drop_reasoning);
        assert_eq!(adapter.thinking_mode(), ThinkingMode::Budget);
    }

    #[test]
    fn adaptive_minimal_is_recorded_as_the_low_effort_it_sends() {
        let adapter = adapter("minimal");
        let attempt = AttemptState::default();
        assert_eq!(adapter.effective_reasoning(&attempt), "low");
        adapter
            .learned_thinking_mode
            .set(ThinkingMode::Budget)
            .unwrap();
        assert_eq!(adapter.effective_reasoning(&attempt), "minimal");
    }
}
