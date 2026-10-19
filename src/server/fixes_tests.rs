use super::super::{Settings, initialize, reconcile, runner};
use super::*;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const COMMIT: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
fn finding() -> Finding {
    serde_json::from_value(json!({"defect_key":"arithmetic","severity":"high","file":"bug.py","start_line":1,"end_line":1,"title":"Wrong sum","claim":"add subtracts","trigger":"add(2, 1)","impact":"wrong result","validation_status":"accepted","validated_fix":"Use addition"})).unwrap()
}
fn pull(head: &str) -> Value {
    json!({"number":7,"title":"PR","state":"open","merged":false,"user":{"login":"author"},"head":{"sha":head,"ref":"topic","repo":{"id":1,"full_name":"owner/repo"}},"base":{"sha":BASE,"ref":"main","repo":{"id":1,"full_name":"owner/repo","private":false}}})
}
async fn fixture() -> (tempfile::TempDir, Arc<Server>, MockServer, String) {
    let mock = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    crate::fsutil::write_secret_atomic(
        data.path(),
        "app.json",
        &serde_json::to_vec(&super::super::tests::test_app()).unwrap(),
    )
    .unwrap();
    crate::fsutil::write_atomic_in(data.path(),"revera.yaml",format!("models:\n  investigator:\n    protocol: openai-chat\n    base_url: {}/v1\n    api_key_env: FIX_TEST_KEY\n    model: fixture\nvera:\n  enabled: false\n",mock.uri()).as_bytes()).unwrap();
    let s = initialize(
        Settings {
            public_url: "https://revera.example.com".into(),
            listen: "127.0.0.1:0".parse().unwrap(),
            data_dir: data.path().into(),
            config: None,
            concurrency: 2,
            reconcile_minutes: 10,
        },
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    let repo = Repo::parse(
        &json!({"id":1,"full_name":"owner/repo","default_branch":"main"}),
        9,
    )
    .unwrap();
    {
        let mut d = s.dashboard.lock().await;
        d.preferences.admins.push("admin".into());
        d.secrets
            .insert("FIX_TEST_KEY".into(), "fixture-model-credential".into());
        d.repos.insert(
            "owner/repo".into(),
            dashboard::RepoInfo {
                repo: repo.clone(),
                private: false,
                html_url: "https://github.com/owner/repo".into(),
                has_override: false,
                open_prs: None,
            },
        );
    }
    s.installations
        .lock()
        .await
        .installations
        .push(reconcile::Installation {
            id: Some(9),
            account: reconcile::Account {
                login: "owner".into(),
                id: Some(1),
                kind: "User".into(),
            },
            repository_selection: Some("all".into()),
            suspended: false,
            permissions: BTreeMap::from([("contents".into(), "write".into())]),
            html_url: String::new(),
            status: "allowed".into(),
            allowed: true,
            repo_count: Some(1),
            error: None,
        });
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token":"fixture-github-credential"})),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull(HEAD)))
        .with_priority(10)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/repos/owner/repo/collaborators/maintainer/permission",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"permission":"write","role_name":"maintain"})),
        )
        .with_priority(10)
        .mount(&mock)
        .await;
    let job = Job::new(
        repo,
        Kind::Review {
            number: 7,
            pr: pull(HEAD),
            force: false,
        },
        "manual",
    );
    let id = job.id.clone();
    let mut summary = dashboard::JobSummary::queued(&job);
    summary.status = "complete".into();
    summary.findings.total = 1;
    summary.check = Some(dashboard::CheckRecovery {
        id: 17,
        repo: job.repo.clone(),
        pending: false,
    });
    s.update_job(summary).await.unwrap();
    write_report(&s, &id, HEAD, "complete", json!([finding()]), json!([]));
    (data, s, mock, id)
}
fn write_report(s: &Server, id: &str, head: &str, state: &str, findings: Value, tracked: Value) {
    let root = dashboard::job_dir(&s.settings.data_dir, id).unwrap();
    crate::fsutil::write_atomic_in(&root,"report.json",&serde_json::to_vec(&json!({"status":state,"head":head,"base":BASE,"findings":findings,"plan":{"state":{"findings":tracked}}})).unwrap()).unwrap();
}
async fn call(
    s: Arc<Server>,
    verb: reqwest::Method,
    url: &str,
    body: Option<Value>,
    csrf: bool,
) -> (StatusCode, Value) {
    let session = auth::Session {
        user: auth::User {
            login: "admin".into(),
            avatar_url: String::new(),
        },
        csrf: "csrf-fixture".into(),
        exp: chrono::Utc::now().timestamp() + 3600,
        kind: "session".into(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = super::super::router(s.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut req = reqwest::Client::new()
        .request(verb, format!("{base}{url}"))
        .header(
            "cookie",
            format!("{}={}", auth::SESSION, auth::sign(&s.session_key, &session)),
        );
    if csrf {
        req = req.header("X-Revera-CSRF", "csrf-fixture");
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let response = req.send().await.unwrap();
    let status = response.status();
    let value = response.json().await.unwrap_or(Value::Null);
    task.abort();
    (status, value)
}
#[tokio::test]
async fn default_requests_converge_without_model_runs_writes_or_settings_mutation() {
    let (_data, s, mock, id) = fixture().await;
    let a = request(&s, "dashboard", "admin", &id, None, false)
        .await
        .unwrap();
    assert_eq!(a.state, "awaiting_confirmation");
    assert!(a.consent.is_none());
    assert!(
        a.events
            .iter()
            .any(|e| e.action == "authorized" && e.detail == "requester permission checked")
    );
    assert!(a.events.iter().all(|e| e.action != "confirmed"));
    let b = request(
        &s,
        "mention",
        "maintainer",
        &id,
        Some(&[finding().id()[..6].into()]),
        false,
    )
    .await
    .unwrap();
    assert_eq!(a.id, b.id);
    assert!(b.events.iter().any(|e| e.action == "authorized"
        && e.detail == "requester permission checked; existing request"));
    assert!(b.events.iter().all(|e| e.action != "confirmed"));
    let payload = json!({"action":"requested_action","requested_action":{"identifier":"revera_fix"},"installation":{"id":9},"repository":{"id":1,"full_name":"owner/repo"},"sender":{"login":"maintainer","type":"User"},"check_run":{"id":17,"name":"Revera","app":{"id":42},"external_id":"7","head_sha":HEAD}});
    check_action(&s, &super::super::tests::test_app(), &payload)
        .await
        .unwrap();
    check_action(&s, &super::super::tests::test_app(), &payload)
        .await
        .unwrap();
    assert_eq!(s.fixes.lock().await.entries.len(), 1);
    let (_, plan) = call(
        s.clone(),
        reqwest::Method::GET,
        &format!("/api/fixes/plan?job={id}"),
        None,
        false,
    )
    .await;
    assert_eq!(plan["existing"]["id"], a.id);
    assert_eq!(plan["consent_required"], true);
    let (status, _) = call(
        s.clone(),
        reqwest::Method::POST,
        &format!("/api/fixes/{}/confirm", a.id),
        Some(json!({"once":true})),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        s.clone(),
        reqwest::Method::POST,
        &format!("/api/fixes/{}/confirm", a.id),
        Some(json!({"once":true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["fix"]["consent"], "once");
    assert_eq!(body["fix"]["state"], "queued");
    assert!(!s.dashboard.lock().await.preferences.fixes.execution);
    let persisted = Store::load(&s.settings.data_dir).unwrap();
    assert_eq!(persisted.entries[&a.id].record.state, "queued");
    assert_eq!(
        persisted.entries[&a.id]
            .record
            .events
            .iter()
            .filter(|e| e.action == "confirmed")
            .count(),
        1
    );
    for req in mock.received_requests().await.unwrap() {
        assert!(!req.url.path().contains("/v1/"));
        assert!(!req.url.path().contains("/git/"));
    }
}
#[test]
fn parser_selection_and_permission_roles_are_strict() {
    use super::super::mentions::{Command, FixCommand, command};
    for body in [
        "`@app fix`",
        "> @app fix",
        "```\n@app fix\n```",
        "    @app fix",
        "~~~\n@app fix\n~~~",
    ] {
        assert!(command(body, "app").is_none(), "{body}");
    }
    assert_eq!(
        command("@app fix all", "app"),
        Some(Command::Fix(FixCommand::Request(None)))
    );
    assert_eq!(
        command("@app fix abcdef 0123456789ab", "app"),
        Some(Command::Fix(FixCommand::Request(Some(vec![
            "abcdef".into(),
            "0123456789ab".into()
        ]))))
    );
    for body in ["@app fix abcde", "@app fix unknown", "@app fix status all"] {
        assert_eq!(command(body, "app"), Some(Command::Help));
    }
    let p = Policy::default();
    assert!(authorized(
        &json!({"permission":"write","role_name":"maintain"}),
        &p
    ));
    assert!(!authorized(
        &json!({"permission":"write","role_name":"write"}),
        &p
    ));
    let mut writers = p;
    writers.requesters = Requesters::Writers;
    assert!(authorized(
        &json!({"permission":"write","role_name":"custom"}),
        &writers
    ));
    assert!(select(&[finding()], Some(&["abcde".into()])).is_err());
    assert!(select(&[finding()], Some(&["abcdef".into()])).is_err());
}

#[tokio::test]
async fn author_previews_do_not_deduplicate_publishable_requests_or_inherit_consent() {
    let (_data, s, mock, job) = fixture().await;
    s.dashboard.lock().await.preferences.fixes.author_preview = true;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/collaborators/author/permission"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"permission":"read"})))
        .mount(&mock)
        .await;
    let preview = request(&s, "mention", "author", &job, None, false)
        .await
        .unwrap();
    assert!(entry(&s, &preview.id).await.unwrap().context.preview_only);
    let (_, plan) = call(
        s.clone(),
        reqwest::Method::GET,
        &format!("/api/fixes/plan?job={job}"),
        None,
        false,
    )
    .await;
    assert!(plan["existing"].is_null());
    let publishable = request(&s, "mention", "maintainer", &job, None, true)
        .await
        .unwrap();
    assert_ne!(preview.id, publishable.id);
    assert!(
        !entry(&s, &publishable.id)
            .await
            .unwrap()
            .context
            .preview_only
    );
    assert_eq!(publishable.consent.as_deref(), Some("once"));
    let repeated = request(&s, "mention", "author", &job, None, false)
        .await
        .unwrap();
    assert_eq!(repeated.id, preview.id);
    assert!(repeated.consent.is_none());
    assert_eq!(repeated.state, "awaiting_confirmation");
    assert_eq!(s.fixes.lock().await.entries.len(), 2);
}

#[tokio::test]
async fn fix_shutdown_persistence_failure_still_cancels_reviews_and_drains_workers() {
    let (_data, s, _mock, job) = fixture().await;
    let fix = request(&s, "dashboard", "admin", &job, None, true)
        .await
        .unwrap();
    change(&s, &fix.id, |e| e.record.state = "generating".into())
        .await
        .unwrap();
    let dir = s.settings.data_dir.join("fixes").join(&fix.id);
    std::fs::remove_file(dir.join("fix.json")).unwrap();
    std::fs::create_dir(dir.join("fix.json")).unwrap();
    let repo = s.dashboard.lock().await.repos["owner/repo"].repo.clone();
    let review = Job::new(
        repo,
        Kind::Review {
            number: 7,
            pr: pull(HEAD),
            force: true,
        },
        "manual",
    );
    s.enqueue(review.clone()).await;
    let (_, mut cancel) = s.queue.lock().await.take().unwrap();
    let (done, received) = tokio::sync::oneshot::channel();
    s.workers.lock().await.spawn(async move {
        cancel.wait_for(|cancel| *cancel).await.unwrap();
        done.send(()).unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let result =
        super::super::serve_until(s.clone(), listener, async {}, Duration::from_millis(100)).await;
    assert!(
        result.is_err(),
        "persistence error is still reported after draining"
    );
    tokio::time::timeout(Duration::from_secs(1), received)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.dashboard.lock().await.jobs[&review.id].status,
        "cancelled"
    );
    assert!(s.workers.lock().await.is_empty());
}
#[tokio::test]
async fn settings_persist_and_repo_yaml_cannot_enable_execution() {
    let (_data, s, _mock, id) = fixture().await;
    let p = Policy {
        execution: true,
        allow_same_branch: true,
        delivery: Delivery::SameBranch,
        ..Default::default()
    };
    let (status, _) = call(
        s.clone(),
        reqwest::Method::PUT,
        "/api/settings/fixes",
        Some(json!({"fixes":p})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        s.clone(),
        reqwest::Method::PUT,
        "/api/repos/owner/repo/fixes",
        Some(json!({"execution":false,"delivery":"patch"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let loaded =
        dashboard::Dashboard::load(&s.settings.data_dir, &reconcile::Seen::default()).unwrap();
    assert!(loaded.preferences.fixes.execution);
    assert_eq!(
        loaded.preferences.repos["owner/repo"]
            .fixes
            .as_ref()
            .unwrap()
            .execution,
        Some(false)
    );
    let ws =
        crate::fsutil::ensure_dir(&s.settings.data_dir, FsPath::new("repos/owner/repo")).unwrap();
    std::fs::write(
        ws.join("revera.yaml"),
        "fixes:\n  execution: true\n  limits:\n    model_requests: 999999\n",
    )
    .unwrap();
    let r = request(&s, "dashboard", "admin", &id, None, false)
        .await
        .unwrap();
    assert_eq!(r.state, "awaiting_confirmation");
    assert!(!policy(&s, "owner/repo").await.execution);
    let mut bad = Policy::default();
    bad.limits.concurrent = 0;
    assert!(bad.validate().is_err());
}

#[tokio::test]
async fn repo_execution_override_precedes_global_for_plan_request_and_confirmation() {
    for global in [false, true] {
        for execution in [None, Some(false), Some(true)] {
            let (_data, s, _mock, job) = fixture().await;
            s.dashboard.lock().await.preferences.fixes.execution = global;
            let (status, _) = call(
                s.clone(),
                reqwest::Method::PUT,
                "/api/repos/owner/repo/fixes",
                Some(json!({"execution":execution})),
                true,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let enabled = execution.unwrap_or(global);
            assert_eq!(policy(&s, "owner/repo").await.execution, enabled);
            let (_, plan) = call(
                s.clone(),
                reqwest::Method::GET,
                &format!("/api/fixes/plan?job={job}"),
                None,
                false,
            )
            .await;
            assert_eq!(plan["execution_enabled"], enabled);
            assert_eq!(plan["consent_required"], !enabled);
            let r = request(&s, "dashboard", "admin", &job, None, false)
                .await
                .unwrap();
            assert_eq!(r.state, "awaiting_confirmation");
            assert_eq!(r.consent.as_deref(), enabled.then_some("persistent"));
            let (status, body) = call(
                s.clone(),
                reqwest::Method::POST,
                &format!("/api/fixes/{}/confirm", r.id),
                Some(json!({"once":false})),
                true,
            )
            .await;
            assert_eq!(
                status,
                if enabled {
                    StatusCode::OK
                } else {
                    StatusCode::CONFLICT
                },
                "global={global}, repo={execution:?}: {body}"
            );
            if enabled {
                assert_eq!(body["fix"]["consent"], "persistent");
                assert_eq!(body["fix"]["state"], "queued");
            } else {
                assert_eq!(
                    entry(&s, &r.id).await.unwrap().record.state,
                    "awaiting_confirmation"
                );
                let (status, body) = call(
                    s.clone(),
                    reqwest::Method::POST,
                    &format!("/api/fixes/{}/confirm", r.id),
                    Some(json!({"once":true})),
                    true,
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(body["fix"]["consent"], "once");
            }
            let loaded =
                dashboard::Dashboard::load(&s.settings.data_dir, &reconcile::Seen::default())
                    .unwrap();
            assert_eq!(loaded.preferences.fixes.execution, global);
            assert_eq!(
                loaded.preferences.repos["owner/repo"]
                    .fixes
                    .as_ref()
                    .unwrap()
                    .execution,
                execution
            );
        }
    }
}
#[tokio::test]
async fn incomplete_reviews_and_unknown_findings_never_create_requests() {
    let (_data, s, _mock, id) = fixture().await;
    for state in ["partial", "failed", "skipped"] {
        s.dashboard.lock().await.jobs.get_mut(&id).unwrap().status = state.into();
        let error = request(&s, "dashboard", "admin", &id, None, true)
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1, "needs a complete review");
    }
    s.dashboard.lock().await.jobs.get_mut(&id).unwrap().status = "complete".into();
    assert!(
        request(
            &s,
            "dashboard",
            "admin",
            &id,
            Some(&["ffffff".into()]),
            true
        )
        .await
        .is_err()
    );
    assert!(s.fixes.lock().await.entries.is_empty());
}
#[tokio::test]
async fn capabilities_show_forks_grants_workflows_external_and_caps() {
    let (_data, s, _mock, id) = fixture().await;
    let r = request(&s, "dashboard", "admin", &id, None, false)
        .await
        .unwrap();
    let mut e = entry(&s, &r.id).await.unwrap();
    let caps = capabilities(&s, &e, Some(&pull(HEAD)), None).await;
    assert!(caps[1].available);
    assert!(!caps[2].available);
    s.dashboard.lock().await.preferences.fixes.allow_same_branch = true;
    assert!(capabilities(&s, &e, Some(&pull(HEAD)), None).await[2].available);
    let mut fork = pull(HEAD);
    fork["head"]["repo"]["full_name"] = json!("contributor/repo");
    let caps = capabilities(&s, &e, Some(&fork), None).await;
    assert!(!caps[1].available && !caps[2].available);
    assert!(caps[2].reason.contains("forks"));
    let grants = BTreeMap::from([("contents".into(), "read".into())]);
    assert!(!capabilities(&s, &e, Some(&pull(HEAD)), Some(&grants)).await[1].available);
    e.record.patch = Some(json!({"files":[{"path":".github/workflows/ci.yml"}]}));
    assert!(!capabilities(&s, &e, Some(&pull(HEAD)), None).await[1].available);
    s.dashboard
        .lock()
        .await
        .preferences
        .fixes
        .allow_workflow_changes = true;
    let grants = BTreeMap::from([
        ("contents".into(), "write".into()),
        ("workflows".into(), "write".into()),
    ]);
    assert!(capabilities(&s, &e, Some(&pull(HEAD)), Some(&grants)).await[1].available);
    e.record.repo_source = Source::External;
    let caps = capabilities(&s, &e, None, None).await;
    assert!(caps[0].available && !caps[1].available && !caps[2].available);
}
#[tokio::test]
async fn cancellation_rate_limits_restart_and_orphan_cleanup() {
    let (_data, s, _mock, id) = fixture().await;
    let r = request(&s, "dashboard", "admin", &id, None, false)
        .await
        .unwrap();
    stop(&s, &r.id, "admin", false).await.unwrap();
    change(&s, &r.id, |e| {
        e.record.transition("ready", None, "revera", "ready")
    })
    .await
    .unwrap();
    assert_eq!(entry(&s, &r.id).await.unwrap().record.state, "cancelled");
    s.dashboard
        .lock()
        .await
        .preferences
        .fixes
        .limits
        .per_user_hour = 1;
    assert!(
        request(&s, "dashboard", "admin", &id, None, false)
            .await
            .unwrap_err()
            .1
            .contains("rate limit")
    );
    for (state, expected) in [
        ("generating", "failed"),
        ("validating", "failed"),
        ("publishing", "publish_failed"),
        ("queued", "queued"),
    ] {
        let mut store = s.fixes.lock().await;
        let mut e = store.entries[&r.id].clone();
        e.record.state = state.into();
        save(&s, &mut store, e).await.unwrap();
        drop(store);
        let workspace = dir(&s.settings.data_dir, &r.id).unwrap().join("workspace");
        std::fs::create_dir_all(workspace.join("generate/out")).unwrap();
        std::fs::write(workspace.join("generate/config.json"), "private").unwrap();
        let loaded = Store::load(&s.settings.data_dir).unwrap();
        assert_eq!(loaded.entries[&r.id].record.state, expected);
        assert!(!workspace.exists());
    }
}
async fn ready(s: &Server, id: &str) -> String {
    let r = request(s, "dashboard", "admin", id, None, true)
        .await
        .unwrap();
    let patch = fix::workspace::Patch {
        files: vec![fix::workspace::FileChange {
            path: "bug.py".into(),
            status: fix::workspace::Status::Modified,
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            binary: false,
            additions: 1,
            deletions: 1,
            size: 6,
            old_content: Some("bug\n".into()),
            new_content: Some("fixed\n".into()),
        }],
        unified: "diff fixture\n".into(),
    };
    let root = dir(&s.settings.data_dir, &r.id).unwrap();
    crate::fsutil::write_atomic_in(&root, "patch.json", &serde_json::to_vec(&patch).unwrap())
        .unwrap();
    crate::fsutil::write_atomic_in(&root, "patch.diff", patch.unified.as_bytes()).unwrap();
    change(s,&r.id,|e|{e.record.tests=Some(json!({"outcome":"passed","output":""}));e.record.verification=Some(json!({"per_finding":[{"id":finding().id(),"verdict":"resolved","rationale":"evidence"}],"new_problems":[]}));e.record.state="ready".into();}).await.unwrap();
    r.id
}
async fn commit_mocks(mock: &MockServer, parent: &str) {
    Mock::given(method("GET"))
        .and(path("/app/installations/9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"id":9,"suspended_at":null,"permissions":{"contents":"write"}}),
            ),
        )
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/owner/repo/git/commits/{parent}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"sha":parent,"tree":{"sha":BASE}})),
        )
        .mount(mock)
        .await;
    for (route, sha) in [
        ("blobs", "cccccccccccccccccccccccccccccccccccccccc"),
        ("trees", "dddddddddddddddddddddddddddddddddddddddd"),
        ("commits", COMMIT),
    ] {
        Mock::given(method("POST"))
            .and(path(format!("/repos/owner/repo/git/{route}")))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha":sha})))
            .expect(1)
            .mount(mock)
            .await;
    }
}
async fn delivery_mocks(
    mock: &MockServer,
    parent: &str,
    mode: Delivery,
    fail: Option<(u16, &str)>,
) {
    commit_mocks(mock, parent).await;
    let branch = if mode == Delivery::SameBranch {
        "topic".into()
    } else {
        format!("revera/fix-{}", &COMMIT[..12])
    };
    let ref_body =
        json!({"ref":format!("refs/heads/{branch}"),"object":{"type":"commit","sha":COMMIT}});
    if mode == Delivery::SameBranch {
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/git/commits/{COMMIT}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"sha":COMMIT,"parents":[{"sha":parent}]})),
            )
            .mount(mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/git/ref/heads/topic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ref":"refs/heads/topic","object":{"type":"commit","sha":parent}}),
            ))
            .mount(mock)
            .await;
        let response = if let Some((code, msg)) = fail {
            ResponseTemplate::new(code).set_body_json(json!({"message":msg}))
        } else {
            ResponseTemplate::new(200).set_body_json(ref_body)
        };
        Mock::given(method("PATCH"))
            .and(path("/repos/owner/repo/git/refs/heads/topic"))
            .and(body_partial_json(json!({"sha":COMMIT,"force":false})))
            .respond_with(response)
            .expect(1)
            .mount(mock)
            .await;
    } else {
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/git/refs"))
            .respond_with(ResponseTemplate::new(201).set_body_json(ref_body))
            .expect(1)
            .mount(mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/pulls"))
            .and(body_partial_json(json!({"base":"topic","draft":true})))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                json!({"number":8,"draft":true,"html_url":"https://github.com/owner/repo/pull/8"}),
            ))
            .expect(1)
            .mount(mock)
            .await;
    }
}
#[tokio::test]
async fn approved_draft_and_same_branch_deliver_once_without_force() {
    for mode in [Delivery::DraftPr, Delivery::SameBranch] {
        let (_data, s, mock, job) = fixture().await;
        {
            let mut d = s.dashboard.lock().await;
            d.preferences.fixes.allow_same_branch = true;
            d.preferences.fixes.rereview = false;
        }
        let id = ready(&s, &job).await;
        delivery_mocks(&mock, HEAD, mode, None).await;
        let (a, b) = tokio::join!(
            publish(&s, &id, "admin", mode),
            publish(&s, &id, "admin", mode)
        );
        assert!(a.is_ok() ^ b.is_ok());
        let record = entry(&s, &id).await.unwrap().record;
        assert_eq!(record.state, "published");
        assert_eq!(record.delivery.unwrap()["commit"], COMMIT);
        assert!(publish(&s, &id, "admin", mode).await.is_err());
    }
}

#[test]
fn delivery_subject_is_one_line_and_truncates_on_character_boundaries() {
    let mut f = finding();
    f.title = "Wrong\r\nsum".into();
    assert_eq!(delivery_subject(&[f.clone()]), "Fix: Wrong  sum");
    f.title = "é".repeat(95);
    assert_eq!(delivery_subject(&[f.clone()]), format!("Fix: {}", f.title));
    f.title = "é".repeat(150);
    let subject = delivery_subject(&[f]);
    assert_eq!(subject, format!("Fix: {}…", "é".repeat(94)));
    assert_eq!(subject.chars().count(), 100);
}

#[test]
fn delivery_subject_sanitizes_again_after_truncating_inline_code() {
    let mut f = finding();
    f.title = format!(
        "`@someone [click](https://example.com) {} `",
        "é".repeat(100)
    );
    let subject = delivery_subject(&[f]);
    assert_eq!(subject.chars().count(), 100);
    assert!(subject.ends_with('…'));
    assert!(subject.contains("@\u{200b}someone"));
    assert!(subject.contains("\\[click\\](h\u{200b}ttps://example.com)"));
    assert!(!subject.contains("@someone"));
    assert!(!subject.contains("https://"));
}

#[tokio::test]
async fn draft_delivery_uses_readable_subjects_and_safe_finding_lists() {
    for case in ["single", "multiple", "untrusted"] {
        let (_data, s, mock, job) = fixture().await;
        s.dashboard.lock().await.preferences.fixes.rereview = false;
        let mut findings = vec![finding()];
        let (subject, titles) = match case {
            "multiple" => {
                let mut second = finding();
                second.defect_key = "another-defect".into();
                second.title = "Missing guard".into();
                second.severity = crate::findings::Severity::Medium;
                findings.push(second);
                ("Fix 2 Revera findings", vec!["Wrong sum", "Missing guard"])
            }
            "untrusted" => {
                findings[0].title = "@someone <!-- [click](https://example.com)\r\nnext".into();
                (
                    "Fix: @\u{200b}someone &lt;!\u{200b}-- \\[click\\](h\u{200b}ttps://example.com)  next",
                    vec![
                        "@\u{200b}someone &lt;!\u{200b}-- \\[click\\](h\u{200b}ttps://example.com)  next",
                    ],
                )
            }
            _ => ("Fix: Wrong sum", vec!["Wrong sum"]),
        };
        write_report(&s, &job, HEAD, "complete", json!(findings), json!([]));
        let id = ready(&s, &job).await;
        delivery_mocks(&mock, HEAD, Delivery::DraftPr, None).await;
        let record = publish(&s, &id, "admin", Delivery::DraftPr).await.unwrap();
        assert_eq!(record.state, "published", "{case}: {:?}", record.reason);
        let requests = mock.received_requests().await.unwrap();
        let commit = requests
            .iter()
            .find(|r| r.method == "POST" && r.url.path().ends_with("/git/commits"))
            .unwrap();
        let commit: Value = serde_json::from_slice(&commit.body).unwrap();
        let list = findings
            .iter()
            .zip(&titles)
            .map(|(f, title)| format!("- {title} ({})", f.id()))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(commit["message"], format!("{subject}\n\n{list}"), "{case}");
        let pr = requests
            .iter()
            .find(|r| r.method == "POST" && r.url.path().ends_with("/pulls"))
            .unwrap();
        let pr: Value = serde_json::from_slice(&pr.body).unwrap();
        assert_eq!(pr["title"], subject, "{case}");
        let list = findings
            .iter()
            .zip(titles)
            .map(|(f, title)| format!("- [{}] {title} (`{}`)", f.severity, f.id()))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            pr["body"],
            format!(
                "Revera fix for #7, approved in the dashboard: https://revera.example.com/fixes/{id}\n\nFindings:\n{list}"
            ),
            "{case}"
        );
    }
}

#[tokio::test]
async fn automatic_delivery_is_only_for_opted_in_available_installed_draft_previews() {
    for case in [
        "default",
        "patch",
        "same_branch",
        "fork",
        "external",
        "author",
        "read_only",
    ] {
        let (_data, s, mock, job) = fixture().await;
        {
            let mut d = s.dashboard.lock().await;
            let p = &mut d.preferences.fixes;
            p.require_approval = case == "default";
            p.delivery = match case {
                "patch" => Delivery::Patch,
                "same_branch" => Delivery::SameBranch,
                _ => Delivery::DraftPr,
            };
            p.allow_same_branch = true;
        }
        let id = ready(&s, &job).await;
        match case {
            "fork" => {
                let mut pr = pull(HEAD);
                pr["head"]["repo"]["full_name"] = json!("contributor/repo");
                Mock::given(method("GET"))
                    .and(path("/repos/owner/repo/pulls/7"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(pr))
                    .mount(&mock)
                    .await;
            }
            "external" => {
                change(&s, &id, |e| {
                    e.record.repo_source = Source::External;
                    e.context.repo.installation = None;
                })
                .await
                .unwrap();
            }
            "author" => {
                change(&s, &id, |e| e.context.preview_only = true)
                    .await
                    .unwrap();
            }
            "read_only" => {
                s.installations.lock().await.installations[0]
                    .permissions
                    .insert("contents".into(), "read".into());
            }
            _ => {}
        }
        assert_eq!(
            auto_publish(&s, &id).await.unwrap().state,
            "ready",
            "{case}"
        );
        assert_eq!(
            auto_publish(&s, &id).await.unwrap().state,
            "ready",
            "{case}"
        );
        assert_eq!(
            publish(&s, &id, "revera", Delivery::DraftPr)
                .await
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        let r = entry(&s, &id).await.unwrap().record;
        assert!(
            r.delivery.is_none() && r.events.iter().all(|e| e.action != "approved"),
            "{case}"
        );
        assert!(
            mock.received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.url.path().contains("/git/")),
            "{case}"
        );
    }
}

#[tokio::test]
async fn automatic_draft_and_concurrent_manual_approvals_share_one_delivery_claim() {
    let (_data, s, mock, job) = fixture().await;
    {
        let mut d = s.dashboard.lock().await;
        d.preferences.fixes.require_approval = false;
        d.preferences.fixes.delivery = Delivery::DraftPr;
        d.preferences.fixes.rereview = false;
    }
    let id = ready(&s, &job).await;
    delivery_mocks(&mock, HEAD, Delivery::DraftPr, None).await;
    let (a, b, manual) = tokio::join!(
        auto_publish(&s, &id),
        auto_publish(&s, &id),
        publish(&s, &id, "admin", Delivery::DraftPr)
    );
    assert!(a.is_ok() && b.is_ok());
    assert!(manual.is_ok() || manual.unwrap_err().0 == StatusCode::CONFLICT);
    assert_eq!(entry(&s, &id).await.unwrap().record.state, "published");
    assert_eq!(auto_publish(&s, &id).await.unwrap().state, "published");
    assert!(publish(&s, &id, "admin", Delivery::DraftPr).await.is_err());
}

#[tokio::test]
async fn automatic_draft_delivery_rechecks_freshness_and_live_contents_grant() {
    for case in ["head", "permission"] {
        let (_data, s, mock, job) = fixture().await;
        {
            let mut d = s.dashboard.lock().await;
            d.preferences.fixes.require_approval = false;
            d.preferences.fixes.delivery = Delivery::DraftPr;
        }
        let id = ready(&s, &job).await;
        if case == "head" {
            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/pulls/7"))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull(BASE)))
                .mount(&mock)
                .await;
        } else {
            Mock::given(method("GET"))
                .and(path("/app/installations/9"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"id":9,"permissions":{"contents":"read"},"suspended_at":null}),
                ))
                .mount(&mock)
                .await;
        }
        let r = auto_publish(&s, &id).await.unwrap();
        assert_eq!(r.state, "publish_failed", "{case}");
        assert!(r.reason.unwrap().contains(if case == "head" {
            "changed since"
        } else {
            "Contents: write"
        }));
        assert!(
            mock.received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.url.path().contains("/git/"))
        );
    }
}
#[tokio::test]
async fn stale_closed_revoked_and_live_permission_changes_block_publish() {
    for case in ["head", "closed", "revoked", "permission"] {
        let (_data, s, mock, job) = fixture().await;
        let id = ready(&s, &job).await;
        match case {
            "head" => {
                Mock::given(method("GET"))
                    .and(path("/repos/owner/repo/pulls/7"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(pull(BASE)))
                    .mount(&mock)
                    .await;
            }
            "closed" => {
                let mut p = pull(HEAD);
                p["state"] = json!("closed");
                Mock::given(method("GET"))
                    .and(path("/repos/owner/repo/pulls/7"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(p))
                    .mount(&mock)
                    .await;
            }
            "revoked" => s.installations.lock().await.installations[0].allowed = false,
            _ => {
                Mock::given(method("GET"))
                    .and(path("/app/installations/9"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(
                        json!({"permissions":{"contents":"read"},"suspended_at":null}),
                    ))
                    .mount(&mock)
                    .await;
            }
        }
        let r = publish(&s, &id, "admin", Delivery::DraftPr).await.unwrap();
        assert_eq!(r.state, "publish_failed", "{case}");
        let reason = r.reason.as_deref().unwrap();
        assert!(
            reason.contains(match case {
                "head" => "changed since",
                "closed" => "PR closed",
                "revoked" => "no longer allowed",
                _ => "Contents: write",
            }),
            "{case}: {reason}"
        );
        assert!(
            mock.received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.url.path().contains("/git/"))
        );
    }
}
#[tokio::test]
async fn protected_branches_fail_honestly_without_retry() {
    let (_data, s, mock, job) = fixture().await;
    s.dashboard.lock().await.preferences.fixes.allow_same_branch = true;
    let id = ready(&s, &job).await;
    delivery_mocks(
        &mock,
        HEAD,
        Delivery::SameBranch,
        Some((403, "Protected branch update failed")),
    )
    .await;
    let r = publish(&s, &id, "admin", Delivery::SameBranch)
        .await
        .unwrap();
    assert_eq!(r.state, "publish_failed");
    assert!(r.reason.unwrap().contains("protect"));
}

#[tokio::test]
async fn approval_head_race_after_commit_creation_never_mutates_a_ref() {
    for mode in [Delivery::DraftPr, Delivery::SameBranch] {
        let (_data, s, mock, job) = fixture().await;
        s.dashboard.lock().await.preferences.fixes.allow_same_branch = true;
        let id = ready(&s, &job).await;
        commit_mocks(&mock, HEAD).await;
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = reads.clone();
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/7"))
            .respond_with(move |_: &wiremock::Request| {
                let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(pull(if n == 0 { HEAD } else { BASE }))
            })
            .expect(2)
            .mount(&mock)
            .await;
        let r = publish(&s, &id, "admin", mode).await.unwrap();
        assert_eq!(r.state, "publish_failed");
        assert!(r.reason.unwrap().contains("changed since"));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(publish(&s, &id, "admin", mode).await.is_err());
        let requests = mock.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r.method == "POST" && r.url.path().ends_with("/git/commits"))
        );
        assert!(requests.iter().all(|r| !r.url.path().contains("/git/ref")
            && !(r.method == "POST" && r.url.path().ends_with("/pulls"))));
    }
}

fn fix_comment(body: &str, id: u64, actor: &str) -> Value {
    json!({"action":"created","installation":{"id":9},"repository":{"id":1,"full_name":"owner/repo","default_branch":"main"},"issue":{"number":7,"pull_request":{}},"comment":{"id":id,"body":body,"author_association":"NONE","user":{"login":actor,"type":"User"}}})
}
async fn invoke_fix(s: &Server, payload: &Value) {
    let app = super::super::tests::test_app();
    let command =
        super::super::mentions::event(payload, &app.slug, &s.dashboard.lock().await.preferences)
            .unwrap()
            .unwrap();
    super::super::mentions::handle(s, &app, command)
        .await
        .unwrap();
}
#[tokio::test]
async fn fix_mentions_reply_or_react_once_with_durable_claims_and_live_authorization() {
    let (_data, s, mock, job) = fixture().await;
    s.dashboard
        .lock()
        .await
        .preferences
        .fixes
        .limits
        .per_user_hour = 4;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":44})))
        .expect(3)
        .mount(&mock)
        .await;
    let payload = fix_comment("@revera-test fix", 33, "maintainer");
    futures::future::join_all((0..4).map(|_| invoke_fix(&s, &payload))).await;
    let r = s
        .fixes
        .lock()
        .await
        .entries
        .values()
        .next()
        .unwrap()
        .record
        .clone();
    assert_eq!(r.state, "awaiting_confirmation");
    let requests = mock.received_requests().await.unwrap().len();
    *s.mentions.lock().await = super::super::mentions::Recent::load(&s.settings.data_dir).unwrap();
    invoke_fix(&s, &payload).await;
    assert_eq!(mock.received_requests().await.unwrap().len(), requests);
    invoke_fix(
        &s,
        &fix_comment(
            &format!("@revera-test fix {}", &finding().id()[..6]),
            34,
            "maintainer",
        ),
    )
    .await;
    invoke_fix(
        &s,
        &fix_comment("@revera-test fix status", 35, "maintainer"),
    )
    .await;
    assert_eq!(s.fixes.lock().await.entries.len(), 1);
    assert_eq!(entry(&s, &r.id).await.unwrap().record.review_job, job);
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/comments/36/reactions"))
        .and(body_partial_json(json!({"content":"eyes"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":45})))
        .expect(1)
        .mount(&mock)
        .await;
    let cancel = fix_comment("@revera-test fix cancel", 36, "maintainer");
    invoke_fix(&s, &cancel).await;
    invoke_fix(&s, &cancel).await;
    assert_eq!(entry(&s, &r.id).await.unwrap().record.state, "cancelled");
    let requests = mock.received_requests().await.unwrap();
    for request in requests
        .iter()
        .filter(|r| r.method == "POST" && r.url.path().ends_with("/issues/7/comments"))
    {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body["body"],
            format!(
                "This fix request is awaiting confirmation. [Open it in the dashboard](https://revera.example.com/fixes/{}) to review and confirm it.",
                r.id
            )
        );
    }
    assert!(
        requests
            .iter()
            .all(|r| !r.url.path().contains("/v1/") && !r.url.path().contains("/git/"))
    );
}

#[tokio::test]
async fn fix_status_mentions_use_readable_state_names() {
    for (state, readable) in [("ready", "ready"), ("publish_failed", "publish failed")] {
        let (_data, s, mock, job) = fixture().await;
        let r = request(&s, "dashboard", "admin", &job, None, false)
            .await
            .unwrap();
        change(&s, &r.id, |e| e.record.state = state.into())
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/issues/7/comments"))
            .and(body_partial_json(json!({"body":format!(
                "Fix request status: {readable}. [Open it in the dashboard](https://revera.example.com/fixes/{}).",
                r.id
            )})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":44})))
            .expect(1)
            .mount(&mock)
            .await;
        invoke_fix(
            &s,
            &fix_comment("@revera-test fix status", 35, "maintainer"),
        )
        .await;
    }
}

#[tokio::test]
async fn fix_mentions_explain_missing_reviews_and_denials_without_trusting_associations() {
    for missing in [true, false] {
        let (_data, s, mock, job) = fixture().await;
        if missing {
            s.dashboard.lock().await.jobs.get_mut(&job).unwrap().status = "partial".into();
        } else {
            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/collaborators/outsider/permission"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"permission":"write","role_name":"write"})),
                )
                .expect(1)
                .mount(&mock)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/issues/7/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":44})))
            .expect(1)
            .mount(&mock)
            .await;
        let mut payload = fix_comment("@revera-test fix", 33, "outsider");
        payload["comment"]["author_association"] = json!("OWNER");
        invoke_fix(&s, &payload).await;
        invoke_fix(&s, &payload).await;
        let requests = mock.received_requests().await.unwrap();
        let reply = requests
            .iter()
            .find(|r| r.method == "POST" && r.url.path().ends_with("/issues/7/comments"))
            .unwrap();
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        assert!(body["body"].as_str().unwrap().contains(if missing {
            "Ask for a review first"
        } else {
            "not authorized"
        }));
        assert!(s.queue.lock().await.pending.is_empty());
        let store = s.fixes.lock().await;
        if missing {
            assert!(store.entries.is_empty());
        } else {
            let r = &store.entries.values().next().unwrap().record;
            assert_eq!(r.state, "failed");
            assert!(r.consent.is_none());
            assert!(r.events.iter().any(|e| e.action == "permission_denied"));
        }
    }
}

#[tokio::test]
async fn denied_commands_do_not_charge_repo_quota_and_reply_only_once_per_user_hour() {
    let (_data, s, mock, job) = fixture().await;
    s.dashboard
        .lock()
        .await
        .preferences
        .fixes
        .limits
        .per_repo_day = 1;
    for actor in ["outsider", "another"] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/owner/repo/collaborators/{actor}/permission"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"permission":"read","role_name":"read"})),
            )
            .mount(&mock)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":44})))
        .expect(1)
        .mount(&mock)
        .await;
    invoke_fix(&s, &fix_comment("@revera-test fix", 50, "outsider")).await;
    invoke_fix(&s, &fix_comment("@revera-test fix", 51, "outsider")).await;
    *s.fixes.lock().await = Store::load(&s.settings.data_dir).unwrap();
    invoke_fix(&s, &fix_comment("@revera-test fix", 52, "outsider")).await;
    assert_eq!(
        request(&s, "mention", "another", &job, None, false)
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert!(s.fixes.lock().await.admissions.is_empty());
    let r = request(&s, "dashboard", "admin", &job, None, true)
        .await
        .unwrap();
    assert_eq!(r.state, "queued");
    assert_eq!(s.fixes.lock().await.admissions.len(), 1);
    assert_eq!(s.fixes.lock().await.denials.len(), 2);
}

#[tokio::test]
async fn duplicate_attempts_are_limited_before_dedup_and_audit_is_bounded_and_coalesced() {
    let (_data, s, mock, job) = fixture().await;
    s.dashboard
        .lock()
        .await
        .preferences
        .fixes
        .limits
        .per_user_hour = 2;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":44})))
        .expect(2)
        .mount(&mock)
        .await;
    for comment in 50..55 {
        invoke_fix(&s, &fix_comment("@revera-test fix", comment, "maintainer")).await;
    }
    let mut r = s
        .fixes
        .lock()
        .await
        .entries
        .values()
        .next()
        .unwrap()
        .record
        .clone();
    assert_eq!(s.fixes.lock().await.entries.len(), 1);
    let requested = r
        .events
        .iter()
        .filter(|e| e.action == "requested")
        .collect::<Vec<_>>();
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[0].count, 2);
    assert!(requested[0].last_at.is_some());
    *s.fixes.lock().await = Store::load(&s.settings.data_dir).unwrap();
    assert_eq!(
        request(&s, "mention", "maintainer", &job, None, false)
            .await
            .unwrap_err()
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    r.audit("admin", "approved", "draft_pr");
    r.audit("revera", "published", "published");
    for i in 0..500 {
        r.audit("revera", "generated", &format!("progress {i}"));
    }
    assert_eq!(r.events.len(), 200);
    for action in ["requested", "approved", "published"] {
        assert!(r.events.iter().any(|e| e.action == action));
    }
    let mut e = entry(&s, &r.id).await.unwrap();
    e.record = r;
    e.record
        .events
        .extend(vec![e.record.events.last().unwrap().clone(); 300]);
    persist(&s.settings.data_dir, &e).unwrap();
    assert_eq!(
        Store::load(&s.settings.data_dir).unwrap().entries[&e.record.id]
            .record
            .events
            .len(),
        200
    );
}

#[tokio::test]
async fn external_preview_requires_opt_in_uses_public_reads_and_rejects_publish() {
    let (_data, s, mock, job) = fixture().await;
    s.dashboard.lock().await.jobs.get_mut(&job).unwrap().source = Source::External;
    let denied = request(&s, "dashboard", "admin", &job, None, true)
        .await
        .unwrap_err();
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    assert!(mock.received_requests().await.unwrap().is_empty());
    s.dashboard.lock().await.preferences.fixes.external = true;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":1,"full_name":"owner/repo","private":false,"size":10,"default_branch":"main"})))
        .mount(&mock).await;
    let id = ready(&s, &job).await;
    let (status, detail) = call(
        s.clone(),
        reqwest::Method::GET,
        &format!("/api/fixes/{id}"),
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["fix"]["repo_source"], "external");
    assert_eq!(detail["patch"], "diff fixture\n");
    assert_eq!(detail["delivery"][0]["available"], true);
    assert_eq!(detail["delivery"][1]["available"], false);
    assert_eq!(detail["actions"]["approve"], false);
    let (status, error) = call(
        s.clone(),
        reqwest::Method::POST,
        &format!("/api/fixes/{id}/approve"),
        Some(json!({"delivery":"draft_pr"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        error["error"],
        "Revera cannot publish to a repository where it is not installed"
    );
    assert_eq!(entry(&s, &id).await.unwrap().record.state, "ready");
    for r in mock.received_requests().await.unwrap() {
        assert_eq!(r.method, "GET");
        assert!(!r.headers.contains_key("authorization"));
        assert!(!r.url.path().contains("/app/"));
    }
}

fn git(path: &FsPath, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(path)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
async fn worker_checkout(s: &Server, job_id: &str) -> (PathBuf, String) {
    let checkout =
        crate::fsutil::ensure_dir(&s.settings.data_dir, FsPath::new("repos/owner/repo")).unwrap();
    git(&checkout, &["init", "-q"]);
    std::fs::write(
        checkout.join("bug.py"),
        "def add(a, b):\n    return a - b\n",
    )
    .unwrap();
    std::fs::write(
        checkout.join("test_bug.py"),
        "from bug import add\nassert add(2, 1) == 3\n",
    )
    .unwrap();
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-qm", "fixture"]);
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    s.dashboard
        .lock()
        .await
        .jobs
        .get_mut(job_id)
        .unwrap()
        .head_sha = Some(head.clone());
    write_report(s, job_id, &head, "complete", json!([finding()]), json!([]));
    (checkout, head)
}

#[tokio::test]
async fn sandbox_empty_patch_retries_exhaust_attempts_and_preserve_honest_report() {
    if std::env::var("REVERA_TEST_SANDBOX").as_deref() != Ok("1") {
        return;
    }
    assert!(fix::sandbox::probe().await.available);
    let (_data, s, mock, job) = fixture().await;
    let (checkout, head) = worker_checkout(&s, &job).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull(&head)))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"fixture","choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"no-fix","type":"function","function":{"name":"submit_fix","arguments":json!({"summary":"No safe fix","per_finding":[{"id":finding().id(),"disposition":"not_fixed","note":"No patch"}]}).to_string()}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":10}})))
        .expect(2).mount(&mock).await;
    let r = request(&s, "dashboard", "admin", &job, None, true)
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(false);
    run(s.clone(), &r.id, rx).await;
    let failed = entry(&s, &r.id).await.unwrap().record;
    assert_eq!(failed.state, "validation_failed", "{}", json!(failed));
    assert_eq!(failed.attempt, 2);
    assert_eq!(failed.usage.as_ref().unwrap()["requests"], 2);
    assert_eq!(failed.reason.as_deref(), Some("no changes"));
    assert_eq!(
        failed.generation.as_ref().unwrap()["per_finding"][0]["disposition"],
        "not_fixed"
    );
    assert!(failed.verification.is_none() && failed.tests.is_none() && failed.delivery.is_none());
    assert!(!regenerable(&failed));
    let root = dir(&s.settings.data_dir, &r.id).unwrap();
    assert!(!root.join("workspace").exists() && !root.join("patch.diff").exists());
    assert_eq!(git(&checkout, &["status", "--porcelain"]), "");
    let (status, _) = call(
        s.clone(),
        reqwest::Method::POST,
        &format!("/api/fixes/{}/regenerate", r.id),
        Some(json!({"note":"try again"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        mock.received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("/git/"))
    );
}

#[tokio::test]
async fn execution_rechecks_requester_or_confirming_admin_and_keeps_author_previews_private() {
    let (_data, s, mock, job) = fixture().await;
    let r = request(&s, "mention", "maintainer", &job, None, true)
        .await
        .unwrap();
    Mock::given(method("GET"))
        .and(path(
            "/repos/owner/repo/collaborators/maintainer/permission",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"permission":"read","role_name":"read"})),
        )
        .mount(&mock)
        .await;
    let (_tx, rx) = watch::channel(false);
    run(s.clone(), &r.id, rx).await;
    let denied = entry(&s, &r.id).await.unwrap().record;
    assert_eq!(denied.state, "failed");
    assert_eq!(denied.attempt, 0);
    assert!(
        denied
            .events
            .iter()
            .any(|e| e.action == "permission_denied")
    );
    assert!(
        mock.received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("/v1/"))
    );

    s.dashboard.lock().await.preferences.fixes.author_preview = true;
    let mut pr = pull(HEAD);
    pr["user"]["login"] = json!("maintainer");
    authorize_execution(&s, &r.id, &pr).await.unwrap();
    assert!(entry(&s, &r.id).await.unwrap().context.preview_only);
    change(&s, &r.id, |e| {
        e.record.state = "awaiting_confirmation".into()
    })
    .await
    .unwrap();
    let (status, _) = call(
        s.clone(),
        reqwest::Method::POST,
        &format!("/api/fixes/{}/confirm", r.id),
        Some(json!({"once":true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    *s.fixes.lock().await = Store::load(&s.settings.data_dir).unwrap();
    let e = entry(&s, &r.id).await.unwrap();
    assert_eq!(e.context.confirmed_by.as_deref(), Some("admin"));
    authorize_execution(&s, &r.id, &pr).await.unwrap();
    assert!(
        !capabilities(&s, &entry(&s, &r.id).await.unwrap(), Some(&pr), None).await[1].available
    );
    s.dashboard.lock().await.preferences.admins.clear();
    assert!(authorize_execution(&s, &r.id, &pr).await.is_err());
}

#[tokio::test]
async fn sandbox_retry_rechecks_live_requester_permission_before_more_paid_work() {
    if std::env::var("REVERA_TEST_SANDBOX").as_deref() != Ok("1") {
        return;
    }
    let (_data, s, mock, job) = fixture().await;
    let (_, head) = worker_checkout(&s, &job).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull(&head)))
        .mount(&mock)
        .await;
    let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = revoked.clone();
    Mock::given(method("GET"))
        .and(path(
            "/repos/owner/repo/collaborators/maintainer/permission",
        ))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    json!({"permission":"read","role_name":"read"})
                } else {
                    json!({"permission":"write","role_name":"maintain"})
                },
            )
        })
        .mount(&mock)
        .await;
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(move|_:&wiremock::Request|{
            revoked.store(true,std::sync::atomic::Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"no-fix","type":"function","function":{"name":"submit_fix","arguments":json!({"summary":"No patch","per_finding":[{"id":finding().id(),"disposition":"not_fixed","note":"No patch"}]}).to_string()}}]},"finish_reason":"tool_calls"}]}))
        }).expect(1).mount(&mock).await;
    let r = request(&s, "mention", "maintainer", &job, None, true)
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(false);
    run(s.clone(), &r.id, rx).await;
    let r = entry(&s, &r.id).await.unwrap().record;
    assert_eq!(r.state, "failed");
    assert_eq!(r.attempt, 1);
    assert!(r.reason.unwrap().contains("not authorized"));
}

