//! Trust, secret containment, safe writes and git revision handling.

use revera::config::{Config, PublishMode};
use revera::{fsutil, git, redact};
use std::path::Path;
use std::process::Command;

fn sh_git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

const BASE_YAML: &str = r#"
review: {strategy: baseline}
models:
  investigator: {protocol: scripted, script: /dev/null, model: m}
  validator: {protocol: scripted, script: /dev/null, model: v}
vera:
  backend: api
  embedding: {base_url: "https://openrouter.ai/api/v1", model: qwen/qwen3-embedding-8b, api_key_env: REVERA_EMBEDDING_API_KEY}
"#;

fn cfg(extra: &str) -> Config {
    Config::parse(&format!("{BASE_YAML}{extra}"), "test").unwrap()
}

// ---------- publication ----------

#[test]
fn validation_disabled_cannot_publish() {
    let c = Config::parse(
        &BASE_YAML.replace("strategy: baseline", "strategy: baseline, validate: false"),
        "t",
    )
    .unwrap();
    assert!(c.check_publication(PublishMode::Comment).is_err());
    assert!(c.check_publication(PublishMode::DryRun).is_ok());
    assert!(cfg("").check_publication(PublishMode::Comment).is_ok());
}

// ---------- cache identity ----------

#[test]
fn index_identity_ignores_reranker_models_and_credentials() {
    let plain = cfg("");
    let rr = cfg(
        "  reranker: {base_url: \"https://openrouter.ai/api/v1\", model: cohere/rerank-4-pro, api_key_env: REVERA_EMBEDDING_API_KEY}\n",
    );
    let other_key = Config::parse(
        &BASE_YAML.replace(
            "api_key_env: REVERA_EMBEDDING_API_KEY",
            "api_key_env: OTHER_EMBED_KEY",
        ),
        "t",
    )
    .unwrap();
    let other_validator = Config::parse(&BASE_YAML.replace("model: v", "model: v2"), "t").unwrap();
    for c in [&rr, &other_key, &other_validator] {
        assert_eq!(plain.vera.index_key(), c.vera.index_key());
    }
    // but they are part of the review fingerprint
    assert_ne!(
        plain.review_fingerprint("baseline"),
        rr.review_fingerprint("baseline")
    );
    assert_ne!(
        plain.review_fingerprint("baseline"),
        other_validator.review_fingerprint("baseline")
    );
    let text = serde_json::to_string(&rr.review_fingerprint("baseline")).unwrap();
    assert!(!text.contains("REVERA_EMBEDDING_API_KEY"), "{text}");
}

#[test]
fn index_identity_tracks_embedding_model_and_exclusions() {
    let plain = cfg("");
    let other_model = Config::parse(
        &BASE_YAML.replace("qwen3-embedding-8b", "qwen3-embedding-4b"),
        "t",
    )
    .unwrap();
    let excl = cfg("  exclude: [\"vendor/**\"]\n");
    assert_ne!(plain.vera.index_key(), other_model.vera.index_key());
    assert_ne!(plain.vera.index_key(), excl.vera.index_key());
    let ex = plain.vera.effective_excludes();
    assert!(ex.iter().any(|e| e.contains(".env")), "{ex:?}");
}

// ---------- trust ----------

#[test]
fn event_mode_rejects_plain_http_and_reserved_env() {
    let http = Config::parse(
        &BASE_YAML.replace("https://openrouter.ai", "http://evil.example"),
        "t",
    )
    .unwrap();
    assert!(http.check_trust(true).is_err());
    assert!(http.check_trust(false).is_ok());
    let loopback = Config::parse(
        &BASE_YAML.replace("https://openrouter.ai", "http://127.0.0.1:9"),
        "t",
    )
    .unwrap();
    assert!(loopback.check_trust(true).is_ok());
    let gh = Config::parse(
        &BASE_YAML.replace(
            "api_key_env: REVERA_EMBEDDING_API_KEY",
            "api_key_env: GITHUB_TOKEN",
        ),
        "t",
    )
    .unwrap();
    assert!(gh.check_trust(false).is_err());
    let bad = Config::parse(
        &BASE_YAML.replace(
            "api_key_env: REVERA_EMBEDDING_API_KEY",
            "api_key_env: \"A;B\"",
        ),
        "t",
    )
    .unwrap();
    assert!(bad.check_trust(false).is_err());
}

