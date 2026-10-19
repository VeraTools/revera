use super::*;
use crate::agent::AgentTools;
use serde_json::json;
use std::path::Path;

fn temp() -> tempfile::TempDir {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/tmp"));
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}
fn git(repo: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().into()
}
fn fixture(root: &Path) -> String {
    std::fs::create_dir(root).unwrap();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("bug.py"), "def add(a, b):\n    return a - b\n").unwrap();
    std::fs::write(
        root.join("test_bug.py"),
        "from bug import add\nassert add(2, 3) == 5\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
        ],
    );
    git(root, &["rev-parse", "HEAD"])
}
fn finding() -> Finding {
    serde_json::from_value(json!({"defect_key":"addition-subtracts","severity":"medium","file":"bug.py","start_line":2,"title":"Addition subtracts","claim":"add(2,3) returns -1 instead of 5","trigger":"positive inputs","impact":"wrong arithmetic result","introduced_by_change":true,"validation_status":"accepted","validated_fix":"Return a + b instead of a - b."})).unwrap()
}
fn request() -> Request {
    Request {
        head: "a".repeat(40),
        findings: vec![finding()],
        budget: Budget::default(),
        constraints: String::new(),
        patch: None,
        tests: None,
    }
}
fn change() -> workspace::FileChange {
    workspace::FileChange {
        path: "src/main.rs".into(),
        status: workspace::Status::Modified,
        old_mode: Some(0o100644),
        new_mode: Some(0o100644),
        binary: false,
        additions: 1,
        deletions: 1,
        size: 4,
        new_content: Some("new\n".into()),
        old_content: Some("old\n".into()),
    }
}
fn patch(f: workspace::FileChange) -> workspace::Patch {
    workspace::Patch {
        files: vec![f],
        unified: "diff".into(),
    }
}

#[test]
fn policy_each_rejection_and_warning() {
    let policy = policy::Policy::default();
    assert!(policy::check(&patch(change()), &policy).allowed());
    for p in [
        "../x",
        "/absolute",
        "a/../../x",
        "a\\b",
        "C:/x",
        ".git/config",
        "a/.GIT/config",
        "a//b",
        "./x",
        "a\0b",
        "a\nb",
    ] {
        let mut f = change();
        f.path = p.into();
        assert!(
            policy::check(&patch(f), &policy)
                .errors
                .iter()
                .any(|e| e.rule == "path"),
            "{p}"
        );
    }
    for (rule, f) in [
        ("symlink", {
            let mut f = change();
            f.new_mode = Some(0o120000);
            f
        }),
        ("symlink", {
            let mut f = change();
            f.old_mode = Some(0o120000);
            f
        }),
        ("gitlink", {
            let mut f = change();
            f.new_mode = Some(0o160000);
            f
        }),
        ("gitlink", {
            let mut f = change();
            f.old_mode = Some(0o160000);
            f
        }),
        ("gitmodules", {
            let mut f = change();
            f.path = ".gitmodules".into();
            f
        }),
        ("executable", {
            let mut f = change();
            f.new_mode = Some(0o100755);
            f
        }),
        ("binary", {
            let mut f = change();
            f.binary = true;
            f
        }),
        ("file_size", {
            let mut f = change();
            f.size = 1024 * 1024 + 1;
            f
        }),
        ("nul", {
            let mut f = change();
            f.new_content = Some("a\0b".into());
            f
        }),
        ("credential_material", {
            let mut f = change();
            f.new_content = Some(format!("ghp_{}", "A".repeat(40)));
            f
        }),
    ] {
        assert!(
            policy::check(&patch(f), &policy)
                .errors
                .iter()
                .any(|e| e.rule == rule),
            "{rule}"
        );
    }
    let mut f = change();
    f.old_mode = Some(0o100755);
    f.new_mode = Some(0o100755);
    assert!(policy::check(&patch(f), &policy).allowed());
    for p in [
        ".env",
        ".ENV",
        "sub/.ENV.LOCAL",
        "sub/.env.local",
        "private.pem",
        ".npmrc",
        "sub/id_rsa",
        ".revera/key",
    ] {
        let mut f = change();
        f.path = p.into();
        assert!(
            policy::check(&patch(f), &policy)
                .errors
                .iter()
                .any(|e| e.rule == "sensitive"),
            "{p}"
        );
    }
    for p in [
        ".github/workflows/test.yml",
        ".github/actions/build/action.yml",
        ".GitHub/Actions/build/action.yml",
        ".GitHub/Workflows/test.yml",
        ".circleci/config.yml",
        ".gitlab-ci.yml",
        "azure-pipelines.yml",
        "Jenkinsfile",
        ".buildkite/pipeline.yml",
        "bitbucket-pipelines.yml",
    ] {
        let mut f = change();
        f.path = p.into();
        assert!(
            policy::check(&patch(f.clone()), &policy)
                .errors
                .iter()
                .any(|e| e.rule == "workflow"),
            "{p}"
        );
        assert!(
            policy::check(
                &patch(f),
                &policy::Policy {
                    allow_workflow_changes: true,
                    ..policy.clone()
                }
            )
            .allowed()
        );
    }
    for (rule, p, f) in [
        (
            "patch_size",
            policy::Policy {
                max_patch_bytes: 3,
                ..policy.clone()
            },
            change(),
        ),
        (
            "file_count",
            policy::Policy {
                max_files: 0,
                ..policy.clone()
            },
            change(),
        ),
        (
            "deleted_file_count",
            policy::Policy {
                max_deleted_files: 0,
                ..policy.clone()
            },
            {
                let mut f = change();
                f.status = workspace::Status::Deleted;
                f
            },
        ),
    ] {
        assert!(
            policy::check(&patch(f), &p)
                .errors
                .iter()
                .any(|e| e.rule == rule)
        );
    }
    for (rule, old, new) in [
        (
            "conflict_markers",
            "old\n",
            "<<<<<<< ours\n=======\n>>>>>>> theirs\n",
        ),
        ("crlf_change", "old\n", "new\r\n"),
        ("trailing_whitespace_only", "text\n", "text  \n"),
    ] {
        let mut f = change();
        f.old_content = Some(old.into());
        f.new_content = Some(new.into());
        let result = policy::check(&patch(f), &policy);
        assert!(result.allowed());
        assert!(result.warnings.iter().any(|e| e.rule == rule));
    }
}