fn completion(name: &str, args: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"fixture-call","type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":10}})
}

#[tokio::test]
async fn sandbox_ready_automatically_publishes_opted_in_draft_once_with_controller_audit() {
    if std::env::var("REVERA_TEST_SANDBOX").as_deref() != Ok("1") {
        return;
    }
    let (_data, s, mock, job) = fixture().await;
    let (_, head) = worker_checkout(&s, &job).await;
    {
        let mut d = s.dashboard.lock().await;
        d.preferences.fixes.require_approval = false;
        d.preferences.fixes.delivery = Delivery::DraftPr;
        d.preferences.fixes.rereview = false;
    }
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull(&head)))
        .mount(&mock)
        .await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(move|request:&wiremock::Request|{
            let body:Value=serde_json::from_slice(&request.body).unwrap();
            let verifier=body["tools"].as_array().unwrap().iter().any(|t|t["function"]["name"]=="submit_fix_verdicts");
            let n=count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            let (name,args)=if verifier {("submit_fix_verdicts",json!({"per_finding":[{"id":finding().id(),"verdict":"resolved","rationale":"addition repaired"}],"new_problems":[]}))}
                else if n==0 {("write_file",json!({"path":"bug.py","content":"def add(a,b):\n    return a+b\n"}))}
                else {("submit_fix",json!({"summary":"Repair addition","per_finding":[{"id":finding().id(),"disposition":"fixed","note":"Use addition"}]}))};
            ResponseTemplate::new(200).set_body_json(completion(name,args))
        }).mount(&mock).await;
    delivery_mocks(&mock, &head, Delivery::DraftPr, None).await;
    let r = request(&s, "dashboard", "admin", &job, None, true)
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(false);
    run(s.clone(), &r.id, rx).await;
    let r = entry(&s, &r.id).await.unwrap().record;
    assert_eq!(r.state, "published", "{}", json!(r));
    assert_eq!(r.delivery.as_ref().unwrap()["mode"], "draft_pr");
    assert!(r.events.iter().any(|e| e.action == "ready"));
    assert!(
        r.events.iter().any(|e| e.actor == "revera"
            && e.action == "published"
            && e.detail == AUTO_DRAFT_DETAIL)
    );
    assert_eq!(auto_publish(&s, &r.id).await.unwrap().state, "published");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(
        !dir(&s.settings.data_dir, &r.id)
            .unwrap()
            .join("workspace")
            .exists()
    );
}

