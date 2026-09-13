use super::{
    ChatMessage, Completion, LedgerEntry, LedgerHandle, ModelClient, ProviderError, Role, ToolCall,
    ToolSpec, Usage,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Deserialize)]
struct ScriptTurn {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ScriptToolCall>>,
    #[serde(default)]
    expect_tool_result_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScriptToolCall {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct ScriptFile {
    roles: HashMap<String, VecDeque<Vec<ScriptTurn>>>,
}

type Store = HashMap<PathBuf, ScriptFile>;

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Offline provider driven by a JSON script. Each `complete` consumes the next
/// turn of the conversation popped for this session's role at construction.
pub struct ScriptedClient {
    role: String,
    conversation: Mutex<VecDeque<ScriptTurn>>,
    terminal_tool: String,
    ledger: LedgerHandle,
    id_counter: std::sync::atomic::AtomicU32,
}

impl ScriptedClient {
    /// Pop the next scripted conversation for `role` in `path`.
    pub fn new(
        path: &std::path::Path,
        role: &str,
        ledger: LedgerHandle,
        terminal_tool: &str,
    ) -> Result<Self, ProviderError> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut g = store().lock().unwrap();
        let file = match g.get_mut(&canon) {
            Some(f) => f,
            None => {
                let text = std::fs::read_to_string(path).map_err(|e| {
                    ProviderError::Other(format!("cannot read script {}: {e}", path.display()))
                })?;
                let f: ScriptFile = serde_json::from_str(&text).map_err(|e| {
                    ProviderError::Other(format!("invalid script {}: {e}", path.display()))
                })?;
                g.insert(canon.clone(), f);
                g.get_mut(&canon).unwrap()
            }
        };
        let conversation: VecDeque<ScriptTurn> = file
            .roles
            .get_mut(role)
            .and_then(|q| q.pop_front())
            .unwrap_or_default()
            .into();
        Ok(Self {
            role: role.to_string(),
            conversation: Mutex::new(conversation),
            terminal_tool: terminal_tool.to_string(),
            ledger,
            id_counter: std::sync::atomic::AtomicU32::new(0),
        })
    }
}

#[async_trait::async_trait]
impl ModelClient for ScriptedClient {
    fn route_label(&self) -> String {
        format!("scripted:{}", self.role)
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let turn = self.conversation.lock().unwrap().pop_front();
        let msg = match turn {
            Some(t) => {
                // check expectation against the last tool message
                if let Some(needle) = &t.expect_tool_result_contains {
                    let last_tool = messages
                        .iter()
                        .rev()
                        .find(|m| m.role == Role::Tool)
                        .and_then(|m| m.content.clone())
                        .unwrap_or_default();
                    if !last_tool.contains(needle.as_str()) {
                        return Err(ProviderError::ScriptExpectationFailed(format!(
                            "role {}: expected tool result to contain {needle:?}, got: {}",
                            self.role,
                            &last_tool[..last_tool.len().min(300)]
                        )));
                    }
                }
                let calls = t
                    .tool_calls
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| ToolCall {
                        id: format!(
                            "script-{}-{}",
                            self.role,
                            self.id_counter
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        ),
                        name: c.name,
                        arguments: c.arguments,
                    })
                    .collect();
                ChatMessage {
                    role: Role::Assistant,
                    content: t.content,
                    tool_calls: calls,
                    tool_call_id: None,
                    name: None,
                }
            }
            None => {
                tracing::warn!(
                    role = self.role,
                    "scripted conversation exhausted; emitting empty terminal call"
                );
                ChatMessage {
                    role: Role::Assistant,
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "script-exhausted".into(),
                        name: self.terminal_tool.clone(),
                        arguments: Value::Object(Default::default()),
                    }],
                    tool_call_id: None,
                    name: None,
                }
            }
        };
        self.ledger.record(LedgerEntry {
            route: self.route_label(),
            model: "scripted".into(),
            latency_ms: 0,
            ..Default::default()
        });
        Ok(Completion {
            message: msg,
            usage: Usage::default(),
            model: "scripted".into(),
            latency_ms: 0,
        })
    }
}
