use super::*;
use std::time::Duration;
use wiremock::matchers::body_partial_json;

async fn without_models(mock: &MockServer) -> (tempfile::TempDir, Arc<Server>) {
    let data = tempfile::tempdir().unwrap();
    let server = initialize(
        settings(data.path()),
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    // Preserve this fixture's automatic-review scenarios under mention-only defaults.
    server.dashboard.lock().await.preferences.default_trigger = dashboard::Trigger::Auto;
    (data, server)
}

#[tokio::test]
async fn missing_default_config_starts_and_exposes_empty_settings_and_configure_stage() {
    let mock = MockServer::start().await;
    let (data, server) = without_models(&mock).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let session: Value = client
        .get(format!("{url}/api/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["setup"]["stage"], "create_app");
    server.setup.lock().await.app = Some(test_app());
    let (cookie, csrf) = administrator(&server).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.installations = 1;
        dashboard
            .health
            .insert("investigator".into(), json!({"ok": true}));
        dashboard
            .health
            .insert("validator".into(), json!({"ok": true}));
    }
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
    assert_eq!(session["setup"]["stage"], "configure");
    let response = client
        .get(format!("{url}/api/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let settings: Value = response.json().await.unwrap();
    assert_eq!(settings["yaml"], "");
    assert_eq!(settings["referenced_env"], json!([]));
    assert_eq!(settings["secrets"], json!([]));
    // The wizard sets its first key before it can save a credential-checked config.
    let response = client
        .put(format!("{url}/api/secrets/REVIEW_API_KEY"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"value": "onboarding-private-key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        !std::fs::read_to_string(data.path().join("secrets.json"))
            .unwrap()
            .is_empty()
    );
    let response = client
        .get(format!("{url}/api/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!response.contains("onboarding-private-key"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(data.path().join("secrets.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!server.config.exists());
    task.abort();
}

#[tokio::test]
async fn invalid_present_and_missing_explicit_server_configs_still_fail_startup() {
    let mock = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut options = settings(data.path());
    options.config = Some(data.path().join("explicit.yaml"));
    let error = initialize(
        options,
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("must exist and be valid"));
    for invalid in ["", "models: [", "models: {}\n"] {
        crate::fsutil::write_atomic_in(data.path(), "revera.yaml", invalid.as_bytes()).unwrap();
        let error = initialize(
            settings(data.path()),
            runner::Runner::current().unwrap(),
            app::Api::new(mock.uri()).unwrap(),
        )
        .await
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("invalid config server config"),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn unconfigured_review_and_index_jobs_skip_without_checks_or_seen_changes() {
    let mock = MockServer::start().await;
    let (data, server) = without_models(&mock).await;
    server.setup.lock().await.app = Some(test_app());
    for current in [
        job("head"),
        events::Job::new(job("head").repo, events::Kind::Index, "webhook"),
    ] {
        server.enqueue(current.clone()).await;
        assert!(server.queue.lock().await.pending.is_empty());
        let meta = job_metadata(data.path(), &current);
        assert_eq!(meta["status"], "skipped");
        assert_eq!(meta["reason"], "models are not configured yet");
        assert!(meta["finished_at"].is_string());
        assert!(meta.get("check").is_none());
        assert!(meta.get("check_run_url").is_none());
        assert!(server.seen.lock().await.0.is_empty());
    }
    // A config removed after enqueue is also caught before any credentials or git work.
    crate::fsutil::write_atomic_in(data.path(), "revera.yaml", CONFIG.as_bytes()).unwrap();
    let current = job("queued-before-removal");
    server.enqueue(current.clone()).await;
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    std::fs::remove_file(&server.config).unwrap();
    runner::run(server.clone(), &running, cancel).await;
    assert_eq!(job_metadata(data.path(), &current)["status"], "skipped");
    assert!(server.seen.lock().await.0.is_empty());
    assert!(mock.received_requests().await.unwrap().is_empty());
    assert!(!data.path().join("seen.json").exists());
    assert!(!data.path().join("repos/owner/repo").exists());
}

#[tokio::test]
async fn first_config_save_reconciles_setup_skips_without_backfilling_other_prs() {
    for initial_inventory in [false, true] {
        let mock = MockServer::start().await;
        let (data, server) = without_models(&mock).await;
        mock_registration(&mock).await;
        server.setup.lock().await.app = Some(test_app());
        let (cookie, csrf) = administrator(&server).await;
        let current = job("new-head");
        let mut existing = pr("existing");
        existing["number"] = json!(8);
        if initial_inventory {
            server
                .seen
                .lock()
                .await
                .observe(&current.repo, &[pr("old-head"), existing.clone()]);
        }
        server.enqueue(current.clone()).await;
        assert!(server.queue.lock().await.pending.is_empty());
        assert_eq!(job_metadata(data.path(), &current)["status"], "skipped");
        Mock::given(method("GET"))
            .and(path("/app/installations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 9, "account": {"login": "owner"}}
            ])))
            .expect(2)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"token": "inventory-key"})),
            )
            .expect(2)
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/installation/repositories"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"repositories": [repository()]})),
            )
            .expect(2)
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([pr("new-head"), existing])),
            )
            .expect(2)
            .mount(&mock)
            .await;
        reconcile::once(&server).await.unwrap();
        let previous = initial_inventory.then_some("old-head".to_owned());
        assert_eq!(
            server.seen.lock().await.0[&1].heads.get(&7).cloned(),
            previous
        );
        assert!(server.queue.lock().await.pending.is_empty());
        let (url, task) = http(server.clone()).await;
        let client = reqwest::Client::new();
        let invalid = client
            .put(format!("{url}/api/settings/config"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"yaml": ""}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), 422);
        assert!(!server.config.exists());
        server
            .dashboard
            .lock()
            .await
            .secrets
            .insert("REVIEW_API_KEY".into(), "first-config-key".into());
        let response = client
            .put(format!("{url}/api/settings/config"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"yaml": CONFIG}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(std::fs::read_to_string(&server.config).unwrap(), CONFIG);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !server
                .queue
                .lock()
                .await
                .pending
                .contains_key(&current.key())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            server.queue.lock().await.pending[&current.key()].head(),
            "new-head"
        );
        assert_eq!(server.queue.lock().await.pending.len(), 1);
        assert_eq!(
            server.seen.lock().await.0[&1].heads.get(&7).cloned(),
            previous
        );
        assert_eq!(server.seen.lock().await.0[&1].heads[&8], "existing");
        assert!(
            server
                .dashboard
                .lock()
                .await
                .unconfigured_reviews("owner/repo")
                .is_empty()
        );
        assert!(
            mock.received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| !request.url.path().contains("check-runs"))
        );
        task.abort();
    }
}