#[tokio::test]
async fn raw_export_diff_and_apply_ignore_attributes_filters_hooks_and_agent_git() {
    let t = temp();
    let repo = t.path().join("repo");
    fixture(&repo);
    std::fs::write(
        repo.join(".gitattributes"),
        "bug.py export-ignore\n*.txt filter=evil\n*.py diff=evil\n*.md export-subst\n",
    )
    .unwrap();
    std::fs::write(repo.join("literal.md"), "$Format:%H$\n").unwrap();
    std::fs::write(repo.join("plain.txt"), "literal\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "attributes",
        ],
    );
    let marker = t.path().join("FILTER_RAN");
    git(
        &repo,
        &[
            "config",
            "filter.evil.smudge",
            &format!("touch {}; cat", marker.display()),
        ],
    );
    git(
        &repo,
        &[
            "config",
            "filter.evil.clean",
            &format!("touch {}; cat", marker.display()),
        ],
    );
    git(
        &repo,
        &[
            "config",
            "diff.evil.command",
            &format!("touch {}", marker.display()),
        ],
    );
    git(&repo, &["config", "core.hooksPath", "/does/not/exist"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let ws = workspace::prepare(&repo.join(".git"), &sha, &t.path().join("job"))
        .await
        .unwrap();
    assert!(ws.tree.join("bug.py").exists());
    assert!(!ws.tree.join(".git").exists());
    assert_eq!(
        std::fs::read_to_string(ws.tree.join("literal.md")).unwrap(),
        "$Format:%H$\n"
    );
    assert!(ws.diff().await.unwrap().files.is_empty());
    std::fs::create_dir_all(ws.tree.join(".git/hooks")).unwrap();
    std::fs::write(
        ws.tree.join(".git/config"),
        "[diff]\n external = touch HACKED\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.tree.join("nested/.git")).unwrap();
    std::fs::write(ws.tree.join("nested/.git/HEAD"), "ref: refs/heads/bad\n").unwrap();
    std::fs::write(ws.tree.join("bug.py"), "def add(a, b):\n    return a + b\n").unwrap();
    std::fs::write(ws.tree.join("new.txt"), "new content\n").unwrap();
    let p = ws.diff().await.unwrap();
    assert_eq!(p.files.len(), 2);
    assert!(p.unified.contains("+    return a + b"));
    assert!(!p.unified.contains(".git/"));
    assert!(!marker.exists());
    assert!(policy::check(&p, &policy::Policy::default()).allowed());
    ws.apply_check(&p).await.unwrap();
    let mut corrupt = p;
    corrupt.unified = corrupt
        .unified
        .replace("-    return a - b", "-    wrong original");
    assert!(ws.apply_check(&corrupt).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn snapshot_symlinks_gitlinks_and_modes_are_not_hidden() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let t = temp();
    let repo = t.path().join("repo");
    let sha = fixture(&repo);
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sha},submodule"),
        ],
    );
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "gitlink",
        ],
    );
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let ws = workspace::prepare(&repo.join(".git"), &sha, &t.path().join("job"))
        .await
        .unwrap();
    assert!(ws.diff().await.unwrap().files.is_empty());
    std::fs::remove_dir(ws.tree.join("submodule")).unwrap();
    std::fs::write(ws.tree.join("submodule"), "not a submodule").unwrap();
    symlink("/etc/passwd", ws.tree.join("escape")).unwrap();
    std::fs::set_permissions(
        ws.tree.join("bug.py"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let p = ws.diff().await.unwrap();
    let r = policy::check(&p, &policy::Policy::default());
    for rule in ["gitlink", "symlink", "executable"] {
        assert!(r.errors.iter().any(|e| e.rule == rule), "{rule}");
    }
}

#[tokio::test]
async fn tools_confinement_unique_replace_and_size_caps() {
    let t = temp();
    let root = t.path().join("tree");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.txt"), "one one\nsecretless\n").unwrap();
    let tools = tools::FixTools::new(&root, true).unwrap();
    std::fs::write(root.join("overlap.txt"), "aaa").unwrap();
    assert!(
        tools
            .call(
                "replace_in_file",
                json!({"path":"overlap.txt","old":"aa","new":"b"})
            )
            .await
            .contains("exactly once")
    );
    for path in [
        "../outside",
        "/etc/passwd",
        "a/../../b",
        ".git/config",
        ".env",
        "C:\\x",
        "a//b",
    ] {
        for (tool, args) in [
            ("read_file", json!({"path":path})),
            ("write_file", json!({"path":path,"content":"bad"})),
            ("delete_file", json!({"path":path})),
        ] {
            assert!(
                tools.call(tool, args).await.contains("error"),
                "{tool}: {path}"
            );
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(t.path(), root.join("escape")).unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("alias")).unwrap();
        for p in ["escape/stolen", "alias"] {
            assert!(
                tools
                    .call("write_file", json!({"path":p,"content":"bad"}))
                    .await
                    .contains("error")
            );
        }
        assert!(
            !tools
                .call("find_files", json!({"glob":"**"}))
                .await
                .contains("escape")
        );
    }
    assert!(
        tools
            .call(
                "replace_in_file",
                json!({"path":"a.txt","old":"one","new":"two"})
            )
            .await
            .contains("exactly once")
    );
    assert!(
        tools
            .call(
                "replace_in_file",
                json!({"path":"a.txt","old":"missing","new":"two"})
            )
            .await
            .contains("exactly once")
    );
    assert_eq!(
        tools
            .call(
                "replace_in_file",
                json!({"path":"a.txt","old":"one one","new":"two"})
            )
            .await,
        "written"
    );
    assert_eq!(
        tools
            .call(
                "write_file",
                json!({"path":"sub/new.txt","content":"needle"})
            )
            .await,
        "written"
    );
    assert!(
        tools
            .call("grep", json!({"pattern":"needle"}))
            .await
            .contains("sub/new.txt:1:needle")
    );
    assert!(
        tools
            .call(
                "write_file",
                json!({"path":"big.txt","content":"x".repeat(1024 * 1024 + 1)})
            )
            .await
            .contains("error")
    );
    std::fs::write(root.join("big.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(
        tools
            .call("read_file", json!({"path":"big.txt"}))
            .await
            .contains("error")
    );
    let ro = tools::FixTools::new(&root, false).unwrap();
    assert!(!ro.specs().iter().any(|s| s.name == "write_file"));
    assert!(
        ro.call("write_file", json!({"path":"a.txt","content":"bad"}))
            .await
            .contains("read-only")
    );
    assert_eq!(
        tools
            .call("delete_file", json!({"path":"sub/new.txt"}))
            .await,
        "deleted"
    );
}

fn scripted_config(t: &Path, turns: Value, role: &str) -> Config {
    let file = t.join(format!("{}.json", uuid::Uuid::new_v4().simple()));
    std::fs::write(&file, json!({"roles":{role:[turns]}}).to_string()).unwrap();
    Config::parse(&json!({"models":{"investigator":{"protocol":"scripted","script":file,"model":"mock"}},"vera":{"enabled":false}}).to_string(), "test").unwrap()
}
#[tokio::test]
async fn generate_verify_terminal_repair_text_and_request_budget() {
    let t = temp();
    let r = request();
    let id = r.findings[0].id();
    let good = json!({"summary":"fixed","per_finding":[{"id":id,"disposition":"fixed","note":"root cause"}]});
    let cfg = scripted_config(
        t.path(),
        json!([
            {"tool_calls":[{"name":"write_file","arguments":{"path":"bug.py","content":"fixed\n"}}]},
            {"expect_tool_result_contains":"written","tool_calls":[{"name":"submit_fix","arguments":{"summary":"bad","per_finding":[]}}]},
            {"expect_tool_result_contains":"each selected finding","content":format!("```json\n{good}\n```")}
        ]),
        "fixer",
    );
    let StageResult::Generate(result) = run_agent(&cfg, &r, t.path(), Stage::Generate)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::Terminal);
    assert!(result.repaired);
    assert!(result.submission.is_some());
    assert_eq!(result.ledger.requests, 3);
    assert_eq!(
        std::fs::read_to_string(t.path().join("bug.py")).unwrap(),
        "fixed\n"
    );
    let cfg = scripted_config(
        t.path(),
        json!([{"tool_calls":[{"name":"submit_fix","arguments":{}}]},{"tool_calls":[{"name":"submit_fix","arguments":{}}]}]),
        "fixer",
    );
    let StageResult::Generate(result) = run_agent(&cfg, &r, t.path(), Stage::Generate)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::InvalidTerminal);
    assert!(result.submission.is_none());
    let cfg = scripted_config(
        t.path(),
        json!([{"tool_calls":[{"name":"read_file","arguments":{"path":"bug.py"}}]},{"content":good.to_string()}]),
        "fixer",
    );
    let mut limited = r.clone();
    limited.budget.model_requests = 1;
    let StageResult::Generate(result) = run_agent(&cfg, &limited, t.path(), Stage::Generate)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::RequestBudget);
    assert_eq!(result.ledger.requests, 1);
    assert!(result.submission.is_none());
    let cfg = scripted_config(
        t.path(),
        json!(
            (0..8)
                .map(|_| json!({"tool_calls":[{"name":"read_file","arguments":{"path":"bug.py"}}]}))
                .collect::<Vec<_>>()
        ),
        "fixer",
    );
    let mut limited = r.clone();
    limited.budget.tool_calls = 1;
    let StageResult::Generate(result) = run_agent(&cfg, &limited, t.path(), Stage::Generate)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::ToolBudget);
    assert_eq!(result.tool_calls, 1);
    let mut vr = r;
    vr.patch = Some(patch(change()));
    vr.tests = Some(tests::TestResult {
        outcome: tests::Outcome::NotConfigured,
        output: None,
        reason: None,
    });
    let cfg = scripted_config(
        t.path(),
        json!([
            {"tool_calls":[{"name":"write_file","arguments":{"path":"bug.py","content":"corrupt"}}]},
            {"expect_tool_result_contains":"read-only","tool_calls":[{"name":"submit_fix_verdicts","arguments":{"per_finding":[{"id":id,"verdict":"resolved","rationale":"re-derived"}],"new_problems":[]}}]}
        ]),
        "fix_verifier",
    );
    let StageResult::Verify(result) = run_agent(&cfg, &vr, t.path(), Stage::Verify).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::Terminal);
    assert!(result.submission.is_some());
    assert_eq!(
        std::fs::read_to_string(t.path().join("bug.py")).unwrap(),
        "fixed\n"
    );
}

