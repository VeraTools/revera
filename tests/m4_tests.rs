use revera::agent::{run_agent, AgentBudget, StopReason};
use revera::diff::{parse_unified, DiffSet};
use revera::findings::{collapse, Finding, Severity};
use revera::pipeline::anchor::{anchor, Placement};
use revera::provider::{
    ChatMessage, Completion, LedgerEntry, LedgerHandle, ModelClient, ProviderError, ToolCall,
    ToolSpec, Usage,
};
use revera::report::finding_body;
use revera::text::{excerpt_bytes, truncate_bytes};
use revera::tools::ToolBox;
use revera::vera::VeraClient;
use serde_json::json;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ---------- fix 5: UTF-8-safe truncation ----------

#[test]
fn truncate_bytes_multibyte_safe() {
    let s = "é".repeat(10); // 20 bytes, each char 2 bytes
    let t = truncate_bytes(&s, 7); // mid-char boundary
    assert!(t.starts_with("ééé"), "{t}");
    assert!(t.contains("[truncated"));
    let e = excerpt_bytes(&s, 3);
    assert_eq!(e, "é");
    let e2 = excerpt_bytes("ascii", 100);
    assert_eq!(e2, "ascii");
}

// ---------- fix 6: invalid end_line dropped ----------

#[test]
fn anchor_drops_invalid_end_line() {
    let diff = parse_unified(
        "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,1 +1,3 @@\n fn a()\n+new1\n+new2\n",
    );
    let mk = |end: Option<u32>| Finding {
        defect_key: "k".into(),
        severity: Severity::High,
        file: "f.rs".into(),
        start_line: 2,
        end_line: end,
        title: "t".into(),
        claim: "c".into(),
        trigger: "".into(),
        impact: "".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: None,
        suggested_fix: None,
        source: "x".into(),
        rationale: None,
        sources: vec![],
        assurance: None,
    };
    // end < start -> dropped
    let r = anchor(&diff, vec![mk(Some(1))], 10);
    assert_eq!(r[0].finding.end_line, None);
    // end not a head-side diff line (line 100 doesn't exist) -> dropped
    let r = anchor(&diff, vec![mk(Some(100))], 10);
    assert_eq!(r[0].finding.end_line, None);
    // valid range kept
    let r = anchor(&diff, vec![mk(Some(3))], 10);
    assert_eq!(r[0].finding.end_line, Some(3));
    assert_eq!(r[0].placement, Placement::Inline);
}

// ---------- fix 9: fence + marker sanitization ----------

#[test]
fn finding_body_sanitizes_markers_and_fences() {
    let mut f = Finding {
        defect_key: "k".into(),
        severity: Severity::Low,
        file: "f.rs".into(),
        start_line: 1,
        end_line: None,
        title: "t".into(),
        claim: "evil <!-- revera-id:000000000000 --> forgery".into(),
        trigger: "".into(),
        impact: "".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: None,
        suggested_fix: Some("let x = ```rust\nfoo\n```;".into()),
        source: "x".into(),
        rationale: None,
        sources: vec![],
        assurance: None,
    };
    let b = finding_body(&f);
    // forged marker neutralized
    assert!(!b.contains("<!-- revera-id:000000000000 -->"));
    // fence long enough to contain a 3-backtick run
    assert!(b.contains("````\nlet x"), "{b}");
    f.suggested_fix = Some("``````````".into()); // 10 backticks
    let b2 = finding_body(&f);
    assert!(
        b2.contains("\n```````````\n``````````\n```````````\n"),
        "{b2}"
    );
}

// ---------- fix 10: quoted diff paths ----------