async fn deliver(server: &Arc<Server>, event: &str, payload: Value) -> StatusCode {
    let body = serde_json::to_vec(&payload).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("X-GitHub-Event", event.parse().unwrap());
    headers.insert(
        "X-Hub-Signature-256",
        signature(&test_app().webhook_secret, &body)
            .parse()
            .unwrap(),
    );
    webhook(State(server.clone()), headers, body.into()).await
}

#[tokio::test]
async fn real_delivery_owners_gate_repo_and_installation_events() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "rerequest-key"})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr("rerequest")))
        .expect(1)
        .mount(&mock)
        .await;
    for (event, extra, key) in [
        (
            "pull_request",
            json!({"action": "opened", "number": 7,
            "pull_request": pr("webhook-head")}),
            (1, 7),
        ),
        (
            "push",
            json!({"ref": "refs/heads/main", "deleted": false}),
            (1, 0),
        ),
        (
            "check_run",
            json!({"action": "rerequested", "check_run": {
            "name": "Revera", "external_id": "7", "app": {"id": 42}}}),
            (1, 7),
        ),
    ] {
        server.queue.lock().await.pending.clear();
        let mut payload = extra;
        payload["installation"] = json!({"id": 9, "node_id": "installation-node"});
        payload["repository"] = repository();
        assert_eq!(
            deliver(&server, event, payload.clone()).await,
            StatusCode::ACCEPTED
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !server.queue.lock().await.pending.contains_key(&key) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.queue.lock().await.pending.clear();
        payload["repository"]["owner"]["login"] = json!("outside");
        assert_eq!(deliver(&server, event, payload).await, StatusCode::ACCEPTED);
        assert!(server.queue.lock().await.pending.is_empty());
    }
    // Installation events have an account, not a repository owner.
    let mut payload = json!({"action": "removed", "installation": {
        "id": 9, "account": {"login": "outside"}}, "repositories_removed": [repository()]});
    deliver(&server, "installation_repositories", payload.clone()).await;
    assert!(server.queue.lock().await.pending.is_empty());
    payload["installation"]["account"]["login"] = json!("owner");
    deliver(&server, "installation_repositories", payload).await;
    assert!(matches!(
        server.queue.lock().await.pending[&(1, 0)].kind,
        events::Kind::Remove
    ));
}

