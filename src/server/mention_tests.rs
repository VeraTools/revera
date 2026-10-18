use super::*;
use dashboard::{Mode, Preferences, RepoPreference, Trigger};
use mentions::Command;

fn comment(body: &str) -> Value {
    json!({"action": "created", "installation": {"id": 9}, "repository": repository(),
        "issue": {"number": 7, "pull_request": {"url": "ignored"}},
        "comment": {"id": 33, "body": body, "author_association": "OWNER",
            "user": {"login": "maintainer", "type": "User"}}})
}

#[test]
fn mention_commands_require_boundaries_and_ignore_markdown_code_and_quotes() {
    for (body, expected) in [
        ("@revera-test review", Command::Review(None)),
        (
            "Please @REVERA-TEST quick",
            Command::Review(Some(Mode::Quick)),
        ),
        ("(@Revera-Test deep", Command::Review(Some(Mode::Deep))),
        (
            "@revera-test review standard",
            Command::Review(Some(Mode::Standard)),
        ),
        (
            "@revera-test review quick",
            Command::Review(Some(Mode::Quick)),
        ),
        (
            "@revera-test review deep",
            Command::Review(Some(Mode::Deep)),
        ),
        ("@revera-test review this, please.", Command::Review(None)),
        ("@revera-test review.", Command::Review(None)),
        ("@revera-test deep!", Command::Review(Some(Mode::Deep))),
        (
            "@revera-test review deep, thanks",
            Command::Review(Some(Mode::Deep)),
        ),
        ("@revera-test help", Command::Help),
        ("@revera-test nonsense", Command::Help),
        ("@revera-test", Command::Help),
        (
            "> @revera-test deep\n@revera-test quick",
            Command::Review(Some(Mode::Quick)),
        ),
        (
            "`@revera-test deep` @revera-test review",
            Command::Review(None),
        ),
    ] {
        assert_eq!(
            mentions::command(body, "revera-test"),
            Some(expected),
            "{body}"
        );
    }
    for body in [
        "@revera-test-other review",
        "email@revera-test review",
        "@revera-test_foo quick",
        "@revera-tester review",
        "é@revera-test quick",
        "> @revera-test review",
        "  > @revera-test quick",
        "```rust\n@revera-test deep\n```",
        "~~~~\n@revera-test review\n~~~~",
        "    @revera-test review",
        "\t@revera-test deep",
        "`@revera-test review`",
        "``@revera-test deep``",
        "```\n@revera-test deep",
    ] {
        assert_eq!(mentions::command(body, "revera-test"), None, "{body}");
    }
}

#[test]
fn mention_events_gate_associations_bots_edits_and_non_pr_comments() {
    let preferences = Preferences::default();
    for association in ["OWNER", "MEMBER", "COLLABORATOR"] {
        let mut value = comment("@revera-test review");
        value["comment"]["author_association"] = json!(association);
        assert!(
            mentions::event(&value, "revera-test", &preferences)
                .unwrap()
                .is_some()
        );
    }
    for association in [
        "NONE",
        "CONTRIBUTOR",
        "FIRST_TIMER",
        "FIRST_TIME_CONTRIBUTOR",
        "",
    ] {
        let mut value = comment("@revera-test review");
        value["comment"]["author_association"] = json!(association);
        assert!(
            mentions::event(&value, "revera-test", &preferences)
                .unwrap()
                .is_none()
        );
    }
    for (field, value) in [
        ("action", json!("edited")),
        ("action", json!("deleted")),
        ("issue", json!({"number": 7})),
        (
            "comment",
            json!({"id": 33, "body": "@revera-test review", "author_association": "OWNER",
            "user": {"login": "robot", "type": "Bot"}}),
        ),
        (
            "comment",
            json!({"id": 33, "body": "@revera-test review", "author_association": "OWNER",
            "user": {"login": "Revera-Test[bot]", "type": "User"}}),
        ),
        (
            "comment",
            json!({"id": 33, "body": "@revera-test review", "author_association": "OWNER",
            "user": {"login": "other[bot]", "type": "User"}}),
        ),
    ] {
        let mut payload = comment("@revera-test review");
        payload[field] = value;
        assert!(
            mentions::event(&payload, "revera-test", &preferences)
                .unwrap()
                .is_none()
        );
    }
    let off = Preferences {
        default_trigger: Trigger::Off,
        ..Default::default()
    };
    assert!(
        mentions::event(&comment("@revera-test help"), "revera-test", &off)
            .unwrap()
            .is_none()
    );
}

