use std::io::Write;
use std::process::Command;

fn write_tmp(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[test]
fn fork_guard_fires_before_credential_check() {
    // investigator key unset: a fork event must still get the partial
    // "review skipped" report (exit 2), not a credential failure (exit 1)
    let yaml = "models:\n  investigator: {protocol: openai-chat, base_url: http://x, model: m, api_key_env: REVERA_T_UNSET_FORK_INV}\n";
    let cfg = write_tmp(yaml);
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pr_event.json")).unwrap();
    v["pull_request"]["head"]["repo"]["full_name"] = serde_json::json!("contributor/widgets");
    let ev = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ev.path(), v.to_string()).unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = repo.path().join("report.json");

    let res = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args([
            "review",
            "--repo",
            repo.path().to_str().unwrap(),
            "--event",
            ev.path().to_str().unwrap(),
            "--config",
            cfg.path().to_str().unwrap(),
            "--publish",
            "comment",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        res.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&res.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["status"], "partial");
    assert!(
        report["reason"]
            .as_str()
            .unwrap()
            .contains("fork PR: review skipped"),
        "{report}"
    );
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn two_commit_repo() -> tempfile::TempDir {
    let r = tempfile::tempdir().unwrap();
    std::fs::create_dir(r.path().join("src")).unwrap();
    std::fs::write(
        r.path().join("src/lib.rs"),
        "pub fn div(a: i32, b: i32) -> i32 {\n    a / b\n}\n",
    )
    .unwrap();
    git(r.path(), &["init", "-q"]);
    git(r.path(), &["add", "-A"]);
    git(r.path(), &["commit", "-qm", "base"]);
    std::fs::write(
        r.path().join("src/lib.rs"),
        "pub fn div(a: i32, b: i32) -> i32 {\n    a / (b - 1)\n}\n",
    )
    .unwrap();
    git(r.path(), &["commit", "-qam", "head"]);
    r
}

fn review_run(
    repo: &std::path::Path,
    cfg: &std::path::Path,
    out: &std::path::Path,
) -> serde_json::Value {
    review_run_with_force(repo, cfg, out, true)
}

fn review_run_with_force(
    repo: &std::path::Path,
    cfg: &std::path::Path,
    out: &std::path::Path,
    force: bool,
) -> serde_json::Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_revera"));
    command
        .args(["review", "--repo"])
        .arg(repo)
        .args(["--base", "HEAD~1", "--head", "HEAD"]);
    if force {
        command.arg("--force");
    }
    let res = command
        .args(["--config"])
        .arg(cfg)
        .arg("--out")
        .arg(out)
        .output()
        .unwrap();
    let text = std::fs::read_to_string(out)
        .unwrap_or_else(|_| panic!("no report: {}", String::from_utf8_lossy(&res.stderr)));
    serde_json::from_str(&text).unwrap()
}

fn validator_requests(rep: &serde_json::Value) -> u64 {
    rep["ledger"]["by_route"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["role"] == "validator")
        .map(|r| r["requests"].as_u64().unwrap_or(0))
        .sum()
}

#[test]
fn validation_disabled_run_does_not_recheck_prior_findings() {
    let repo = two_commit_repo();
    let w = tempfile::tempdir().unwrap();
    let finding = r#"{"defect_key": "div-zero", "severity": "high", "file": "src/lib.rs", "start_line": 2, "title": "division by zero when b == 1", "claim": "b - 1 is zero for b == 1", "suggested_fix": "candidate remedy"}"#;
    let inv = format!(
        r#"{{"roles": {{"investigator": [[{{"tool_calls": [{{"name": "submit_findings", "arguments": {{"findings": [{finding}], "coverage": "x"}}}}]}}]]}}}}"#
    );
    std::fs::write(w.path().join("inv.json"), &inv).unwrap();
    let accept = r#"[{"tool_calls": [{"name": "submit_verdict", "arguments": {"validation_status": "accepted", "rationale": "r", "counterevidence_checked": ["callers"]}}]}]"#;
    std::fs::write(
        w.path().join("val.json"),
        format!(r#"{{"roles": {{"validator": [{accept}, {accept}, {accept}]}}}}"#),
    )
    .unwrap();
    let yaml = |validate: bool| {
        format!(
            "review: {{min_severity: low, validate: {validate}}}\nvera: {{enabled: false}}\nmodels:\n  investigator: {{protocol: scripted, script: \"{}\", model: i}}\n  validator: {{protocol: scripted, script: \"{}\", model: v}}\n",
            w.path().join("inv.json").display(),
            w.path().join("val.json").display()
        )
    };
    let cfg = w.path().join("cfg.yaml");

    std::fs::write(&cfg, yaml(true)).unwrap();
    let first = review_run(repo.path(), &cfg, &w.path().join("r1.json"));
    assert_eq!(first["stats"]["accepted"], 1, "{first}");
    assert!(repo.path().join(".revera/state.json").exists());

    std::fs::write(&cfg, yaml(false)).unwrap();
    let second = review_run(repo.path(), &cfg, &w.path().join("r2.json"));
    assert_eq!(second["stats"]["validation"], "disabled", "{second}");
    assert_eq!(validator_requests(&second), 0, "{second}");
    assert_eq!(second["stats"]["accepted"], 0, "{second}");
    for f in second["findings"].as_array().unwrap() {
        assert!(f["validation_status"].is_null(), "{f}");
        assert_eq!(f["suggested_fix"], "candidate remedy", "{f}");
    }
    assert!(second["plan"]["inline"].as_array().unwrap().is_empty());
}

#[test]
fn validator_fix_replaces_unsafe_investigator_fix_and_reuse_drops_it() {
    let repo = two_commit_repo();
    let w = tempfile::tempdir().unwrap();
    let inv = serde_json::json!({
        "roles": {
            "investigator": [[{
                "tool_calls": [{
                    "name": "submit_findings",
                    "arguments": {
                        "findings": [{
                            "defect_key": "div-zero",
                            "severity": "high",
                            "file": "src/lib.rs",
                            "start_line": 2,
                            "title": "division can fail",
                            "claim": "b - 1 can be zero",
                            "introduced_by_change": true,
                            "suggested_fix": "return 0; // unsafe suggestion"
                        }],
                        "coverage": "checked"
                    }
                }]
            }]]
        }
    });
    let val = serde_json::json!({
        "roles": {
            "validator": [[{
                "tool_calls": [{
                    "name": "submit_verdict",
                    "arguments": {
                        "validation_status": "accepted",
                        "rationale": "confirmed",
                        "fix": "return a / b; // verified replacement"
                    }
                }]
            }]]
        }
    });
    std::fs::write(w.path().join("inv.json"), inv.to_string()).unwrap();
    std::fs::write(w.path().join("val.json"), val.to_string()).unwrap();
    let cfg = w.path().join("cfg.yaml");
    std::fs::write(
        &cfg,
        format!(
            "review: {{min_severity: low, validate: true}}\nmodels:\n  investigator: {{protocol: scripted, script: \"{}\", model: i}}\n  validator: {{protocol: scripted, script: \"{}\", model: v}}\n",
            w.path().join("inv.json").display(),
            w.path().join("val.json").display()
        ),
    )
    .unwrap();

    let first = review_run(repo.path(), &cfg, &w.path().join("r1.json"));
    let body = first["plan"]["inline"][0]["body"].as_str().unwrap();
    assert!(
        body.contains("return a / b; // verified replacement"),
        "{body}"
    );
    assert!(!body.contains("return 0; // unsafe suggestion"), "{body}");
    assert!(
        first["findings"][0].get("suggested_fix").is_none(),
        "{}",
        first["findings"][0]
    );
    assert_eq!(
        first["findings"][0]["validated_fix"],
        "return a / b; // verified replacement"
    );

    // A completed identical review is short-circuited. Its state projection
    // is rebuilt from StateFinding::to_finding, which intentionally has no
    // remedy text.
    let reused = review_run_with_force(repo.path(), &cfg, &w.path().join("r2.json"), false);
    assert_eq!(reused["stats"]["reused"], true, "{reused}");
    assert_eq!(reused["stats"]["validation"], "reused", "{reused}");
    assert!(reused["findings"].as_array().unwrap().is_empty());
    assert!(
        reused["plan"]["state"]["findings"][0]["validated_fix"].is_null(),
        "{reused}"
    );
}

#[test]
fn fork_without_base_config_reports_skip() {
    // base commit has no revera.yaml; the fork's head adds one
    let repo = two_commit_repo();
    let base = String::from_utf8(
        Command::new("git")
            .current_dir(repo.path())
            .args(["rev-parse", "HEAD~1"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::fs::write(
        repo.path().join("revera.yaml"),
        "models:\n  investigator: {protocol: openai-chat, base_url: http://x, model: m, api_key_env: REVERA_T_UNSET_FORK_INV}\n",
    )
    .unwrap();
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pr_event.json")).unwrap();
    v["pull_request"]["head"]["repo"]["full_name"] = serde_json::json!("contributor/widgets");
    v["pull_request"]["base"]["sha"] = serde_json::json!(base.trim());
    let ev = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ev.path(), v.to_string()).unwrap();
    let out = repo.path().join("report.json");
    let res = Command::new(env!("CARGO_BIN_EXE_revera"))
        .current_dir(repo.path())
        .args(["review", "--repo", ".", "--event"])
        .arg(ev.path())
        .args(["--config", "revera.yaml", "--publish", "comment", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(
        res.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&res.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["status"], "partial");
    assert!(
        report["reason"]
            .as_str()
            .unwrap()
            .contains("fork PR: review skipped"),
        "{report}"
    );
}

#[test]
fn cache_key_reads_event_base_config_not_head() {
    let repo = two_commit_repo();
    std::fs::write(repo.path().join("revera.yaml"), "models:\n  investigator: {protocol: scripted, script: /dev/null, model: m}\nvera: {enabled: true}\n").unwrap();
    git(repo.path(), &["add", "revera.yaml"]);
    git(repo.path(), &["commit", "-qm", "config"]);
    let base = String::from_utf8(
        Command::new("git")
            .current_dir(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    // PR head disables Vera and references a variable
    std::fs::write(
        repo.path().join("revera.yaml"),
        "models:\n  investigator: {protocol: scripted, script: /dev/null, model: m}\nvera: {enabled: false, version: \"${REVERA_T_CACHE_SECRET}\"}\n",
    )
    .unwrap();
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pr_event.json")).unwrap();
    v["pull_request"]["base"]["sha"] = serde_json::json!(base.trim());
    let ev = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ev.path(), v.to_string()).unwrap();
    let key = |event: bool| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_revera"));
        c.current_dir(repo.path())
            .env("REVERA_T_CACHE_SECRET", "s3cr3t")
            .args(["cache-key", "--config", "revera.yaml"]);
        if event {
            c.arg("--event").arg(ev.path());
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    };
    assert_eq!(key(false), "disabled");
    let k = key(true);
    assert_ne!(k, "disabled");
    assert_eq!(k.len(), 24, "{k}");
}
