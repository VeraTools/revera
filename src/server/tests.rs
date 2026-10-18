use super::*;
use aws_lc_rs::{encoding::AsDer, signature::KeyPair};
use base64::{Engine, prelude::BASE64_STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{path::Path, sync::OnceLock};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

fn test_app() -> app::App {
    static APP: OnceLock<app::App> = OnceLock::new();
    APP.get_or_init(|| {
        let key = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            BASE64_STANDARD.encode(key.as_der().unwrap().as_ref())
        );
        app::App {
            id: 42,
            slug: "revera-test".into(),
            client_id: "client".into(),
            client_secret: "test-client-secret".into(),
            name: "Revera test".into(),
            pem,
            webhook_secret: "test-webhook-secret".into(),
            owner: "owner".into(),
            html_url: "https://github.com/apps/revera-test".into(),
        }
    })
    .clone()
}

fn repository() -> Value {
    json!({"id": 1, "full_name": "owner/repo", "owner": {"login": "owner"},
        "default_branch": "main", "pushed_at": "one"})
}

fn pr(head: &str) -> Value {
    json!({
        "number": 7, "title": "PR", "body": "", "draft": false, "state": "open",
        "user": {"login": "human"},
        "head": {"sha": head, "ref": "topic", "repo": {"full_name": "owner/repo"}},
        "base": {"sha": "b".repeat(40), "ref": "main"}
    })
}

fn job(head: &str) -> events::Job {
    events::Job {
        id: uuid::Uuid::new_v4().simple().to_string(),
        trigger: "webhook".into(),
        mode: dashboard::Mode::Standard,
        repo: events::Repo::parse(&repository(), 9).unwrap(),
        kind: events::Kind::Review {
            number: 7,
            pr: pr(head),
            force: false,
        },
    }
}

fn signature(secret: &str, bytes: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(bytes);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[test]
fn webhook_signatures_and_setup_tokens() {
    let body = b"{\"a\":1}";
    let signed = signature("secret", body);
    assert!(app::signature_valid("secret", body, &signed));
    for bad in ["", "sha1=abc", "sha256=not-hex", "sha256=00"] {
        assert!(!app::signature_valid("secret", body, bad));
    }
    assert!(!app::signature_valid("other", body, &signed));
    assert!(!app::signature_valid("secret", b"{}", &signed));
    assert!(app::token_matches("bootstrap-token", "bootstrap-token"));
    assert!(!app::token_matches("bootstrap-token", "bootstrap-tokeN"));
    assert!(!app::token_matches("bootstrap-token", ""));
}

#[test]
fn event_mapping() {
    let mut v = json!({
        "installation": {"id": 9}, "repository": repository(), "number": 7,
        "pull_request": pr(&"a".repeat(40))
    });
    for action in ["opened", "reopened", "synchronize", "ready_for_review"] {
        v["action"] = json!(action);
        assert!(matches!(
            events::map("pull_request", &v, 42).unwrap(),
            events::Event::Job(_)
        ));
    }
    v["pull_request"]["draft"] = json!(true);
    assert!(matches!(
        events::map("pull_request", &v, 42).unwrap(),
        events::Event::Ignore
    ));
    v["pull_request"]["draft"] = json!(false);
    for bot in ["dependabot[bot]", "renovate[bot]"] {
        v["pull_request"]["user"]["login"] = json!(bot);
        assert!(matches!(
            events::map("pull_request", &v, 42).unwrap(),
            events::Event::Ignore
        ));
    }
    v["action"] = json!("closed");
    assert!(matches!(
        events::map("pull_request", &v, 42).unwrap(),
        events::Event::Close(1, 7)
    ));
    v["ref"] = json!("refs/heads/topic");
    assert!(matches!(
        events::map("push", &v, 42).unwrap(),
        events::Event::Ignore
    ));
    v["ref"] = json!("refs/heads/main");
    assert!(matches!(
        events::map("push", &v, 42).unwrap(),
        events::Event::Job(events::Job {
            kind: events::Kind::Index,
            ..
        })
    ));
    v["action"] = json!("rerequested");
    v["check_run"] = json!({"name": "Revera", "external_id": "7", "app": {"id": 42}});
    assert!(matches!(
        events::map("check_run", &v, 42).unwrap(),
        events::Event::Rerequest(_, 7)
    ));
    v["check_run"]["app"]["id"] = json!(99);
    assert!(matches!(
        events::map("check_run", &v, 42).unwrap(),
        events::Event::Ignore
    ));
    v["action"] = json!("deleted");
    assert!(matches!(
        events::map("installation", &v, 42).unwrap(),
        events::Event::RemoveInstallation(9)
    ));
    v["action"] = json!("removed");
    v["repositories_removed"] = json!([repository()]);
    assert!(matches!(
        events::map("installation_repositories", &v, 42).unwrap(),
        events::Event::RemoveRepos(_)
    ));
    let mut invalid = repository();
    invalid["full_name"] = json!("owner/..");
    assert!(events::Repo::parse(&invalid, 9).is_err());
}

#[test]
fn webhook_and_rest_pushed_at_compare_equal() {
    let mut rest = repository();
    rest["pushed_at"] = json!("2025-06-22T01:38:37Z");
    let mut webhook = repository();
    webhook["pushed_at"] = json!(1750556317);
    let rest = events::Repo::parse(&rest, 9).unwrap();
    assert_eq!(rest.pushed_at, "1750556317");
    assert_eq!(
        rest.pushed_at,
        events::Repo::parse(&webhook, 9).unwrap().pushed_at
    );
}

#[test]
fn queue_coalesces_cancels_and_serializes() {
    let mut queue = queue::Queue::default();
    queue.enqueue(job("one"));
    queue.enqueue(job("two"));
    assert_eq!(queue.pending.len(), 1);
    let (running, cancel) = queue.take().unwrap();
    assert_eq!(running.head(), "two");
    queue.enqueue(job("two"));
    assert!(queue.pending.is_empty());
    assert!(!*cancel.borrow());
    queue.enqueue(job("three"));
    assert!(*cancel.borrow());
    assert_eq!(queue.pending.len(), 1);
    let mut index = job("");
    index.kind = events::Kind::Index;
    queue.enqueue(index.clone());
    queue.enqueue(index);
    assert_eq!(queue.pending.len(), 2);
    assert!(queue.take().is_none(), "same repository cannot run twice");
    let mut other = job("four");
    other.repo.id = 2;
    other.repo.full_name = "owner/other".into();
    queue.enqueue(other);
    assert_eq!(queue.take().unwrap().0.repo.id, 2);
    queue.close((1, 7));
    assert!(!queue.pending.contains_key(&(1, 7)));
}

#[test]
fn reconcile_no_backfill_and_finished_only() {
    let mut seen = reconcile::Seen::default();
    let mut current = job("one");
    assert!(seen.observe(&current.repo, &[pr("one")]).is_empty());
    assert!(seen.observe(&current.repo, &[pr("one")]).is_empty());
    let jobs = seen.observe(&current.repo, &[pr("two")]);
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        seen.0[&1].heads[&7], "one",
        "enqueue never records a finished head"
    );
    current = job("two");
    seen.finished(&current);
    assert!(seen.observe(&current.repo, &[pr("two")]).is_empty());
    current.repo.pushed_at = "changed".into();
    assert!(matches!(
        seen.observe(&current.repo, &[pr("two")])[0].kind,
        events::Kind::Index
    ));
    current.kind = events::Kind::Index;
    seen.finished(&current);
    assert!(seen.observe(&current.repo, &[pr("two")]).is_empty());
    let mut draft = pr("three");
    draft["draft"] = json!(true);
    assert!(seen.observe(&current.repo, &[draft]).is_empty());
}