async fn mention_token(mock: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .and(body_json(json!({"repository_ids": [1], "permissions": {
            "issues": "write", "pull_requests": "read", "metadata": "read"
        }})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "mention-token"})))
        .expect(1)
        .mount(mock)
        .await;
}

async fn invoke(server: &Server, payload: Value) {
    let request = mentions::event(
        &payload,
        &test_app().slug,
        &server.dashboard.lock().await.preferences,
    )
    .unwrap()
    .unwrap();
    mentions::handle(server, &test_app(), request)
        .await
        .unwrap();
}

#[tokio::test]
async fn mentions_acknowledge_then_force_current_head_and_ignore_auto_filters() {
    for (text, expected) in [
        ("review", Mode::Deep),
        ("quick", Mode::Quick),
        ("deep", Mode::Deep),
        ("review standard", Mode::Standard),
        ("review quick", Mode::Quick),
    ] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        {
            let mut dashboard = server.dashboard.lock().await;
            dashboard.preferences.default_trigger = Trigger::Mention;
            dashboard.preferences.default_mode = Mode::Deep;
        }
        mention_token(&mock).await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/issues/comments/33/reactions"))
            .and(body_json(json!({"content": "eyes"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 44})))
            .expect(1)
            .mount(&mock)
            .await;
        let mut current = pr("current-head-not-webhook-head");
        current["draft"] = json!(true);
        current["user"]["login"] = json!("dependabot[bot]");
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(current))
            .expect(1)
            .mount(&mock)
            .await;
        let (url, task) = http(server.clone()).await;
        let bytes = serde_json::to_vec(&comment(&format!("@revera-test {text}"))).unwrap();
        assert_eq!(
            reqwest::Client::new()
                .post(format!("{url}/webhook"))
                .header("X-GitHub-Event", "issue_comment")
                .header(
                    "X-Hub-Signature-256",
                    signature(&test_app().webhook_secret, &bytes)
                )
                .body(bytes)
                .send()
                .await
                .unwrap()
                .status(),
            202
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), server.notify.notified())
            .await
            .unwrap();
        let queue = server.queue.lock().await;
        let review = &queue.pending[&(1, 7)];
        assert_eq!(review.head(), "current-head-not-webhook-head");
        assert_eq!(review.mode, expected);
        assert_eq!(review.trigger, "mention");
        assert!(matches!(
            review.kind,
            events::Kind::Review { force: true, .. }
        ));
        let meta = job_metadata(data.path(), review);
        assert_eq!(meta["mode"], json!(expected));
        assert_eq!(meta["trigger"], "mention");
        let requests = mock.received_requests().await.unwrap();
        assert!(requests[1].url.path().ends_with("/reactions"));
        assert_eq!(requests[2].url.path(), "/repos/owner/repo/pulls/7");
        task.abort();
    }
}

#[tokio::test]
async fn help_and_unknown_commands_reply_once_without_reaction_or_review() {
    for text in ["help", "unknown", "summarize this"] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        mention_token(&mock).await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/issues/7/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 44})))
            .expect(1)
            .mount(&mock)
            .await;
        invoke(
            &server,
            comment(&format!("@revera-test {text}\n@revera-test help")),
        )
        .await;
        assert!(server.queue.lock().await.pending.is_empty());
        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        for command in ["review", "quick", "standard", "deep", "help"] {
            assert!(body["body"].as_str().unwrap().contains(command));
        }
    }
}

#[tokio::test]
async fn closed_pr_mentions_reply_instead_of_reviewing() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    mention_token(&mock).await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/comments/33/reactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 44})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"state": "closed"})))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .and(body_json(
            json!({"body": "This pull request is closed; no review was queued."}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 44})))
        .expect(1)
        .mount(&mock)
        .await;
    invoke(&server, comment("@revera-test review")).await;
    assert!(server.queue.lock().await.pending.is_empty());
}