#[tokio::test]
async fn probes_never_forward_credentials_through_redirects() {
    let origin = MockServer::start().await;
    let other = MockServer::start().await;
    Mock::given(|_: &wiremock::Request| true)
        .respond_with(ResponseTemplate::new(307).insert_header("Location", other.uri()))
        .mount(&origin)
        .await;
    Mock::given(|_: &wiremock::Request| true)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&other)
        .await;
    for protocol in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
        let route = serde_json::from_value(json!({"protocol": protocol,
            "base_url": origin.uri(), "api_key_env": "PROBE_REDIRECT_KEY", "model": "test",
            "extra_headers": {"x-custom-key": "custom-probe-secret"}}))
        .unwrap();
        let before = origin.received_requests().await.unwrap().len();
        assert!(
            crate::provider::probe::models(&route, "redirect-probe-secret")
                .await
                .is_err()
        );
        assert!(
            crate::provider::probe::model(route, "redirect-probe-secret".into())
                .await
                .is_err()
        );
        assert!(origin.received_requests().await.unwrap().len() >= before + 2);
    }
    let endpoint = serde_json::from_value(json!({"base_url": origin.uri(),
        "api_key_env": "PROBE_REDIRECT_KEY", "model": "embedding"}))
    .unwrap();
    assert!(
        crate::provider::probe::embedding(endpoint, "redirect-probe-secret".into())
            .await
            .is_err()
    );
    assert!(other.received_requests().await.unwrap().is_empty());
}

async fn open_events(server: &Arc<Server>, url: &str, cookie: &str) -> reqwest::Response {
    let previous = server.events.receiver_count();
    let request = reqwest::Client::new()
        .get(format!("{url}/api/events"))
        .header("Cookie", cookie);
    let response = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.events.receiver_count() <= previous {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server
        .events
        .send(("repos", json!({"initial": true})))
        .unwrap();
    let mut response = response.await.unwrap();
    assert!(response.chunk().await.unwrap().is_some());
    response
}

#[tokio::test]
async fn sse_ends_before_emitting_to_revoked_or_expired_sessions() {
    for expired in [false, true] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let (mut cookie, _) = administrator(&server).await;
        let exp = chrono::Utc::now().timestamp() + 2;
        if expired {
            let mut session: auth::Session =
                auth::verify(&server.session_key, cookie.split_once('=').unwrap().1).unwrap();
            session.exp = exp;
            cookie = format!(
                "{}={}",
                auth::SESSION,
                auth::sign(&server.session_key, &session)
            );
        }
        let (url, task) = http(server.clone()).await;
        let mut response = open_events(&server, &url, &cookie).await;
        if expired {
            while chrono::Utc::now().timestamp() < exp {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        } else {
            server.dashboard.lock().await.preferences.admins.clear();
        }
        server
            .events
            .send(("job", json!({"private": "must not arrive"})))
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), response.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        task.abort();
    }
}