#[tokio::test]
async fn sandbox_overall_deadline_cancels_generation_tests_and_cumulative_retries() {
    if std::env::var("REVERA_TEST_SANDBOX").as_deref() != Ok("1") {
        return;
    }
    assert!(seconds_left(tokio::time::Instant::now() - Duration::from_millis(1)).is_err());
    for stage in ["generation", "tests", "retries"] {
        let (_data, s, mock, job) = fixture().await;
        let (_, head) = worker_checkout(&s, &job).await;
        {
            let mut d = s.dashboard.lock().await;
            d.preferences.fixes.limits.seconds = 1;
            d.preferences.fixes.limits.attempts = 3;
            d.preferences.fixes.test_command = vec![
                "/usr/bin/python3".into(),
                "-c".into(),
                "import time; time.sleep(30)".into(),
            ];
        }
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull(&head)))
            .mount(&mock)
            .await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        Mock::given(method("POST")).and(path("/v1/chat/completions"))
            .respond_with(move|request:&wiremock::Request|{
                let body:Value=serde_json::from_slice(&request.body).unwrap();
                assert!(!body["tools"].as_array().unwrap().iter().any(|t|t["function"]["name"]=="submit_fix_verdicts"),"verifier started after deadline in {stage}");
                let n=count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let (name,args)=if stage!="retries" && n==0 { ("write_file",json!({"path":"bug.py","content":"def add(a,b):\n    return a+b\n"})) }
                    else { ("submit_fix",json!({"summary":"Fixture","per_finding":[{"id":finding().id(),"disposition":if stage=="retries" {"not_fixed"} else {"fixed"},"note":"fixture"}]})) };
                let response=ResponseTemplate::new(200).set_body_json(completion(name,args));
                if stage=="generation" { response.set_delay(Duration::from_secs(3)) }
                else if stage=="retries" { response.set_delay(Duration::from_millis(600)) }
                else {response}
            }).mount(&mock).await;
        let r = request(&s, "dashboard", "admin", &job, None, true)
            .await
            .unwrap();
        let (_tx, rx) = watch::channel(false);
        let started = std::time::Instant::now();
        run(s.clone(), &r.id, rx).await;
        assert!(started.elapsed() < Duration::from_secs(2), "{stage}");
        let r = entry(&s, &r.id).await.unwrap().record;
        assert_eq!(r.state, "failed", "{stage}: {}", json!(r));
        assert_eq!(r.reason.as_deref(), Some("time limit reached"), "{stage}");
        assert!(
            r.verification.is_none() && r.tests.is_none() && r.delivery.is_none(),
            "{stage}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            if stage == "generation" { 1 } else { 2 },
            "{stage}"
        );
        assert_eq!(r.attempt, if stage == "retries" { 2 } else { 1 });
        assert!(
            !dir(&s.settings.data_dir, &r.id)
                .unwrap()
                .join("workspace")
                .exists()
        );
    }
}

