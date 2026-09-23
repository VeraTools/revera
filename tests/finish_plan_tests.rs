//! Regressions for truthful terminal outcomes, exact review reuse, finding
//! lifecycle, owned summary comments and Vera-independent lexical discovery.

use revera::agent::{run_agent_checked, AgentBudget, StopReason};
use revera::diff::DiffSet;
use revera::findings::{Finding, Severity, ValidationStatus};
use revera::github::api::GhComment;
use revera::github::publish::{encode_state, find_managed, Identity};
use revera::pipeline::common::{findings_terminal_check, parse_findings_checked};
use revera::provider::{
    ChatMessage, Completion, LedgerHandle, ModelClient, ProviderError, Role, ToolCall, ToolSpec,
    Usage,
};
use revera::report::RunStatus;
use revera::state::{review_key, FindingState, ReviewState, STATE_VERSION};
use revera::tools::{terminal_submit_findings_spec, ToolBox};
use revera::vera::VeraClient;
use serde_json::json;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn finding(file: &str, key: &str, line: u32) -> Finding {
    Finding {
        defect_key: key.into(),
        severity: Severity::High,
        file: file.into(),
        start_line: line,
        end_line: None,
        title: format!("title {key}"),
        claim: "claim".into(),
        trigger: "trigger".into(),
        impact: "impact".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: Some(ValidationStatus::Accepted),
        suggested_fix: None,
        source: "investigator".into(),
        rationale: None,
        sources: vec![],
        assurance: None,
        quoted_code: None,
        suggested_replacement: None,
        quote_anchored: false,
    }
}

fn valid_finding_json() -> serde_json::Value {
    json!({
        "defect_key": "k", "severity": "high", "file": "src/a.rs", "start_line": 3,
        "title": "t", "claim": "c", "trigger": "tr", "impact": "i",
        "introduced_by_change": true
    })
}

// ---------- terminal parsing ----------

#[test]
fn consensus_findings_include_attribution_badge() {
    let mut f1 = finding("src/lib.rs", "key1", 10);
    f1.sources = vec!["panel:security".into(), "panel:concurrency".into()];
    let body = revera::report::finding_body(&f1);
    assert!(body.contains("Consensus"));
    assert!(body.contains("Flagged independently by 2 review lenses"));
    assert!(body.contains("panel:security, panel:concurrency"));
}

#[test]
fn empty_findings_list_is_complete_but_missing_list_is_not() {
    assert!(findings_terminal_check(&json!({"findings": [], "coverage": "x"})).is_ok());
    assert!(findings_terminal_check(&json!({})).is_err());
    assert!(findings_terminal_check(&json!({"findings": "nope"})).is_err());
    let all_bad = parse_findings_checked(&json!({"findings": [{"bogus": 1}, 42]}));
    assert!(all_bad.findings.is_empty());
    assert_eq!(all_bad.dropped, 2);
    assert!(all_bad.problem.is_some());
}

#[test]
fn mixed_validity_keeps_valid_and_flags_problem() {
    let p = parse_findings_checked(&json!({"findings": [valid_finding_json(), {"bogus": 1}]}));
    assert_eq!(p.findings.len(), 1);
    assert_eq!(p.dropped, 1);
    assert!(p.problem.unwrap().contains("1 of 2"));
}

// ---------- one bounded repair ----------