#[tokio::test]
async fn webhook_owner_admission_runs_before_mention_api_work() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (url, task) = http(server.clone()).await;
    let mut payload = comment("@revera-test review");
    payload["repository"]["owner"]["login"] = json!("outside");
    payload["repository"]["full_name"] = json!("outside/repo");
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{url}/webhook"))
            .header("X-GitHub-Event", "issue_comment")
            .header(
                "X-Hub-Signature-256",
                signature(&test_app().webhook_secret, &bytes)
            )
            .body(bytes)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert!(mock.received_requests().await.unwrap().is_empty());
    assert!(server.queue.lock().await.pending.is_empty());
    task.abort();
}

#[test]
fn triggers_gate_webhooks_and_reconcile_but_keep_mention_indexes_warm() {
    for trigger in [Trigger::Auto, Trigger::Mention, Trigger::Off] {
        let prefs = Preferences {
            default_trigger: trigger,
            default_mode: Mode::Quick,
            ..Default::default()
        };
        let mut payload = json!({"installation": {"id": 9}, "repository": repository(),
            "number": 7, "action": "synchronize", "pull_request": pr("new"),
            "ref": "refs/heads/main"});
        let review = events::map_with_preferences("pull_request", &payload, 42, &prefs).unwrap();
        assert_eq!(
            matches!(review, events::Event::Job(_)),
            trigger == Trigger::Auto
        );
        let index = events::map_with_preferences("push", &payload, 42, &prefs).unwrap();
        assert_eq!(
            matches!(index, events::Event::Job(_)),
            trigger != Trigger::Off
        );
        let mut seen = reconcile::Seen::default();
        let repo = job("").repo;
        seen.observe_with_preferences(&repo, &[pr("old")], &prefs, &Default::default(), false);
        let mut changed = repo.clone();
        changed.pushed_at = "new-push".into();
        let jobs = seen.observe_with_preferences(
            &changed,
            &[pr("new")],
            &prefs,
            &Default::default(),
            false,
        );
        assert_eq!(
            jobs.iter().any(|j| matches!(j.kind, events::Kind::Index)),
            trigger != Trigger::Off
        );
        assert_eq!(
            jobs.iter()
                .any(|j| matches!(j.kind, events::Kind::Review { .. })),
            trigger == Trigger::Auto
        );
        if trigger == Trigger::Auto {
            let auto = jobs
                .iter()
                .find(|j| matches!(j.kind, events::Kind::Review { .. }))
                .unwrap();
            assert_eq!(auto.trigger, "auto");
            assert_eq!(auto.mode, Mode::Quick);
        }
        payload["action"] = json!("closed");
        assert!(matches!(
            events::map_with_preferences("pull_request", &payload, 42, &prefs).unwrap(),
            events::Event::Close(1, 7)
        ));
    }
}