#[tokio::test]
async fn after_review_requires_persistent_execution_and_never_loops_from_followups() {
    let (_data, s, _mock, id) = fixture().await;
    let repo = s.dashboard.lock().await.repos["owner/repo"].repo.clone();
    let mut job = Job::new(
        repo,
        Kind::Review {
            number: 7,
            pr: pull(HEAD),
            force: false,
        },
        "manual",
    );
    job.id = id;
    s.dashboard.lock().await.preferences.fixes.trigger = Trigger::AfterReview;
    review_finished(&s, &job).await.unwrap();
    assert!(s.fixes.lock().await.entries.is_empty());
    {
        let mut d = s.dashboard.lock().await;
        d.preferences.fixes.execution = true;
        d.preferences.fixes.trigger = Trigger::Manual;
    }
    review_finished(&s, &job).await.unwrap();
    assert!(s.fixes.lock().await.entries.is_empty());
    s.dashboard.lock().await.preferences.fixes.trigger = Trigger::AfterReview;
    job.trigger = "fix".into();
    review_finished(&s, &job).await.unwrap();
    assert!(s.fixes.lock().await.entries.is_empty());
    job.trigger = "manual".into();
    s.dashboard
        .lock()
        .await
        .jobs
        .get_mut(&job.id)
        .unwrap()
        .status = "partial".into();
    review_finished(&s, &job).await.unwrap();
    assert!(s.fixes.lock().await.entries.is_empty());
    s.dashboard
        .lock()
        .await
        .jobs
        .get_mut(&job.id)
        .unwrap()
        .status = "complete".into();
    review_finished(&s, &job).await.unwrap();
    review_finished(&s, &job).await.unwrap();
    let store = s.fixes.lock().await;
    assert_eq!(store.entries.len(), 1);
    let r = &store.entries.values().next().unwrap().record;
    assert_eq!(r.source, "auto");
    assert_eq!(r.consent.as_deref(), Some("persistent"));
    assert_eq!(r.state, "queued");
    assert!(r.delivery.is_none() && r.generation.is_none());
}