#[test]
fn finished_webhook_before_first_reconcile_does_not_backfill_other_prs() {
    let mut seen = reconcile::Seen::default();
    let current = job("finished");
    seen.finished(&current);
    let mut older = pr("already-open");
    older["number"] = json!(8);
    assert!(
        seen.observe(&current.repo, &[pr("finished"), older])
            .is_empty()
    );
}

#[test]
fn forced_same_head_queues_without_cancelling_running_review() {
    let mut queue = queue::Queue::default();
    queue.enqueue(job("same"));
    let (_, cancel) = queue.take().unwrap();
    let mut forced = job("same");
    if let events::Kind::Review { force, .. } = &mut forced.kind {
        *force = true;
    }
    queue.enqueue(forced);
    assert_eq!(queue.pending.len(), 1);
    assert!(!*cancel.borrow());
}

#[test]
fn jwt_signs_and_verifies_generated_key() {
    let app = test_app();
    let encoded = app
        .pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    let key =
        aws_lc_rs::rsa::KeyPair::from_pkcs8(&BASE64_STANDARD.decode(encoded).unwrap()).unwrap();
    let token = app.jwt().unwrap();
    let decoded = jsonwebtoken::decode::<Value>(
        &token,
        &jsonwebtoken::DecodingKey::from_rsa_der(key.public_key().as_ref()),
        &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256),
    )
    .unwrap();
    let now = chrono::Utc::now().timestamp();
    assert_eq!(decoded.claims["iss"], "42");
    assert!((decoded.claims["iat"].as_i64().unwrap() - (now - 60)).abs() <= 1);
    assert!((decoded.claims["exp"].as_i64().unwrap() - (now + 540)).abs() <= 1);
}

#[test]
fn manifest_fields() {
    let v = app::manifest("https://review.example.com", "revera-example");
    assert_eq!(v["name"], "revera-example");
    assert_eq!(v["public"], false);
    assert_eq!(v["url"], "https://github.com/VeraTools/revera");
    assert_eq!(
        v["hook_attributes"]["url"],
        "https://review.example.com/webhook"
    );
    assert_eq!(
        v["redirect_url"],
        "https://review.example.com/setup/callback"
    );
    assert_eq!(v["setup_url"], "https://review.example.com/");
    assert_eq!(
        v["default_permissions"],
        json!({"contents": "read", "pull_requests": "write", "checks": "write",
            "metadata": "read", "issues": "write"})
    );
    assert_eq!(
        v["default_events"],
        json!(["pull_request", "push", "check_run", "issue_comment"])
    );
}

#[test]
fn conclusions_and_summary_stripping() {
    let report = json!({"status": "complete", "findings": [],
        "plan": {"summary_markdown": "hello<!-- revera-state:secret --> world"}});
    assert_eq!(
        runner::conclusion(0, Some(&report), false),
        ("success", "No findings".into(), "hello world".into())
    );
    let mut finding = report.clone();
    finding["findings"] = json!([{"validation_status": "accepted"}]);
    assert_eq!(runner::conclusion(0, Some(&finding), false).0, "neutral");
    assert_eq!(runner::conclusion(0, Some(&finding), false).1, "1 findings");
    assert_eq!(runner::conclusion(2, Some(&report), false).0, "neutral");
    assert_eq!(runner::conclusion(1, Some(&report), false).0, "failure");
    assert_eq!(runner::conclusion(0, None, false).0, "failure");
    assert_eq!(runner::conclusion(0, Some(&report), true).0, "cancelled");
    assert_eq!(runner::strip_state("x<!-- revera-state:unfinished"), "x");
    assert!(runner::strip_state(&"é".repeat(40000)).len() <= 65535);
}

const CONFIG: &str = "models:
  investigator:
    protocol: openai-chat
    base_url: https://api.example.com/v1
    api_key_env: REVIEW_API_KEY
    model: test-model
";

fn settings(data: &Path) -> Settings {
    Settings {
        public_url: "https://revera.example.com".into(),
        listen: "127.0.0.1:0".parse().unwrap(),
        data_dir: data.into(),
        config: None,
        concurrency: 2,
        reconcile_minutes: 10,
    }
}