#[test]
fn reranker_endpoint_path_is_validated() {
    for p in ["rerank", "https://x/rerank", "/../rerank"] {
        let c = cfg(&format!(
            "  reranker: {{base_url: \"https://x.example/v1\", model: r, api_key_env: K_RR, endpoint_path: \"{p}\"}}\n"
        ));
        assert!(c.check_trust(true).is_err(), "{p}");
    }
}

fn with_embedding_extra(extra: &str) -> Config {
    Config::parse(
        &BASE_YAML.replace(
            "api_key_env: REVERA_EMBEDDING_API_KEY}",
            &format!("api_key_env: REVERA_EMBEDDING_API_KEY, {extra}}}"),
        ),
        "t",
    )
    .unwrap()
}

#[test]
fn embedding_throughput_settings_parse_and_default() {
    let plain = cfg("");
    let e = plain.vera.embedding.as_ref().unwrap();
    assert_eq!(
        e.embedding_config_pairs(),
        vec![
            ("embedding.max_concurrent_requests", "8".to_string()),
            ("embedding.max_in_flight_inputs", "256".to_string()),
            ("embedding.timeout_secs", "120".to_string()),
        ]
    );
    let tuned = with_embedding_extra(
        "max_concurrent_requests: 4, max_in_flight_inputs: 64, timeout_secs: 300",
    );
    assert!(tuned.check_trust(false).is_ok());
    let e = tuned.vera.embedding.as_ref().unwrap();
    assert_eq!(
        e.embedding_config_pairs(),
        vec![
            ("embedding.max_concurrent_requests", "4".to_string()),
            ("embedding.max_in_flight_inputs", "64".to_string()),
            ("embedding.timeout_secs", "300".to_string()),
        ]
    );
}

#[test]
fn embedding_throughput_settings_are_embedding_only_and_positive() {
    for extra in [
        "max_concurrent_requests: 2",
        "max_in_flight_inputs: 128",
        "timeout_secs: 120",
    ] {
        let c = cfg(&format!(
            "  reranker: {{base_url: \"https://x.example/v1\", model: r, api_key_env: K_RR, {extra}}}\n"
        ));
        let e = c.check_trust(false).unwrap_err().to_string();
        assert!(e.contains("vera.embedding only"), "{extra}: {e}");
    }
    for extra in [
        "max_concurrent_requests: 0",
        "max_in_flight_inputs: 0",
        "timeout_secs: 0",
    ] {
        let e = with_embedding_extra(extra)
            .check_trust(false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("positive integer"), "{extra}: {e}");
    }
    let neg = BASE_YAML.replace(
        "api_key_env: REVERA_EMBEDDING_API_KEY}",
        "api_key_env: REVERA_EMBEDDING_API_KEY, timeout_secs: -1}",
    );
    assert!(Config::parse(&neg, "t").is_err());
    let local = Config::parse(
        &BASE_YAML.replace("backend: api", "backend: local").replace(
            "api_key_env: REVERA_EMBEDDING_API_KEY}",
            "api_key_env: REVERA_EMBEDDING_API_KEY, timeout_secs: 90}",
        ),
        "t",
    )
    .unwrap();
    let e = local.check_trust(false).unwrap_err().to_string();
    assert!(e.contains("vera.backend: api only"), "{e}");
}

#[test]
fn embedding_throughput_settings_keep_index_and_review_identity() {
    let plain = cfg("");
    let tuned = with_embedding_extra(
        "max_concurrent_requests: 4, max_in_flight_inputs: 64, timeout_secs: 300",
    );
    assert_eq!(plain.vera.index_key(), tuned.vera.index_key());
    assert_eq!(
        plain.review_fingerprint("baseline"),
        tuned.review_fingerprint("baseline")
    );
}

#[test]
fn embedding_throughput_pairs_apply_to_api_backend_only() {
    let repo = tempfile::tempdir().unwrap();
    let api = Config::parse(
        &BASE_YAML.replace(
            "api_key_env: REVERA_EMBEDDING_API_KEY}",
            "api_key_env: REVERA_TEST_KEY, timeout_secs: 90}",
        ),
        "t",
    )
    .unwrap();
    let client = revera::vera::VeraClient::from_config(&api.vera, repo.path()).unwrap();
    assert_eq!(
        client.embedding_pairs,
        vec![
            ("embedding.max_concurrent_requests", "8".to_string()),
            ("embedding.max_in_flight_inputs", "256".to_string()),
            ("embedding.timeout_secs", "90".to_string()),
        ]
    );
    let local = Config::parse(&BASE_YAML.replace("backend: api", "backend: local"), "t").unwrap();
    let client = revera::vera::VeraClient::from_config(&local.vera, repo.path()).unwrap();
    assert!(client.embedding_pairs.is_empty());
}

