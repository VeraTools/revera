use revera::config::{Config, EffectiveLane, Strategy};
use revera::pipeline::common::ReviewRequest;
use revera::pipeline::lens_router::{decide, select_lanes, LensRouterConfig};
use serde_json::json;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn router(base_url: &str, key_env: &str) -> LensRouterConfig {
    serde_yaml::from_str(&format!(
        "{{base_url: \"{base_url}\", api_key_env: {key_env}}}"
    ))
    .unwrap()
}

fn lanes(cfg_yaml: &str) -> Vec<EffectiveLane> {
    let c: Config = serde_yaml::from_str(cfg_yaml).unwrap();
    c.panel
        .effective_lanes(&c.models.investigator, c.models.scouts.as_deref())
        .unwrap()
}

fn three_lanes() -> Vec<EffectiveLane> {
    lanes("models:\n  investigator: {protocol: scripted, script: /tmp/s.json, model: m}\npanel: {focuses: [security, performance, documentation]}\n")
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn answers(probs: &[f64]) -> serde_json::Value {
    let a: serde_json::Map<String, serde_json::Value> = probs
        .iter()
        .enumerate()
        .map(|(i, p)| (format!("lane_{i}"), json!({"type": "noul", "noul": p})))
        .collect();
    json!({"model": "jev-1.13.0", "answers": a, "usage": {"input_tokens": 1, "output_tokens": 1}})
}

#[test]
fn decide_keeps_relevant_lanes_and_never_none() {
    assert_eq!(decide(&[0.9, 0.05, 0.2], 0.2), vec![true, false, true]);
    // nothing qualifies: the most relevant lane still runs
    assert_eq!(decide(&[0.01, 0.15, 0.1], 0.2), vec![false, true, false]);
}

#[tokio::test]
async fn skips_irrelevant_lanes_with_one_batched_request() {
    std::env::set_var("REVERA_LR_KEY_OK", "ts-test");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer ts-test"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let qs = body["questions"].as_object().unwrap();
            assert_eq!(qs.len(), 3);
            assert_eq!(body["model"], "jev-latest");
            assert_eq!(qs["lane_0"]["type"], "noul");
            assert_eq!(
                qs["lane_2"]["instructions"]["reviewer_focus"],
                "documentation"
            );
            assert!(body["state"]["diff"].as_str().unwrap().contains("fn f"));
            ResponseTemplate::new(200).set_body_json(answers(&[0.93, 0.61, 0.04]))
        })
        .expect(1)
        .mount(&server)
        .await;
    let cfg = router(&format!("{}/v1", server.uri()), "REVERA_LR_KEY_OK");
    let d = select_lanes(&cfg, "+fn f() {}", &three_lanes(), deadline()).await;
    assert_eq!(d.keep, vec![true, true, false]);
    let note = d.note.unwrap();
    assert!(note.contains("documentation (0.04)"), "{note}");
}

async fn fails_open_with(resp: ResponseTemplate, needle: &str) {
    std::env::set_var("REVERA_LR_KEY_FAIL", "ts-test");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(resp)
        .mount(&server)
        .await;
    let cfg = router(&format!("{}/v1", server.uri()), "REVERA_LR_KEY_FAIL");
    let d = select_lanes(&cfg, "+x", &three_lanes(), deadline()).await;
    assert_eq!(d.keep, vec![true, true, true]);
    let note = d.note.unwrap();
    assert!(
        note.contains("all lanes ran") && note.contains(needle),
        "{note}"
    );
}

#[tokio::test]
async fn overload_runs_every_lane() {
    fails_open_with(ResponseTemplate::new(529), "HTTP 529").await;
}

#[tokio::test]
async fn missing_or_invalid_answers_run_every_lane() {
    fails_open_with(
        ResponseTemplate::new(200).set_body_json(answers(&[0.9, 0.1])),
        "lane 2",
    )
    .await;
    fails_open_with(
        ResponseTemplate::new(200).set_body_json(answers(&[0.9, 1.7, 0.1])),
        "lane 1",
    )
    .await;
    fails_open_with(
        ResponseTemplate::new(200).set_body_string("not json"),
        "invalid response",
    )
    .await;
}