#[test]
fn inheritance_trust_and_terminal_ids() {
    let mut cfg = Config::parse(r#"models: {investigator: {protocol: openai-chat, base_url: "https://api.example.com/v1", api_key_env: MODEL_KEY, model: primary}, validator: {protocol: openai-chat, base_url: "https://api.example.com/v1", api_key_env: MODEL_KEY, model: independent}}"#, "test").unwrap();
    assert_eq!(cfg.effective_fixer().model, "primary");
    assert_eq!(cfg.effective_fix_verifier().model, "independent");
    cfg.models.fixer = Some(cfg.effective_validator().clone());
    cfg.models.fix_verifier = Some(cfg.models.investigator.clone());
    assert_eq!(cfg.effective_fixer().model, "independent");
    assert_eq!(cfg.effective_fix_verifier().model, "primary");
    cfg.models.fix_verifier.as_mut().unwrap().api_key_env = Some("GITHUB_TOKEN".into());
    assert!(cfg.check_trust(true).is_err());
    assert!(check_route(cfg.effective_fix_verifier(), "OTHER_GH_TOKEN").is_err());
    let r = request();
    let id = r.findings[0].id();
    for v in [
        json!({"summary":"x","per_finding":[]}),
        json!({"summary":"x","per_finding":[{"id":"other","disposition":"fixed","note":"x"}]}),
        json!({"summary":"x","per_finding":[{"id":id,"disposition":"fixed","note":"x"},{"id":id,"disposition":"fixed","note":"x"}]}),
        json!({"summary":"x","per_finding":[{"id":id,"disposition":"resolved","note":"x"}]}),
    ] {
        assert!(check_terminal(&v, Stage::Generate, &r).is_err());
    }
    let mut r = r;
    r.findings[0].validation_status = Some(ValidationStatus::Uncertain);
    assert!(validate_request(&r, Stage::Generate).is_err());
}

struct SlowClient;

struct CarriedContextClient;
#[async_trait::async_trait]
impl ModelClient for CarriedContextClient {
    fn route_label(&self) -> String {
        "carried-context".into()
    }
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let user = messages
            .iter()
            .find_map(|m| {
                m.content
                    .as_deref()
                    .filter(|s| s.contains("UNTRUSTED INPUT DATA"))
            })
            .unwrap();
        assert!(user.contains("reported at an earlier commit; line numbers may have moved"));
        assert!(user.contains(&format!("\"carried_ids\":[\"{}\"]", finding().id())));
        for expected in [
            "Addition subtracts",
            "add(2,3)",
            "positive inputs",
            "wrong arithmetic result",
        ] {
            assert!(user.contains(expected), "{expected}: {user}");
        }
        let verify = tools.iter().any(|t| t.name == "submit_fix_verdicts");
        let (name, arguments) = if verify {
            (
                "submit_fix_verdicts",
                json!({"per_finding":[{"id":finding().id(),"verdict":"resolved","rationale":"Re-derived"}],"new_problems":[]}),
            )
        } else {
            (
                "submit_fix",
                json!({"summary":"Fixed","per_finding":[{"id":finding().id(),"disposition":"fixed","note":"Checked"}]}),
            )
        };
        Ok(Completion {
            message: serde_json::from_value(json!({"role":"assistant","tool_calls":[{"id":"done","name":name,"arguments":arguments}]})).unwrap(),
            usage: Default::default(), model: "mock".into(), latency_ms: 0,
        })
    }
}

