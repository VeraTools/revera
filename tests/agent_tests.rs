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
            semantic_fallbacks: Default::default(),
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

/// Stalls on its first turn (still thinking when the soft deadline passes),
/// then answers the budget notice with a terminal submission.
struct SlowThenSubmit {
    calls: Mutex<u32>,
    capped: Mutex<u32>,
}

#[async_trait::async_trait]
impl ModelClient for SlowThenSubmit {
    async fn complete(
        &self,
        m: &[ChatMessage],
        _t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        if n == 1 {
            std::future::pending::<()>().await;
        }
        let asked = m
            .last()
            .and_then(|x| x.content.as_deref())
            .is_some_and(|c| c.contains("Budget exhausted"));
        assert!(asked, "the wrap-up turn must carry the budget notice");
        Ok(Completion {
            message: assistant_calls(vec![(
                "submit_findings",
                json!({"findings": [{"title": "t"}], "coverage": "partial"}),
            )]),
            usage: Usage::default(),
            model: "stub".into(),
            latency_ms: 0,
        })
    }
    async fn complete_capped(
        &self,
        m: &[ChatMessage],
        t: &[ToolSpec],
        cap: revera::config::ReasoningEffort,
    ) -> Result<Completion, ProviderError> {
        assert_eq!(cap, revera::config::ReasoningEffort::Low);
        *self.capped.lock().unwrap() += 1;
        self.complete(m, t).await
    }
    fn route_label(&self) -> String {
        "slow".into()
    }
}

#[tokio::test]
async fn budget_notices_ask_for_unfinished_areas_when_the_terminal_accepts_them() {
    for (spec, asks) in [
        (revera::tools::terminal_submit_findings_spec(), true),
        (terminal(), false),
    ] {
        let replies = (0..10)
            .map(|_| assistant_calls(vec![("list_changed_files", json!({}))]))
            .collect();
        let rec = Recorder::new(replies);
        let b = AgentBudget {
            max_tool_calls: 10,
            max_seconds: 600,
        };
        run_agent(&rec, "s", "u", &toolbox(), &spec, &b)
            .await
            .unwrap();
        let reqs = rec.requests.lock().unwrap();
        let user: Vec<&str> = reqs
            .last()
            .unwrap()
            .0
            .iter()
            .filter(|m| m.role == Role::User)
            .filter_map(|m| m.content.as_deref())
            .collect();
        for prefix in ["2 tool calls left", "Budget exhausted"] {
            let notice = user.iter().find(|t| t.starts_with(prefix)).unwrap();
            assert_eq!(notice.contains("`not_checked`"), asks, "{notice}");
        }
    }
}

/// Researches until only the terminal tool is offered; every reply is slow.
struct SlowResearcher;

#[async_trait::async_trait]
impl ModelClient for SlowResearcher {
    async fn complete(
        &self,
        _m: &[ChatMessage],
        t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let message = if t.len() == 1 {
            assistant_calls(vec![(
                "submit_findings",
                json!({"findings": [], "coverage": "partial"}),
            )])
        } else {
            assistant_calls(vec![("list_changed_files", json!({}))])
        };
        Ok(Completion {
            message,
            usage: Usage::default(),
            model: "slow".into(),
            latency_ms: 1100,
        })
    }
    fn route_label(&self) -> String {
        "slow".into()
    }
}

struct FixedLatencyResearcher {
    started: Mutex<Vec<bool>>,
    completed: Mutex<Vec<bool>>,
}

#[async_trait::async_trait]
impl ModelClient for FixedLatencyResearcher {
    async fn complete(
        &self,
        _messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let submitting = tools.len() == 1;
        self.started.lock().unwrap().push(submitting);
        // Longer than the 1 s minimum timeout, so the second research turn
        // is interrupted even when that floor extends the adaptive deadline.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        self.completed.lock().unwrap().push(submitting);
        let message = if submitting {
            assistant_calls(vec![(
                "submit_findings",
                json!({"findings": [], "coverage": "partial"}),
            )])
        } else {
            assistant_calls(vec![("list_changed_files", json!({}))])
        };
        Ok(Completion {
            message,
            usage: Usage::default(),
            model: "fixed-latency".into(),
            latency_ms: 1100,
        })
    }

