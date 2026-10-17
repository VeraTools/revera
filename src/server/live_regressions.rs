use super::*;
use std::time::Duration;

async fn session_stage(server: Arc<Server>) -> String {
    let (url, task) = http(server).await;
    let value: Value = reqwest::get(format!("{url}/api/session"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    task.abort();
    value["setup"]["stage"].as_str().unwrap().into()
}

async fn complete_setup(mock: &MockServer) -> (tempfile::TempDir, Arc<Server>) {
    let (data, server) = server(mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let yaml = CONFIG.replace("https://api.example.com/v1", &format!("{}/v1", mock.uri()));
    for (route, value) in [
        (
            "/api/secrets/REVIEW_API_KEY",
            json!({"value": "restart-model-key"}),
        ),
        ("/api/settings/config", json!({"yaml": yaml})),
    ] {
        assert_eq!(
            client
                .put(format!("{url}{route}"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&value)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 9, "account": {"login": "owner"}}
        ])))
        .expect(1)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token": "restart-inventory"})),
        )
        .expect(1)
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"repositories": [repository()]})),
        )
        .expect(1)
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(mock)
        .await;
    reconcile::once(&server).await.unwrap();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("Authorization", "Bearer restart-model-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "OK"}}]
        })))
        .expect(1)
        .mount(mock)
        .await;
    let result: Value = client
        .post(format!("{url}/api/settings/test"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"role": "investigator"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(session_stage(server.clone()).await, "done");
    task.abort();
    (data, server)
}

async fn restart(data: &Path, mock: &MockServer) -> Arc<Server> {
    initialize(
        settings(data),
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn completed_setup_is_done_on_restart_before_any_reconcile() {
    let mock = MockServer::start().await;
    let (data, original) = complete_setup(&mock).await;
    let requests = mock.received_requests().await.unwrap().len();
    drop(original);
    let restarted = restart(data.path(), &mock).await;
    assert_eq!(session_stage(restarted).await, "done");
    assert_eq!(mock.received_requests().await.unwrap().len(), requests);
    let health = std::fs::read_to_string(data.path().join("health.json")).unwrap();
    assert!(!health.contains("restart-model-key"));
    assert!(!health.contains("restart-inventory"));
}

#[tokio::test]
async fn probe_health_survives_noop_and_unrelated_saves_but_not_referenced_changes() {
    let mock = MockServer::start().await;
    let (data, server) = complete_setup(&mock).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let original = std::fs::read_to_string(data.path().join("health.json")).unwrap();
    let yaml = std::fs::read_to_string(&server.config).unwrap();
    for (route, body) in [
        ("/api/settings/config", json!({"yaml": yaml})),
        (
            "/api/secrets/UNRELATED_KEY",
            json!({"value": "not-a-probe-input"}),
        ),
        (
            "/api/secrets/REVIEW_API_KEY",
            json!({"value": "restart-model-key"}),
        ),
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
        assert_eq!(session_stage(server.clone()).await, "done");
        assert_eq!(
            std::fs::read_to_string(data.path().join("health.json")).unwrap(),
            original
        );
    }
    assert_eq!(
        session_stage(restart(data.path(), &mock).await).await,
        "done"
    );
    assert_eq!(
        client
            .put(format!("{url}/api/secrets/REVIEW_API_KEY"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"value": "changed-model-key"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(session_stage(server.clone()).await, "configure");
    assert_eq!(
        session_stage(restart(data.path(), &mock).await).await,
        "configure"
    );
    let health: Value = dashboard::read_json(data.path(), "health.json").unwrap();
    assert_eq!(health["models"], json!({}));
    assert!(
        !serde_json::to_string(&health)
            .unwrap()
            .contains("changed-model-key")
    );
    task.abort();
}

#[tokio::test]
async fn persisted_health_rejects_config_or_referenced_secret_edits_outside_the_api() {
    for field in ["yaml", "REVIEW_API_KEY", "UNRELATED_KEY"] {
        let mock = MockServer::start().await;
        let (data, server) = complete_setup(&mock).await;
        drop(server);
        if field == "yaml" {
            let yaml = format!(
                "{}# changed outside the dashboard\n",
                std::fs::read_to_string(data.path().join("revera.yaml")).unwrap()
            );
            crate::fsutil::write_atomic_in(data.path(), "revera.yaml", yaml.as_bytes()).unwrap();
        } else {
            let mut secrets: std::collections::BTreeMap<String, String> =
                dashboard::read_json(data.path(), "secrets.json").unwrap();
            secrets.insert(field.into(), "externally-changed-secret".into());
            crate::fsutil::write_secret_atomic(
                data.path(),
                "secrets.json",
                &serde_json::to_vec(&secrets).unwrap(),
            )
            .unwrap();
        }
        let expected = if field == "UNRELATED_KEY" {
            "done"
        } else {
            "configure"
        };
        assert_eq!(
            session_stage(restart(data.path(), &mock).await).await,
            expected
        );
    }
}

#[tokio::test]
async fn installation_restart_uses_seen_fallback_and_persisted_empty_inventories() {
    for state in ["seen-only", "installed-without-repos", "uninstalled"] {
        let mock = MockServer::start().await;
        let (data, server) = complete_setup(&mock).await;
        match state {
            "seen-only" => std::fs::remove_file(data.path().join("inventory.json")).unwrap(),
            "installed-without-repos" => {
                reconcile::save(&server, &reconcile::Seen::default())
                    .await
                    .unwrap();
                dashboard::write_inventory(data.path(), 1, &Default::default()).unwrap();
            }
            _ => dashboard::write_inventory(data.path(), 0, &Default::default()).unwrap(),
        }
        drop(server);
        let expected = if state == "uninstalled" {
            "install"
        } else {
            "done"
        };
        assert_eq!(
            session_stage(restart(data.path(), &mock).await).await,
            expected
        );
    }
}

#[tokio::test]
async fn health_fingerprint_covers_quoted_and_expanded_key_references() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let mut dashboard = server.dashboard.lock().await;
    for env_field in ["'api_key_env': QUOTED_KEY", "api_key_env: ${KEY_ENV_NAME}"] {
        let yaml = CONFIG.replace("api_key_env: REVIEW_API_KEY", env_field);
        dashboard
            .secrets
            .insert("KEY_ENV_NAME".into(), "QUOTED_KEY".into());
        dashboard
            .secrets
            .insert("QUOTED_KEY".into(), "first-secret-value".into());
        dashboard
            .sync_health(data.path(), Some(&yaml), &server.session_key)
            .unwrap();
        let first: Value = dashboard::read_json(data.path(), "health.json").unwrap();
        dashboard
            .secrets
            .insert("QUOTED_KEY".into(), "next-secret-value".into());
        dashboard
            .sync_health(data.path(), Some(&yaml), &server.session_key)
            .unwrap();
        let changed: Value = dashboard::read_json(data.path(), "health.json").unwrap();
        assert_ne!(first["key"], changed["key"], "{env_field}");
    }
}

async fn index_fixture(
    mock: &MockServer,
    output: &str,
    initialized: bool,
) -> (tempfile::TempDir, Arc<Server>, events::Job) {
    let runner = runner::Runner {
        executable: "/bin/sh".into(),
        prefix: vec![
            "-c".into(),
            format!("cat <<'OUTPUT' >&2\n{output}\nOUTPUT\nexit 1\n"),
            "fixture".into(),
        ],
    };
    let (data, server) = server(mock, runner, true).await;
    let source = data.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-qb", "main"]);
    std::fs::write(source.join("file"), "content").unwrap();
    git(&source, &["add", "."]);
    git(
        &source,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
        ],
    );
    let ws = crate::fsutil::ensure_dir(data.path(), Path::new("repos/owner/repo")).unwrap();
    git(
        &ws,
        &["clone", "--no-checkout", source.to_str().unwrap(), "."],
    );
    let mut repo = job("unused").repo;
    if initialized {
        repo.pushed_at = "before-failed-index".into();
        let mut seen = server.seen.lock().await;
        seen.observe(&repo, &[]);
        reconcile::save(&server, &seen).await.unwrap();
    }
    repo.pushed_at = "after-failed-index".into();
    let current = events::Job::new(repo, events::Kind::Index, "webhook");
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "index-job-token"})))
        .expect(1)
        .mount(mock)
        .await;
    server.enqueue(current.clone()).await;
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        runner::run(server.clone(), &running, cancel),
    )
    .await
    .unwrap();
    assert_eq!(job_metadata(data.path(), &current)["status"], "failed");
    (data, server, current)
}