struct Stub {
    replies: Mutex<VecDeque<ChatMessage>>,
    seen: Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl ModelClient for Stub {
    async fn complete(
        &self,
        m: &[ChatMessage],
        _t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        self.seen.lock().unwrap().push(m.to_vec());
        let msg = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("stub out of replies");
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

fn terminal_call(args: serde_json::Value) -> ChatMessage {
    ChatMessage {
        role: Role::Assistant,
        content: None,
        tool_calls: vec![ToolCall {
            id: "t1".into(),
            name: "submit_findings".into(),
            arguments: args,
        }],
        tool_call_id: None,
        name: None,
        provider_state: None,
    }
}

fn text(t: &str) -> ChatMessage {
    ChatMessage {
        role: Role::Assistant,
        content: Some(t.into()),
        tool_calls: vec![],
        tool_call_id: None,
        name: None,
        provider_state: None,
    }
}

fn toolbox(repo: &Path) -> ToolBox {
    ToolBox::new(
        repo.to_path_buf(),
        Arc::new(DiffSet::default()),
        Arc::new(VeraClient::disabled(repo)),
        12000,
    )
}

fn budget() -> AgentBudget {
    AgentBudget {
        max_tool_calls: 5,
        max_seconds: 120,
    }
}

#[tokio::test]
async fn malformed_terminal_gets_exactly_one_repair() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![
            terminal_call(json!({})),
            terminal_call(json!({"findings": [], "coverage": "fixed"})),
        ])),
        seen: Mutex::new(vec![]),
    };
    let tb = toolbox(&std::env::temp_dir());
    let r = run_agent_checked(
        &stub,
        "s",
        "u",
        &tb,
        &terminal_submit_findings_spec(),
        &budget(),
        &findings_terminal_check,
    )
    .await
    .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert!(r.repaired);
    assert_eq!(r.final_call.unwrap().arguments["coverage"], "fixed");
    // the repair round fed the error back as a tool result
    let second = &stub.seen.lock().unwrap()[1];
    assert!(second
        .iter()
        .any(|m| m.role == Role::Tool && m.content.as_deref().unwrap_or("").contains("findings")));
}

#[tokio::test]
async fn second_malformed_submission_is_final() {
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![
            terminal_call(json!({"findings": "nope"})),
            terminal_call(json!({"findings": [{"bogus": 1}]})),
        ])),
        seen: Mutex::new(vec![]),
    };
    let tb = toolbox(&std::env::temp_dir());
    let r = run_agent_checked(
        &stub,
        "s",
        "u",
        &tb,
        &terminal_submit_findings_spec(),
        &budget(),
        &findings_terminal_check,
    )
    .await
    .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert!(r.repaired);
    // no third request was made
    assert_eq!(stub.seen.lock().unwrap().len(), 2);
    let parsed = parse_findings_checked(&r.final_call.unwrap().arguments);
    assert!(parsed.problem.is_some());
}

struct Hang;

#[async_trait::async_trait]
impl ModelClient for Hang {
    async fn complete(
        &self,
        _m: &[ChatMessage],
        _t: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        std::future::pending().await
    }
    fn route_label(&self) -> String {
        "hang".into()
    }
}

/// A stalled provider is cut at the remaining budget, not at budget plus a
/// grace period; the grace is reserved for the reply to the budget notice.
#[tokio::test]
async fn stalled_provider_is_cut_at_the_time_budget() {
    let tb = toolbox(&std::env::temp_dir());
    let t0 = std::time::Instant::now();
    let r = run_agent_checked(
        &Hang,
        "s",
        "u",
        &tb,
        &terminal_submit_findings_spec(),
        &AgentBudget {
            max_tool_calls: 5,
            max_seconds: 1,
        },
        &findings_terminal_check,
    )
    .await
    .unwrap();
    assert_eq!(r.stopped, StopReason::TimeBudget);
    assert!(r.final_call.is_none());
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        t0.elapsed()
    );
}

#[tokio::test]
async fn bare_text_unrelated_json_is_not_a_submission() {
    // `{}` as text must not count as a terminal call; the nudge then gets a
    // proper call
    let stub = Stub {
        replies: Mutex::new(VecDeque::from(vec![
            text("{}"),
            terminal_call(json!({"findings": [], "coverage": "c"})),
        ])),
        seen: Mutex::new(vec![]),
    };
    let tb = toolbox(&std::env::temp_dir());
    let r = run_agent_checked(
        &stub,
        "s",
        "u",
        &tb,
        &terminal_submit_findings_spec(),
        &budget(),
        &findings_terminal_check,
    )
    .await
    .unwrap();
    assert_eq!(r.stopped, StopReason::Terminal);
    assert!(!r.repaired);
}

