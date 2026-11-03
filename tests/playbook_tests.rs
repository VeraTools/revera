use revera::config::{Config, Strategy};
use revera::findings::{Finding, collapse};
use revera::pipeline::common::ReviewRequest;
use revera::playbook::Playbook;
use revera::state::{FindingState, ReviewState};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::method};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write(root: &Path, path: &str, text: &str) {
    revera::fsutil::write_repo_file(root, Path::new(path), text.as_bytes()).unwrap();
}

fn fixture() -> (tempfile::TempDir, ReviewRequest) {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    write(d.path(), "bug.rs", "old\nold\nold\n");
    write(
        d.path(),
        ".agents/skills/project/SKILL.md",
        "---\nname: project\ndescription: Project procedure sentinel\n---\nproject-skill-sentinel\n",
    );
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-qm", "base"]);
    write(d.path(), "bug.rs", "new\nnew\nnew\n");
    git(d.path(), &["commit", "-qam", "head"]);
    let req = ReviewRequest {
        repo: d.path().into(),
        base: "HEAD~1".into(),
        head: None,
        title: Some("fixture".into()),
        body: String::new(),
        strategy_override: None,
        force: true,
        repository_public: false,
    };
    (d, req)
}

fn config(url: &str, strategy: &str) -> Config {
    Config::parse(&format!("review: {{strategy: {strategy}, guidance: skills, notes: operator-note-sentinel}}\nmodels:\n  investigator: {{protocol: openai-chat, base_url: {url}, api_key_env: REVERA_TEST_KEY, model: investigator}}\n  validator: {{protocol: openai-chat, base_url: {url}, api_key_env: REVERA_TEST_KEY, model: validator}}\n  lead: {{protocol: openai-chat, base_url: {url}, api_key_env: REVERA_TEST_KEY, model: lead}}\nvera: {{enabled: false}}\nbudget: {{retries: 0}}\n"), "playbook-test").unwrap()
}

fn candidate(key: &str, severity: &str, line: u32) -> Value {
    json!({"defect_key":key,"severity":severity,"category":" LoGiC ","file":"bug.rs","start_line":line,"title":key,"claim":"A concrete claim","trigger":"Input","impact":"Wrong result"})
}

fn terminal(name: &str, arguments: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"done","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]}}]}))
}

fn terminal_name(request: &Value) -> &str {
    request["tools"].as_array().unwrap().last().unwrap()["function"]["name"]
        .as_str()
        .unwrap()
}

fn seed_posted(root: &Path) -> String {
    let f: Finding = serde_json::from_value(candidate("tracked-sentinel", "medium", 3)).unwrap();
    let mut state = ReviewState::default();
    state.upsert(&f, FindingState::Open);
    state.mark_posted(&[f.id()]);
    state.save(root).unwrap();
    f.id()
}