async fn server(
    mock: &MockServer,
    runner: runner::Runner,
    configured: bool,
) -> (tempfile::TempDir, Arc<Server>) {
    let data = tempfile::tempdir().unwrap();
    // These pre-mention fixtures exercise automatic-review behavior explicitly.
    dashboard::write_preferences(
        data.path(),
        &dashboard::Preferences {
            default_trigger: dashboard::Trigger::Auto,
            ..Default::default()
        },
    )
    .unwrap();
    crate::fsutil::write_atomic_in(data.path(), "revera.yaml", CONFIG.as_bytes()).unwrap();
    if configured {
        crate::fsutil::write_secret_atomic(
            data.path(),
            "app.json",
            &serde_json::to_vec(&test_app()).unwrap(),
        )
        .unwrap();
    }
    let server = initialize(
        settings(data.path()),
        runner,
        app::Api::new(mock.uri())
            .unwrap()
            .with_oauth_url(format!("{}/login/oauth/access_token", mock.uri())),
    )
    .await
    .unwrap();
    (data, server)
}

async fn http(server: Arc<Server>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router(server)).await.unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn operator_paths_and_proxy_url_prefix_are_supported() {
    let mock = MockServer::start().await;
    let scratch = tempfile::tempdir().unwrap();
    let data = crate::fsutil::ensure_dir(scratch.path(), Path::new("data")).unwrap();
    crate::fsutil::write_atomic_in(&data, "revera.yaml", CONFIG.as_bytes()).unwrap();
    let mut options = settings(&scratch.path().join("unused/../data"));
    options.public_url = "https://review.example.com/revera/".into();
    let server = initialize(
        options,
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(server.settings.data_dir, data);
    assert_eq!(
        server.settings.public_url,
        "https://review.example.com/revera"
    );
    assert_eq!(
        app::manifest(&server.settings.public_url, "revera")["hook_attributes"]["url"],
        "https://review.example.com/revera/webhook"
    );
}

#[tokio::test]
async fn setup_gating_state_and_signature_before_json() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), false).await;
    let token = server.setup.lock().await.token.clone().unwrap();
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let denied = client
        .get(format!("{url}/setup?token=bad"))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
    assert!(denied.headers().get("set-cookie").is_none());
    assert_eq!(
        client
            .get(format!("{url}/setup/callback?code=abc&state=bad"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let form = client
        .get(format!("{url}/setup?token={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 200);
    assert!(
        form.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .starts_with("revera_setup=")
    );
    let cookie = form.headers()["set-cookie"].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    for (header, authorized) in [(None, false), (Some(cookie), true)] {
        let mut request = client.get(format!("{url}/api/session"));
        if let Some(header) = header {
            request = request.header("cookie", header);
        }
        let session: Value = request.send().await.unwrap().json().await.unwrap();
        assert_eq!(session["setup"]["authorized"], authorized);
    }
    let manifest: Value = client
        .get(format!("{url}/api/setup/manifest?token={token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manifest["manifest"]["name"], "revera-example");
    assert_eq!(
        manifest["manifest"]["callback_urls"],
        json!(["https://revera.example.com/auth/callback"])
    );
    let app = test_app();
    let mut value = serde_json::to_value(&app).unwrap();
    value["owner"] = json!({"login": "owner"});
    Mock::given(method("POST"))
        .and(path("/app-manifests/abc/conversions"))
        .respond_with(ResponseTemplate::new(201).set_body_json(value))
        .expect(1)
        .mount(&mock)
        .await;
    let response = client
        .get(format!("{url}/setup/callback?code=abc&state={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(
        response.headers()["location"],
        "https://revera.example.com/auth/login"
    );
    assert!(!data.path().join("setup-token").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(data.path().join("app.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    for path in ["setup", "setup/callback"] {
        assert_eq!(
            client
                .get(format!("{url}/{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    let bad = b"not JSON";
    assert_eq!(
        client
            .post(format!("{url}/webhook"))
            .header("X-Hub-Signature-256", "bad")
            .body(bad.to_vec())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{url}/webhook"))
            .header("X-Hub-Signature-256", signature(&app.webhook_secret, bad))
            .body(bad.to_vec())
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    server.enqueue(job("private-head")).await;
    let status = client
        .get(format!("{url}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!status.contains("owner/repo"));
    assert!(!status.contains("private-head"));
    assert!(server.allowed(&app, "OWNER"));
    assert!(!server.allowed(&app, "other"));
    task.abort();
}

#[test]
fn child_environment_is_token_scoped() {
    let app = test_app();
    let env = runner::child_env(
        "CUSTOM_TOKEN",
        "installation-token",
        Path::new("/data"),
        "owner/repo",
    );
    let map: std::collections::BTreeMap<_, _> = env.into_iter().collect();
    assert_eq!(map["CUSTOM_TOKEN"], "installation-token");
    assert_eq!(map["XDG_CACHE_HOME"], "/data/cache/owner/repo");
    assert_eq!(map["GIT_CONFIG_COUNT"], "1");
    assert_eq!(
        map["GIT_CONFIG_KEY_0"],
        "http.https://github.com/.extraheader"
    );
    assert_eq!(
        map["GIT_CONFIG_VALUE_0"],
        format!(
            "AUTHORIZATION: basic {}",
            BASE64_STANDARD.encode("x-access-token:installation-token")
        )
    );
    assert!(
        map.values()
            .all(|value| !value.contains(&app.pem) && !value.contains(&app.webhook_secret))
    );
}

fn git(directory: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn injectable_child_runs_with_base_config_and_completes_check() {
    for mode in [
        dashboard::Mode::Quick,
        dashboard::Mode::Standard,
        dashboard::Mode::Deep,
    ] {
        let mock = MockServer::start().await;
        let script_dir = tempfile::tempdir().unwrap();
        let script = script_dir.path().join("child.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
[ "$CUSTOM_TOKEN" = 'installation-token' ] || exit 1
[ "$GIT_CONFIG_COUNT" = 1 ] || exit 1
profile=standard
while [ $# -gt 0 ]; do
  case $1 in
    --out) out=$2; shift;;
    --bot-login) [ "$2" = 'revera-test[bot]' ] || exit 1; shift;;
    --config) [ "$2" = revera.yaml ] || exit 1; shift;;
    --profile) profile=$2; shift;;
  esac
  shift
done
[ "$profile" = "$EXPECTED_PROFILE" ] || exit 1
echo installation-token
cat > "$out" <<'JSON'
{"status":"complete","findings":[],
 "plan":{"summary_markdown":"clean<!-- revera-state:hidden -->"}}
JSON
"#,
        )
        .unwrap();
        let runner = runner::Runner {
            executable: "/bin/sh".into(),
            prefix: vec![script.display().to_string()],
        };
        let (data, server) = server(&mock, runner, true).await;
        server.dashboard.lock().await.secrets.insert(
            "EXPECTED_PROFILE".into(),
            mode.profile().unwrap_or("standard").into(),
        );
        let source = script_dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        git(&source, &["init", "-q"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "test"]);
        std::fs::write(
            source.join("revera.yaml"),
            format!("{CONFIG}github:\n  token_env: CUSTOM_TOKEN\n"),
        )
        .unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-qm", "base"]);
        let head = crate::git::current_head(&source).await.unwrap();
        git(&source, &["update-ref", "refs/pull/7/head", &head]);
        let ws = crate::fsutil::ensure_dir(data.path(), Path::new("repos/owner/repo")).unwrap();
        git(
            &ws,
            &["clone", "--no-checkout", source.to_str().unwrap(), "."],
        );
        let mut current = job(&head);
        current.trigger = "mention".into();
        current.mode = mode;
        if let events::Kind::Review { pr, .. } = &mut current.kind {
            pr["base"]["sha"] = json!(head);
        }
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .and(body_json(json!({"repository_ids": [1], "permissions": {
                "contents": "read", "pull_requests": "write", "checks": "write", "metadata": "read"
            }})))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"token": "installation-token"})),
            )
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/check-runs"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 100})))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/repos/owner/repo/check-runs/100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
            .expect(1)
            .mount(&mock)
            .await;
        let (_tx, rx) = tokio::sync::watch::channel(false);
        runner::run(server.clone(), &current, rx).await;
        assert_eq!(server.seen.lock().await.0[&1].heads[&7], current.head());
        let requests = mock.received_requests().await.unwrap();
        let check: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        assert_eq!(check["conclusion"], "success");
        assert_eq!(check["output"]["summary"], "clean");
        let logs: Vec<_> = std::fs::read_dir(data.path().join("jobs"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("log.txt"))
            .filter(|p| p.is_file())
            .collect();
        assert_eq!(logs.len(), 1);
        let log = std::fs::read_to_string(&logs[0]).unwrap();
        assert!(!log.contains("installation-token"));
        assert!(log.contains("[REDACTED]"));
        let meta: Value = serde_json::from_slice(
            &std::fs::read(data.path().join("jobs").join(&current.id).join("meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["status"], "complete");
        assert!(meta["started_at"].is_string());
        assert!(meta["finished_at"].is_string());
        assert_eq!(meta["mode"], json!(mode));
        assert_eq!(meta["trigger"], "mention");
    }
}

#[tokio::test]
async fn reconcile_api_first_sighting_and_stale_head_are_not_backfilled() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 9, "account": {"login": "owner"}},
            {"id": 10, "account": {"login": "outside"}}
        ])))
        .expect(3)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .and(body_json(json!({"permissions": {
            "contents": "read", "pull_requests": "read", "metadata": "read"
        }})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "reconcile-token"})))
        .expect(3)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"repositories": [repository()]})),
        )
        .expect(3)
        .mount(&mock)
        .await;
    let first = Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr("first")])))
        .expect(1)
        .mount_as_scoped(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    assert!(server.queue.lock().await.pending.is_empty());
    drop(first);
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr("later")])))
        .expect(2)
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    assert_eq!(server.queue.lock().await.pending[&(1, 7)].head(), "later");
    assert_eq!(server.seen.lock().await.0[&1].heads[&7], "first");
    reconcile::once(&server).await.unwrap();
    assert_eq!(server.queue.lock().await.pending.len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn supersede_kills_child_group_cancels_check_and_does_not_record_head() {
    let mock = MockServer::start().await;
    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join("slow.sh");
    let marker = scratch.path().join("started");
    let program = format!("sleep 600 &\necho $! > '{}'\nwait\n", marker.display());
    std::fs::write(&script, program).unwrap();
    let runner = runner::Runner {
        executable: "/bin/sh".into(),
        prefix: vec![script.display().to_string()],
    };
    let (data, server) = server(&mock, runner, true).await;
    let source = scratch.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.email", "test@example.com"]);
    git(&source, &["config", "user.name", "test"]);
    std::fs::write(source.join("file"), "base").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "base"]);
    let head = crate::git::current_head(&source).await.unwrap();
    git(&source, &["update-ref", "refs/pull/7/head", &head]);
    let ws = crate::fsutil::ensure_dir(data.path(), Path::new("repos/owner/repo")).unwrap();
    git(
        &ws,
        &["clone", "--no-checkout", source.to_str().unwrap(), "."],
    );
    let mut current = job(&head);
    if let events::Kind::Review { pr, .. } = &mut current.kind {
        pr["base"]["sha"] = json!(head);
    }
    server
        .seen
        .lock()
        .await
        .observe(&current.repo, &[pr("previous")]);
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "cancel-token"})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/check-runs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 100})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/check-runs/100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
        .expect(1)
        .mount(&mock)
        .await;
    server.enqueue(current).await;
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    let server_copy = server.clone();
    let task = tokio::spawn(async move {
        runner::run(server_copy, &running, cancel).await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    server.enqueue(job(&"c".repeat(40))).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.seen.lock().await.0[&1].heads[&7], "previous");
    let requests = mock.received_requests().await.unwrap();
    let check: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    assert_eq!(check["conclusion"], "cancelled");
    #[cfg(target_os = "linux")]
    {
        // A killed descendant may briefly remain as a zombie awaiting its host reaper.
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
        assert!(state.is_none_or(|s| s.split(") ").nth(1).unwrap().starts_with('Z')));
    }
    #[cfg(not(target_os = "linux"))]
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err());
}

