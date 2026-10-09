use revera::agent::{AgentBudget, StopReason, run_agent};
use revera::diff::DiffSet;
use revera::provider::{
    ChatMessage, Completion, LedgerHandle, ModelClient, ProviderError, Role, ToolCall, ToolSpec,
    Usage,
};
use revera::tools::ToolBox;
use revera::vera::VeraClient;
use serde_json::json;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Stub {
    replies: Mutex<VecDeque<ChatMessage>>,
}

#[async_trait::async_trait]
impl ModelClient for Stub {
    async fn complete(
        &self,
        _m: &[ChatMessage],
        _t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let msg = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| ChatMessage {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "x".into(),
                    name: "submit_findings".into(),
                    arguments: json!({"findings": [], "coverage": ""}),
                }],
                tool_call_id: None,
                name: None,
                provider_state: None,
            });
        Ok(Completion {
            message: msg,
            usage: Usage::default(),
            model: "stub".into(),
            latency_ms: 0,
        })
    }
    fn route_label(&self) -> String {
        "stub".into()
    }
}

fn assistant_calls(calls: Vec<(&str, serde_json::Value)>) -> ChatMessage {
    ChatMessage {
        role: Role::Assistant,
        content: None,
        tool_calls: calls
            .into_iter()
            .map(|(n, a)| ToolCall {
                id: format!("id-{n}"),
                name: n.into(),
                arguments: a,
            })
            .collect(),
        tool_call_id: None,
        name: None,
        provider_state: None,
    }
}

fn assistant_text(t: &str) -> ChatMessage {
    ChatMessage {
        role: Role::Assistant,
        content: Some(t.into()),
        tool_calls: vec![],
        tool_call_id: None,
        name: None,
        provider_state: None,
    }
}

fn toolbox() -> ToolBox {
    ToolBox::new(
        std::env::temp_dir(),
        Arc::new(DiffSet::default()),
        Arc::new(VeraClient {
            exe: PathBuf::from("true"),
            repo_root: std::env::temp_dir(),
            env: vec![],
            backend: "api".into(),
            exclude: vec![],
            home: std::env::temp_dir(),
            rerank: None,
            embedding_pairs: vec![],
            index_key: String::new(),
            rerank_fallbacks: Default::default(),
            deadline: None,
        }),
        12000,
    )
}

fn terminal() -> ToolSpec {
    ToolSpec {
        name: "submit_findings".into(),
        description: "done".into(),
        parameters: json!({"type": "object"}),
    }
}

fn budget() -> AgentBudget {
    AgentBudget {
        max_tool_calls: 5,
        max_seconds: 60,
    }
}

#[tokio::test]
async fn terminal_on_first_call() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![assistant_calls(vec![(
            "submit_findings",
            json!({"findings": [], "coverage": "c"}),
        )])])),
    };
    let r = run_agent(&stub, "s", "u", &toolbox(), &terminal(), &budget())
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(r.final_call.unwrap().name, "submit_findings");
}

#[tokio::test]
async fn tool_then_terminal() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![
            assistant_calls(vec![("list_changed_files", json!({}))]),
            assistant_calls(vec![(
                "submit_findings",
                json!({"findings":[],"coverage":"c"}),
            )]),
        ])),
    };
    let r = run_agent(&stub, "s", "u", &toolbox(), &terminal(), &budget())
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(r.tool_calls, 1);
}

#[tokio::test]
async fn budget_exhaustion_path() {
    // endless non-terminal calls -> ToolBudget after the one grace completion
    let replies = (0..10)
        .map(|_| assistant_calls(vec![("list_changed_files", json!({}))]))
        .collect();
    let stub = Stub {
        replies: Mutex::new(replies),
    };
    let b = AgentBudget {
        max_tool_calls: 2,
        max_seconds: 600,
    };
    let r = run_agent(&stub, "s", "u", &toolbox(), &terminal(), &b)
        .await
        .unwrap();
    // stub runs out of canned replies -> default reply is terminal -> Terminal
    // is acceptable too; but with <2 tool calls per reply, tool budget trips.
    assert!(matches!(
        r.stopped,
        StopReason::Terminal | StopReason::ToolBudget
    ));
}

#[tokio::test]
async fn post_budget_nonterminal_stops_without_tools() {
    // A model that keeps calling tools after the budget notice (and after
    // the one reminder) ends with ToolBudget; no further tool calls run.
    let replies = (0..5)
        .map(|_| assistant_calls(vec![("list_changed_files", json!({}))]))
        .collect();
    let stub = Stub {
        replies: Mutex::new(replies),
    };
    let b = AgentBudget {
        max_tool_calls: 2,
        max_seconds: 600,
    };
    let r = run_agent(&stub, "s", "u", &toolbox(), &terminal(), &b)
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::ToolBudget);
    assert_eq!(r.tool_calls, 2);
    assert!(r.final_call.is_none());
}