#[tokio::test]
async fn shutdown_cancels_before_bounded_http_and_sse_drain() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let yaml = CONFIG.replace("https://api.example.com/v1", &format!("{}/v1", mock.uri()));
    crate::fsutil::write_output(&server.settings.data_dir, &server.config, yaml.as_bytes())
        .unwrap();
    server
        .dashboard
        .lock()
        .await
        .secrets
        .insert("REVIEW_API_KEY".into(), "drain-probe-key".into());
    // Start with real cached health so the startup tick does not race the manual
    // probe whose HTTP drain this test exercises.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "OK"}}]
        })))
        .expect(1)
        .mount(&mock)
        .await;
    api::test_untested(&server).await;
    assert_eq!(
        server.dashboard.lock().await.health["investigator"]["ok"],
        true
    );
    assert_eq!(
        server.dashboard.lock().await.health["validator"]["ok"],
        true
    );
    mock.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(json!({"choices": [{"message": {"role": "assistant",
                "content": "OK"}}]})),
        )
        .expect(1)
        .mount(&mock)
        .await;
    let current = job("head");
    server.enqueue(current.clone()).await;
    let (_, mut cancel) = server.queue.lock().await.take().unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = cancelled.clone();
    server.workers.lock().await.spawn(async move {
        cancel.wait_for(|cancel| *cancel).await.unwrap();
        observed.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (signal, received) = tokio::sync::oneshot::channel();
    let serving = server.clone();
    let task = tokio::spawn(async move {
        serve_until(
            serving,
            listener,
            async {
                received.await.unwrap();
            },
            Duration::from_millis(150),
        )
        .await
        .unwrap();
    });
    let mut events = open_events(&server, &url, &cookie).await;
    let request = reqwest::Client::new()
        .post(format!("{url}/api/settings/test"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", csrf)
        .json(&json!({"role": "investigator"}));
    let slow = tokio::spawn(async move { request.send().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while mock
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.url.path() != "/v1/chat/completions")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    signal.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        server.dashboard.lock().await.jobs[&current.id].status,
        "cancelled"
    );
    assert!(events.chunk().await.unwrap().is_none());
    slow.abort();
}

async fn review_fixture(
    mock: &MockServer,
    report: Value,
) -> (tempfile::TempDir, Arc<Server>, events::Job) {
    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join("review.sh");
    let program = format!(
        "while [ $# -gt 0 ]; do\n\
        if [ \"$1\" = --out ]; then out=$2; shift; fi\nshift\ndone\n\
        cat > \"$out\" <<'REPORT'\n{report}\nREPORT\n"
    );
    std::fs::write(&script, program).unwrap();
    let runner = runner::Runner {
        executable: "/bin/sh".into(),
        prefix: vec![script.display().to_string()],
    };
    let (data, server) = server(mock, runner, true).await;
    // Keep the injectable child beside its workspace for the fixture's lifetime.
    let saved = data.path().join("review.sh");
    std::fs::copy(script, &saved).unwrap();
    let runner = runner::Runner {
        executable: "/bin/sh".into(),
        prefix: vec![saved.display().to_string()],
    };
    let mut server = Arc::try_unwrap(server).ok().unwrap();
    server.runner = runner;
    let server = Arc::new(server);
    let source = data.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-qb", "main"]);
    std::fs::write(source.join("revera.yaml"), CONFIG).unwrap();
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
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "review-token"})))
        .expect(1)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/check-runs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 100,
            "html_url": "https://github.com/owner/repo/runs/100"})))
        .expect(1)
        .mount(mock)
        .await;
    server.enqueue(current.clone()).await;
    (data, server, current)
}