async fn administrator(server: &Server) -> (String, String) {
    server
        .dashboard
        .lock()
        .await
        .authorize("admin", true, &server.settings.data_dir)
        .unwrap();
    let csrf = uuid::Uuid::new_v4().simple().to_string();
    let session = auth::Session {
        user: auth::User {
            login: "admin".into(),
            avatar_url: "https://example.com/avatar".into(),
        },
        csrf: csrf.clone(),
        exp: chrono::Utc::now().timestamp() + 3600,
        kind: "session".into(),
    };
    (
        format!(
            "{}={}",
            auth::SESSION,
            auth::sign(&server.session_key, &session)
        ),
        csrf,
    )
}

#[test]
fn session_sign_verify_and_tamper() {
    let key = [7; 32];
    let payload = json!({"kind": "session", "login": "admin", "exp": 1234567});
    let cookie = auth::sign(&key, &payload);
    assert_eq!(auth::verify::<Value>(&key, &cookie), Some(payload));
    assert!(auth::verify::<Value>(&[8; 32], &cookie).is_none());
    let mut tampered = cookie.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    assert!(auth::verify::<Value>(&key, &tampered).is_none());
    assert!(auth::verify::<Value>(&key, "invalid").is_none());
}

#[tokio::test]
async fn csrf_secrets_and_config_validation() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let yaml = format!("{CONFIG}github:\n  token_env: CUSTOM_TOKEN\n");
    crate::fsutil::write_atomic_in(data.path(), "revera.yaml", yaml.as_bytes()).unwrap();
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let public: Value = client
        .get(format!("{url}/api/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public["authenticated"], false);
    assert!(public.get("csrf").is_none());
    let session: Value = client
        .get(format!("{url}/api/session"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["authenticated"], true);
    assert_eq!(session["csrf"], csrf);
    let endpoint = format!("{url}/api/secrets/REVIEW_API_KEY");
    assert_eq!(
        client
            .put(&endpoint)
            .json(&json!({"value": "secret-value"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .put(&endpoint)
            .header("Cookie", &cookie)
            .json(&json!({"value": "secret-value"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let value = "dashboard-only-credential-value";
    let response = client
        .put(&endpoint)
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"value": value}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert!(!text.contains(value));
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        status,
        json!({"name": "REVIEW_API_KEY", "source": "dashboard"})
    );
    let settings = client
        .get(format!("{url}/api/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!settings.contains(value));
    assert!(settings.contains("referenced_env"));
    for name in [
        "BAD-NAME",
        "1BAD",
        "GITHUB_TOKEN",
        "ACTIONS_RUNTIME_TOKEN",
        "CUSTOM_TOKEN",
    ] {
        let response = client
            .put(format!("{url}/api/secrets/{name}"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"value": value}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422, "{name}");
        assert!(!response.text().await.unwrap().contains(value));
    }
    for bad in [
        "unknown: true",
        &yaml.replace("https://api.example.com", "http://evil.example.com"),
    ] {
        let response = client
            .put(format!("{url}/api/settings/config"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"yaml": bad}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422);
        assert!(response.json::<Value>().await.unwrap()["error"].is_string());
        assert_eq!(std::fs::read_to_string(&server.config).unwrap(), yaml);
    }
    assert_eq!(
        client
            .put(format!("{url}/api/settings/config"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"yaml": yaml}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in ["secrets.json", "session-key"] {
            assert_eq!(
                std::fs::metadata(data.path().join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    task.abort();
}

#[test]
fn dashboard_secrets_override_inherited_child_environment() {
    let secrets = [("REVIEW_API_KEY".into(), "dashboard-value".into())]
        .into_iter()
        .collect();
    let env = runner::child_env_with_secrets(
        "CUSTOM_TOKEN",
        "job-token",
        Path::new("/data"),
        "owner/repo",
        &secrets,
    );
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("printf '%s' \"$REVIEW_API_KEY\"")
        .env("REVIEW_API_KEY", "environment-value")
        .envs(env)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"dashboard-value");
}

#[tokio::test]
async fn bootstrap_admin_requires_setup_cookie_and_is_consumed_once() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), false).await;
    let token = server.setup.lock().await.token.clone().unwrap();
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{url}/setup?token={token}"))
        .send()
        .await
        .unwrap();
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Cookie", cookie.parse().unwrap());
    assert!(auth::bootstrap(&server, &headers));
    assert!(!auth::bootstrap(&server, &axum::http::HeaderMap::new()));
    let mut dashboard = server.dashboard.lock().await;
    assert!(dashboard.authorize("intruder", false, data.path()).is_err());
    dashboard
        .authorize(
            "first-admin",
            auth::bootstrap(&server, &headers),
            data.path(),
        )
        .unwrap();
    assert_eq!(dashboard.preferences.admins, ["first-admin"]);
    assert!(dashboard.authorize("intruder", true, data.path()).is_err());
    dashboard.preferences.admins.clear();
    assert!(dashboard.authorize("intruder", true, data.path()).is_err());
    task.abort();
}

#[tokio::test]
async fn oauth_sign_in_rejects_non_admin_and_discards_user_token() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let login = client
        .get(format!("{url}/auth/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 303);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let value = cookie.split_once('=').unwrap().1;
    let state: Value = auth::verify(&server.session_key, value).unwrap();
    let token = format!("opaque-user-secret-{}", uuid::Uuid::new_v4().simple());
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token": token})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "login": "intruder", "avatar_url": "https://example.com/avatar"
        })))
        .expect(1)
        .mount(&mock)
        .await;
    let response = client
        .get(format!(
            "{url}/auth/callback?code=abc&state={}",
            state["nonce"].as_str().unwrap()
        ))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert!(server.dashboard.lock().await.preferences.admins.is_empty());
    assert_eq!(crate::redact::text(&token), token);
    task.abort();
}

#[tokio::test]
async fn job_metadata_lifecycle_and_pruning() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let current = job("head");
    server.enqueue(current.clone()).await;
    let meta = data.path().join("jobs").join(&current.id).join("meta.json");
    let read = || serde_json::from_slice::<Value>(&std::fs::read(&meta).unwrap()).unwrap();
    assert_eq!(read()["status"], "queued");
    server.transition(&current, "running", None).await.unwrap();
    assert_eq!(read()["status"], "running");
    assert!(read()["started_at"].is_string());
    server.transition(&current, "complete", None).await.unwrap();
    assert_eq!(read()["status"], "complete");
    assert!(read()["duration_ms"].is_u64());
    server.queue.lock().await.pending.remove(&current.key());
    for n in 0..500 {
        let mut summary = dashboard::JobSummary::queued(&job("head"));
        summary.id = format!("prune-{n}");
        summary.set_state("complete", None);
        server.update_job(summary).await.unwrap();
    }
    server.prune_jobs().await.unwrap();
    assert_eq!(server.dashboard.lock().await.jobs.len(), 500);
    assert_eq!(
        std::fs::read_dir(data.path().join("jobs")).unwrap().count(),
        500
    );
    assert!(!meta.exists());
}

#[tokio::test]
async fn disabled_repository_suppresses_webhook_and_reconcile_jobs() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    server.dashboard.lock().await.preferences.repos.insert(
        "owner/repo".into(),
        dashboard::RepoPreference {
            trigger: Some(dashboard::Trigger::Off),
            mode: None,
        },
    );
    let (url, task) = http(server.clone()).await;
    let payload = json!({"installation": {"id": 9, "node_id": "installation-node"},
        "repository": repository(), "number": 7, "action": "synchronize",
        "pull_request": pr("later")});
    let body = serde_json::to_vec(&payload).unwrap();
    let response = reqwest::Client::new()
        .post(format!("{url}/webhook"))
        .header("X-GitHub-Event", "pull_request")
        .header(
            "X-Hub-Signature-256",
            signature(&test_app().webhook_secret, &body),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    assert!(server.queue.lock().await.pending.is_empty());
    server
        .seen
        .lock()
        .await
        .observe(&job("old").repo, &[pr("old")]);
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 9, "account": {"login": "owner"}}
        ])))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "reconcile-token"})))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"repositories": [repository()]})),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr("later")])))
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    assert!(server.queue.lock().await.pending.is_empty());
    assert!(server.dashboard.lock().await.jobs.is_empty());
    task.abort();
}