#[tokio::test]
async fn event_config_comes_from_base_not_head() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    sh_git(r, &["init", "-q"]);
    std::fs::write(r.join("revera.yaml"), BASE_YAML).unwrap();
    sh_git(r, &["add", "."]);
    sh_git(r, &["commit", "-qm", "base"]);
    let base = sh_git(r, &["rev-parse", "HEAD"]);
    // PR head redirects the embedding endpoint (and its credential)
    std::fs::write(
        r.join("revera.yaml"),
        BASE_YAML.replace(
            "https://openrouter.ai/api/v1",
            "https://attacker.example/v1",
        ),
    )
    .unwrap();
    sh_git(r, &["commit", "-qam", "head"]);
    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(r).unwrap();
    let res = revera::cli::load_event_config(r, None, None, &base).await;
    std::env::set_current_dir(prev).unwrap();
    let (origin, c) = res.unwrap();
    assert!(origin.to_string().contains(&base), "{origin}");
    let e = c.vera.embedding.unwrap();
    assert_eq!(e.base_url, "https://openrouter.ai/api/v1");
}

// ---------- redaction ----------

#[test]
fn redaction_masks_registered_and_recognizable_secrets() {
    assert_eq!(
        redact::secret_env("REVERA_TEST_SECRET_REDACT").as_deref(),
        Some("zz-secret-value-12345"),
        "REVERA_TEST_SECRET_REDACT comes from .cargo/config.toml: run through cargo test, with it unset in the shell"
    );
    let s = "key=zz-secret-value-12345 gh=ghp_abcdefghijklmnopqrstuvwxyz0123456789 or=sk-or-v1-0123456789abcdef0123456789abcdef";
    let out = redact::text(s);
    assert!(!out.contains("zz-secret-value-12345"), "{out}");
    assert!(!out.contains("ghp_abcdefghijklmnop"), "{out}");
    assert!(!out.contains("sk-or-v1-0123456789"), "{out}");
    let mut v = serde_json::json!({"a": ["x zz-secret-value-12345"], "b": {"c": "fine"}});
    redact::json(&mut v);
    assert_eq!(v["a"][0], format!("x {}", redact::REDACTED));
    assert_eq!(v["b"]["c"], "fine");
    // short values are never registered (would redact ordinary text)
    redact::register("abc");
    assert_eq!(redact::text("abc"), "abc");
}

// ---------- safe writes ----------