#[tokio::test]
async fn cancellation_during_check_completion_overrides_check_and_seen() {
    let mock = MockServer::start().await;
    let (data, server, current) = review_fixture(
        &mock,
        json!({"status": "complete",
        "findings": [], "plan": {"summary_markdown": "clean"}}),
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/check-runs/100"))
        .and(body_partial_json(json!({"conclusion": "success"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(json!({"id": 100})),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/check-runs/100"))
        .and(body_partial_json(json!({"conclusion": "cancelled"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
        .expect(1)
        .mount(&mock)
        .await;
    server
        .seen
        .lock()
        .await
        .observe(&current.repo, &[pr("previous")]);
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    let worker = server.clone();
    let task = tokio::spawn(async move {
        runner::run(worker, &running, cancel).await;
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while mock
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "PATCH")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let meta = job_metadata(data.path(), &current);
    assert_eq!(meta["check"]["id"], 100);
    assert_eq!(meta["check"]["repo"]["installation"], 9);
    assert_eq!(meta["check"]["repo"]["id"], 1);
    server.close(current.key()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.seen.lock().await.0[&1].heads[&7], "previous");
    assert_eq!(job_metadata(data.path(), &current)["status"], "cancelled");
    let last = mock.received_requests().await.unwrap().pop().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&last.body).unwrap()["conclusion"],
        "cancelled"
    );
}

#[tokio::test]
async fn clean_preserves_index_identity_but_removes_review_state() {
    let mock = MockServer::start().await;
    let (data, server, current) = review_fixture(
        &mock,
        json!({"status": "complete",
        "findings": [], "plan": {"summary_markdown": "clean"}}),
    )
    .await;
    let ws = data.path().join("repos/owner/repo");
    for (path, content) in [
        (".vera/index", "warm vectors"),
        (
            ".revera/vera-cache.json",
            "{\"index_key\":\"warm-identity\"}",
        ),
        (".revera/state.json", "untrusted state"),
        (".revera/other", "stale data"),
    ] {
        let path = ws.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/check-runs/100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
        .expect(1)
        .mount(&mock)
        .await;
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    runner::run(server.clone(), &running, cancel).await;
    assert_eq!(job_metadata(data.path(), &current)["status"], "complete");
    assert_eq!(
        std::fs::read_to_string(ws.join(".revera/vera-cache.json")).unwrap(),
        "{\"index_key\":\"warm-identity\"}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join(".vera/index")).unwrap(),
        "warm vectors"
    );
    assert!(!ws.join(".revera/state.json").exists());
    assert!(!ws.join(".revera/other").exists());
}

#[tokio::test]
async fn rejected_candidates_do_not_count_as_findings() {
    let mock = MockServer::start().await;
    let report = json!({"status": "complete", "stats": {"candidates": 1, "accepted": 0},
        "findings": [{"validation_status": "rejected", "severity": "high"}],
        "plan": {"summary_markdown": "no new findings"}});
    assert_eq!(runner::conclusion(0, Some(&report), false).0, "success");
    assert_eq!(runner::conclusion(0, Some(&report), false).1, "No findings");
    let (data, server, current) = review_fixture(&mock, report).await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/check-runs/100"))
        .and(body_partial_json(json!({"conclusion": "success",
            "output": {"title": "No findings"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
        .expect(1)
        .mount(&mock)
        .await;
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    runner::run(server.clone(), &running, cancel).await;
    assert_eq!(
        job_metadata(data.path(), &current)["findings"],
        json!({"total": 0, "high": 0, "medium": 0, "low": 0})
    );
    let mixed = json!({"status": "complete", "findings": [
        {"validation_status": "accepted", "severity": "low"},
        {"validation_status": "uncertain", "severity": "high"},
        {"severity": "high"}, {"validation_status": "rejected", "severity": "medium"}
    ]});
    assert_eq!(runner::accepted_findings(&mixed).count(), 1);
    assert_eq!(runner::conclusion(0, Some(&mixed), false).1, "1 findings");
}

#[test]
fn repository_cache_homes_are_distinct_and_override_dashboard_env() {
    let secrets = [("XDG_CACHE_HOME".into(), "/shared".into())]
        .into_iter()
        .collect();
    let env = |repo| {
        runner::child_env_with_secrets("GITHUB_TOKEN", "token", Path::new("/data"), repo, &secrets)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let first = env("owner/first");
    let second = env("owner/second");
    assert_eq!(first["XDG_CACHE_HOME"], "/data/cache/owner/first");
    assert_eq!(second["XDG_CACHE_HOME"], "/data/cache/owner/second");
    assert_ne!(first["XDG_CACHE_HOME"], second["XDG_CACHE_HOME"]);
}

#[tokio::test]
async fn restart_recovers_orphan_checks_with_fresh_scoped_tokens() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let mut orphan = dashboard::JobSummary::queued(&job("head"));
    orphan.set_state("running", None);
    orphan.check = Some(dashboard::CheckRecovery {
        id: 100,
        repo: job("head").repo,
        pending: true,
    });
    server.update_job(orphan.clone()).await.unwrap();
    let mut already = orphan.clone();
    already.id = "already-finished".into();
    already.check.as_mut().unwrap().id = 101;
    server.update_job(already.clone()).await.unwrap();
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .and(body_json(json!({"repository_ids": [1],
            "permissions": {"checks": "write", "metadata": "read"}})))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token": "fresh-check-token"})),
        )
        .expect(2)
        .mount(&mock)
        .await;
    for (id, status) in [(100, "in_progress"), (101, "completed")] {
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/check-runs/{id}")))
            .and(header("Authorization", "Bearer fresh-check-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": status})))
            .expect(1)
            .mount(&mock)
            .await;
    }
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/check-runs/100"))
        .and(header("Authorization", "Bearer fresh-check-token"))
        .and(body_partial_json(
            json!({"status": "completed", "conclusion": "cancelled",
            "output": {"summary": "server restarted"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
        .expect(1)
        .mount(&mock)
        .await;
    let restarted = initialize(
        settings(data.path()),
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    for id in [&orphan.id, &already.id] {
        let summary = restarted.dashboard.lock().await.jobs[id].clone();
        assert_eq!(summary.status, "cancelled");
        assert_eq!(summary.reason.as_deref(), Some("server restarted"));
        assert!(!summary.check.as_ref().unwrap().pending);
        let public = serde_json::to_value(&summary).unwrap();
        assert!(public.get("check").is_none());
        let meta: Value = serde_json::from_slice(
            &std::fs::read(data.path().join("jobs").join(id).join("meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["check"]["pending"], false);
    }
    restarted.recover_checks().await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn pruning_protects_queue_members_even_with_cancelled_metadata() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let running = job("head");
    server.enqueue(running.clone()).await;
    let _ = server.queue.lock().await.take().unwrap();
    let pending = events::Job::new(running.repo.clone(), events::Kind::Index, "manual");
    server.enqueue(pending.clone()).await;
    for job in [&running, &pending] {
        let mut summary = dashboard::JobSummary::queued(job);
        summary.queued_at = "1999".into();
        summary.set_state("cancelled", None);
        server.update_job(summary).await.unwrap();
        let dir = dashboard::job_dir(data.path(), &job.id).unwrap();
        crate::fsutil::write_atomic_in(&dir, "report.json", b"{}").unwrap();
    }
    for n in 0..501 {
        let mut summary = dashboard::JobSummary::queued(&job("finished"));
        summary.id = format!("terminal-{n:03}");
        summary.queued_at = format!("2000-{n:03}");
        summary.set_state("complete", None);
        server.update_job(summary).await.unwrap();
    }
    server.prune_jobs().await.unwrap();
    for job in [&running, &pending] {
        let dir = dashboard::job_dir(data.path(), &job.id).unwrap();
        assert!(dir.join("meta.json").is_file());
        assert!(dir.join("report.json").is_file());
        crate::fsutil::write_atomic_in(&dir, "log.txt", b"late finalization").unwrap();
    }
    assert!(!data.path().join("jobs/terminal-000").exists());
    assert_eq!(server.dashboard.lock().await.jobs.len(), 502);
}

#[tokio::test]
async fn successful_inventory_removes_missing_repos_but_failure_does_not() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let repo = job("head").repo;
    server.seen.lock().await.observe(&repo, &[pr("head")]);
    let ws = crate::fsutil::ensure_dir(data.path(), Path::new("repos/owner/repo")).unwrap();
    std::fs::write(ws.join("private-file"), "private checkout").unwrap();
    let failed = Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount_as_scoped(&mock)
        .await;
    assert!(reconcile::once(&server).await.is_err());
    assert!(server.queue.lock().await.pending.is_empty());
    assert!(server.seen.lock().await.0.contains_key(&repo.id));
    assert!(ws.exists());
    drop(failed);
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    let (removal, cancel) = server.queue.lock().await.take().unwrap();
    assert!(matches!(removal.kind, events::Kind::Remove));
    runner::run(server.clone(), &removal, cancel).await;
    assert!(!ws.exists());
    assert!(!server.seen.lock().await.0.contains_key(&repo.id));
    let persisted: Value =
        serde_json::from_slice(&std::fs::read(data.path().join("seen.json")).unwrap()).unwrap();
    assert!(persisted.get("1").is_none());
}

#[tokio::test]
async fn server_rejects_evaluation_only_configuration_for_publication() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    server
        .dashboard
        .lock()
        .await
        .secrets
        .insert("REVIEW_API_KEY".into(), "publication-key".into());
    let (url, task) = http(server.clone()).await;
    let yaml = format!("{CONFIG}review:\n  validate: false\n");
    let response = reqwest::Client::new()
        .put(format!("{url}/api/settings/config"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", csrf)
        .json(&json!({"yaml": yaml}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    assert!(
        response.json::<Value>().await.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("evaluation-only")
    );
    assert_eq!(std::fs::read_to_string(&server.config).unwrap(), CONFIG);
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.installations = 1;
        dashboard
            .health
            .insert("investigator".into(), json!({"ok": true}));
    }
    crate::fsutil::write_output(&server.settings.data_dir, &server.config, yaml.as_bytes())
        .unwrap();
    let session: Value = reqwest::Client::new()
        .get(format!("{url}/api/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["setup"]["stage"], "configure");
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn partial_clone_reads_and_reconcile_use_git_auth_environment() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let source = data.path().join("promisor");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-qb", "main"]);
    git(&source, &["config", "uploadpack.allowFilter", "true"]);
    git(
        &source,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    std::fs::write(source.join("revera.yaml"), CONFIG).unwrap();
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
    let head = crate::git::current_head(&source).await.unwrap();
    let auth = runner::child_env(
        "GITHUB_TOKEN",
        "lazy-fetch-token",
        data.path(),
        "owner/repo",
    );
    let header = auth
        .iter()
        .find(|(name, _)| name == "GIT_CONFIG_VALUE_0")
        .unwrap()
        .1
        .clone();
    let helper = data.path().join("authenticated-upload.sh");
    std::fs::write(
        &helper,
        format!(
            "[ \"$GIT_CONFIG_VALUE_0\" = '{header}' ] || exit 1\n\
        exec git-upload-pack '{}'\n",
            source.display()
        ),
    )
    .unwrap();
    let clone = |directory: &Path| {
        std::fs::create_dir_all(directory).unwrap();
        git(
            directory,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &format!("file://{}", source.display()),
                ".",
            ],
        );
        git(directory, &["config", "protocol.ext.allow", "always"]);
        git(
            directory,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("ext::sh {}", helper.display()),
            ],
        );
    };
    let ws = data.path().join("repos/owner/repo");
    let direct = data.path().join("direct-read");
    clone(&direct);
    clone(&ws);
    // The missing blob really reaches a credential-gated lazy fetch.
    assert!(
        crate::git::read_blob(&direct, &head, "revera.yaml", 1024 * 1024)
            .await
            .is_err()
    );
    let (config_path, config) = runner::config(&server, &direct, &head, &auth)
        .await
        .unwrap();
    assert_eq!(config_path, Path::new("revera.yaml"));
    assert_eq!(config.models.investigator.model, "test-model");
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
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token": "lazy-fetch-token"})),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"repositories": [repository()]})),
        )
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
    assert!(server.dashboard.lock().await.repos["owner/repo"].has_override);
}