#[tokio::test]
async fn sse_job_event_and_reserved_spa_paths() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    for path in [
        "/api/missing",
        "/auth/missing",
        "/webhook/missing",
        "/healthz/missing",
    ] {
        assert_eq!(
            client
                .get(format!("{url}{path}"))
                .header("Cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{path}"
        );
    }
    assert_eq!(
        client
            .get(format!("{url}/reviews/example"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let request = client
        .get(format!("{url}/api/events"))
        .header("Cookie", &cookie);
    let response = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while server.events.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let current = job("head");
    server.enqueue(current.clone()).await;
    let mut response = response.await.unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("event: job"));
    assert!(text.contains(&current.id));
    assert!(text.contains("\"status\":\"queued\""));
    task.abort();
}

fn job_metadata(data: &Path, job: &events::Job) -> Value {
    let path = data.join("jobs").join(&job.id).join("meta.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn removal_cancels_pending_and_running_job_metadata() {
    for installation in [false, true] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let running = job("head");
        server.enqueue(running.clone()).await;
        let (_, cancel) = server.queue.lock().await.take().unwrap();
        server.transition(&running, "running", None).await.unwrap();
        let pending = events::Job::new(running.repo.clone(), events::Kind::Index, "manual");
        server.enqueue(pending.clone()).await;
        let mut other = job("other-head");
        other.repo.id = 2;
        other.repo.installation = 10;
        other.repo.full_name = "owner/other".into();
        server.enqueue(other.clone()).await;
        let event = if installation {
            events::Event::RemoveInstallation(9)
        } else {
            events::Event::RemoveRepos(vec![running.repo.clone()])
        };
        server.handle(&test_app(), event).await.unwrap();
        assert!(*cancel.borrow());
        for job in [&running, &pending] {
            let meta = job_metadata(data.path(), job);
            assert_eq!(meta["status"], "cancelled");
            assert_eq!(meta["reason"], "repository removed");
            assert!(meta["finished_at"].is_string());
        }
        assert_eq!(job_metadata(data.path(), &other)["status"], "queued");
        assert!(matches!(
            server.queue.lock().await.pending[&(1, 0)].kind,
            events::Kind::Remove
        ));
        // A runner that was already finishing must not overwrite cancellation.
        server.transition(&running, "complete", None).await.unwrap();
        assert_eq!(job_metadata(data.path(), &running)["status"], "cancelled");
    }
}

#[tokio::test]
async fn shutdown_cancels_pending_and_running_job_metadata() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let running = job("head");
    server.enqueue(running.clone()).await;
    let (_, cancel) = server.queue.lock().await.take().unwrap();
    server.transition(&running, "running", None).await.unwrap();
    let pending = events::Job::new(running.repo.clone(), events::Kind::Index, "manual");
    server.enqueue(pending.clone()).await;
    server.cancel_jobs().await.unwrap();
    assert!(*cancel.borrow());
    assert!(server.queue.lock().await.pending.is_empty());
    for job in [&running, &pending] {
        let meta = job_metadata(data.path(), job);
        assert_eq!(meta["status"], "cancelled");
        assert_eq!(meta["reason"], "server shutting down");
        assert!(meta["finished_at"].is_string());
    }
    server.transition(&running, "running", None).await.unwrap();
    server
        .transition(&running, "failed", Some("late failure".into()))
        .await
        .unwrap();
    assert_eq!(job_metadata(data.path(), &running)["status"], "cancelled");
    assert_eq!(
        job_metadata(data.path(), &running)["reason"],
        "server shutting down"
    );
}

#[tokio::test]
async fn setup_stage_requires_active_retrieval_keys() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    administrator(&server).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.installations = 1;
        dashboard
            .secrets
            .insert("REVIEW_API_KEY".into(), "stage-model-key".into());
    }
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let retrieval = "vera:
  enabled: true
  backend: api
  embedding:
    base_url: https://embedding.example.com/v1
    model: embedding-model
    api_key_env: SERVER_STAGE_EMBED_KEY
  reranker:
    base_url: https://reranker.example.com/v1
    model: reranker-model
    api_key_env: SERVER_STAGE_RERANK_KEY