// ---------- review identity & reuse ----------

#[test]
fn reuse_requires_complete_outcome_and_identical_key() {
    let mut s = ReviewState::default();
    s.record_outcome("b", "h", "p", "k1", RunStatus::Partial);
    assert!(!s.can_reuse("k1"), "partial results are never reused");
    s.record_outcome("b", "h", "p", "k1", RunStatus::Failed);
    assert!(!s.can_reuse("k1"));
    s.record_outcome("b", "h", "p", "k1", RunStatus::Complete);
    assert!(s.can_reuse("k1"));
    assert!(!s.can_reuse("k2"));
    assert_eq!(s.version, STATE_VERSION);
}

#[test]
fn legacy_state_without_outcome_is_not_reused_but_keeps_ids() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".revera")).unwrap();
    // v1-shaped state: no version / review_key / last_status
    let legacy = json!({
        "reviewed_head": "h", "reviewed_base": "b", "patch_id": "p",
        "summary_comment_id": 77,
        "findings": [{
            "id": "abc", "status": "open", "file": "f.rs", "start_line": 1,
            "title": "t", "posted": true
        }]
    });
    std::fs::write(
        ReviewState::path(dir.path()),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let s = ReviewState::load(dir.path()).unwrap().unwrap();
    assert!(!s.can_reuse(&review_key("b", "tree", "p", &json!({}))));
    assert_eq!(s.summary_comment_id, Some(77));
    assert!(s.has_posted("abc"));
}

#[test]
fn corrupt_state_is_set_aside_and_forces_fresh_review() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".revera")).unwrap();
    let p = ReviewState::path(dir.path());
    std::fs::write(&p, b"{not json").unwrap();
    assert!(ReviewState::load(dir.path()).unwrap().is_none());
    assert!(!p.exists());
    assert!(p.with_extension("json.corrupt").exists());
}

#[test]
fn review_key_is_sensitive_to_tree_and_config_not_secrets() {
    let cfg = json!({"strategy": "baseline", "model": "m1"});
    let k = review_key("b", "tree1", "p", &cfg);
    // same patch-id (whitespace-insensitive) but different exact content
    assert_ne!(k, review_key("b", "tree2", "p", &cfg));
    assert_ne!(k, review_key("b2", "tree1", "p", &cfg));
    assert_ne!(
        k,
        review_key(
            "b",
            "tree1",
            "p",
            &json!({"strategy": "baseline", "model": "m2"})
        )
    );
    assert_eq!(k, review_key("b", "tree1", "p", &cfg));
    // the fingerprint built from a real config never carries key material
    let yaml = r#"
review: {strategy: baseline}
models:
  investigator: {protocol: openai-chat, base_url: "http://x", model: m, api_key_env: SOME_KEY_ENV}
  validator: {protocol: openai-chat, base_url: "http://x", model: m, api_key_env: SOME_KEY_ENV}
vera: {enabled: false}
"#;
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), yaml).unwrap();
    std::env::set_var("SOME_KEY_ENV", "sk-dummy-value-for-test");
    let c = revera::config::Config::load(f.path()).unwrap();
    let fp = c.review_fingerprint("baseline").to_string();
    assert!(!fp.contains("SOME_KEY_ENV"), "{fp}");
    assert!(!fp.contains("sk-dummy"), "{fp}");
    assert!(fp.contains("baseline"));
    // every behaviour-shaping knob changes the key, header values do not
    let base = review_key("b", "t", "p", &c.review_fingerprint("baseline"));
    let mut c2 = c.clone();
    c2.review.concurrency += 1;
    assert_ne!(
        base,
        review_key("b", "t", "p", &c2.review_fingerprint("baseline"))
    );
    let mut c3 = c.clone();
    c3.budget.retries += 1;
    assert_ne!(
        base,
        review_key("b", "t", "p", &c3.review_fingerprint("baseline"))
    );
    let mut c4 = c.clone();
    c4.delegated.max_questions += 1;
    assert_ne!(
        base,
        review_key("b", "t", "p", &c4.review_fingerprint("baseline"))
    );
    let mut c5 = c.clone();
    c5.panel.focuses.push("extra".into());
    assert_ne!(
        base,
        review_key("b", "t", "p", &c5.review_fingerprint("baseline"))
    );
    let mut c6 = c.clone();
    c6.models
        .investigator
        .extra_headers
        .insert("X-Secret".into(), "sk-header-value".into());
    let fp6 = c6.review_fingerprint("baseline").to_string();
    assert_ne!(
        base,
        review_key("b", "t", "p", &c6.review_fingerprint("baseline"))
    );
    assert!(
        fp6.contains("X-Secret") && !fp6.contains("sk-header-value"),
        "{fp6}"
    );
}