#[tokio::test]
async fn recovered_queued_fixes_wait_for_successful_installation_discovery() {
    let (_data, s, mock, job) = fixture().await;
    let r = request(&s, "dashboard", "admin", &job, None, true)
        .await
        .unwrap();
    *s.fixes.lock().await = Store::load(&s.settings.data_dir).unwrap();
    *s.installations.lock().await = reconcile::Installations::default();
    let scheduler = tokio::spawn(dispatch(s.clone()));
    let mut events = s.events.subscribe();
    s.fix_notify.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), events.recv())
            .await
            .is_err()
    );
    assert_eq!(entry(&s, &r.id).await.unwrap().record.state, "queued");
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    assert!(
        reconcile::discover(&s, &super::super::tests::test_app())
            .await
            .is_err()
    );
    s.fix_notify.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), events.recv())
            .await
            .is_err()
    );
    assert_eq!(entry(&s, &r.id).await.unwrap().record.state, "queued");
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"permissions":{"contents":"read"},"owner":{"type":"User"}})),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET")).and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id":9,"account":{"login":"owner","id":1,"type":"User"},"permissions":{"contents":"write"},"suspended_at":null}]))).mount(&mock).await;
    reconcile::discover(&s, &super::super::tests::test_app())
        .await
        .unwrap();
    // After discovery the request proceeds past the installation check. It then
    // stops at the sandbox probe (hosts without bubblewrap) or the missing checkout.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = entry(&s, &r.id).await.unwrap().record;
            if matches!(r.state.as_str(), "failed" | "unsupported") {
                break r;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    let reason = ended.reason.unwrap();
    match ended.state.as_str() {
        "failed" => assert!(reason.contains("checkout is unavailable"), "{reason}"),
        "unsupported" => assert!(!reason.trim().is_empty(), "missing sandbox diagnostic"),
        state => panic!("unexpected terminal state: {state}: {reason}"),
    }
    scheduler.abort();
    let _ = scheduler.await;
}