#[test]
fn old_settings_migrate_to_mentions_and_explicit_off_without_legacy_output_fields() {
    for (legacy, expected) in [
        (json!({}), Trigger::Mention),
        (json!({"default_enabled": true}), Trigger::Mention),
        (json!({"default_enabled": false}), Trigger::Off),
    ] {
        let data = tempfile::tempdir().unwrap();
        crate::fsutil::ensure_dir(data.path(), Path::new("jobs")).unwrap();
        let mut value = legacy;
        value["repos"] =
            json!({"owner/off": {"enabled": false}, "owner/inherit": {"enabled": true}});
        crate::fsutil::write_atomic_in(
            data.path(),
            "settings.json",
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let dashboard =
            dashboard::Dashboard::load(data.path(), &reconcile::Seen::default()).unwrap();
        assert_eq!(dashboard.preferences.default_trigger, expected);
        assert_eq!(dashboard.preferences.default_mode, Mode::Standard);
        assert_eq!(
            dashboard.preferences.repos["owner/off"].trigger,
            Some(Trigger::Off)
        );
        assert_eq!(dashboard.preferences.repos["owner/inherit"].trigger, None);
        assert_eq!(dashboard.preferences.trigger("owner/inherit"), expected);
        let saved = std::fs::read_to_string(data.path().join("settings.json")).unwrap();
        assert!(!saved.contains("enabled"));
    }
    let fresh = tempfile::tempdir().unwrap();
    crate::fsutil::ensure_dir(fresh.path(), Path::new("jobs")).unwrap();
    let dashboard = dashboard::Dashboard::load(fresh.path(), &reconcile::Seen::default()).unwrap();
    assert_eq!(dashboard.preferences.default_trigger, Trigger::Mention);
    assert_eq!(dashboard.preferences.default_mode, Mode::Standard);
}

fn add_repo(dashboard: &mut dashboard::Dashboard) {
    dashboard.repos.insert(
        "owner/repo".into(),
        dashboard::RepoInfo {
            repo: job("").repo,
            private: false,
            html_url: String::new(),
            has_override: false,
        },
    );
}

#[tokio::test]
async fn repository_settings_api_distinguishes_inheritance_omission_and_legacy_enabled() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.preferences.default_trigger = Trigger::Mention;
        add_repo(&mut dashboard);
    }
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    for (patch, trigger, mode, trigger_override, mode_override) in [
        (
            json!({"mode": "deep"}),
            "mention",
            "deep",
            Value::Null,
            json!("deep"),
        ),
        (
            json!({"trigger": "auto"}),
            "auto",
            "deep",
            json!("auto"),
            json!("deep"),
        ),
        (
            json!({"mode": null}),
            "auto",
            "standard",
            json!("auto"),
            Value::Null,
        ),
        (
            json!({"trigger": null}),
            "mention",
            "standard",
            Value::Null,
            Value::Null,
        ),
        (
            json!({"enabled": false}),
            "off",
            "standard",
            json!("off"),
            Value::Null,
        ),
        (
            json!({"enabled": true}),
            "mention",
            "standard",
            Value::Null,
            Value::Null,
        ),
    ] {
        assert_eq!(
            client
                .patch(format!("{url}/api/repos/owner/repo"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&patch)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let repos: Value = client
            .get(format!("{url}/api/repos"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(repos[0]["trigger"], trigger);
        assert_eq!(repos[0]["mode"], mode);
        assert_eq!(repos[0]["trigger_override"], trigger_override);
        assert_eq!(repos[0]["mode_override"], mode_override);
    }
    let mut prefs: Value = client
        .get(format!("{url}/api/settings/server"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prefs["default_trigger"], "mention");
    assert!(prefs.get("default_enabled").is_none());
    prefs["default_trigger"] = json!("auto");
    prefs["default_mode"] = json!("quick");
    assert_eq!(
        client
            .put(format!("{url}/api/settings/server"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&prefs)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        server.dashboard.lock().await.preferences.mode("owner/repo"),
        Mode::Quick
    );
    for patch in [json!({"mode": "extreme"}), json!({"trigger": "enabled"})] {
        assert_eq!(
            client
                .patch(format!("{url}/api/repos/owner/repo"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&patch)
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    task.abort();
}

#[tokio::test]
async fn manual_and_rerun_api_accept_modes_and_off_rejects_new_work() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        add_repo(&mut dashboard);
        dashboard.preferences.repos.insert(
            "owner/repo".into(),
            RepoPreference {
                trigger: Some(Trigger::Mention),
                mode: Some(Mode::Quick),
            },
        );
    }
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "manual-token"})))
        .expect(3)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr("current")))
        .expect(3)
        .mount(&mock)
        .await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let mut id = String::new();
    for (mode, expected, rerun) in [
        (Some("deep"), Mode::Deep, false),
        (Some("standard"), Mode::Standard, true),
        (None, Mode::Quick, false),
    ] {
        let route = if rerun {
            format!("/api/jobs/{id}/rerun")
        } else {
            "/api/repos/owner/repo/pulls/7/review".into()
        };
        let mut request = client
            .post(format!("{url}{route}"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf);
        if let Some(mode) = mode {
            request = request.json(&json!({"mode": mode}));
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        id = body["id"].as_str().unwrap().into();
        let queue = server.queue.lock().await;
        assert_eq!(queue.pending[&(1, 7)].mode, expected);
        assert_eq!(
            queue.pending[&(1, 7)].trigger,
            if rerun { "rerun" } else { "manual" }
        );
    }
    server
        .dashboard
        .lock()
        .await
        .preferences
        .repos
        .get_mut("owner/repo")
        .unwrap()
        .trigger = Some(Trigger::Off);
    for route in [
        "/api/repos/owner/repo/pulls/7/review".into(),
        "/api/repos/owner/repo/index".into(),
        format!("/api/jobs/{id}/rerun"),
    ] {
        assert_eq!(
            client
                .post(format!("{url}{route}"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
    }
    assert_eq!(server.queue.lock().await.pending[&(1, 7)].id, id);
    task.abort();
}

#[tokio::test]
async fn overview_exposes_subscription_and_organization_app_settings_url() {
    for subscribed in [false, true] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let (cookie, _) = administrator(&server).await;
        Mock::given(method("GET"))
            .and(path("/app"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": if subscribed { vec!["issue_comment"] } else { vec!["pull_request"] },
                "owner": {"type": "Organization"}
            })))
            .expect(1)
            .mount(&mock)
            .await;
        let (url, task) = http(server).await;
        let body: Value = reqwest::Client::new()
            .get(format!("{url}/api/overview"))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["app"]["issue_comment_subscribed"], subscribed);
        assert_eq!(
            body["app"]["settings_url"],
            "https://github.com/organizations/owner/settings/apps/revera-test/permissions"
        );
        task.abort();
    }
}

#[test]
fn builtin_profiles_apply_without_changing_strategy_validation_or_index_identity() {
    let standard = crate::config::Config::parse(CONFIG, "test").unwrap();
    let mut quick = standard.clone();
    quick.apply_profile("quick").unwrap();
    let mut deep = standard.clone();
    deep.apply_profile("deep").unwrap();
    assert_eq!(
        (
            quick.budget.agent_max_tool_calls,
            quick.budget.agent_max_seconds,
            quick.budget.run_max_seconds
        ),
        (12, 150, 420)
    );
    assert_eq!(
        (
            deep.budget.agent_max_tool_calls,
            deep.budget.agent_max_seconds,
            deep.budget.run_max_seconds
        ),
        (50, 600, 1800)
    );
    assert_eq!(
        deep.review.max_diff_bytes,
        2 * standard.review.max_diff_bytes
    );
    assert_eq!(deep.review.max_findings, 20);
    for config in [&quick, &deep] {
        assert_eq!(config.review.strategy, standard.review.strategy);
        assert!(config.review.validate);
        assert_eq!(config.vera.index_identity(), standard.vera.index_identity());
    }
    for strategy in [
        crate::config::Strategy::Panel,
        crate::config::Strategy::Delegated,
    ] {
        for profile in ["quick", "deep"] {
            let mut config = standard.clone();
            config.review.strategy = strategy;
            config.apply_profile(profile).unwrap();
            assert_eq!(config.review.strategy, strategy);
            assert!(config.review.validate);
        }
    }
    let fingerprints: Vec<_> = [&standard, &quick, &deep]
        .into_iter()
        .map(|c| c.review_fingerprint("baseline"))
        .collect();
    assert_ne!(fingerprints[0], fingerprints[1]);
    assert_ne!(fingerprints[1], fingerprints[2]);
    assert_ne!(fingerprints[0], fingerprints[2]);
    let mut custom = crate::config::Config::parse(
        &format!(
            "{CONFIG}profiles:\n  quick:\n    budget: \
        {{agent_max_tool_calls: 77}}\n  deep: {{}}\n"
        ),
        "test",
    )
    .unwrap();
    custom.apply_profile("quick").unwrap();
    assert_eq!(custom.budget.agent_max_tool_calls, 77);
    assert_eq!(
        custom.budget.agent_max_seconds,
        standard.budget.agent_max_seconds
    );
    custom.apply_profile("deep").unwrap();
    assert_eq!(custom.review.max_diff_bytes, standard.review.max_diff_bytes);
    assert_eq!(custom.review.max_findings, standard.review.max_findings);
    let mut user_quick = crate::config::Config::parse(
        &format!(
            "{CONFIG}profiles:\n  quick:\n    budget: \
        {{agent_max_tool_calls: 12, agent_max_seconds: 150, run_max_seconds: 420}}\n"
        ),
        "test",
    )
    .unwrap();
    user_quick.apply_profile("quick").unwrap();
    assert_eq!(user_quick.review_fingerprint("baseline"), fingerprints[1]);
}