// ---------- lifecycle ----------

#[test]
fn resolved_then_reintroduced_reopens_and_republishes() {
    let mut s = ReviewState::default();
    let f = finding("src/a.rs", "k", 10);
    assert!(!s.upsert(&f, FindingState::Open));
    s.mark_posted(&[f.id()]);
    assert!(s.has_posted(&f.id()));

    s.mark(&f.id(), FindingState::Resolved);
    assert!(
        !s.has_posted(&f.id()),
        "resolved findings are republishable"
    );
    assert!(!s.is_tracked_open(&f.id()));

    // reintroduced with the same identity: reopened, posted flag reset
    let reopened = s.upsert(&f, FindingState::Open);
    assert!(reopened);
    assert!(!s.has_posted(&f.id()));
    assert!(s.is_tracked_open(&f.id()));

    // canonical detail survives the round trip so it can be re-validated
    let back = s.find(&f.id()).unwrap().to_finding();
    assert_eq!(back.claim, "claim");
    assert_eq!(back.trigger, "trigger");
    assert_eq!(back.id(), f.id());
}

#[test]
fn state_is_bounded() {
    let mut s = ReviewState::default();
    for i in 0..(revera::state::MAX_STATE_FINDINGS + 50) {
        let f = finding("f.rs", &format!("k{i}"), i as u32 + 1);
        s.upsert(&f, FindingState::Resolved);
    }
    let latest = finding("f.rs", "latest", 1);
    s.upsert(&latest, FindingState::Open);
    s.record_outcome("b", "h", "p", "k", RunStatus::Complete);
    assert!(s.findings.len() <= revera::state::MAX_STATE_FINDINGS);
    assert!(
        s.is_tracked_open(&latest.id()),
        "open findings survive pruning"
    );
}

// ---------- owned summary comment ----------

fn comment(id: u64, body: &str, author: Option<&str>, bot: bool) -> GhComment {
    GhComment {
        id,
        body: body.into(),
        author: author.map(Into::into),
        author_is_bot: bot,
        ..Default::default()
    }
}