#[tokio::test]
async fn carried_context_reaches_fixer_and_verifier_without_a_current_verdict() {
    let t = temp();
    let mut state = crate::state::ReviewState::default();
    state.upsert(&finding(), crate::state::FindingState::Open);
    state.mark_posted(&[finding().id()]);
    let carried = crate::report::carried_findings(&state, &[])
        .remove(0)
        .to_finding();
    assert!(carried.validation_status.is_none());
    assert!(!crate::pipeline::anchor::is_publishable(&carried));
    for stage in [Stage::Generate, Stage::Verify] {
        let mut r = request();
        r.findings = vec![carried.clone()];
        r.patch = Some(patch(change()));
        r.tests = Some(tests::TestResult {
            outcome: tests::Outcome::NotConfigured,
            output: None,
            reason: None,
        });
        validate_request(&r, stage).unwrap();
        let result = run_with_client(
            &r,
            t.path(),
            stage,
            Arc::new(CarriedContextClient),
            LedgerHandle::new(),
            Instant::now(),
        )
        .await
        .unwrap();
        match result {
            StageResult::Generate(r) => assert!(r.submission.is_some()),
            StageResult::Verify(r) => assert!(r.submission.is_some()),
        }
    }
}

#[async_trait::async_trait]
impl ModelClient for SlowClient {
    fn route_label(&self) -> String {
        "slow-mock".into()
    }
    async fn complete(
        &self,
        _: &[ChatMessage],
        _: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Err(ProviderError::Other("unexpected completion".into()))
    }
}