#[cfg(unix)]
#[test]
fn repo_writes_refuse_symlinks_and_traversal() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();

    // symlinked .revera directory
    symlink(outside.path(), root.path().join(".revera")).unwrap();
    assert!(fsutil::write_repo_file(root.path(), Path::new(".revera/state.json"), b"{}").is_err());
    assert!(!outside.path().join("state.json").exists());
    std::fs::remove_file(root.path().join(".revera")).unwrap();

    // symlinked target file
    std::fs::create_dir(root.path().join(".revera")).unwrap();
    let victim = outside.path().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    symlink(&victim, root.path().join(".revera/state.json")).unwrap();
    assert!(fsutil::write_repo_file(root.path(), Path::new(".revera/state.json"), b"{}").is_err());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    std::fs::remove_file(root.path().join(".revera/state.json")).unwrap();

    // traversal
    assert!(fsutil::write_repo_file(root.path(), Path::new("../x/state.json"), b"{}").is_err());
    assert!(fsutil::write_output(root.path(), Path::new("../escape.md"), b"x").is_err());

    // atomic replace leaves no temp files behind
    fsutil::write_repo_file(root.path(), Path::new(".revera/state.json"), b"one").unwrap();
    fsutil::write_repo_file(root.path(), Path::new(".revera/state.json"), b"two").unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join(".revera/state.json")).unwrap(),
        "two"
    );
    let names: Vec<_> = std::fs::read_dir(root.path().join(".revera"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");

    // absolute output: final symlink component refused
    let link = outside.path().join("out.md");
    symlink(&victim, &link).unwrap();
    assert!(fsutil::write_output(root.path(), &link, b"x").is_err());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");

    // absolute output inside the checkout: symlinked ancestors refused
    symlink(outside.path(), root.path().join("reports")).unwrap();
    let inside = root.path().join("reports/sub/out.md");
    assert!(fsutil::write_output(root.path(), &inside, b"x").is_err());
    assert!(!outside.path().join("sub").exists());
    let ok = root.path().join("real/out.md");
    fsutil::write_output(root.path(), &ok, b"y").unwrap();
    assert_eq!(std::fs::read_to_string(&ok).unwrap(), "y");
}

// ---------- git ----------

fn two_branch_repo() -> (tempfile::TempDir, String, String, String) {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    sh_git(r, &["init", "-q", "-b", "main"]);
    std::fs::write(r.join("a.txt"), "a\n").unwrap();
    sh_git(r, &["add", "."]);
    sh_git(r, &["commit", "-qm", "root"]);
    let root = sh_git(r, &["rev-parse", "HEAD"]);
    sh_git(r, &["checkout", "-qb", "pr"]);
    std::fs::write(r.join("pr.txt"), "pr change\n").unwrap();
    sh_git(r, &["add", "."]);
    sh_git(r, &["commit", "-qm", "pr"]);
    let head = sh_git(r, &["rev-parse", "HEAD"]);
    sh_git(r, &["checkout", "-q", "main"]);
    std::fs::write(r.join("main-only.txt"), "base moved\n").unwrap();
    sh_git(r, &["add", "."]);
    sh_git(r, &["commit", "-qm", "main moves"]);
    let base = sh_git(r, &["rev-parse", "HEAD"]);
    let _ = root;
    let mb = sh_git(r, &["merge-base", "main", "pr"]);
    (d, base, head, mb)
}

#[tokio::test]
async fn diff_uses_merge_base_and_ignores_textconv() {
    let (d, base, head, mb) = two_branch_repo();
    let r = d.path();
    assert_eq!(git::merge_base(r, &base, &head).await.unwrap(), mb);
    // a repo-configured external diff / textconv must never run
    std::fs::write(r.join(".gitattributes"), "*.txt diff=evil\n").unwrap();
    sh_git(r, &["config", "diff.evil.textconv", "touch PWNED; cat"]);
    sh_git(r, &["config", "diff.external", "touch PWNED2; false"]);
    let diff = git::diff(r, &base, &head).await.unwrap();
    assert!(diff.contains("pr.txt"), "{diff}");
    assert!(
        !diff.contains("main-only.txt"),
        "three-dot diff must exclude base-only changes: {diff}"
    );
    assert!(!r.join("PWNED").exists() && !r.join("PWNED2").exists());
}

#[tokio::test]
async fn revisions_are_validated_object_ids() {
    let (d, _base, head, _) = two_branch_repo();
    let r = d.path();
    assert_eq!(git::resolve_commit(r, "pr").await.unwrap(), head);
    assert!(git::resolve_commit(r, "--output=/tmp/x").await.is_err());
    assert!(git::read_blob(r, "HEAD", "a.txt", 100).await.is_err());
    assert_eq!(
        git::read_blob(r, &head, "a.txt", 100)
            .await
            .unwrap()
            .unwrap(),
        b"a\n"
    );
    assert!(git::read_blob(r, &head, "a.txt", 1).await.is_err());
    assert!(
        git::read_blob(r, &head, "missing", 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(git::is_oid(&head) && !git::is_oid("HEAD") && !git::is_oid(&head[..39]));
}

#[cfg(unix)]
#[tokio::test]
async fn read_blob_refuses_symlink_entries() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    sh_git(r, &["init", "-q"]);
    std::os::unix::fs::symlink("/etc/passwd", r.join("revera.yaml")).unwrap();
    sh_git(r, &["add", "."]);
    sh_git(r, &["commit", "-qm", "link"]);
    let head = sh_git(r, &["rev-parse", "HEAD"]);
    assert!(
        git::read_blob(r, &head, "revera.yaml", 1 << 20)
            .await
            .unwrap()
            .is_none()
    );
}

#[test]
fn sensitive_globs_cover_nested_credentials_only() {
    let s = revera::tools::glob_set(revera::config::SENSITIVE_GLOBS);
    for p in [
        ".env",
        "app/.env",
        "app/.env.production",
        "certs/server.pem",
        "k.key",
    ] {
        assert!(s.is_match(p), "{p}");
    }
    for p in ["src/env.rs", "docs/keys.md", "environment.yaml"] {
        assert!(!s.is_match(p), "{p}");
    }
}
