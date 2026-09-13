use revera::agent::{run_agent, AgentBudget, StopReason};
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
    // Once the budget notice has been sent, a non-terminal completion must end
    // the loop with ToolBudget without executing further tool calls.
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
