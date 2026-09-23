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
fn preview_reports_triage_and_staffing_without_credentials_or_calls() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(repo.join("Cargo.lock"), "v = 1\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "revera.yaml\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    std::fs::write(repo.join("Cargo.lock"), "v = 2\n").unwrap();
    std::fs::write(repo.join(".npmrc"), "//r/:_authToken=x\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "change"]);
    // an HTTP route whose key is unset and whose endpoint is unreachable:
    // any credential check or provider call would fail the command
    std::fs::write(
        repo.join("revera.yaml"),
        "review: {strategy: panel}\npanel: {focuses: [security, performance, docs]}\ntriage: {risk_tiers: true}\nmodels:\n  investigator: {protocol: openai-chat, base_url: \"http://127.0.0.1:1\", api_key_env: REVERA_PREVIEW_UNSET_KEY, model: inv-model}\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args([
            "review",
            "--preview",
            "--base",
            &base,
            "--config",
            "revera.yaml",
        ])
        .env_remove("REVERA_PREVIEW_UNSET_KEY")
        .current_dir(repo)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    for want in [
        "reviewed files (1):",
        "  M src/lib.rs (+1 -1)",
        "  Cargo.lock: lockfile",
        "  .npmrc: credential file; content withheld from models",
        "strategy: panel (config)",
        "risk tier: trivial (2 changed lines, 1 files)",
        "reviewers: single investigator (trivial tier)",
        "validation: fresh-context validator per candidate",
    ] {
        assert!(stdout.contains(want), "missing {want:?} in:\n{stdout}");
    }
    assert!(!repo.join(".revera").exists(), "preview wrote state");

    // --strategy opts out of tiers and shows the configured lanes
    let out = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args([
            "review",
            "--preview",
            "--base",
            &base,
            "--config",
            "revera.yaml",
            "--strategy",
            "panel",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("risk tier: not applied"), "{stdout}");
    assert!(
        stdout
            .contains("reviewers: security (inv-model), performance (inv-model), docs (inv-model)"),
        "{stdout}"
    );

    // preview is local only
    let out = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args(["review", "--preview", "--event", "/nonexistent.json"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(!out.status.success());
}
