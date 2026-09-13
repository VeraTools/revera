pub mod anthropic;
pub mod gemini;
pub mod http;
pub mod openai_chat;
pub mod openai_responses;
pub mod scripted;

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(s: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(s.into()),
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
        }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(s.into()),
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
        }
    }
    pub fn tool(
        id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
            name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub message: ChatMessage,
    pub usage: Usage,
    pub model: String,
    pub latency_ms: u64,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("model request failed: {0}")]
    Http(String),
    #[error("rate limited / server error after retries: {0}")]
    RetryExhausted(String),
    #[error("request budget exhausted (run_max_requests)")]
    BudgetExhausted,
    #[error("scripted expectation failed: {0}")]
    ScriptExpectationFailed(String),
    #[error("provider error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, Default)]
pub struct LedgerEntry {
    pub route: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub latency_ms: u64,
    pub retries: u32,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct RunLedger {
    pub entries: Vec<LedgerEntry>,
    /// In-flight reservations (requests sent but not yet recorded).
    pub reserved: u32,
}

#[derive(Debug, Clone, Default)]
pub struct LedgerHandle(pub Arc<Mutex<RunLedger>>);

impl LedgerHandle {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn record(&self, e: LedgerEntry) {
        let mut g = self.0.lock().unwrap();
        g.reserved = g.reserved.saturating_sub(1);
        g.entries.push(e);
    }
    /// Atomically reserve a request slot; false when the budget is spent.
    /// Every HTTP attempt (incl. retries and the max_completion_tokens
    /// fallback) must hold a reservation.
    pub fn try_reserve(&self, max_requests: u32) -> bool {
        let mut g = self.0.lock().unwrap();
        let used = g.entries.len() as u32 + g.reserved;
        if used >= max_requests {
            return false;
        }
        g.reserved += 1;
        true
    }
    /// Release a reservation without recording (e.g. request build failure).
    pub fn release(&self) {
        let mut g = self.0.lock().unwrap();
        g.reserved = g.reserved.saturating_sub(1);
    }
    pub fn request_count(&self) -> u32 {
        self.0.lock().unwrap().entries.len() as u32
    }
    pub fn totals(&self) -> (u64, u64, u64) {
        let g = self.0.lock().unwrap();
        let r = g.entries.len() as u64;
        let p = g.entries.iter().map(|e| e.prompt_tokens).sum();
        let c = g.entries.iter().map(|e| e.completion_tokens).sum();
        (r, p, c)
    }
}

#[async_trait::async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError>;
    fn route_label(&self) -> String;
}