#[tokio::test]
async fn failed_job_reason_uses_last_redacted_error_or_exit_status() {
    for (output, expected) in [
        (
            "Error: earlier failure\n  \
            Error: indexing failed: embedding status 504 index-job-token  \n\
            cleanup finished",
            "Error: indexing failed: embedding status 504 [REDACTED]".to_owned(),
        ),
        (
            "ordinary output\nno explicit error",
            "process exited with status 1".into(),
        ),
        (
            &format!("Error: {}", "é".repeat(400)),
            format!("Error: {}", "é".repeat(293)),
        ),
    ] {
        let mock = MockServer::start().await;
        let (data, _server, current) = index_fixture(&mock, output, true).await;
        let reason = job_metadata(data.path(), &current)["reason"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(reason, expected);
        assert!(reason.chars().count() <= 300);
        assert!(!reason.contains("index-job-token"));
    }
}

#[tokio::test]
async fn failed_index_preserves_pushed_at_and_next_reconcile_retries() {
    for state in ["known", "new", "legacy-advanced"] {
        let initialized = state != "new";
        let mock = MockServer::start().await;
        let (data, server, current) =
            index_fixture(&mock, "Error: indexing failed", initialized).await;
        let before = if initialized {
            "before-failed-index"
        } else {
            ""
        };
        let pushed_at = server
            .seen
            .lock()
            .await
            .0
            .get(&1)
            .map(|known| known.pushed_at.clone())
            .unwrap_or_default();
        assert_eq!(pushed_at, before);
        let saved: reconcile::Seen = dashboard::read_json(data.path(), "seen.json").unwrap();
        assert_eq!(
            saved
                .0
                .get(&1)
                .map(|known| known.pushed_at.as_str())
                .unwrap_or(""),
            before
        );
        if state == "legacy-advanced" {
            // The previous server also checkpointed failed runs already on disk.
            let mut seen = server.seen.lock().await;
            seen.0
                .get_mut(&1)
                .unwrap()
                .pushed_at
                .clone_from(&current.repo.pushed_at);
            reconcile::save(&server, &seen).await.unwrap();
        }
        mock.reset().await;
        Mock::given(method("GET"))
            .and(path("/app/installations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 9, "account": {"login": "owner"}}
            ])))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "inventory"})))
            .expect(1)
            .mount(&mock)
            .await;
        let mut repo = repository();
        repo["pushed_at"] = json!(current.repo.pushed_at);
        Mock::given(method("GET"))
            .and(path("/installation/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"repositories": [repo]})))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&mock)
            .await;
        reconcile::once(&server).await.unwrap();
        let queue = server.queue.lock().await;
        let retry = queue
            .pending
            .get(&current.key())
            .expect("failed index must be retried");
        assert!(matches!(retry.kind, events::Kind::Index));
        assert_eq!(retry.repo.pushed_at, current.repo.pushed_at);
    }
}