#[tokio::test]
async fn retention_keeps_latest_200_and_durable_rate_limits_survive_eviction() {
    let (_data, s, _mock, job) = fixture().await;
    let id = ready(&s, &job).await;
    let mut store = s.fixes.lock().await;
    let mut template = store.entries[&id].clone();
    template.record.state = "discarded".into();
    let now = chrono::Utc::now();
    template.record.created_at = (now - chrono::Duration::seconds(2000)).to_rfc3339();
    save(&s, &mut store, template.clone()).await.unwrap();
    for i in 1..=201 {
        let mut e = template.clone();
        e.record.id = format!("{i:032x}");
        e.record.created_at = (now - chrono::Duration::seconds(1000 - i)).to_rfc3339();
        save(&s, &mut store, e).await.unwrap();
    }
    store.prune(&s.settings.data_dir).unwrap();
    assert_eq!(store.entries.len(), 200);
    assert!(!store.entries.contains_key(&id));
    assert!(!s.settings.data_dir.join("fixes").join(&id).exists());
    assert!(
        !s.settings
            .data_dir
            .join("fixes")
            .join(format!("{:032x}", 1))
            .exists()
    );
    assert!(store.entries.contains_key(&format!("{:032x}", 201)));
    *store = Store::load(&s.settings.data_dir).unwrap();
    assert_eq!(store.entries.len(), 200);
    assert_eq!(store.admissions.len(), 1);
    drop(store);
    s.dashboard
        .lock()
        .await
        .preferences
        .fixes
        .limits
        .per_user_hour = 1;
    assert!(
        request(&s, "dashboard", "admin", &job, None, true)
            .await
            .unwrap_err()
            .1
            .contains("rate limit")
    );
}