    fn route_label(&self) -> String {
        "fixed-latency".into()
    }
}

#[tokio::test]
async fn adaptive_deadline_cuts_second_research_turn_and_preserves_submission() {
    let client = FixedLatencyResearcher {
        started: Mutex::new(vec![]),
        completed: Mutex::new(vec![]),
    };
    let start = std::time::Instant::now();
    let run = run_agent(
        &client,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &AgentBudget {
            max_tool_calls: 10,
            max_seconds: 4,
        },
    )
    .await
    .unwrap();
    assert_eq!(run.stopped, StopReason::Terminal);
    assert!(run.final_call.is_some());
    assert_eq!(*client.started.lock().unwrap(), [false, false, true]);
    assert_eq!(*client.completed.lock().unwrap(), [false, true]);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(4500),
        "ran {:?} on a 4 s budget",
        start.elapsed()
    );
}

/// A completion still in flight at the soft deadline is dropped and the
/// reserve is spent on a low-effort wrap-up turn, so candidates are
/// submitted instead of the whole investigation being discarded. The run
/// still ends within the hard budget.
#[tokio::test]
async fn slow_turn_at_the_deadline_still_submits() {
    let t0 = std::time::Instant::now();
    let client = SlowThenSubmit {
        calls: Mutex::new(0),
        capped: Mutex::new(0),
    };
    let r = run_agent(
        &client,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    let call = r.final_call.expect("submission made in the reserve");
    assert_eq!(call.arguments["findings"].as_array().unwrap().len(), 1);
    assert_eq!(*client.calls.lock().unwrap(), 2);
    // only the wrap-up turn runs at the capped effort
    assert_eq!(*client.capped.lock().unwrap(), 1);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(4),
        "{:?}",
        t0.elapsed()
    );
}

/// Answers its first turn with a submission, `delay` after it was asked;
/// with `fail_first`, that turn fails instead and the next one submits.
struct LateSubmit {
    delay: std::time::Duration,
    fail_first: bool,
    calls: Mutex<u32>,
    wrap_ups: Mutex<u32>,
}

#[async_trait::async_trait]
impl ModelClient for LateSubmit {
    async fn complete(
        &self,
        m: &[ChatMessage],
        _t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        if m.last()
            .and_then(|x| x.content.as_deref())
            .is_some_and(|c| c.contains("Budget exhausted"))
        {
            *self.wrap_ups.lock().unwrap() += 1;
        }
        let n = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        if n == 1 {
            tokio::time::sleep(self.delay).await;
            if self.fail_first {
                return Err(ProviderError::Http("late failure".into()));
            }
        }
        Ok(Completion {
            message: assistant_calls(vec![(
                "submit_findings",
                json!({"findings": [{"title": "late"}], "coverage": "complete"}),
            )]),
            usage: Usage::default(),
            model: "stub".into(),
            latency_ms: 0,
        })
    }
    fn route_label(&self) -> String {
        "late-submit".into()
    }
}

/// A reply still in flight at the soft deadline is awaited while the
/// wrap-up keeps half of the reserve, so a slow turn is not thrown away.
#[tokio::test(start_paused = true)]
async fn reply_in_flight_at_the_soft_deadline_is_awaited_before_wrap_up() {
    let client = LateSubmit {
        // a 900 s budget has a 90 s reserve; this reply lands 40 s past the
        // soft deadline, inside the 45 s the wrap-up can spare
        delay: std::time::Duration::from_secs(850),
        fail_first: false,
        calls: Mutex::new(0),
        wrap_ups: Mutex::new(0),
    };
    let budget = AgentBudget {
        max_tool_calls: 5,
        max_seconds: 900,
    };
    let r = run_agent(&client, "s", "u", &toolbox(), &terminal(), &budget)
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(
        r.final_call.unwrap().arguments["findings"][0]["title"],
        "late"
    );
    assert_eq!(*client.calls.lock().unwrap(), 1);
}