";
    for (backend, enabled, embedding, reranker, expected) in [
        ("api", true, None, None, "configure"),
        ("api", true, Some("embedding-key"), None, "configure"),
        ("api", true, Some("embedding-key"), Some("   "), "configure"),
        (
            "api",
            true,
            Some("embedding-key"),
            Some("reranker-key"),
            "done",
        ),
        ("local", true, None, Some("reranker-key"), "done"),
        ("local", true, None, None, "configure"),
        ("api", false, None, None, "done"),
    ] {
        let retrieval = retrieval
            .replace("backend: api", &format!("backend: {backend}"))
            .replace("enabled: true", &format!("enabled: {enabled}"));
        let yaml = format!("{CONFIG}{retrieval}");
        crate::fsutil::write_output(&server.settings.data_dir, &server.config, yaml.as_bytes())
            .unwrap();
        let mut dashboard = server.dashboard.lock().await;
        for (name, value) in [
            ("SERVER_STAGE_EMBED_KEY", embedding),
            ("SERVER_STAGE_RERANK_KEY", reranker),
        ] {
            dashboard.secrets.remove(name);
            if let Some(value) = value {
                dashboard.secrets.insert(name.into(), value.into());
            }
        }
        dashboard
            .sync_health(&server.settings.data_dir, Some(&yaml), &server.session_key)
            .unwrap();
        for role in ["investigator", "validator", "embedding"] {
            dashboard.health.insert(role.into(), json!({"ok": true}));
        }
        drop(dashboard);
        let session: Value = client
            .get(format!("{url}/api/session"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session["setup"]["stage"], expected, "{backend}/{enabled}");
    }
    task.abort();
}