async fn linked_followup(s: &Server, job: &str, mode: Delivery) -> (String, Job) {
    let id = ready(s, job).await;
    let repo = s.dashboard.lock().await.repos["owner/repo"].repo.clone();
    let follow = Job::new(
        repo,
        Kind::Review {
            number: 7,
            pr: pull(COMMIT),
            force: true,
        },
        "fix",
    );
    let mut summary = dashboard::JobSummary::queued(&follow);
    summary.status = "complete".into();
    s.update_job(summary).await.unwrap();
    change(s, &id, |e| {
        e.record.state = "verifying".into();
        e.record.delivery = Some(json!({"mode":mode,"commit":COMMIT,"pr":7}));
        e.record.verify_review = Some(json!({"job":follow.id,"state":"queued","resolved":[],"still_open":[],"new_findings":0,"note":""}));
    }).await.unwrap();
    (id, follow)
}

#[tokio::test]
async fn incomplete_followups_never_claim_verified_or_resolved() {
    for case in [
        "partial",
        "failed",
        "partial_report",
        "wrong_head",
        "missing_report",
        "malformed_report",
        "restart",
    ] {
        let (_data, s, _mock, job) = fixture().await;
        let (id, follow) = linked_followup(&s, &job, Delivery::SameBranch).await;
        match case {
            "partial" | "failed" => {
                s.dashboard
                    .lock()
                    .await
                    .jobs
                    .get_mut(&follow.id)
                    .unwrap()
                    .status = case.into()
            }
            "partial_report" => {
                write_report(&s, &follow.id, COMMIT, "partial", json!([]), json!([]))
            }
            "wrong_head" => write_report(&s, &follow.id, HEAD, "complete", json!([]), json!([])),
            "malformed_report" => {
                let root = dashboard::job_dir(&s.settings.data_dir, &follow.id).unwrap();
                crate::fsutil::write_atomic_in(&root, "report.json", b"{").unwrap();
            }
            "restart" => *s.fixes.lock().await = Store::load(&s.settings.data_dir).unwrap(),
            _ => {}
        }
        if case != "restart" {
            review_finished(&s, &follow).await.unwrap();
        }
        let r = entry(&s, &id).await.unwrap().record;
        assert_eq!(r.state, "published", "{case}");
        let expected = if matches!(case, "partial" | "failed") {
            "follow-up review did not complete"
        } else {
            "follow-up review incomplete"
        };
        assert_eq!(r.reason.as_deref(), Some(expected), "{case}");
        let v = r.verify_review.unwrap();
        assert_eq!(v["note"], expected, "{case}");
        assert_eq!(v["resolved"], json!([]), "{case}");
    }
}

#[tokio::test]
async fn queued_followup_terminal_transitions_notify_controller_without_a_runner() {
    for case in ["cancel", "supersede", "remove", "failed", "skipped"] {
        let (_data, s, _mock, job) = fixture().await;
        let (id, follow) = linked_followup(&s, &job, Delivery::SameBranch).await;
        s.enqueue(follow.clone()).await;
        match case {
            "cancel" => s.close(follow.key()).await.unwrap(),
            "supersede" => {
                let mut replacement = Job::new(follow.repo.clone(), follow.kind.clone(), "manual");
                replacement.id = "replacement".into();
                s.enqueue(replacement).await;
            }
            "remove" => s.remove_repo(follow.repo.clone()).await.unwrap(),
            state => s
                .transition(&follow, state, Some("fixture termination".into()))
                .await
                .unwrap(),
        }
        let r = entry(&s, &id).await.unwrap().record;
        assert_eq!(r.state, "published", "{case}");
        let mut events = s.events.subscribe();
        review_finished(&s, &follow).await.unwrap();
        assert_eq!(
            entry(&s, &id).await.unwrap().record.events.len(),
            r.events.len(),
            "{case}"
        );
        assert!(
            events.try_recv().is_err(),
            "duplicate status event for {case}"
        );
        let v = r.verify_review.unwrap();
        assert_eq!(v["note"], "follow-up review did not complete", "{case}");
        assert_eq!(v["resolved"], json!([]));
        assert_eq!(
            Store::load(&s.settings.data_dir).unwrap().entries[&id]
                .record
                .state,
            "published"
        );
    }
}