#[test]
fn spoofed_marker_comment_is_not_selected() {
    let marker = "<!-- revera-summary -->";
    let state = ReviewState::default();
    let real = format!("{marker}\n## Revera review\n{}", encode_state(&state));
    let comments = vec![
        // someone quoting the marker in the middle of their comment
        comment(
            1,
            &format!("lol look: {marker} {}", encode_state(&state)),
            Some("mallory"),
            false,
        ),
        // marker first but no state blob
        comment(2, &format!("{marker}\nfake"), Some("mallory"), false),
        // marker + blob but a human author while the viewer is known
        comment(3, &real, Some("mallory"), false),
        // another bot seeding a state blob
        comment(5, &real, Some("other-app[bot]"), true),
        comment(4, &real, Some("github-actions[bot]"), true),
    ];
    let me = |viewer: Option<&str>| Identity {
        viewer: viewer.map(Into::into),
        bot_login: "github-actions[bot]".into(),
    };
    let got = find_managed(&comments, marker, None, &me(Some("revera-bot"))).map(|c| c.id);
    assert_eq!(got, None, "unknown human author must not be trusted");
    let got = find_managed(&comments, marker, None, &me(None)).map(|c| c.id);
    assert_eq!(
        got,
        Some(4),
        "only the configured bot login is accepted when viewer unknown"
    );
    let got = find_managed(&comments, marker, None, &me(Some("mallory"))).map(|c| c.id);
    assert_eq!(got, Some(3));
    // a recorded comment id wins outright
    let got = find_managed(&comments, marker, Some(4), &me(Some("revera-bot"))).map(|c| c.id);
    assert_eq!(got, Some(4));
}

// ---------- lexical discovery without Vera ----------

fn git(repo: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

#[tokio::test]
async fn grep_repo_finds_untouched_caller_and_respects_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/pricing.rs"),
        "pub fn discount_for_tier() {}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/checkout.rs"),
        "fn total() { let d = discount_for_tier(); }\n",
    )
    .unwrap();
    std::fs::write(repo.join("untracked.rs"), "discount_for_tier()\n").unwrap();
    git(repo, &["add", "src"]);
    git(repo, &["commit", "-qm", "init"]);

    let tb = toolbox(repo);
    let out = tb
        .call(
            "grep_repo",
            json!({"pattern": "discount_for_tier\\(", "path_glob": "src/**"}),
        )
        .await;
    assert!(out.contains("src/checkout.rs:1:"), "{out}");
    assert!(
        !out.contains("untracked.rs"),
        "untracked files are out of scope: {out}"
    );

    let files = tb.call("find_files", json!({"glob": "**/*.rs"})).await;
    assert!(files.contains("src/checkout.rs") && files.contains("src/pricing.rs"));
    assert!(!files.contains("untracked.rs"));

    // Vera tools are hidden when disabled and never invoked
    let specs: Vec<String> = {
        let mut t = tb;
        t.hide_vera_tools();
        t.specs().into_iter().map(|s| s.name).collect()
    };
    assert!(specs.iter().any(|n| n == "grep_repo"));
    assert!(!specs.iter().any(|n| n.starts_with("vera_")));
}

/// Delegated workers get the same Vera-independent discovery as baseline:
/// with Vera disabled a worker can still locate an untouched caller.
#[tokio::test]
async fn delegated_worker_can_discover_untouched_caller_without_vera() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/pricing.rs"), "pub fn discount() {}\n").unwrap();
    std::fs::write(repo.join("src/checkout.rs"), "fn total() { discount(); }\n").unwrap();
    git(repo, &["add", "src"]);
    git(repo, &["commit", "-qm", "init"]);

    let mut full = toolbox(repo);
    full.hide_vera_tools();
    let worker = full.restricted(revera::pipeline::delegated::WORKER_TOOLS);
    let specs: Vec<String> = worker.specs().into_iter().map(|s| s.name).collect();
    assert!(specs.iter().any(|n| n == "grep_repo"), "{specs:?}");
    assert!(specs.iter().any(|n| n == "find_files"), "{specs:?}");
    assert!(!specs.iter().any(|n| n.starts_with("vera_")));
    let out = worker
        .call(
            "grep_repo",
            json!({"pattern": "discount\\(", "path_glob": "src/**"}),
        )
        .await;
    assert!(out.contains("src/checkout.rs:1:"), "{out}");
}

#[test]
fn publication_failure_downgrades_reusable_outcome() {
    let mut st = ReviewState::default();
    st.record_outcome("b", "h", "p", "key", RunStatus::Complete);
    assert!(st.can_reuse("key"));
    st.mark_publication_incomplete();
    assert!(
        !st.can_reuse("key"),
        "a completed review that never reached the PR must be redone"
    );
}