struct BudgetTerminalClient(std::sync::atomic::AtomicBool);
#[async_trait::async_trait]
impl ModelClient for BudgetTerminalClient {
    fn route_label(&self) -> String {
        "budget-terminal".into()
    }
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let value = if !self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
            // Cross the between-turn deadline without hanging an await.
            std::thread::sleep(Duration::from_millis(1100));
            json!({"tool_calls":[{"id":"read","name":"read_file","arguments":{"path":"bug.py"}}]})
        } else {
            assert!(messages.iter().any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("Budget exhausted"))
            }));
            tokio::time::sleep(Duration::from_millis(100)).await;
            json!({"content":json!({"summary":"checked","per_finding":[{"id":finding().id(),"disposition":"not_fixed","note":"budget"}]}).to_string()})
        };
        let mut value = value;
        value["role"] = json!("assistant");
        Ok(Completion {
            message: serde_json::from_value(value).unwrap(),
            usage: Default::default(),
            model: "mock".into(),
            latency_ms: 0,
        })
    }
}

#[tokio::test]
async fn time_budget_keeps_terminal_grace_and_tool_accounting() {
    let t = temp();
    std::fs::write(t.path().join("bug.py"), "original\n").unwrap();
    let mut r = request();
    r.budget.seconds = 1;
    let StageResult::Generate(result) = run_with_client(
        &r,
        t.path(),
        Stage::Generate,
        Arc::new(BudgetTerminalClient(std::sync::atomic::AtomicBool::new(
            false,
        ))),
        LedgerHandle::new(),
        Instant::now(),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::Terminal);
    assert!(result.submission.is_some());
    assert_eq!(result.tool_calls, 1);
}

#[cfg(unix)]
#[test]
fn sandbox_rejects_symlink_bind_aliases() {
    let t = temp();
    let real = t.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = t.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let status = sandbox::SandboxStatus {
        available: true,
        proc: false,
        reason: None,
    };
    for source in [
        link.clone(),
        PathBuf::from(format!("{}/", link.display())),
        link.join("."),
        link.join("child"),
    ] {
        std::fs::create_dir_all(real.join("child")).unwrap();
        let p = sandbox::Profile {
            binds_ro: vec![(source, "/work".into())],
            ..Default::default()
        };
        assert!(matches!(
            sandbox::command(&status, &p, &["true".into()]),
            Err(sandbox::Error::Profile(_))
        ));
    }
}