/// A request that fails while it is awaited past the soft deadline still
/// leads to the wrap-up, as it would had it been dropped at the deadline.
#[tokio::test(start_paused = true)]
async fn request_failing_after_the_soft_deadline_still_gets_a_wrap_up() {
    let client = LateSubmit {
        delay: std::time::Duration::from_secs(850),
        fail_first: true,
        calls: Mutex::new(0),
        wrap_ups: Mutex::new(0),
    };
    let budget = AgentBudget {
        max_tool_calls: 5,
        max_seconds: 900,
    };
    let r = run_agent(&client, "s", "u", &toolbox(), &terminal(), &budget)
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(*client.calls.lock().unwrap(), 2);
    assert_eq!(*client.wrap_ups.lock().unwrap(), 1);
}

/// A reply that does not arrive within the extension is still dropped, and
/// the wrap-up submits inside the hard budget.
#[tokio::test(start_paused = true)]
async fn reply_still_missing_after_the_extension_is_dropped_for_wrap_up() {
    let start = tokio::time::Instant::now();
    let client = SlowThenSubmit {
        calls: Mutex::new(0),
        capped: Mutex::new(0),
    };
    let budget = AgentBudget {
        max_tool_calls: 5,
        max_seconds: 900,
    };
    let r = run_agent(&client, "s", "u", &toolbox(), &terminal(), &budget)
        .await
        .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert_eq!(*client.calls.lock().unwrap(), 2);
    assert_eq!(*client.capped.lock().unwrap(), 1);
    let elapsed = start.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(855)
            && elapsed < std::time::Duration::from_secs(900),
        "{elapsed:?}"
    );
}

/// Records the text of every message each call receives.
struct Recording {
    inner: Stub,
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelClient for Recording {
    async fn complete(
        &self,
        m: &[ChatMessage],
        t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let text: Vec<&str> = m.iter().filter_map(|x| x.content.as_deref()).collect();
        self.seen.lock().unwrap().push(text.join("\n"));
        self.inner.complete(m, t).await
    }
    fn route_label(&self) -> String {
        "recording".into()
    }
}

/// A malformed verdict is sent back once with the parse error; the
/// corrected resubmission is the run's final call.
#[tokio::test]
async fn malformed_verdict_gets_one_corrected_resubmission() {
    let client = Recording {
        seen: Mutex::new(vec![]),
        inner: Stub {
            replies: Mutex::new(VecDeque::from(vec![
                assistant_calls(vec![("submit_findings", json!({"verdict": "accepted"}))]),
                assistant_calls(vec![(
                    "submit_findings",
                    json!({
                        "validation_status": "accepted",
                        "counterevidence_checked": ["a.rs:1"],
                        "rationale": "seen"
                    }),
                )]),
            ])),
        },
    };
    let r = revera::agent::run_agent_checked(
        &client,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &budget(),
        &revera::pipeline::validate::verdict_terminal_check,
    )
    .await
    .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert!(r.repaired, "the first submission was refused and repaired");
    assert_eq!(
        r.final_call.unwrap().arguments["validation_status"],
        "accepted"
    );
    // the retry turn carries the parse error that names the missing field
    let seen = client.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(!seen[0].contains("validation_status"), "{}", seen[0]);
    assert!(seen[1].contains("validation_status"), "{}", seen[1]);
}

/// Finishes a research turn after the adaptive soft deadline (inside the hard
/// budget), then hangs specifically on the terminal-only wrap-up request.
struct LateThenHang {
    terminal_requested: Mutex<bool>,
}

#[async_trait::async_trait]
impl ModelClient for LateThenHang {
    async fn complete(
        &self,
        _m: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        if tools.len() == 1 {
            *self.terminal_requested.lock().unwrap() = true;
            std::future::pending::<()>().await;
            unreachable!()
        }
        tokio::time::sleep(std::time::Duration::from_millis(1450)).await;
        Ok(Completion {
            message: assistant_calls(vec![("read_file", json!({"path": "nope.rs"}))]),
            usage: Usage::default(),
            model: "stub".into(),
            latency_ms: 0,
        })
    }
    fn route_label(&self) -> String {
        "late".into()
    }
}

/// The notice sent after the soft deadline gets only what is left of the
/// hard budget, not the extra grace that applies once it is already spent.
#[tokio::test]
async fn wrap_up_after_the_soft_deadline_stays_inside_the_hard_budget() {
    let client = LateThenHang {
        terminal_requested: Mutex::new(false),
    };
    let t0 = std::time::Instant::now();
    let r = run_agent(
        &client,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 2,
        },
    )
    .await
    .unwrap();
    assert!(*client.terminal_requested.lock().unwrap());
    assert_eq!(r.stopped, StopReason::TimeBudget);
    assert!(
        t0.elapsed() <= std::time::Duration::from_millis(2500),
        "ran {:?} on a 2 s budget",
        t0.elapsed()
    );
}

