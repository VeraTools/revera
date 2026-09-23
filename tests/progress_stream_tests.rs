use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
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

#[test]
fn progress_json_streams_every_pipeline_event_as_ndjson() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(
        repo.join(".gitignore"),
        ".revera/\nrevera.yaml\nscript.json\nprogress.ndjson\n",
    )
    .unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    git(repo, &["commit", "-q", "-am", "change"]);
    std::fs::write(
        repo.join("script.json"),
        r#"{"roles": {"investigator": [[{"tool_calls": [{"name": "submit_findings", "arguments": {"findings": [], "coverage": "src/lib.rs"}}]}]]}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("revera.yaml"),
        "review: {validate: false}\nmodels:\n  investigator: {protocol: scripted, script: script.json, model: m}\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args([
            "review",
            "--base",
            &base,
            "--config",
            "revera.yaml",
            "--progress-json",
            "progress.ndjson",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(repo.join("progress.ndjson")).unwrap();
    let events: Vec<String> = text
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect(l);
            v["event"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        events.first().map(String::as_str),
        Some("diff_parsed"),
        "{events:?}"
    );
    assert_eq!(
        events.last().map(String::as_str),
        Some("review_complete"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e == "static_rules_checked"),
        "{events:?}"
    );

    // an unwritable destination fails before any review work
    let out = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args([
            "review",
            "--base",
            &base,
            "--config",
            "revera.yaml",
            "--progress-json",
            "/nonexistent-dir/p.ndjson",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot write progress"));
}