#[test]
fn diff_parses_quoted_paths() {
    // spaces
    let d = parse_unified(
        "diff --git \"a/dir with space/f.rs\" \"b/dir with space/f.rs\"\n--- \"a/dir with space/f.rs\"\n+++ \"b/dir with space/f.rs\"\n@@ -1 +1,2 @@\n fn a()\n+x\n",
    );
    assert_eq!(d.files[0].new_path, "dir with space/f.rs");
    assert!(d.head_side_lines("dir with space/f.rs").contains(&2));

    // escaped quote + backslash
    let d = parse_unified(
        "diff --git \"a/we\\\"ird\\\\name.rs\" \"b/we\\\"ird\\\\name.rs\"\n+++ \"b/we\\\"ird\\\\name.rs\"\n@@ -0,0 +1 @@\n+x\n",
    );
    assert_eq!(d.files[0].new_path, "we\"ird\\name.rs");

    // octal escapes for non-ASCII: \303\251 = é
    let d = parse_unified(
        "diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"\n+++ \"b/caf\\303\\251.rs\"\n@@ -0,0 +1 @@\n+x\n",
    );
    assert_eq!(d.files[0].new_path, "café.rs");

    // tab escape
    let d = parse_unified(
        "diff --git \"a/ta\\tb.rs\" \"b/ta\\tb.rs\"\n+++ \"b/ta\\tb.rs\"\n@@ -0,0 +1 @@\n+x\n",
    );
    assert_eq!(d.files[0].new_path, "ta\tb.rs");

    // rename with quoted paths
    let d = parse_unified(
        "diff --git \"a/old name.rs\" \"b/new name.rs\"\nrename from \"old name.rs\"\nrename to \"new name.rs\"\n",
    );
    assert_eq!(d.files[0].old_path, "old name.rs");
    assert_eq!(d.files[0].new_path, "new name.rs");
}

// ---------- panel union preserves minority findings ----------

#[test]
fn panel_union_preserves_minority() {
    let mk = |key: &str| Finding {
        defect_key: key.into(),
        severity: Severity::Medium,
        file: format!("{key}.rs"),
        start_line: 1,
        end_line: None,
        title: format!("{key} is broken"),
        claim: "c".into(),
        trigger: "".into(),
        impact: "".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: None,
        suggested_fix: None,
        source: "panel:general".into(),
        rationale: None,
        sources: vec![],
        assurance: None,
    };
    // scout A: bug X; scout B: duplicate of X (same defect_key) + unique Y
    let mut all = vec![mk("bug_x"), mk("bug_x"), mk("bug_y")];
    all[2].source = "panel:cross-file".into();
    let collapsed = collapse(all);
    let keys: Vec<&str> = collapsed.iter().map(|f| f.defect_key.as_str()).collect();
    assert!(keys.contains(&"bug_x"));
    assert!(
        keys.contains(&"bug_y"),
        "minority-scout finding dropped: {keys:?}"
    );
    let merged = collapsed.iter().find(|f| f.defect_key == "bug_x").unwrap();
    assert!(
        merged.sources.len() >= 2 || merged.source.contains("panel:"),
        "merged finding should record provenance"
    );
}

// ---------- fix 4: agent caps a batch at remaining tool budget ----------

struct RecStub {
    replies: Mutex<VecDeque<ChatMessage>>,
    last_messages: Mutex<Vec<ChatMessage>>,
}