/// A provider future can finish after the deadline without yielding to the
/// timer. Its notice is sent after the hard deadline and receives grace;
/// the late tool batch is accounted for but never executed.
struct BlockingThenSubmit;

#[async_trait::async_trait]
impl ModelClient for BlockingThenSubmit {
    async fn complete(
        &self,
        m: &[ChatMessage],
        t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        if m.len() == 2 {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            return Ok(Completion {
                message: assistant_calls(vec![("read_file", json!({"path": "nope.rs"}))]),
                usage: Usage::default(),
                model: "blocking".into(),
                latency_ms: 0,
            });
        }
        assert!(m.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|s| s.contains("Budget exhausted"))
        }));
        assert!(m.iter().any(|message| {
            message.role == Role::Tool
                && message
                    .content
                    .as_deref()
                    .is_some_and(|s| s.contains("not executed: time budget"))
        }));
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "submit_findings");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(Completion {
            message: assistant_calls(vec![(
                "submit_findings",
                json!({"findings": [], "coverage": "budget"}),
            )]),
            usage: Usage::default(),
            model: "blocking".into(),
            latency_ms: 0,
        })
    }

    fn route_label(&self) -> String {
        "blocking".into()
    }
}

#[tokio::test]
async fn successful_ordinary_turn_past_hard_deadline_allows_notice_grace() {
    let start = std::time::Instant::now();
    let run = run_agent(
        &BlockingThenSubmit,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(run.stopped, StopReason::Terminal);
    assert_eq!(run.tool_calls, 1);
    assert!(run.final_call.is_some());
    assert!(start.elapsed() <= std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn successful_ordinary_turn_past_adaptive_deadline_skips_tools() {
    let run = run_agent(
        &BlockingThenSubmit,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(run.stopped, StopReason::Terminal);
    assert_eq!(run.tool_calls, 1);
    assert!(run.final_call.is_some());
}

#[tokio::test]
async fn already_spent_hard_budget_still_allows_one_grace_submission() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::new()),
    };
    let run = run_agent(
        &stub,
        "s",
        "u",
        &toolbox(),
        &terminal(),
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(run.stopped, StopReason::Terminal);
}

#[tokio::test]
async fn time_budget_notice_comes_while_a_reply_still_fits() {
    let b = AgentBudget {
        max_tool_calls: 100,
        max_seconds: 5,
    };
    let run = run_agent(&SlowResearcher, "s", "u", &toolbox(), &terminal(), &b)
        .await
        .unwrap();
    assert_eq!(run.stopped, StopReason::Terminal);
    assert!(run.final_call.is_some());
}