#[tokio::test]
async fn secret_config_removed_on_error_and_cancellation() {
    let t = temp();
    let path = t.path().join("config.json");
    let fail = || -> Result<(), std::io::Error> {
        let _guard = SecretConfig(path.clone());
        std::fs::write(&path, "secret headers")?;
        std::fs::read(t.path().join("missing"))?;
        Ok(())
    };
    assert!(fail().is_err());
    assert!(!path.exists());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let child_path = path.clone();
    let task = tokio::spawn(async move {
        let _guard = SecretConfig(child_path.clone());
        std::fs::write(child_path, "secret headers").unwrap();
        tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!path.exists());
    let io = t.path().join("io");
    let cfg = scripted_config(t.path(), json!([]), "fixer");
    assert!(matches!(
        run_in_sandbox(
            &t.path().join("missing"),
            &cfg,
            &request(),
            t.path(),
            &io,
            Stage::Generate,
            sandbox::Limits::default()
        )
        .await,
        Err(Error::Io(_))
    ));
    assert!(!io.exists());
}
#[tokio::test]
async fn hard_time_budget_stops_a_hung_provider() {
    let t = temp();
    let mut r = request();
    r.budget.seconds = 1;
    let start = Instant::now();
    let StageResult::Generate(result) = run_with_client(
        &r,
        t.path(),
        Stage::Generate,
        Arc::new(SlowClient),
        LedgerHandle::new(),
        start,
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::TimeBudget);
    assert!(result.submission.is_none());
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn configured_fixer_fallback_reuses_provider_chain_and_ledger() {
    let t = temp();
    let r = request();
    let id = r.findings[0].id();
    let mut cfg = scripted_config(
        t.path(),
        json!([{"tool_calls":[{"name":"submit_fix","arguments":{"summary":"fallback","per_finding":[{"id":id,"disposition":"not_fixed","note":"checked"}]}}]}]),
        "fixer",
    );
    let backup = cfg.models.investigator.clone();
    let mut primary = backup.clone();
    primary.protocol = Protocol::OpenaiChat;
    primary.base_url = Some("https://api.example.com/v1".into());
    primary.api_key_env = Some("REVERA_D1_UNSET_TEST_FALLBACK_KEY".into());
    primary.model = "missing-key-primary".into();
    primary.script = None;
    primary.fallback = vec![backup];
    cfg.models.fixer = Some(primary);
    let StageResult::Generate(result) = run_agent(&cfg, &r, t.path(), Stage::Generate)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(result.stopped, Stop::Terminal);
    assert_eq!(result.submission.unwrap().summary, "fallback");
    assert_eq!(result.ledger.requests, 1);
    assert_eq!(
        result.ledger.unavailable_routes[0].model,
        "missing-key-primary"
    );
    assert_eq!(result.ledger.fallback_events[0].to_model, "mock");
    assert_eq!(result.ledger.by_route[0].role, "fixer");
}

#[tokio::test]
async fn child_redacts_selected_route_header_credentials() {
    let t = temp();
    let r = request();
    let id = r.findings[0].id();
    let secret = "d1-model-header-private-value";
    let mut cfg = scripted_config(
        t.path(),
        json!([{"tool_calls":[{"name":"submit_fix","arguments":{"summary":secret,"per_finding":[{"id":id,"disposition":"not_fixed","note":"checked"}]}}]}]),
        "fixer",
    );
    cfg.models
        .investigator
        .extra_headers
        .insert("X-Model-Key".into(), secret.into());
    let result = run_agent(&cfg, &r, t.path(), Stage::Generate)
        .await
        .unwrap();
    assert!(!serde_json::to_string(&result).unwrap().contains(secret));
}

/// Opt-in real-provider proof. The fixture, controller diff, policy report,
/// generate/verify ledgers and test outcome are retained outside the checkout.
#[tokio::test]
async fn live_generate_diff_test_verify() {
    if std::env::var("REVERA_TEST_FIX_LIVE").as_deref() != Ok("1") {
        return;
    }
    let root = PathBuf::from(std::env::var("REVERA_FIX_LIVE_DIR").expect("live proof directory"));
    std::fs::create_dir(&root).unwrap();
    let cfg = Config::load(Path::new(
        &std::env::var("REVERA_FIX_LIVE_CONFIG").expect("live proof configuration"),
    ))
    .unwrap();
    let executable =
        PathBuf::from(std::env::var("REVERA_FIX_LIVE_EXE").expect("built revera executable"));
    let repo = root.join("repo");
    let sha = fixture(&repo);
    let ws = workspace::prepare(&repo.join(".git"), &sha, &root.join("workspace"))
        .await
        .unwrap();
    let mut r = request();
    r.head = sha;
    r.budget.seconds = 180;
    r.budget.tool_calls = 20;
    r.budget.model_requests = 25;
    r.constraints = "Only repair bug.py. Leave test_bug.py unchanged. The controller will run python3 test_bug.py after generation.".into();
    let before = tests::run_copy(
        &sandbox::Profile::default(),
        &["/usr/bin/python3".into(), "test_bug.py".into()],
        &ws.tree,
        &root.join("before-tests"),
    )
    .await
    .unwrap();
    assert_eq!(before.outcome, tests::Outcome::Failed);
    let generated = run_in_sandbox(
        &executable,
        &cfg,
        &r,
        &ws.tree,
        &root.join("generate"),
        Stage::Generate,
        sandbox::Limits::default(),
    )
    .await
    .unwrap();
    let p = ws.diff().await.unwrap();
    let policy = policy::check(&p, &policy::Policy::default());
    ws.apply_check(&p).await.unwrap();
    let tests = tests::run_copy(
        &sandbox::Profile::default(),
        &["/usr/bin/python3".into(), "test_bug.py".into()],
        &ws.tree,
        &root.join("after-tests"),
    )
    .await
    .unwrap();
    r.patch = Some(p.clone());
    r.tests = Some(tests.clone());
    let verified = run_in_sandbox(
        &executable,
        &cfg,
        &r,
        &ws.tree,
        &root.join("verify"),
        Stage::Verify,
        sandbox::Limits::default(),
    )
    .await
    .unwrap();
    let mut result = json!({"head":r.head,"sandbox":sandbox::probe().await,"before_tests":before,"generate":generated,"patch":p,"policy":policy,"tests":tests,"verify":verified});
    crate::redact::json(&mut result);
    crate::fsutil::write_atomic_in(
        &root,
        "result.json",
        &serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    let StageResult::Generate(g) = generated else {
        panic!()
    };
    assert_eq!(g.stopped, Stop::Terminal);
    assert!(g.submission.is_some());
    assert!(policy.allowed());
    assert_eq!(tests.outcome, tests::Outcome::Passed);
    assert_eq!(p.files.len(), 1);
    assert_eq!(p.files[0].path, "bug.py");
    let StageResult::Verify(v) = verified else {
        panic!()
    };
    assert_eq!(v.stopped, Stop::Terminal);
    let v = v.submission.unwrap();
    assert!(matches!(v.per_finding[0].verdict, Verdict::Resolved));
    assert!(v.new_problems.is_empty());
}

#[tokio::test]
async fn sandbox_isolation_limits_and_test_runner() {
    if std::env::var("REVERA_TEST_SANDBOX").as_deref() != Ok("1") {
        return;
    }
    let status = sandbox::probe().await;
    assert!(status.available, "{status:?}");
    let t = temp();
    let tree = t.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    let secret = t.path().join("secret.txt");
    std::fs::write(&secret, "host-only-secret").unwrap();
    let p = sandbox::Profile {
        binds_rw: vec![(tree.clone(), "/work".into())],
        ..Default::default()
    };
    // Exercise the controller's child-error boundary with real sandbox stderr.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = t.path().join("child");
        std::fs::write(&executable, "#!/bin/sh\necho 'https://provider.example/v1?opaque=private-query-value' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cfg = Config::parse("models: {investigator: {protocol: openai-chat, model: mock, api_key_env: REVERA_FIX_TEST_KEY, base_url: 'https://provider.example/v1'}}", "test").unwrap();
        let io = t.path().join("child-io");
        let error = run_in_sandbox(
            &executable,
            &cfg,
            &request(),
            &tree,
            &io,
            Stage::Generate,
            sandbox::Limits::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Child(_)), "{error}");
        assert!(!error.to_string().contains("private-query-value"));
        assert!(error.to_string().contains("query removed"));
        assert!(!io.join("config.json").exists());
    }
    let run = |s: String| {
        let p = p.clone();
        let status = status.clone();
        async move {
            sandbox::run(&status, &p, &["/bin/sh".into(), "-c".into(), s])
                .await
                .unwrap()
        }
    };
    let o = run(format!("test -z \"$HOME_SECRET\" && test -z \"$GITHUB_TOKEN\" && test ! -e {} && test ! -e /home && ! touch /escape && touch /work/ok", secret.display())).await;
    assert_eq!(o.code, Some(0), "{o:?}");
    assert!(tree.join("ok").exists());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "import socket\ns=socket.socket()\ns.settimeout(1)\ntry:\n s.connect(('127.0.0.1',{port}))\nexcept OSError:\n pass\nelse:\n raise SystemExit('network escaped')\n"
    );
    std::fs::write(tree.join("network.py"), script).unwrap();
    let o = sandbox::run(
        &status,
        &p,
        &["/usr/bin/python3".into(), "/work/network.py".into()],
    )
    .await
    .unwrap();
    assert_eq!(o.code, Some(0), "{o:?}");
    let mut c = sandbox::command(&status, &p, &["/usr/bin/env".into()]).unwrap();
    c.env("REVERA_SECRET_PROBE", "must-not-be-inherited")
        .env("GITHUB_TOKEN", "must-not-be-inherited");
    let o = c.output().await.unwrap();
    assert!(o.status.success());
    let env = String::from_utf8(o.stdout).unwrap();
    let actual: std::collections::BTreeMap<_, _> =
        env.lines().map(|l| l.split_once('=').unwrap()).collect();
    let expected: std::collections::BTreeMap<_, _> = sandbox::FIXED_ENV.iter().copied().collect();
    assert_eq!(actual, expected, "unexpected child environment");
    let mut small = p.clone();
    small.limits.wall = Duration::from_millis(150);
    let o = sandbox::run(
        &status,
        &small,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "sleep .4; touch /work/escaped".into(),
        ],
    )
    .await
    .unwrap();
    assert!(o.timed_out);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !tree.join("escaped").exists(),
        "timeout left a live descendant"
    );
    small.limits.wall = Duration::from_secs(5);
    small.limits.fsize = 1024;
    let o = sandbox::run(
        &status,
        &small,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "exec dd if=/dev/urandom of=/work/large bs=4096 count=8".into(),
        ],
    )
    .await
    .unwrap();
    assert_ne!(o.code, Some(0));
    assert!(std::fs::metadata(tree.join("large")).unwrap().len() <= 1024);
    small.limits.cpu_s = 1;
    let o = sandbox::run(
        &status,
        &small,
        &["/bin/sh".into(), "-c".into(), "while :; do :; done".into()],
    )
    .await
    .unwrap();
    assert_ne!(o.code, Some(0));
    assert!(!o.timed_out, "CPU limit not enforced");
    let mut memory = p.clone();
    memory.limits.mem_bytes = 64 * 1024 * 1024;
    let o = sandbox::run(&status, &memory, &["/usr/bin/python3".into(), "-c".into(), "try:\n bytearray(128*1024*1024)\nexcept MemoryError:\n raise SystemExit(23)\nraise SystemExit(24)".into()]).await.unwrap();
    assert_eq!(o.code, Some(23), "address-space limit not enforced: {o:?}");
    let mut processes = p.clone();
    processes.limits.nproc = 1;
    let o = sandbox::run(&status, &processes, &["/usr/bin/python3".into(), "-c".into(), "import os,errno\ntry:\n os.fork()\nexcept OSError as e:\n raise SystemExit(0 if e.errno==errno.EAGAIN else 25)\nraise SystemExit(26)".into()]).await.unwrap();
    assert_eq!(o.code, Some(0), "process limit not enforced: {o:?}");
    let mut cancelled = sandbox::command(
        &status,
        &p,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "sleep .4; touch /work/cancel-escaped".into(),
        ],
    )
    .unwrap()
    .spawn()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancelled.kill().await.unwrap();
    drop(cancelled);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !tree.join("cancel-escaped").exists(),
        "cancellation left a live descendant"
    );
    let disabled = sandbox::SandboxStatus {
        available: false,
        proc: false,
        reason: Some("no isolation".into()),
    };
    assert!(matches!(
        sandbox::command(&disabled, &p, &["true".into()]),
        Err(sandbox::Error::Unavailable(_))
    ));
    let profile = sandbox::Profile {
        net: true,
        env: vec![("SHOULD_NOT_PASS".into(), "secret".into())],
        ..p.clone()
    };
    let result = tests::run(
        &profile,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "test -z \"$SHOULD_NOT_PASS\" && echo passed".into(),
        ],
        &tree,
    )
    .await;
    assert_eq!(result.outcome, tests::Outcome::Passed);
    assert_eq!(
        tests::run(&p, &[], &tree).await.outcome,
        tests::Outcome::NotConfigured
    );
    assert_eq!(
        tests::run(&p, &["/usr/bin/false".into()], &tree)
            .await
            .outcome,
        tests::Outcome::Failed
    );
    let result = tests::run(
        &p,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "yes line | head -c 200000".into(),
        ],
        &tree,
    )
    .await;
    let output = result.output.unwrap();
    assert!(output.truncated);
    assert!(output.stdout.len() + output.stderr.len() <= 64 * 1024);
    // Explicitly prove the same boundary without /proc, even on hosts where it works.
    let no_proc = sandbox::SandboxStatus {
        proc: false,
        ..status
    };
    assert_eq!(
        sandbox::run(&no_proc, &p, &["/usr/bin/true".into()])
            .await
            .unwrap()
            .code,
        Some(0)
    );
}