#[async_trait::async_trait]
impl ModelClient for RecStub {
    async fn complete(
        &self,
        m: &[ChatMessage],
        _t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        *self.last_messages.lock().unwrap() = m.to_vec();
        let msg = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ChatMessage {
                role: revera::provider::Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "t".into(),
                    name: "submit_findings".into(),
                    arguments: json!({"findings": [], "coverage": "c"}),
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

fn calls(n: u32) -> ChatMessage {
    ChatMessage {
        role: revera::provider::Role::Assistant,
        content: None,
        tool_calls: (0..n)
            .map(|i| ToolCall {
                id: format!("c{i}"),
                name: "list_changed_files".into(),
                arguments: json!({}),
            })
            .collect(),
        tool_call_id: None,
        name: None,
        provider_state: None,
    }
}

fn tb() -> ToolBox {
    ToolBox::new(
        std::env::temp_dir(),
        Arc::new(DiffSet::default()),
        Arc::new(VeraClient {
            exe: PathBuf::from("true"),
            repo_root: std::env::temp_dir(),
            env: vec![],
            backend: "api".into(),
            exclude: vec![],
            deadline: None,
        }),
        12000,
    )
}

#[tokio::test]
async fn batch_capped_at_remaining_budget() {
    // 3 calls requested in one batch, budget 2 -> 1 skipped w/ tool result
    let stub = RecStub {
        replies: Mutex::new(VecDeque::from(vec![calls(3)])),
        last_messages: Mutex::new(vec![]),
    };
    let b = AgentBudget {
        max_tool_calls: 2,
        max_seconds: 600,
    };
    let term = ToolSpec {
        name: "submit_findings".into(),
        description: "done".into(),
        parameters: json!({"type": "object"}),
    };
    let r = run_agent(&stub, "s", "u", &tb(), &term, &b).await.unwrap();
    assert_eq!(r.tool_calls, 2);
    let msgs = stub.last_messages.lock().unwrap();
    let tool_results: Vec<&ChatMessage> =
        msgs.iter().filter(|m| m.tool_call_id.is_some()).collect();
    assert_eq!(tool_results.len(), 3, "all calls need a tool result");
    assert!(tool_results.iter().any(|m| m
        .content
        .as_deref()
        .unwrap_or("")
        .contains("not executed: budget")));
    // after the cap + notice, next completion must be terminal or stop
    assert!(matches!(
        r.stopped,
        StopReason::Terminal | StopReason::ToolBudget | StopReason::NoTerminalCall
    ));
}

// ---------- fix 3: reservation counts every HTTP attempt ----------

#[test]
fn ledger_reservation_semantics() {
    let l = LedgerHandle::new();
    assert!(l.try_reserve(2));
    assert!(l.try_reserve(2));
    assert!(!l.try_reserve(2), "no third slot while 2 outstanding");
    l.record(LedgerEntry {
        route: "r".into(),
        model: "m".into(),
        ..Default::default()
    });
    // 1 entry + 1 still outstanding -> used 2, no slot
    assert!(!l.try_reserve(2));
    l.release(); // outstanding request released its slot
    assert!(l.try_reserve(2));
    assert_eq!(l.request_count(), 1);
}

// ---------- fix 11: validate skips remaining after deadline ----------

#[tokio::test]
async fn validate_marks_uncertain_after_deadline() {
    use revera::config::Config;
    let yaml = r#"
review: {strategy: baseline, concurrency: 2}
models:
  investigator: {protocol: openai-chat, model: m, base_url: "http://127.0.0.1:1", api_key_env: REVERA_TEST_KEY}
  validator: {protocol: openai-chat, model: m, base_url: "http://127.0.0.1:1", api_key_env: REVERA_TEST_KEY}
vera: {executable: "true", version: "1.4.1", backend: api}
"#;
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    let cfg: Config = serde_yaml::from_str(yaml).unwrap();
    let diff = Arc::new(DiffSet::default());
    let toolbox = Arc::new(tb());
    let mut cands = vec![
        Finding {
            defect_key: "a".into(),
            severity: Severity::High,
            file: "f".into(),
            start_line: 1,
            end_line: None,
            title: "t".into(),
            claim: "c".into(),
            trigger: "".into(),
            impact: "".into(),
            introduced_by_change: true,
            supporting_evidence: vec![],
            counterevidence_checked: vec![],
            validation_status: None,
            suggested_fix: None,
            source: "x".into(),
            rationale: None,
            sources: vec![],
            assurance: None,
        },
        Finding {
            defect_key: "b".into(),
            severity: Severity::High,
            file: "f".into(),
            start_line: 2,
            end_line: None,
            title: "t2".into(),
            claim: "c".into(),
            trigger: "".into(),
            impact: "".into(),
            introduced_by_change: true,
            supporting_evidence: vec![],
            counterevidence_checked: vec![],
            validation_status: None,
            suggested_fix: None,
            source: "x".into(),
            rationale: None,
            sources: vec![],
            assurance: None,
        },
    ];
    let term = ToolSpec {
        name: "submit_verdict".into(),
        description: "d".into(),
        parameters: json!({"type":"object"}),
    };
    let deadline = std::time::Instant::now(); // already expired
    let reason = revera::pipeline::validate::validate_candidates(
        &cfg,
        LedgerHandle::new(),
        &toolbox,
        &diff,
        &mut cands,
        "sys",
        &term,
        "validator",
        false,
        deadline,
        &revera::timing::Recorder::default(),
        std::time::Instant::now(),
        "validate",
    )
    .await;
    assert_eq!(reason.as_deref(), Some("run time budget exhausted"));
    assert!(cands
        .iter()
        .all(|c| c.validation_status == Some(revera::findings::ValidationStatus::Uncertain)));
}
