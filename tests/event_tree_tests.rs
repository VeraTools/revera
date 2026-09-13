use revera::config::Config;
use revera::git::{current_head, materialize_head};
use revera::pipeline::common::{prepare, PrepareOut, ReviewRequest};
use serde_json::json;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Revera Test",
            "-c",
            "user.email=test@example.com",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn config() -> Config {
    serde_yaml::from_str(
        r#"
review: {strategy: baseline}
models:
  investigator: {protocol: scripted, script: /tmp/revera-event-script.json, model: m}
  validator: {protocol: scripted, script: /tmp/revera-event-script.json, model: m}
vera: {enabled: false}
"#,
    )
    .unwrap()
}

fn make_repo() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    git(repo, &["checkout", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn f(x: i32) -> i32 { x }\n").unwrap();
    git(repo, &["add", "src/lib.rs"]);
    git(repo, &["commit", "-q", "-m", "A"]);
    let a = git(repo, &["rev-parse", "HEAD"]);

    git(repo, &["checkout", "-q", "-b", "pr"]);
    std::fs::write(repo.join("src/other.rs"), "pub fn other() {}\n").unwrap();
    git(repo, &["add", "src/other.rs"]);
    git(repo, &["commit", "-q", "-m", "P"]);
    let p = git(repo, &["rev-parse", "HEAD"]);

    git(repo, &["checkout", "-q", "main"]);
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn f(x: i32) -> i32 { if x < 0 { return 0; } x }\n",
    )
    .unwrap();
    git(repo, &["add", "src/lib.rs"]);
    git(repo, &["commit", "-q", "-m", "B"]);
    git(repo, &["merge", "--no-ff", "-q", "pr", "-m", "M"]);
    let m = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", "-q", "--detach", &m]);
    (dir, a, p, m)
}

fn request(repo: &Path, base: &str, head: &str) -> ReviewRequest {
    ReviewRequest {
        repo: repo.to_path_buf(),
        base: base.into(),
        head: Some(head.into()),
        title: None,
        body: String::new(),
        strategy_override: None,
        force: false,
    }
}

#[tokio::test]
async fn prepare_rejects_head_not_checked_out() {
    let (dir, a, p, m) = make_repo();
    let err = match prepare(&config(), &request(dir.path(), &a, &p), "baseline").await {
        Ok(_) => panic!("prepare unexpectedly accepted a synthetic merge tree"),
        Err(err) => err,
    };
    let text = err.to_string();
    assert!(text.contains(&p), "{text}");
    assert!(text.contains(&m), "{text}");
    assert!(text.contains("check out"), "{text}");
}

#[tokio::test]
async fn materialize_head_exposes_pr_tree() {
    let (dir, a, p, _m) = make_repo();
    materialize_head(dir.path(), &p).await.unwrap();
    assert_eq!(current_head(dir.path()).await.unwrap(), p);

    let prep = match prepare(&config(), &request(dir.path(), &a, &p), "baseline")
        .await
        .unwrap()
    {
        PrepareOut::Ready(prep) => prep,
        PrepareOut::ShortCircuit(_, _) => panic!("unexpected short circuit"),
    };
    let content = prep
        .toolbox
        .call(
            "read_file",
            json!({"path": "src/lib.rs", "start_line": 1, "end_line": 5}),
        )
        .await;
    assert!(!content.contains("x < 0"), "{content}");
    assert!(dir.path().join("src/other.rs").exists());

    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn f(x: i32) -> i32 { x + 1 }\n",
    )
    .unwrap();
    let err = materialize_head(dir.path(), &p).await.unwrap_err();
    assert!(err.to_string().contains("working tree"), "{err:#}");
}