#[tokio::test]
async fn prompts_cover_baseline_panel_and_delegated_context_and_tools() {
    for strategy in ["baseline", "panel", "delegated"] {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(|request: &Request| {
            let v: Value = serde_json::from_slice(&request.body).unwrap();
            match terminal_name(&v) {
                "submit_plan" => terminal("submit_plan", json!({"questions":[{"id":"q1","question":"Check behavior","symbols":[],"expected_evidence":"changed lines","stop_condition":"established","files_hint":["bug.rs"]}]})),
                "submit_worker_result" => terminal("submit_worker_result", json!({"result":"answered","answer":"checked","candidate_findings":[]})),
                "submit_verdict" => terminal("submit_verdict", json!({"validation_status":"accepted","rationale":"still applies"})),
                "submit_findings" => terminal("submit_findings", json!({"findings":[],"coverage":"checked"})),
                name => panic!("unexpected terminal {name}"),
            }
        }).mount(&server).await;
        let (d, req) = fixture();
        seed_posted(d.path());
        let mut cfg = config(&server.uri(), strategy);
        cfg.panel.focuses = vec!["general".into(), "logic,errors,state".into()];
        let (report, _) = revera::pipeline::run(&cfg, &req).await.unwrap();
        assert_eq!(report.status, revera::report::RunStatus::Complete);
        let requests: Vec<Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        let mut investigation = 0;
        for request in &requests {
            let name = terminal_name(request);
            let system = request["messages"][0]["content"].as_str().unwrap();
            let user = request["messages"][1]["content"].as_str().unwrap();
            if name == "submit_verdict" {
                assert!(!user.contains("Class checklist"), "recheck is unchanged");
                continue;
            }
            investigation += 1;
            assert!(system.contains("## Defect classes"), "{strategy}: {name}");
            assert!(system.contains("### filesystem"));
            assert!(
                user.contains("operator-note-sentinel"),
                "{strategy}: {name}"
            );
            assert!(user.contains("tracked-sentinel"), "{strategy}: {name}");
            if name == "submit_worker_result"
                || (strategy == "delegated" && name == "submit_findings")
            {
                assert!(!system.contains("# Reviewer playbook"));
                assert!(
                    request["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|t| t["function"]["name"] == "read_skill")
                );
                assert!(user.contains("Project procedure sentinel"));
            } else {
                assert!(system.contains("# Reviewer playbook"));
            }
        }
        assert_eq!(
            investigation,
            match strategy {
                "baseline" => 1,
                "panel" => 2,
                _ => 3,
            }
        );
        if strategy == "panel" {
            assert!(requests.iter().any(|r| {
                r["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("these defect classes: logic, errors, state")
            }));
            assert!(requests.iter().any(|r| {
                r["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("Cover the whole changed surface")
            }));
        }
    }
}

#[tokio::test]
async fn delegated_synthesis_not_checked_marks_partial_with_legacy_fallback() {
    for (fields, expected_gaps) in [
        (
            json!({"not_checked": [" changed schema ", "", 42]}),
            vec!["changed schema"],
        ),
        (json!({"unresolved": ["legacy area"]}), vec!["legacy area"]),
        (
            json!({"not_checked": ["current area"], "unresolved": ["old area"]}),
            vec!["current area"],
        ),
        (
            json!({"not_checked": [], "unresolved": ["old area"]}),
            vec![],
        ),
    ] {
        let mut submission = json!({"findings": [], "coverage": "checked"});
        submission
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(move |request: &Request| {
                let v: Value = serde_json::from_slice(&request.body).unwrap();
                match terminal_name(&v) {
                    "submit_plan" => terminal(
                        "submit_plan",
                        json!({"questions":[{"id":"q1","question":"Check behavior"}]}),
                    ),
                    "submit_worker_result" => terminal(
                        "submit_worker_result",
                        json!({"result":"answered","answer":"checked","candidate_findings":[]}),
                    ),
                    "submit_findings" => terminal("submit_findings", submission.clone()),
                    name => panic!("unexpected terminal {name}"),
                }
            })
            .expect(3)
            .mount(&server)
            .await;
        let (_data, req) = fixture();
        let cfg = config(&server.uri(), "delegated");
        let (report, _) = revera::pipeline::run(&cfg, &req).await.unwrap();
        assert_eq!(report.coverage_gaps, expected_gaps);
        if expected_gaps.is_empty() {
            assert_eq!(report.status, revera::report::RunStatus::Complete);
            assert_eq!(report.reason, None);
        } else {
            assert_eq!(report.status, revera::report::RunStatus::Partial);
            assert_eq!(
                report.reason.as_deref(),
                Some("lead synthesis reported 1 unchecked area(s)")
            );
        }
    }
}

#[tokio::test]
async fn validator_checklist_is_user_only_and_dedup_removes_plan_and_state_entries() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request: &Request| {
        let v: Value = serde_json::from_slice(&request.body).unwrap();
        match terminal_name(&v) {
            "submit_findings" => terminal("submit_findings", json!({"findings":[candidate("First bug", "medium", 1),candidate("Unrelated title", "high", 2)],"coverage":"checked"})),
            "submit_verdict" => terminal("submit_verdict", json!({"validation_status":"accepted","rationale":"confirmed"})),
            "submit_groups" => terminal("submit_groups", json!({"groups":[["new:0","new:1"]]})),
            _ => panic!("unexpected terminal"),
        }
    }).mount(&server).await;
    let (d, req) = fixture();
    let cfg = config(&server.uri(), "baseline");
    let (report, state) = revera::pipeline::run(&cfg, &req).await.unwrap();
    assert_eq!(report.stats.duplicates, 1);
    assert_eq!(report.stats.accepted, 1);
    assert_eq!(report.findings.len(), 2);
    assert_eq!(report.plan.inline.len(), 1);
    let duplicate = report
        .findings
        .iter()
        .find(|f| f.duplicate_of.is_some())
        .unwrap();
    assert_eq!(
        duplicate.duplicate_of,
        Some(
            report
                .findings
                .iter()
                .find(|f| f.duplicate_of.is_none())
                .unwrap()
                .id()
        )
    );
    assert_eq!(state.findings.len(), 1);
    assert!(state.find(&duplicate.id()).is_none());
    assert!(
        ReviewState::load(d.path())
            .unwrap()
            .unwrap()
            .find(&duplicate.id())
            .is_none()
    );
    assert_eq!(report.plan.state.findings.len(), 1);
    assert_eq!(report.actionable_findings().len(), 1);
    assert_eq!(revera::report::surfaced_ids(&report).len(), 1);
    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    let validators: Vec<_> = requests
        .iter()
        .filter(|r| terminal_name(r) == "submit_verdict")
        .collect();
    assert_eq!(validators.len(), 2);
    assert_eq!(validators[0]["messages"][0], validators[1]["messages"][0]);
    for r in validators {
        assert!(
            !r["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Class checklist")
        );
        assert!(
            r["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("Class checklist (logic). Use it to look for counterevidence:")
        );
    }
    assert!(report.findings.iter().all(|f| f.category == "logic"));
    let dedup = requests
        .iter()
        .find(|r| terminal_name(r) == "submit_groups")
        .unwrap();
    assert_eq!(dedup["model"], "validator");
    assert_eq!(dedup["tools"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dedup_carried_wins_and_errors_or_budget_fail_open() {
    for mode in ["posted", "malformed", "tool", "provider", "budget"] {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(move |request: &Request| {
            let v: Value = serde_json::from_slice(&request.body).unwrap();
            match terminal_name(&v) {
                "submit_findings" => terminal("submit_findings", json!({"findings":[candidate("New wording", "high", 1)],"coverage":"checked"})),
                "submit_verdict" => terminal("submit_verdict", json!({"validation_status":"accepted","rationale":"confirmed"})),
                "submit_groups" => match mode {
                    "posted" => terminal("submit_groups", json!({"groups":[["new:0","posted:0"]]})),
                    "malformed" => terminal("submit_groups", json!({"groups":[["new:0","posted:0"],["unknown","new:0"]]})),
                    "tool" => terminal("read_file", json!({"path":"bug.rs"})),
                    "provider" => ResponseTemplate::new(500),
                    _ => panic!("budget should skip dedup"),
                },
                _ => panic!("unexpected terminal"),
            }
        }).mount(&server).await;
        let (d, req) = fixture();
        let prior = seed_posted(d.path());
        let mut cfg = config(&server.uri(), "baseline");
        if mode == "budget" {
            cfg.budget.run_max_requests = 3;
        }
        let (report, state) = revera::pipeline::run(&cfg, &req).await.unwrap();
        assert_eq!(
            report.status,
            revera::report::RunStatus::Complete,
            "{mode}: {:?}",
            report.reason
        );
        assert!(report.reason.is_none());
        if mode == "posted" {
            assert_eq!(report.stats.duplicates, 1);
            assert_eq!(report.findings[0].duplicate_of, Some(prior.clone()));
            assert!(report.plan.inline.is_empty());
            assert_eq!(state.findings.len(), 1);
            assert_eq!(report.carried.len(), 1);
        } else {
            assert_eq!(report.stats.duplicates, 0);
            assert!(report.findings[0].duplicate_of.is_none());
            assert_eq!(report.plan.inline.len(), 1);
            assert_eq!(state.findings.len(), 2);
        }
        assert!(state.find(&prior).unwrap().posted);
    }
}

#[test]
fn categories_normalize_without_changing_identity_and_legacy_state_decodes() {
    let p = Playbook::builtin();
    assert_eq!(p.normalize_category(" LoGiC "), "logic");
    assert_eq!(p.normalize_category("unknown"), "other");
    assert_eq!(p.normalize_category(""), "other");
    let mut first: Finding = serde_json::from_value(candidate("key", "low", 1)).unwrap();
    let id = first.id();
    first.category.clear();
    let mut second = first.clone();
    second.category = "state".into();
    let mut third = first.clone();
    third.category = "errors".into();
    let merged = collapse(vec![first, second, third]);
    assert_eq!(merged[0].category, "state");
    assert_eq!(merged[0].id(), id);
    let state: revera::state::StateFinding = serde_json::from_value(
        json!({"id":id,"status":"open","file":"bug.rs","start_line":1,"title":"legacy"}),
    )
    .unwrap();
    assert!(state.category.is_empty());
    let mut carried =
        serde_json::to_value(revera::report::CarriedFinding::from_state(&state)).unwrap();
    carried.as_object_mut().unwrap().remove("category");
    let carried: revera::report::CarriedFinding = serde_json::from_value(carried).unwrap();
    assert!(carried.to_finding().category.is_empty());
}

async fn queued_lanes_respect_investigation_deadline(strategy: &str, seconds: u64) {
    use std::time::Duration;
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request: &Request| {
        let v: Value = serde_json::from_slice(&request.body).unwrap();
        match terminal_name(&v) {
            "submit_plan" => terminal("submit_plan", json!({"questions":(1..=5).map(|i| json!({"id":format!("q{i}"),"question":"Check behavior"})).collect::<Vec<_>>()})),
            "submit_worker_result" => terminal("submit_worker_result", json!({"result":"answered","answer":"checked","candidate_findings":[]})).set_delay(Duration::from_secs(4)),
            "submit_findings" => {
                let response = terminal("submit_findings", json!({"findings":[],"coverage":"checked"}));
                if v["model"] == "lead" { response } else { response.set_delay(Duration::from_secs(4)) }
            }
            name => panic!("unexpected terminal {name}"),
        }
    }).mount(&server).await;
    let (_d, req) = fixture();
    let mut cfg = config(&server.uri(), strategy);
    // Serial admission keeps later lanes queued until the one-second cutoff.
    cfg.review.concurrency = if seconds == 1 { 1 } else { 4 };
    cfg.panel.focuses = vec![
        "general".into(),
        "logic,errors,state".into(),
        "contracts,config,concurrency".into(),
        "filesystem,subprocess,security,resources".into(),
        "docs,tests".into(),
    ];
    cfg.delegated.max_questions = 5;
    cfg.budget.run_max_seconds = seconds;
    let (report, _) = revera::pipeline::run(&cfg, &req).await.unwrap();
    let lanes: Vec<_> = report
        .timing
        .phases
        .iter()
        .filter(|p| p.phase == "lane")
        .collect();
    assert_eq!(lanes.len(), 5);
    let skipped: Vec<_> = lanes.iter().filter(|p| p.outcome == "skipped").collect();
    if seconds == 1 {
        assert!(
            !skipped.is_empty(),
            "queued lanes must be skipped: {lanes:?}"
        );
    }
    // Under load the fifth lane may reach admission on either side of the
    // cutoff; derive counts from actual admission, not a scheduling guess.
    let ran = lanes.len() - skipped.len();
    assert!(ran > 0, "fixture must exercise a running lane: {lanes:?}");
    assert!(
        lanes
            .iter()
            .filter(|p| p.outcome != "skipped")
            .all(|p| p.outcome == "timeout")
    );
    let investigation_ms = if seconds == 4 { 3000 } else { 1000 };
    // 800 ms tolerates timeout wake-up jitter but rejects an extra 2 s grace turn.
    assert!(
        lanes
            .iter()
            .all(|p| p.start_ms + p.duration_ms < investigation_ms + 800),
        "lanes must leave the validation reserve untouched: {lanes:?}"
    );
    assert_eq!(report.status, revera::report::RunStatus::Partial);
    // A running lane must have passed admission before the investigation
    // cutoff, even if the runtime resumes its timeout late.
    assert!(
        lanes
            .iter()
            .filter(|p| p.outcome != "skipped")
            .all(|p| p.start_ms < investigation_ms)
    );
    if !skipped.is_empty() {
        assert!(report.reason.as_deref().unwrap().contains(&format!(
            "investigation deadline or run budget exhausted before {} lanes",
            skipped.len()
        )));
    }
    let requests = server.received_requests().await.unwrap();
    let lane_requests = requests
        .iter()
        .filter(|r| {
            let v: Value = serde_json::from_slice(&r.body).unwrap();
            if strategy == "panel" {
                terminal_name(&v) == "submit_findings"
            } else {
                terminal_name(&v) == "submit_worker_result"
            }
        })
        .count();
    // every lane that ran sent its first request; a lane cut off at its
    // soft deadline may also send one wrap-up request
    assert!(
        (ran..=2 * ran).contains(&lane_requests),
        "lane requests: {lane_requests}"
    );
    if strategy == "delegated" {
        assert!(
            report
                .timing
                .phases
                .iter()
                .any(|p| p.phase == "synthesis" && p.outcome == "skipped")
        );
    }
}

#[tokio::test]
async fn panel_soft_deadline_allows_terminal_wrap_up_before_investigation_cutoff() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(|request: &Request| {
            let v: Value = serde_json::from_slice(&request.body).unwrap();
            if v["tools"].as_array().unwrap().len() == 1 {
                terminal(
                    "submit_findings",
                    json!({"findings":[],"coverage":"checked"}),
                )
            } else {
                terminal("list_changed_files", json!({}))
                    .set_delay(std::time::Duration::from_millis(1200))
            }
        })
        .mount(&server)
        .await;
    let (_data, req) = fixture();
    let mut cfg = config(&server.uri(), "panel");
    cfg.panel.focuses = vec!["general".into()];
    cfg.budget.run_max_seconds = 6;
    let (report, _) = revera::pipeline::run(&cfg, &req).await.unwrap();
    assert_eq!(
        report.status,
        revera::report::RunStatus::Complete,
        "{:?}",
        report.reason
    );
    let requests = server.received_requests().await.unwrap();
    let final_request: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    assert_eq!(final_request["tools"].as_array().unwrap().len(), 1);
    assert!(
        final_request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"]
                .as_str()
                .is_some_and(|text| text.contains("Budget exhausted")))
    );
    assert!(
        report
            .timing
            .phases
            .iter()
            .any(|p| p.phase == "lane" && p.outcome == "ok")
    );
}

#[tokio::test]
async fn queued_panel_lanes_skip_after_investigation_deadline() {
    for seconds in [4, 1] {
        queued_lanes_respect_investigation_deadline("panel", seconds).await;
    }
}

#[tokio::test]
async fn queued_delegated_workers_skip_after_investigation_deadline() {
    for seconds in [4, 1] {
        queued_lanes_respect_investigation_deadline("delegated", seconds).await;
    }
}

#[tokio::test]
async fn dedup_uses_post_validation_time_but_preserves_finalization_margin() {
    use revera::findings::ValidationStatus;
    use revera::pipeline::common::{PrepareOut, prepare};
    use std::time::{Duration, Instant};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(terminal(
            "submit_groups",
            json!({"groups":[["new:0","new:1"]]}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let (_d, req) = fixture();
    let cfg = config(&server.uri(), "baseline");
    let PrepareOut::Ready(mut prep) = prepare(&cfg, &req, "baseline").await.unwrap() else {
        panic!("expected prepared review")
    };
    let mut findings: Vec<Finding> = [candidate("A", "medium", 1), candidate("B", "high", 2)]
        .into_iter()
        .map(|v| {
            let mut f: Finding = serde_json::from_value(v).unwrap();
            f.validation_status = Some(ValidationStatus::Accepted);
            f
        })
        .collect();
    prep.deadline = Instant::now() + Duration::from_secs(30);
    prep.reserve = Duration::from_secs(120); // investigation is over; validation finished
    assert_eq!(
        revera::pipeline::dedup::run(&cfg, &prep, &mut findings).await,
        1
    );
    assert_eq!(findings[0].duplicate_of, Some(findings[1].id()));
    findings[0].duplicate_of = None;
    prep.deadline = Instant::now() + Duration::from_secs(9);
    assert_eq!(
        revera::pipeline::dedup::run(&cfg, &prep, &mut findings).await,
        0
    );
    assert!(findings.iter().all(|f| f.duplicate_of.is_none()));
    assert!(prep.partial_reasons.is_empty());
}

#[tokio::test]
async fn rediscovered_unposted_recheck_has_one_id_before_dedup() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request: &Request| {
        let v: Value = serde_json::from_slice(&request.body).unwrap();
        match terminal_name(&v) {
            "submit_findings" => terminal("submit_findings", json!({"findings":[candidate("A", "medium", 1), candidate("B different title", "high", 2)],"coverage":"checked"})),
            "submit_verdict" => terminal("submit_verdict", json!({"validation_status":"accepted","rationale":"confirmed"})),
            "submit_groups" => terminal("submit_groups", json!({"groups":[["new:0","new:1"]]})),
            name => panic!("unexpected terminal {name}"),
        }
    }).mount(&server).await;
    let (d, req) = fixture();
    let a: Finding = serde_json::from_value(candidate("A", "medium", 1)).unwrap();
    let b: Finding = serde_json::from_value(candidate("B different title", "high", 2)).unwrap();
    let mut prior = ReviewState::default();
    prior.upsert(&a, FindingState::Open);
    prior.save(d.path()).unwrap();
    let cfg = config(&server.uri(), "baseline");
    let (report, state) = revera::pipeline::run(&cfg, &req).await.unwrap();
    assert_eq!(
        report.stats.accepted, 1,
        "rediscovered A must not escape removal via its reentry"
    );
    assert_eq!(report.stats.duplicates, 1);
    assert_eq!(report.findings.len(), 2);
    assert_eq!(
        report.findings.iter().filter(|f| f.id() == a.id()).count(),
        1
    );
    assert_eq!(
        report
            .findings
            .iter()
            .find(|f| f.id() == a.id())
            .unwrap()
            .duplicate_of,
        Some(b.id())
    );
    assert_eq!(report.plan.inline.len(), 1);
    assert_eq!(report.actionable_findings()[0].id(), b.id());
    assert!(state.find(&a.id()).is_none());
    assert!(report.plan.state.find(&a.id()).is_none());
    assert!(
        ReviewState::load(d.path())
            .unwrap()
            .unwrap()
            .find(&a.id())
            .is_none()
    );
    let requests = server.received_requests().await.unwrap();
    let dedup: Value = requests
        .iter()
        .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap())
        .find(|r| terminal_name(r) == "submit_groups")
        .unwrap();
    let items: Vec<Value> =
        serde_json::from_str(dedup["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn deep_profile_and_fingerprint_obey_playbook_and_explicit_focuses() {
    let mut cfg = config("https://api.example.com/v1", "baseline");
    let original = cfg.clone();
    cfg.apply_profile("deep").unwrap();
    assert_eq!(cfg.review.strategy, Strategy::Panel);
    assert_eq!(
        cfg.effective_strategy(Some(Strategy::Baseline)),
        Strategy::Panel
    );
    assert_eq!(
        cfg.effective_strategy(Some(Strategy::Delegated)),
        Strategy::Delegated
    );
    assert_eq!(
        cfg.panel.focuses,
        [
            "general",
            "logic,errors,state",
            "contracts,config,concurrency",
            "filesystem,subprocess,security,resources",
            "docs,tests"
        ]
    );
    assert_eq!(
        cfg.budget.agent_max_tool_calls,
        original.budget.agent_max_tool_calls
    );
    assert_eq!(
        cfg.budget.agent_max_seconds,
        original.budget.agent_max_seconds
    );
    assert_eq!(
        cfg.budget.run_max_requests,
        original.budget.run_max_requests * 3
    );
    assert_eq!(
        cfg.budget.run_max_seconds,
        (original.budget.run_max_seconds * 2).min(3000)
    );
    assert_eq!(cfg.review.max_diff_bytes, 400000);
    assert_eq!(cfg.review.max_findings, 20);
    assert_eq!(
        cfg.panel.scout_max_tool_calls,
        cfg.budget.agent_max_tool_calls
    );
    let mut explicit = Config::parse("panel: {focuses: [general]}\nmodels: {investigator: {protocol: scripted, model: m, script: /dev/null}}\n", "test").unwrap();
    explicit.apply_profile("deep").unwrap();
    assert_eq!(explicit.panel.focuses, ["general"]);
    assert_eq!(
        explicit.panel.scout_max_tool_calls,
        explicit.budget.agent_max_tool_calls
    );
    explicit.budget.run_max_seconds = 4000;
    explicit.apply_profile("deep").unwrap();
    assert_eq!(explicit.budget.run_max_seconds, 3000);
    let mut custom_deep = Config::parse("profiles: {deep: {review: {max_findings: 17}}}\nmodels: {investigator: {protocol: scripted, model: m, script: /dev/null}}\n", "test").unwrap();
    custom_deep.apply_profile("deep").unwrap();
    assert_eq!(custom_deep.effective_strategy(None), Strategy::Baseline);
    assert_eq!(custom_deep.review.max_findings, 17);
    assert_eq!(custom_deep.panel.focuses, ["general", "cross-file"]);
    assert_eq!(
        custom_deep.panel.scout_max_tool_calls,
        original.panel.scout_max_tool_calls
    );
    let mut explicit_calls = Config::parse("panel: {scout_max_tool_calls: 15}\nbudget: {agent_max_tool_calls: 50}\nmodels: {investigator: {protocol: scripted, model: m, script: /dev/null}}\n", "test").unwrap();
    explicit_calls.apply_profile("deep").unwrap();
    assert_eq!(explicit_calls.panel.scout_max_tool_calls, 15);
    assert_eq!(explicit_calls.budget.agent_max_tool_calls, 50);
    let mut delegated = config("https://api.example.com/v1", "delegated");
    delegated.apply_profile("deep").unwrap();
    assert_eq!(delegated.review.strategy, Strategy::Delegated);
    assert_eq!(
        delegated.panel.scout_max_tool_calls,
        original.panel.scout_max_tool_calls
    );
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "reviewer.md", "An alternate operator method");
    let mut changed = original.clone();
    changed.playbook = Playbook::load(Some(d.path())).unwrap();
    assert_ne!(
        original.review_fingerprint("baseline"),
        changed.review_fingerprint("baseline")
    );
    assert_eq!(original.vera.index_key(), changed.vera.index_key());
    assert!(
        !changed
            .review_fingerprint("baseline")
            .to_string()
            .contains("alternate operator")
    );
}

#[test]
fn disabled_skills_leave_generated_focuses_but_not_explicit_ones() {
    let d = tempfile::tempdir().unwrap();
    for name in ["logic", "docs", "tests"] {
        write(
            d.path(),
            &format!("skills/{name}.md"),
            &format!("---\nname: {name}\ndescription: off\nenabled: false\n---\nOff\n"),
        );
    }
    let playbook = Playbook::load(Some(d.path())).unwrap();
    let mut cfg = config("https://api.example.com/v1", "delegated");
    cfg.apply_profile("deep").unwrap();
    assert_eq!(
        cfg.effective_strategy(Some(Strategy::Baseline)),
        Strategy::Panel
    );
    assert_eq!(cfg.scout_tool_calls(), cfg.budget.agent_max_tool_calls);
    cfg.playbook = playbook.clone();
    cfg.prune_disabled_focuses();
    assert_eq!(
        cfg.panel.focuses,
        [
            "general",
            "errors,state",
            "contracts,config,concurrency",
            "filesystem,subprocess,security,resources"
        ]
    );
    revera::pipeline::panel::validate_focuses(&cfg.panel.focuses, &cfg.playbook).unwrap();
    let mut explicit = Config::parse("panel: {focuses: [general, logic]}\nmodels: {investigator: {protocol: scripted, model: m, script: /dev/null}}\n", "test").unwrap();
    explicit.playbook = playbook;
    explicit.prune_disabled_focuses();
    assert_eq!(explicit.panel.focuses, ["general", "logic"]);
    assert!(
        revera::pipeline::panel::validate_focuses(&explicit.panel.focuses, &explicit.playbook)
            .is_err()
    );
}

#[tokio::test]
async fn focuses_allow_custom_classes_and_aliases_but_unknown_names_fail_before_models() {
    let server = MockServer::start().await;
    let (_d, req) = fixture();
    let mut cfg = config(&server.uri(), "panel");
    cfg.panel.focuses = vec!["nonexistent".into()];
    let error = revera::pipeline::run(&cfg, &req)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("nonexistent") && error.contains("valid names") && error.contains("logic")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "skills/custom.md",
        "---\nname: custom\ndescription: Custom checks\n---\nCheck custom behavior\n",
    );
    let p = Playbook::load(Some(d.path())).unwrap();
    assert!(
        revera::pipeline::panel::focus_addendum_for("custom", &p)
            .unwrap()
            .contains("custom")
    );
    assert!(
        revera::pipeline::panel::focus_addendum_for("cross-file,data", &p)
            .unwrap()
            .contains("contracts, logic")
    );
    assert!(revera::pipeline::panel::focus_addendum_for("custom,missing", &p).is_err());
    assert!(
        revera::tools::submit_findings_spec(&p)
            .parameters
            .to_string()
            .contains("custom")
    );
    assert!(
        revera::tools::submit_worker_result_spec(&p).parameters["properties"]["candidate_findings"]
            ["items"]["properties"]["category"]
            .is_object()
    );
}
