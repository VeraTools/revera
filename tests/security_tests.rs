use revera::diff::{DiffLine, DiffLineKind, DiffSet, FileDiff, FileStatus, Hunk};
use revera::findings::{Evidence, Finding, Severity};
use revera::pipeline::validate::has_verifiable_evidence;
use revera::tools::ToolBox;

fn make_test_diff(file: &str) -> DiffSet {
    DiffSet {
        files: vec![FileDiff {
            old_path: file.into(),
            new_path: file.into(),
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                old_start: 1,
                old_len: 5,
                new_start: 1,
                new_len: 5,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Add,
                    old_no: None,
                    new_no: Some(3),
                    text: "+ valid line".into(),
                }],
            }],
        }],
    }
}

fn finding(file: &str, line: u32, evidence: Vec<Evidence>) -> Finding {
    Finding {
        defect_key: "k".into(),
        severity: Severity::High,
        file: file.into(),
        start_line: line,
        end_line: None,
        title: "t".into(),
        claim: "c".into(),
        trigger: "tr".into(),
        impact: "i".into(),
        introduced_by_change: true,
        supporting_evidence: evidence,
        counterevidence_checked: vec![],
        validation_status: None,
        suggested_fix: None,
        source: "scout:test".into(),
        rationale: None,
        sources: vec!["scout:test".into()],
        assurance: None,
        quoted_code: None,
        suggested_replacement: None,
        quote_anchored: false,
    }
}

#[test]
fn secret_redaction_replaces_credentials_in_finding_body() {
    let mut f = finding("src/config.rs", 10, vec![]);
    f.title = "Found AWS key AKIAIOSFODNN7EXAMPLE in config".into();
    f.claim = "Leaked OpenAI token sk-proj-12345678901234567890 in comments".into();
    f.trigger = "User passes ghp_123456789012345678901234567890123456".into();
    f.suggested_fix = Some("Use env var instead of AKIAIOSFODNN7EXAMPLE".into());

    let body = revera::report::finding_body(&f);
    assert!(!body.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(!body.contains("sk-proj-12345678901234567890"));
    assert!(!body.contains("ghp_123456789012345678901234567890123456"));
    assert!(body.contains("[REDACTED_CREDENTIAL]"));
}

#[test]
fn eval_gate_rejects_empty_or_non_existent_file() {
    let diff = make_test_diff("src/valid.rs");
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    // Empty file path -> rejected
    let f1 = finding("", 3, vec![]);
    assert!(!has_verifiable_evidence(&f1, &diff, root));

    // Zero start line -> rejected
    let f2 = finding("src/valid.rs", 0, vec![]);
    assert!(!has_verifiable_evidence(&f2, &diff, root));

    // Non-existent file not in diff -> rejected
    let f3 = finding("src/phantom.rs", 3, vec![]);
    assert!(!has_verifiable_evidence(&f3, &diff, root));

    // Path traversal in supporting evidence -> rejected
    let f4 = finding(
        "src/valid.rs",
        3,
        vec![Evidence {
            path: "../../etc/passwd".into(),
            start_line: 1,
            end_line: 1,
            note: "".into(),
        }],
    );
    assert!(!has_verifiable_evidence(&f4, &diff, root));

    // Absolute path in supporting evidence -> rejected
    let f5 = finding(
        "src/valid.rs",
        3,
        vec![Evidence {
            path: "/var/log/secret".into(),
            start_line: 1,
            end_line: 1,
            note: "".into(),
        }],
    );
    assert!(!has_verifiable_evidence(&f5, &diff, root));

    // Valid evidence on actual diff file -> accepted by EVAL gate
    let f_valid = finding("src/valid.rs", 3, vec![]);
    assert!(has_verifiable_evidence(&f_valid, &diff, root));
}

#[test]
fn eval_gate_keeps_cross_file_findings_outside_the_diff() {
    let diff = make_test_diff("src/valid.rs");
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/caller.rs"), "fn caller() {}\n").unwrap();
    std::fs::write(repo.path().join(".env"), "KEY=1\n").unwrap();

    // an existing file untouched by the diff is a valid (summary) location
    let cross = finding("src/caller.rs", 1, vec![]);
    assert!(has_verifiable_evidence(&cross, &diff, repo.path()));
    // a sensitive file is never a valid location even though it exists
    let secret = finding(".env", 1, vec![]);
    assert!(!has_verifiable_evidence(&secret, &diff, repo.path()));
}

#[test]
fn tool_path_containment_blocks_traversal_and_sensitive_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn ok() {}").unwrap();

    // Valid file path inside repo
    assert!(ToolBox::is_safe_repo_path(root, "src/lib.rs").is_ok());

    // Path traversal with ..
    assert!(ToolBox::is_safe_repo_path(root, "../outside.rs").is_err());
    assert!(ToolBox::is_safe_repo_path(root, "src/../../outside.rs").is_err());

    // Absolute path
    assert!(ToolBox::is_safe_repo_path(root, "/etc/passwd").is_err());

    // Sensitive files
    assert!(ToolBox::is_safe_repo_path(root, ".env").is_err());
    assert!(ToolBox::is_safe_repo_path(root, "config/.env.local").is_err());
    assert!(ToolBox::is_safe_repo_path(root, ".ssh/id_rsa").is_err());
    assert!(ToolBox::is_safe_repo_path(root, "keys/id_ed25519").is_err());
    assert!(ToolBox::is_safe_repo_path(root, ".aws/credentials").is_err());
}
