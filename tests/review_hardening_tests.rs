use revera::diff::{DiffLine, DiffLineKind, DiffSet, FileDiff, FileStatus, Hunk};
use revera::findings::{Finding, Severity, ValidationStatus};
use revera::report::{fails_severity_gate, redact_secrets, PublicationPlan, RunReport, RunStatus};
use revera::rules::scan_diff;
use revera::state::{FindingState, ReviewState};
use revera::tools::ToolBox;
use std::process::Command;

fn added_line(file: &str, text: &str) -> DiffSet {
    DiffSet {
        files: vec![FileDiff {
            old_path: file.into(),
            new_path: file.into(),
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                old_start: 1,
                old_len: 0,
                new_start: 1,
                new_len: 1,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Add,
                    old_no: None,
                    new_no: Some(1),
                    text: text.into(),
                }],
            }],
        }],
    }
}

#[test]
fn static_secret_findings_never_carry_the_secret() {
    let diff = added_line("src/config.rs", "let k = \"AKIAIOSFODNN7EXAMPLE\";");
    let findings = scan_diff(&diff, None);
    assert_eq!(findings.len(), 1);
    let json = serde_json::to_string(&findings[0]).unwrap();
    assert!(!json.contains("AKIAIOSFODNN7EXAMPLE"), "{json}");
}

#[test]
fn long_multibyte_matching_line_does_not_panic() {
    // a multi-byte character straddles byte 80 of the matched line
    let line = format!("{}é let k = \"AKIAIOSFODNN7EXAMPLE\";", "x".repeat(79));
    let findings = scan_diff(&added_line("src/a.rs", &line), None);
    assert_eq!(findings.len(), 1);
}

#[test]
fn redaction_covers_whole_pem_blocks_and_fine_grained_tokens() {
    let pem = "key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\nabc\n-----END RSA PRIVATE KEY-----\ndone";
    let out = redact_secrets(pem);
    assert!(!out.contains("MIIEpAIBAAKCAQEA"), "{out}");
    assert!(out.ends_with("done"), "{out}");

    let pat = format!("token github_pat_{}", "A1b2".repeat(20));
    assert!(!redact_secrets(&pat).contains("github_pat_"));
}

#[cfg(unix)]
#[test]
fn symlinks_to_credential_files_are_refused() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
    std::os::unix::fs::symlink(root.join(".env"), root.join("innocent.txt")).unwrap();
    std::fs::create_dir_all(root.join(".ssh")).unwrap();
    std::fs::write(root.join(".ssh/config"), "Host x\n").unwrap();

    let e = ToolBox::is_safe_repo_path(root, "innocent.txt").unwrap_err();
    assert!(e.contains("sensitive"), "{e}");
    assert!(ToolBox::is_safe_repo_path(root, ".ssh/config").is_err());
}

fn finding(sev: Severity, status: Option<ValidationStatus>) -> Finding {
    serde_json::from_value(serde_json::json!({
        "defect_key": "k",
        "severity": sev.to_string(),
        "file": "src/a.rs",
        "start_line": 1,
        "title": "t",
        "claim": "c",
        "trigger": "x",
        "impact": "i",
    }))
    .map(|mut f: Finding| {
        f.validation_status = status;
        f
    })
    .unwrap()
}

fn report(findings: Vec<Finding>) -> RunReport {
    RunReport {
        status: RunStatus::Complete,
        reason: None,
        base: "b".into(),
        head: "h".into(),
        strategy: "baseline".into(),
        findings,
        plan: PublicationPlan {
            inline: vec![],
            summary_markdown: String::new(),
            state: Default::default(),
        },
        ledger: Default::default(),
        publication: Default::default(),
        coverage_gaps: vec![],
        timing: Default::default(),
        stats: Default::default(),
    }
}

#[test]
fn severity_gate_counts_only_findings_the_review_stands_behind() {
    let empty = ReviewState::default();
    let rejected = report(vec![finding(
        Severity::High,
        Some(ValidationStatus::Rejected),
    )]);
    assert!(!fails_severity_gate(&rejected, &empty, Severity::High));
    let uncertain = report(vec![finding(
        Severity::High,
        Some(ValidationStatus::Uncertain),
    )]);
    assert!(!fails_severity_gate(&uncertain, &empty, Severity::High));

    let accepted = report(vec![finding(
        Severity::Medium,
        Some(ValidationStatus::Accepted),
    )]);
    assert!(fails_severity_gate(&accepted, &empty, Severity::Medium));
    assert!(!fails_severity_gate(&accepted, &empty, Severity::High));
}