#[test]
fn ledger_handle_smoke() {
    let l = LedgerHandle::new();
    assert_eq!(l.request_count(), 0);
}

// ---- presentation: every outcome renders status-first and truthfully -------

fn render(s: &revera::report::Summary<'_>) -> String {
    revera::report::summary_markdown(s)
}

#[test]
fn summary_presentation_complete_clean() {
    use revera::report::Summary;
    let s = render(&Summary {
        coverage: "call sites of parse_config",
        status: Some(RunStatus::Complete),
        strategy: "baseline",
        ..Default::default()
    });
    assert!(s.starts_with("## Revera review\n\n**Status: complete**"));
    assert!(s.contains("no new findings"));
    assert!(!s.contains("incomplete"));
}

#[test]
fn summary_presentation_complete_with_findings() {
    use revera::report::Summary;
    let f = finding("src/a.rs", "k", 7);
    let s = render(&Summary {
        findings: &[f],
        coverage: "x",
        status: Some(RunStatus::Complete),
        strategy: "baseline",
        ..Default::default()
    });
    assert!(s.contains("**Status: complete**"));
    assert!(s.contains("1 high"));
    assert!(s.contains("`src/a.rs`:7"));
}

#[test]
fn summary_presentation_partial_zero_findings_is_not_clean() {
    use revera::report::Summary;
    let s = render(&Summary {
        coverage: "nothing",
        status: Some(RunStatus::Partial),
        reason: Some("run deadline reached before validation"),
        strategy: "baseline",
        ..Default::default()
    });
    assert!(s.contains("**Status: partial**"));
    assert!(s.contains("Review incomplete: run deadline reached before validation"));
    assert!(s.contains("Absence of findings is not evidence"));
}

#[test]
fn summary_presentation_reused_and_carried_forward() {
    use revera::report::Summary;
    let s = render(&Summary {
        coverage: "x",
        status: Some(RunStatus::Complete),
        strategy: "baseline",
        reused: true,
        carried_open: 2,
        ..Default::default()
    });
    assert!(s.contains("**Status: complete**"));
    assert!(s.contains("reused completed review of identical content"));
    assert!(s.contains("2 open findings carried forward"));
}

#[test]
fn summary_presentation_resolved_and_reopened() {
    use revera::report::Summary;
    let s = render(&Summary {
        coverage: "x",
        status: Some(RunStatus::Complete),
        strategy: "baseline",
        resolved: &["off-by-one in pager".into()],
        reopened: &["unchecked unwrap in loader".into()],
        ..Default::default()
    });
    assert!(s.contains("### Resolved since last review\n\n- off-by-one in pager"));
    assert!(s.contains(
        "### Reopened (previously resolved, reintroduced)\n\n- unchecked unwrap in loader"
    ));
}

#[test]
fn summary_presentation_retrieval_unavailable_and_provider_timeout() {
    use revera::report::Summary;
    let s = render(&Summary {
        coverage: "x",
        status: Some(RunStatus::Partial),
        reason: Some("investigator: provider request timed out"),
        strategy: "baseline",
        retrieval_unavailable: Some("vera update timed out after 120s"),
        ..Default::default()
    });
    assert!(s.contains("**Status: partial**"));
    assert!(s.contains("provider request timed out"));
    assert!(s.contains("Semantic retrieval unavailable (vera update timed out after 120s)"));
    assert!(s.contains("lexical search only"));
}

#[test]
fn summary_presentation_failed_publication_note() {
    use revera::report::Summary;
    let s = render(&Summary {
        coverage: "x",
        status: Some(RunStatus::Complete),
        strategy: "baseline",
        note: Some("inline review could not be posted (GitHub 422); findings listed here only"),
        ..Default::default()
    });
    assert!(s.contains("inline review could not be posted"));
}