#[tokio::test]
async fn fallback_probes_select_routes_without_affecting_primary_health_or_setup() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    server.dashboard.lock().await.installations = 1;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let yaml = format!(
        "{CONFIG}    fallback:\n      - protocol: openai-chat\n        base_url: {}/v1\n        api_key_env: FALLBACK_PROBE_KEY\n        model: backup-model\nvera:\n  enabled: false\n",
        mock.uri()
    );
    for (route, body) in [
        (
            "/api/secrets/REVIEW_API_KEY",
            json!({"value": "primary-probe-secret"}),
        ),
        (
            "/api/secrets/FALLBACK_PROBE_KEY",
            json!({"value": "fallback-probe-secret"}),
        ),
        ("/api/settings/config", json!({"yaml": yaml})),
    ] {
        assert_eq!(
            client
                .put(format!("{url}{route}"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("Authorization", "Bearer fallback-probe-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "OK"}}]
        })))
        .expect(2)
        .mount(&mock)
        .await;
    for role in ["investigator", "validator"] {
        let response = client
            .post(format!("{url}/api/settings/test"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"role": role, "fallback": 0}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["ok"], true);
        assert!(server.dashboard.lock().await.health.is_empty());
        let session: Value = client
            .get(format!("{url}/api/session"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session["setup"]["stage"], "configure");
    }
    for input in [
        json!({"role": "investigator", "fallback": 1}),
        json!({"role": "embedding", "fallback": 0}),
        json!({"role": "other", "fallback": 0}),
    ] {
        assert_eq!(
            client
                .post(format!("{url}/api/settings/test"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    let requests = mock.received_requests().await.unwrap();
    assert!(requests.iter().all(|request| {
        serde_json::from_slice::<Value>(&request.body).unwrap()["model"] == "backup-model"
    }));
    mock.reset().await;
    let explicit = yaml.replace(
        "vera:\n",
        &format!(
            "  validator:\n    protocol: openai-chat\n    base_url: https://api.example.com/v1\n    api_key_env: REVIEW_API_KEY\n    model: validator-model\n    fallback:\n      - protocol: openai-chat\n        base_url: {}/validator/v1\n        api_key_env: FALLBACK_PROBE_KEY\n        model: validator-backup\nvera:\n",
            mock.uri()
        ),
    );
    assert_eq!(
        client
            .put(format!("{url}/api/settings/config"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"yaml": explicit}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    Mock::given(method("POST"))
        .and(path("/validator/v1/chat/completions"))
        .and(header("Authorization", "Bearer fallback-probe-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "OK"}}]
        })))
        .expect(1)
        .mount(&mock)
        .await;
    let result: Value = client
        .post(format!("{url}/api/settings/test"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"role": "validator", "fallback": 0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["ok"], true);
    assert!(server.dashboard.lock().await.health.is_empty());
    let requests = mock.received_requests().await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].body).unwrap()["model"],
        "validator-backup"
    );
    // Unavailable optional fallbacks must not prevent setup after the primaries passed.
    client
        .delete(format!("{url}/api/secrets/FALLBACK_PROBE_KEY"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .send()
        .await
        .unwrap();
    let mut dashboard = server.dashboard.lock().await;
    for role in ["investigator", "validator"] {
        dashboard.health.insert(role.into(), json!({"ok": true}));
    }
    drop(dashboard);
    let result: Value = client
        .post(format!("{url}/api/settings/test"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"role": "validator", "fallback": 0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["ok"], false);
    assert!(!result.to_string().contains("fallback-probe-secret"));
    let session: Value = client
        .get(format!("{url}/api/session"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["setup"]["stage"], "done");
    assert_eq!(server.dashboard.lock().await.health.len(), 2);
    task.abort();
}

#[tokio::test]
async fn provider_probe_labels_inheritance_and_cache_invalidation() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let yaml = CONFIG.replace("https://api.example.com/v1", &format!("{}/v1", mock.uri()));
    for (route_path, body) in [
        (
            "/api/secrets/REVIEW_API_KEY",
            json!({"value": "wiremock-probe-key"}),
        ),
        ("/api/settings/config", json!({"yaml": yaml})),
    ] {
        assert_eq!(
            client
                .put(format!("{url}{route_path}"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("Authorization", "Bearer wiremock-probe-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "OK"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1}
        })))
        .expect(1)
        .mount(&mock)
        .await;
    let request = || {
        client
            .post(format!("{url}/api/settings/test"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"role": "investigator"}))
    };
    let result: Value = request().send().await.unwrap().json().await.unwrap();
    assert_eq!(result["ok"], true);
    assert!(result["latency_ms"].is_u64());
    assert!(result.get("error").is_none());
    let overview: Value = client
        .get(format!("{url}/api/overview"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for health in overview["health"]["models"].as_array().unwrap() {
        assert_eq!(health["model"], "test-model");
        assert_eq!(health["ok"], true);
        assert!(health["checked_at"].is_string());
    }
    mock.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "wiremock-probe-key is invalid"}
        })))
        .expect(1)
        .mount(&mock)
        .await;
    let failure = request().send().await.unwrap().text().await.unwrap();
    assert!(!failure.contains("wiremock-probe-key"));
    assert_eq!(
        serde_json::from_str::<Value>(&failure).unwrap()["ok"],
        false
    );
    assert_eq!(
        server.dashboard.lock().await.health["investigator"]["model"],
        "test-model"
    );
    assert_eq!(
        server.dashboard.lock().await.health["validator"]["ok"],
        false
    );
    mock.reset().await;
    // Both kinds of change must invalidate a probe that is already on the wire.
    for (route_path, body) in [
        (
            "/api/settings/config",
            json!({"yaml": format!("{yaml}# changed during the probe\n")}),
        ),
        (
            "/api/secrets/REVIEW_API_KEY",
            json!({"value": "replacement-probe-key"}),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_secs(1))
                    .set_body_json(json!({
                        "choices": [{"message": {"role": "assistant", "content": "OK"}}]
                    })),
            )
            .expect(1)
            .mount(&mock)
            .await;
        let outgoing = request();
        let probe = tokio::spawn(async move { outgoing.send().await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while mock.received_requests().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client
                .put(format!("{url}{route_path}"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert!(server.dashboard.lock().await.health.is_empty());
        let response: Value = probe.await.unwrap().json().await.unwrap();
        assert_eq!(response["ok"], true);
        assert!(server.dashboard.lock().await.health.is_empty());
        let persisted: Value =
            dashboard::read_json(&server.settings.data_dir, "health.json").unwrap();
        assert_eq!(persisted["models"], json!({}));
        mock.reset().await;
    }
    assert_eq!(
        client
            .delete(format!("{url}/api/secrets/REVIEW_API_KEY"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let failure: Value = request().send().await.unwrap().json().await.unwrap();
    assert_eq!(failure["ok"], false);
    assert_eq!(
        server.dashboard.lock().await.health["investigator"]["model"],
        "test-model"
    );
    assert!(mock.received_requests().await.unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn job_api_omits_absent_optional_fields_and_state_blob() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let current = events::Job::new(job("head").repo, events::Kind::Index, "manual");
    server.enqueue(current.clone()).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let endpoint = format!("{url}/api/jobs/{}", current.id);
    let result: Value = client
        .get(&endpoint)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for field in [
        "pr",
        "title",
        "author",
        "head_sha",
        "reason",
        "started_at",
        "finished_at",
        "duration_ms",
        "tokens",
        "check_run_url",
        "pr_url",
    ] {
        assert!(result["job"].get(field).is_none(), "{field}");
    }
    for field in ["summary_markdown", "timing", "ledger"] {
        assert!(result.get(field).is_none(), "{field}");
    }
    let report = json!({"plan": {"summary_markdown": "hello<!-- revera-state:private -->"},
        "findings": [{"severity": "low", "file": "file.rs", "title": "Finding",
            "validation_status": "accepted"}]});
    let dir = dashboard::job_dir(data.path(), &current.id).unwrap();
    crate::fsutil::write_atomic_in(&dir, "report.json", &serde_json::to_vec(&report).unwrap())
        .unwrap();
    let result: Value = client
        .get(&endpoint)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["summary_markdown"], "hello");
    assert!(result["findings"][0].get("line").is_none());
    assert!(result["findings"][0].get("body").is_none());
    assert!(!result.to_string().contains("revera-state"));
    let jobs: Value = client
        .get(format!("{url}/api/jobs"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(jobs.get("next").is_none());
    task.abort();
}

#[path = "regressions.rs"]
mod regressions;

#[path = "live_regressions.rs"]
mod live_regressions;

#[path = "backoff_tests.rs"]
mod backoff_tests;

#[path = "mention_tests.rs"]
mod mention_tests;