#[test]
fn severity_gate_still_fails_a_reused_review_with_open_findings() {
    let mut state = ReviewState::default();
    state.upsert(
        &finding(Severity::High, Some(ValidationStatus::Accepted)),
        FindingState::Open,
    );
    // a reused review reports no findings of its own
    assert!(fails_severity_gate(&report(vec![]), &state, Severity::High));
}

#[test]
fn uncommitted_is_rejected_with_an_event() {
    let out = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args(["review", "--uncommitted", "--event", "/nonexistent.json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot be used with"), "{err}");
}

fn cfg(extra: &str) -> revera::config::Config {
    serde_yaml::from_str(&format!(
        "models:\n  investigator: {{protocol: scripted, script: /tmp/s.json, model: inv}}\n{extra}"
    ))
    .unwrap()
}

#[test]
fn review_identity_tracks_guidance_content() {
    let a =
        cfg("review:\n  path_instructions: [{path: \"src/**\", instructions: \"check auth\"}]\n");
    let b =
        cfg("review:\n  path_instructions: [{path: \"src/**\", instructions: \"check perf\"}]\n");
    assert_ne!(
        a.review_fingerprint("baseline"),
        b.review_fingerprint("baseline")
    );
    let c = cfg("review: {knowledge_base: [docs/a.md]}\n");
    let d = cfg("review: {knowledge_base: [docs/b.md]}\n");
    assert_ne!(
        c.review_fingerprint("baseline"),
        d.review_fingerprint("baseline")
    );
}

#[test]
fn unfocused_personas_do_not_bind_to_an_unfocused_scout() {
    let c = cfg(
        "  scouts:\n    - {name: s1, protocol: scripted, script: /tmp/s.json, model: scout1}\n    - {name: s2, protocol: scripted, script: /tmp/s.json, model: scout2}\npanel:\n  personas: [{name: p1}, {name: p2}]\n",
    );
    let lanes = c
        .panel
        .effective_lanes(&c.models.investigator, c.models.scouts.as_deref())
        .unwrap();
    let models: Vec<&str> = lanes.iter().map(|l| l.route.model.as_str()).collect();
    assert_eq!(models, vec!["inv", "inv"]);
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[tokio::test]
async fn recheck_of_a_finding_outside_the_new_diff_goes_to_the_validator() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(repo.join("src/other.rs"), "pub fn g() { unsafe_call() }\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".revera/\nscript.json\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    git(repo, &["commit", "-q", "-am", "change"]);

    // a finding published by an earlier run on a file this push does not touch
    let mut prior = finding(Severity::High, Some(ValidationStatus::Accepted));
    prior.file = "src/other.rs".into();
    let id = prior.id();
    let mut state = ReviewState::default();
    state.upsert(&prior, FindingState::Open);
    state.mark_posted(std::slice::from_ref(&id));
    state.save(repo).unwrap();

    let script = repo.join("script.json");
    std::fs::write(
        &script,
        r#"{"roles": {"validator": [[{"tool_calls": [{"name": "submit_verdict", "arguments": {"validation_status": "accepted", "rationale": "still present"}}]}]]}}"#,
    )
    .unwrap();
    let cfg = cfg(&format!(
        "  validator: {{protocol: scripted, script: {}, model: val}}\n",
        script.display()
    ));
    let cfg = revera::config::Config {
        models: revera::config::ModelsConfig {
            investigator: revera::config::ModelRoute {
                script: Some(script.clone()),
                ..cfg.models.investigator.clone()
            },
            ..cfg.models
        },
        ..cfg
    };
    let req = revera::pipeline::common::ReviewRequest {
        repo: repo.to_path_buf(),
        base,
        head: None,
        title: None,
        body: String::new(),
        strategy_override: None,
        force: true,
        uncommitted: false,
        progress: None,
    };
    let (rep, state) = revera::pipeline::run(&cfg, &req).await.unwrap();
    let kept = state.findings.iter().find(|f| f.id == id).unwrap();
    assert_eq!(kept.status, FindingState::Open);
    assert!(
        !rep.plan
            .summary_markdown
            .contains("Resolved since last review"),
        "{}",
        rep.plan.summary_markdown
    );
}