#[tokio::test]
async fn post_budget_tool_call_gets_one_reminder() {
    // a model that answers the tool-budget notice with another tool call is
    // told once that it was not executed, and may still submit
    let mut replies: VecDeque<ChatMessage> = (0..3)
        .map(|_| assistant_calls(vec![("list_changed_files", json!({}))]))
        .collect();
    replies.push_back(assistant_calls(vec![(
        "submit_findings",
        json!({"findings": [], "coverage": "c"}),
    )]));
    let rec = Recorder::new(replies);
    let b = AgentBudget {
        max_tool_calls: 2,
        max_seconds: 600,
    };
    let r = run_agent(&rec, "s", "u", &toolbox(), &terminal(), &b)
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(r.tool_calls, 2);
    assert_eq!(r.final_call.unwrap().arguments["coverage"], "c");
    let reqs = rec.requests.lock().unwrap();
    // the reminder answers the post-notice tool call, and only the
    // terminal tool is offered after the notice
    let last = reqs.last().unwrap();
    let reminder = last.0.last().unwrap();
    assert_eq!(reminder.role, Role::Tool);
    assert!(
        reminder
            .content
            .as_deref()
            .unwrap()
            .contains("tool budget exhausted"),
        "{reminder:?}"
    );
    assert_eq!(last.1, vec!["submit_findings".to_string()]);
}

#[tokio::test]
async fn post_budget_text_gets_one_reminder() {
    let mut replies: VecDeque<ChatMessage> = (0..2)
        .map(|_| assistant_calls(vec![("list_changed_files", json!({}))]))
        .collect();
    replies.push_back(assistant_text("Let me summarise what I found so far."));
    replies.push_back(assistant_calls(vec![(
        "submit_findings",
        json!({"findings": [], "coverage": "c"}),
    )]));
    let rec = Recorder::new(replies);
    let b = AgentBudget {
        max_tool_calls: 2,
        max_seconds: 600,
    };
    let r = run_agent(&rec, "s", "u", &toolbox(), &terminal(), &b)
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(r.tool_calls, 2);
    let reqs = rec.requests.lock().unwrap();
    let reminder = reqs.last().unwrap().0.last().unwrap();
    assert_eq!(reminder.role, Role::User);
    assert!(
        reminder
            .content
            .as_deref()
            .unwrap()
            .starts_with("Tool budget exhausted"),
        "{reminder:?}"
    );
}

#[tokio::test]
async fn text_json_fallback() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![assistant_text(
            "{\"findings\": [], \"coverage\": \"done\"}",
        )])),
    };
    let r = run_agent(&stub, "s", "u", &toolbox(), &terminal(), &budget())
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
}

#[tokio::test]
async fn no_terminal_after_nudge() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![
            assistant_text("just prose"),
            assistant_text("still prose"),
        ])),
    };
    let r = run_agent(&stub, "s", "u", &toolbox(), &terminal(), &budget())
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::NoTerminalCall);
}

#[test]
fn ledger_records() {
    let l = LedgerHandle::new();
    l.record(revera::provider::LedgerEntry {
        route: "r".into(),
        model: "m".into(),
        prompt_tokens: 1,
        ..Default::default()
    });
    assert_eq!(l.request_count(), 1);
    assert_eq!(l.totals(), (1, 1, 0, 0));
}

/// Stub that also records every request (messages and offered tool names).
struct Recorder {
    inner: Stub,
    requests: Mutex<Vec<(Vec<ChatMessage>, Vec<String>)>>,
}

impl Recorder {
    fn new(replies: VecDeque<ChatMessage>) -> Self {
        Recorder {
            inner: Stub {
                replies: Mutex::new(replies),
            },
            requests: Mutex::new(vec![]),
        }
    }
}

#[async_trait::async_trait]
impl ModelClient for Recorder {
    async fn complete(
        &self,
        m: &[ChatMessage],
        t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let names = t.iter().map(|s| s.name.clone()).collect();
        self.requests.lock().unwrap().push((m.to_vec(), names));
        self.inner.complete(m, t).await
    }
    fn route_label(&self) -> String {
        "recorder".into()
    }
}

#[tokio::test]
async fn low_tool_budget_is_announced_once_before_exhaustion() {
    let replies = (0..10)
        .map(|_| assistant_calls(vec![("list_changed_files", json!({}))]))
        .collect();
    let rec = Recorder::new(replies);
    let b = AgentBudget {
        max_tool_calls: 10,
        max_seconds: 600,
    };
    run_agent(&rec, "s", "u", &toolbox(), &terminal(), &b)
        .await
        .unwrap();
    let reqs = rec.requests.lock().unwrap();
    let all = &reqs.last().unwrap().0;
    let user: Vec<&str> = all
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| m.content.as_deref())
        .collect();
    let warnings: Vec<_> = user
        .iter()
        .filter(|t| t.contains("tool calls left"))
        .collect();
    assert_eq!(warnings.len(), 1, "{user:?}");
    assert!(warnings[0].starts_with("2 tool calls left"), "{user:?}");
    assert!(
        user.iter().any(|t| t.starts_with("Budget exhausted")),
        "{user:?}"
    );
}