fn managed_summary(s: &Server, head: &str) -> String {
    let state = crate::state::ReviewState {
        reviewed_head: head.into(),
        ..Default::default()
    };
    let yaml = s.config_yaml().unwrap().unwrap();
    let marker = crate::config::Config::parse(&yaml, "fixture")
        .unwrap()
        .github
        .summary_marker;
    format!(
        "{marker}\nReview prose\n{}",
        crate::github::publish::encode_state(&state)
    )
}
#[tokio::test]
async fn status_rereads_managed_state_and_preserves_concurrent_review_prose() {
    for changed_state in [false, true] {
        let (_data, s, mock, job) = fixture().await;
        let id = ready(&s, &job).await;
        let old = managed_summary(&s, HEAD);
        let new = format!(
            "{}\nNew CLI prose",
            managed_summary(&s, if changed_state { BASE } else { HEAD })
        );
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/issues/7/comments"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!([{"id":44,"user":{"login":"revera-test[bot]"},"body":old}]),
                ),
            )
            .expect(1)
            .with_priority(1)
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/issues/comments/44"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":44,"body":new})))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/repos/owner/repo/issues/comments/44"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(if changed_state { 0 } else { 1 })
            .mount(&mock)
            .await;
        update_status(&s, &entry(&s, &id).await.unwrap().record)
            .await
            .unwrap();
        for request in mock
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method == "PATCH")
        {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body["body"].as_str().unwrap().contains("New CLI prose"));
            assert!(body["body"].as_str().unwrap().contains("Fix: **ready**"));
        }
    }
}

#[tokio::test]
async fn status_defers_for_queued_running_reviews_and_reapplies_latest_on_completion() {
    let (_data, s, mock, job) = fixture().await;
    let r = request(&s, "dashboard", "admin", &job, None, false)
        .await
        .unwrap();
    let repo = s.dashboard.lock().await.repos["owner/repo"].repo.clone();
    let review = Job::new(
        repo,
        Kind::Review {
            number: 7,
            pr: pull(HEAD),
            force: true,
        },
        "manual",
    );
    s.enqueue(review.clone()).await;
    let before = mock.received_requests().await.unwrap().len();
    update_status(&s, &r).await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), before);
    s.queue.lock().await.take().unwrap();
    change(&s, &r.id, |e| e.record.state = "ready".into())
        .await
        .unwrap();
    update_status(&s, &r).await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), before);
    let body = managed_summary(&s, HEAD);
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id":44,"user":{"login":"revera-test[bot]"},"body":body}])),
        )
        .expect(1)
        .with_priority(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/comments/44"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":44,"body":body})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/issues/comments/44"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&mock)
        .await;
    s.transition(&review, "complete", None).await.unwrap();
    s.queue.lock().await.running.remove(&review.key());
    refresh_review_status(&s, &review).await;
    let requests = mock.received_requests().await.unwrap();
    let patch = requests.iter().find(|r| r.method == "PATCH").unwrap();
    let body: Value = serde_json::from_slice(&patch.body).unwrap();
    assert!(body["body"].as_str().unwrap().contains("Fix: **ready**"));
    assert!(
        !body["body"]
            .as_str()
            .unwrap()
            .contains("awaiting_confirmation")
    );
}

#[tokio::test]
async fn followup_resolution_requires_state_or_independent_verifier_evidence() {
    for case in [
        "open",
        "uncertain",
        "missing_state",
        "empty_state",
        "omitted_id",
        "resolved",
        "rejected",
        "draft_unresolved",
        "draft_resolved",
    ] {
        let (_data, s, _mock, job) = fixture().await;
        let mode = if case.starts_with("draft") {
            Delivery::DraftPr
        } else {
            Delivery::SameBranch
        };
        let (id, follow) = linked_followup(&s, &job, mode).await;
        let tracked = if matches!(case, "open" | "uncertain" | "resolved" | "rejected") {
            json!([{"id":finding().id(),"status":case}])
        } else if case == "empty_state" {
            json!([])
        } else if case == "omitted_id" {
            json!([{"id":"other-finding","status":"resolved"}])
        } else {
            Value::Null
        };
        if case == "draft_unresolved" {
            change(&s,&id,|e| e.record.verification = Some(json!({"per_finding":[{"id":finding().id(),"verdict":"unresolved"}],"new_problems":[]}))).await.unwrap();
        }
        write_report(&s, &follow.id, COMMIT, "complete", json!([]), tracked);
        review_finished(&s, &follow).await.unwrap();
        let r = entry(&s, &id).await.unwrap().record;
        assert_eq!(r.state, "verified");
        let v = r.verify_review.unwrap();
        if matches!(case, "draft_resolved" | "resolved") {
            assert_eq!(v["resolved"], json!([finding().id()]));
        } else {
            assert_eq!(v["resolved"], json!([]), "{case}");
            assert_eq!(v["still_open"], json!([finding().id()]));
        }
        if case == "missing_state" {
            assert!(
                v["note"]
                    .as_str()
                    .unwrap()
                    .contains("no resolution inferred")
            );
        }
        if case.starts_with("draft") {
            assert!(
                v["note"]
                    .as_str()
                    .unwrap()
                    .contains("pre-publish independent verifier")
            );
        }
    }
}
#[tokio::test]
async fn sandbox_full_worker_generate_tests_verify_approve_deliver_followup() {
    if std::env::var("REVERA_TEST_SANDBOX").as_deref() != Ok("1") {
        return;
    }
    assert!(fix::sandbox::probe().await.available);
    let (_data, s, mock, job_id) = fixture().await;
    let (checkout, head) = worker_checkout(&s, &job_id).await;
    {
        let mut d = s.dashboard.lock().await;
        d.jobs.get_mut(&job_id).unwrap().head_sha = Some(head.clone());
        d.preferences.fixes.test_command = vec!["/usr/bin/python3".into(), "test_bug.py".into()];
        d.preferences.fixes.allow_same_branch = true;
    }
    write_report(
        &s,
        &job_id,
        &head,
        "complete",
        json!([finding()]),
        json!([]),
    );
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull(&head)))
        .mount(&mock)
        .await;
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = count.clone();
    let id = finding().id();
    Mock::given(method("POST")).and(path("/v1/chat/completions")).respond_with(move|r:&wiremock::Request|{
        let body:Value=serde_json::from_slice(&r.body).unwrap();assert!(r.headers.get("authorization").unwrap().to_str().unwrap().contains("fixture-model-credential"));assert!(!String::from_utf8_lossy(&r.body).contains("fixture-github-credential"));
        let verifier=body["tools"].as_array().unwrap().iter().any(|t|t["function"]["name"]=="submit_fix_verdicts");let n=calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let (name,args)=if verifier{("submit_fix_verdicts",json!({"per_finding":[{"id":id,"verdict":"resolved","rationale":"bug.py now returns a+b; add(2,1) is 3"}],"new_problems":[]}))}else if n==0{("write_file",json!({"path":"bug.py","content":"def add(a, b):\n    return a + b\n"}))}else{("submit_fix",json!({"summary":"Repair addition","per_finding":[{"id":id,"disposition":"fixed","note":"Use addition"}]}))};
        ResponseTemplate::new(200).set_body_json(json!({"id":"fixture","choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":format!("call-{n}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":10}}))
    }).mount(&mock).await;
    let r = request(&s, "dashboard", "admin", &job_id, None, true)
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(false);
    run(s.clone(), &r.id, rx).await;
    let ready = entry(&s, &r.id).await.unwrap().record;
    assert_eq!(ready.state, "ready", "{}", json!(ready));
    assert_eq!(ready.tests.as_ref().unwrap()["outcome"], "passed");
    assert!(
        mock.received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("/git/")),
        "default approval policy wrote before manual approval"
    );
    assert!(count.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    assert!(
        !dir(&s.settings.data_dir, &r.id)
            .unwrap()
            .join("workspace")
            .exists()
    );
    delivery_mocks(&mock, &head, Delivery::DraftPr, None).await;
    let mut fixed = pull(COMMIT);
    fixed["number"] = json!(8);
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixed))
        .mount(&mock)
        .await;
    let published = publish(&s, &r.id, "admin", Delivery::DraftPr)
        .await
        .unwrap();
    assert_eq!(published.state, "verifying");
    let follow = published.verify_review.as_ref().unwrap()["job"]
        .as_str()
        .unwrap()
        .to_owned();
    let queued = s
        .queue
        .lock()
        .await
        .pending
        .values()
        .find(|j| j.id == follow)
        .unwrap()
        .clone();
    assert_eq!(queued.head(), COMMIT);
    assert_eq!(queued.trigger, "fix");
    assert!(matches!(queued.kind, Kind::Review { force: true, .. }));
    {
        let mut d = s.dashboard.lock().await;
        d.jobs.get_mut(&follow).unwrap().status = "complete".into();
    }
    write_report(&s, &follow, COMMIT, "complete", json!([]), json!([]));
    review_finished(&s, &queued).await.unwrap();
    let verified = entry(&s, &r.id).await.unwrap().record;
    assert_eq!(verified.state, "verified");
    assert_eq!(
        verified.verify_review.unwrap()["resolved"],
        json!([finding().id()])
    );
    assert_eq!(
        git(&checkout, &["show", "HEAD:bug.py"]),
        "def add(a, b):\n    return a - b"
    );
}
