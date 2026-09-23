use revera::config::Config;
use revera::pipeline::common::ReviewRequest;
use revera::report::RunStatus;
use serde_json::{json, Value};
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

fn finding(key: &str, line: u32) -> Value {
    json!({"defect_key": key, "severity": "high", "file": "src/lib.rs", "start_line": line,
           "title": format!("defect {key}"), "claim": "c", "trigger": "t"})
}

/// One scripted investigator session submitting `findings`; `None` is a
/// session that ends without a valid submission.
fn session(findings: Option<Vec<Value>>) -> Value {
    match findings {
        Some(f) => json!([{"tool_calls": [{"name": "submit_findings",
            "arguments": {"findings": f, "coverage": "src/lib.rs"}}]}]),
        None => json!([{"content": "no submission"}]),
    }
}

/// Run a baseline review with `rounds` recall rounds over scripted sessions;
/// returns (status, finding keys, investigator sessions consumed).
async fn run(rounds: u32, sessions: Vec<Value>) -> (RunStatus, Vec<String>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".revera/\nscript.json\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    let body: String = (1..=5)
        .map(|i| format!("pub const C{i}: u8 = {i};\n"))
        .collect();
    std::fs::write(repo.join("src/lib.rs"), format!("pub fn f() {{}}\n{body}")).unwrap();
    git(repo, &["commit", "-q", "-am", "change"]);
    let script = repo.join("script.json");
    std::fs::write(
        &script,
        json!({"roles": {"investigator": sessions}}).to_string(),
    )
    .unwrap();
    let cfg: Config = serde_yaml::from_str(&format!(
        "review: {{validate: false, recall_rounds: {rounds}}}\nmodels:\n  investigator: {{protocol: scripted, script: {}, model: m}}\n",
        script.display()
    ))
    .unwrap();
    let req = ReviewRequest {
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
    let (rep, _) = revera::pipeline::run(&cfg, &req).await.unwrap();
    let mut keys: Vec<String> = rep.findings.iter().map(|f| f.defect_key.clone()).collect();
    keys.sort();
    let sessions = rep
        .timing
        .phases
        .iter()
        .filter(|p| p.label.starts_with("investigator"))
        .count();
    (rep.status, keys, sessions)
}

#[tokio::test]
async fn later_rounds_add_new_findings_until_one_adds_nothing() {
    let (status, keys, sessions) = run(
        3,
        vec![
            session(Some(vec![finding("a", 2)])),
            session(Some(vec![finding("b", 3)])),
            session(Some(vec![])),
        ],
    )
    .await;
    assert_eq!(status, RunStatus::Complete);
    assert_eq!(keys, vec!["a", "b"]);
    assert_eq!(sessions, 3);
}

#[tokio::test]
async fn a_round_that_only_repeats_stops_the_loop() {
    let (_, keys, sessions) = run(
        3,
        vec![
            session(Some(vec![finding("a", 2)])),
            session(Some(vec![finding("a", 2)])),
            // must never be reached
            session(Some(vec![finding("c", 4)])),
        ],
    )
    .await;
    assert_eq!(keys, vec!["a"]);
    assert_eq!(sessions, 2);
}

#[tokio::test]
async fn a_failed_recall_round_does_not_make_the_run_partial() {
    let (status, keys, _) = run(2, vec![session(Some(vec![finding("a", 2)])), session(None)]).await;
    assert_eq!(status, RunStatus::Complete);
    assert_eq!(keys, vec!["a"]);
}

#[tokio::test]
async fn one_round_is_the_default() {
    let (_, keys, sessions) = run(
        1,
        vec![
            session(Some(vec![finding("a", 2)])),
            session(Some(vec![finding("b", 3)])),
        ],
    )
    .await;
    assert_eq!(keys, vec!["a"]);
    assert_eq!(sessions, 1);
}

#[test]
fn recall_rounds_are_bounded() {
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        f.path(),
        "review: {recall_rounds: 4}\nmodels:\n  investigator: {protocol: scripted, script: /tmp/s.json, model: m}\n",
    )
    .unwrap();
    let e = Config::load(f.path()).unwrap_err();
    assert!(e.to_string().contains("recall_rounds"), "{e}");
}