#[tokio::test]
async fn slow_router_times_out_and_runs_every_lane() {
    std::env::set_var("REVERA_LR_KEY_SLOW", "ts-test");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(answers(&[0.0, 0.0, 0.0]))
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let mut cfg = router(&format!("{}/v1", server.uri()), "REVERA_LR_KEY_SLOW");
    cfg.timeout_seconds = 1;
    let d = select_lanes(&cfg, "+x", &three_lanes(), deadline()).await;
    assert_eq!(d.keep, vec![true, true, true]);
}

#[tokio::test]
async fn missing_key_runs_every_lane_without_a_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let cfg = router(
        &format!("{}/v1", server.uri()),
        "REVERA_LR_KEY_NEVER_SET_123",
    );
    let d = select_lanes(&cfg, "+x", &three_lanes(), deadline()).await;
    assert_eq!(d.keep, vec![true, true, true]);
    assert!(d
        .note
        .unwrap()
        .contains("REVERA_LR_KEY_NEVER_SET_123 is not set"));
}

#[test]
fn config_refuses_bad_router_settings() {
    let write = |router: &str| {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            f.path(),
            format!("models:\n  investigator: {{protocol: scripted, script: /tmp/s.json, model: m}}\npanel:\n  lens_router: {router}\n"),
        )
        .unwrap();
        f
    };
    let e = Config::load(write("{api_key_env: K, min_probability: 1.5}").path()).unwrap_err();
    assert!(e.to_string().contains("min_probability"), "{e}");
    assert!(Config::load(write("{api_key_env: K, base_url: \"not a url\"}").path()).is_err());
    assert!(Config::load(write("{api_key_env: K, threshold: 0.3}").path()).is_err());

    // panel runs require the router key; other strategies do not
    let c = Config::load(write("{api_key_env: REVERA_LR_KEY_UNSET_456}").path()).unwrap();
    let e = c.validate_for(Strategy::Panel).unwrap_err();
    assert!(e.to_string().contains("REVERA_LR_KEY_UNSET_456"), "{e}");
    assert!(c.validate_for(Strategy::Baseline).is_ok());
}

fn git(repo: &Path, args: &[&str]) {
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
}

/// Run a panel review of a change to `file` with the router pointed at `server`.
async fn panel_summary(server: &MockServer, file: &str) -> String {
    std::env::set_var("REVERA_LR_KEY_E2E", "ts-test");
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(file), "pub fn f() {}\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".revera/\nscript.json\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    std::fs::write(repo.join(file), "pub fn f() -> u8 { 1 }\n").unwrap();
    git(repo, &["commit", "-q", "-am", "change"]);
    let script = repo.join("script.json");
    std::fs::write(&script, r#"{"roles": {}}"#).unwrap();
    let cfg: Config = serde_yaml::from_str(&format!(
        "review: {{strategy: panel, validate: false}}\npanel:\n  focuses: [security, performance, documentation]\n  lens_router: {{base_url: \"{}/v1\", api_key_env: REVERA_LR_KEY_E2E}}\nmodels:\n  investigator: {{protocol: scripted, script: {}, model: m}}\n",
        server.uri(),
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
    rep.plan.summary_markdown
}

#[tokio::test]
async fn panel_runs_only_the_lanes_the_router_selects() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answers(&[0.8, 0.1, 0.05])))
        .expect(1)
        .mount(&server)
        .await;
    let summary = panel_summary(&server, "src/lib.rs").await;
    assert!(summary.contains("panel: 1 scouts"), "{summary}");
    assert!(
        summary.contains("lens router skipped performance (0.10), documentation (0.05)"),
        "{summary}"
    );
}

#[tokio::test]
async fn security_sensitive_changes_bypass_the_router() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answers(&[0.0, 0.0, 0.0])))
        .expect(0)
        .mount(&server)
        .await;
    let summary = panel_summary(&server, "src/auth.rs").await;
    assert!(summary.contains("panel: 3 scouts"), "{summary}");
    assert!(!summary.contains("lens router"), "{summary}");
}
