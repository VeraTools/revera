use super::*;
use dashboard::{FindingCounts, JobSummary, Mode, RepoInfo, Trigger};

fn limits(config: &crate::config::Config) -> (u32, u64, u32, u64) {
    (
        config.budget.agent_max_tool_calls,
        config.budget.agent_max_seconds,
        config.budget.run_max_requests,
        config.budget.run_max_seconds,
    )
}

#[test]
fn builtin_profiles_scale_raised_budgets() {
    let standard = crate::config::Config::parse(
        &format!(
            "{CONFIG}budget: {{agent_max_tool_calls: 50, agent_max_seconds: 900, \
             run_max_requests: 300, run_max_seconds: 2400, retries: 7}}\n"
        ),
        "test",
    )
    .unwrap();
    let mut quick = standard.clone();
    quick.apply_profile("quick").unwrap();
    assert_eq!(limits(&quick), (25, 450, 300, 1200));
    assert_eq!(quick.review.max_diff_bytes, standard.review.max_diff_bytes);
    assert_eq!(quick.review.max_findings, standard.review.max_findings);
    let mut deep = standard.clone();
    deep.apply_profile("deep").unwrap();
    assert_eq!(limits(&deep), (100, 1800, 600, 3000));
    assert_eq!(deep.review.max_diff_bytes, 400000);
    assert_eq!(deep.review.max_findings, 20);
    for config in [quick, deep] {
        assert_eq!(config.budget.retries, 7);
        assert_eq!(config.review.strategy, standard.review.strategy);
        assert_eq!(config.review.validate, standard.review.validate);
        assert_eq!(config.vera.index_identity(), standard.vera.index_identity());
    }
}

#[test]
fn deep_preserves_larger_review_limits_and_budgets_saturate() {
    let mut config = crate::config::Config::parse(
        &format!("{CONFIG}review: {{max_diff_bytes: 600000, max_findings: 30}}\n"),
        "test",
    )
    .unwrap();
    config.budget.agent_max_tool_calls = u32::MAX;
    config.budget.run_max_requests = u32::MAX;
    config.budget.agent_max_seconds = u64::MAX;
    config.budget.run_max_seconds = u64::MAX;
    config.apply_profile("deep").unwrap();
    assert_eq!(limits(&config), (u32::MAX, u64::MAX, u32::MAX, u64::MAX));
    assert_eq!(config.review.max_diff_bytes, 600000);
    assert_eq!(config.review.max_findings, 30);

    config.budget.agent_max_tool_calls = 1;
    config.budget.agent_max_seconds = 1;
    config.budget.run_max_seconds = 1;
    config.apply_profile("quick").unwrap();
    assert_eq!(limits(&config), (1, 30, u32::MAX, 60));
}

fn repo_info(name: &str) -> RepoInfo {
    let mut repo = job("").repo;
    repo.full_name = name.into();
    // Old inventories have no open_prs field.
    serde_json::from_value(json!({
        "repo": repo, "private": false, "html_url": "", "has_override": false
    }))
    .unwrap()
}

async fn get_json(client: &reqwest::Client, url: &str, cookie: &str) -> Value {
    let response = client
        .get(url)
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

#[tokio::test]
async fn bulk_repository_settings_set_clear_and_preserve_omitted_overrides() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        for name in ["owner/a", "owner/b", "owner/untouched"] {
            dashboard.repos.insert(name.into(), repo_info(name));
        }
    }
    let mut events = server.events.subscribe();
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    for (fields, trigger, mode) in [
        (
            json!({"trigger": "auto", "mode": "deep"}),
            Some(Trigger::Auto),
            Some(Mode::Deep),
        ),
        (
            json!({"mode": "quick"}),
            Some(Trigger::Auto),
            Some(Mode::Quick),
        ),
        (json!({"trigger": null}), None, Some(Mode::Quick)),
        (json!({"mode": null}), None, None),
    ] {
        let mut input = fields;
        input["repos"] = json!(["owner/a", "owner/b", "owner/a"]);
        let response = client
            .patch(format!("{url}/api/repos"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"ok": true, "updated": 2})
        );
        let saved: Value = dashboard::read_json(data.path(), "settings.json").unwrap();
        let dashboard = server.dashboard.lock().await;
        for name in ["owner/a", "owner/b"] {
            assert_eq!(dashboard.preferences.repos[name].trigger, trigger);
            assert_eq!(dashboard.preferences.repos[name].mode, mode);
            assert_eq!(saved["repos"][name]["trigger"], json!(trigger));
            assert_eq!(saved["repos"][name]["mode"], json!(mode));
        }
        assert!(!dashboard.preferences.repos.contains_key("owner/untouched"));
        assert_eq!(events.try_recv().unwrap().0, "repos");
        assert!(
            events.try_recv().is_err(),
            "batch emitted more than one event"
        );
    }
    task.abort();
}

#[tokio::test]
async fn bulk_repository_settings_reject_invalid_batches_without_writing() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    server
        .dashboard
        .lock()
        .await
        .repos
        .insert("owner/a".into(), repo_info("owner/a"));
    let before = std::fs::read(data.path().join("settings.json")).unwrap();
    let mut events = server.events.subscribe();
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    for (input, status) in [
        (
            json!({"repos": ["owner/a", "owner/missing"], "mode": "deep"}),
            404,
        ),
        (json!({"repos": [], "trigger": "auto"}), 422),
        (json!({"repos": vec!["owner/a"; 1001], "mode": "deep"}), 422),
        (json!({"repos": ["owner/a"]}), 422),
        (json!({"repos": ["owner/a"], "mode": "extreme"}), 422),
        (json!({"repos": ["owner/a"], "trigger": "enabled"}), 422),
    ] {
        assert_eq!(
            client
                .patch(format!("{url}/api/repos"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            status
        );
        assert_eq!(
            std::fs::read(data.path().join("settings.json")).unwrap(),
            before
        );
        assert!(server.dashboard.lock().await.preferences.repos.is_empty());
        assert!(events.try_recv().is_err());
    }
    assert_eq!(
        client
            .patch(format!("{url}/api/repos"))
            .header("Cookie", &cookie)
            .json(&json!({"repos": ["owner/a"], "mode": "deep"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        std::fs::read(data.path().join("settings.json")).unwrap(),
        before
    );
    task.abort();
}

#[tokio::test]
async fn repositories_expose_push_time_inventory_and_review_only_counts() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        for (name, pushed, open_prs) in [
            ("owner/a", "1750556317", Some(3)),
            ("owner/b", "", None),
            ("owner/c", "not-a-time", Some(0)),
            ("owner/d", "9223372036854775807", None),
        ] {
            let mut info = repo_info(name);
            info.repo.pushed_at = pushed.into();
            info.open_prs = open_prs;
            dashboard.repos.insert(name.into(), info);
        }
        for (minutes, (repo, kind, findings)) in [
            ("owner/a", "index", 99),
            ("owner/a", "review", 2),
            ("owner/a", "review", 3),
            ("owner/b", "review", 7),
        ]
        .into_iter()
        .enumerate()
        {
            let mut summary = JobSummary::queued(&job("head"));
            summary.queued_at =
                (chrono::Utc::now() - chrono::Duration::minutes(minutes as i64)).to_rfc3339();
            summary.repo = repo.into();
            summary.kind = kind.into();
            summary.status = "failed".into();
            summary.findings.total = findings;
            dashboard.jobs.insert(summary.id.clone(), summary);
        }
        dashboard::write_inventory(data.path(), 1, &dashboard.repos).unwrap();
    }
    let loaded = dashboard::Dashboard::load(data.path(), &reconcile::Seen::default()).unwrap();
    assert_eq!(loaded.repos["owner/a"].open_prs, Some(3));
    assert_eq!(loaded.repos["owner/b"].open_prs, None);
    let (url, task) = http(server).await;
    let repos = get_json(
        &reqwest::Client::new(),
        &format!("{url}/api/repos"),
        &cookie,
    )
    .await;
    assert_eq!(repos[0]["pushed_at"], "2025-06-22T01:38:37Z");
    assert_eq!(repos[0]["open_prs"], 3);
    assert_eq!(repos[0]["review_count"], 2);
    assert_eq!(repos[0]["finding_count"], 5);
    assert_eq!(repos[0]["last_job"]["kind"], "index");
    assert_eq!(repos[0]["last_review"]["kind"], "review");
    assert_eq!(repos[0]["last_review"]["findings"]["total"], 2);
    assert!(repos[3].get("last_review").is_none());
    assert_eq!(repos[1]["review_count"], 1);
    assert_eq!(repos[1]["finding_count"], 7);
    assert!(repos[1].get("open_prs").is_none());
    assert_eq!(repos[2]["open_prs"], 0);
    for index in 1..4 {
        assert!(repos[index].get("pushed_at").is_none());
    }
    assert_eq!(repos[3]["review_count"], 0);
    assert_eq!(repos[3]["finding_count"], 0);
    task.abort();
}

#[tokio::test]
async fn overview_windows_sum_review_findings_failures_tokens_and_median() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    // Avoid starting a local Vera probe from this API test.
    crate::fsutil::write_atomic_in(
        data.path(),
        "revera.yaml",
        format!("{CONFIG}vera: {{enabled: false}}\n").as_bytes(),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .with_priority(1)
        .expect(3)
        .mount(&mock)
        .await;
    let now = chrono::Utc::now();
    let mut entries = Vec::new();
    for (hours, kind, status, duration, high, medium, low, tokens) in [
        (
            1,
            "review",
            "complete",
            Some(100),
            1,
            2,
            3,
            Some(json!({"prompt": 10, "completion": 2, "cached": 4})),
        ),
        (
            2,
            "review",
            "partial",
            Some(201),
            0,
            1,
            2,
            Some(json!({"prompt": 20, "completion": 3, "cached": 8})),
        ),
        (3, "review", "failed", Some(300), 0, 0, 0, None),
        (4, "review", "failed", None, 0, 0, 0, None),
        (5, "review", "running", Some(10000), 0, 0, 0, None),
        (6, "review", "cancelled", Some(9999), 0, 0, 0, None),
        (
            7,
            "index",
            "failed",
            Some(1),
            99,
            99,
            99,
            Some(json!({"prompt": 5, "completion": 1, "cached": 1})),
        ),
        (
            48,
            "review",
            "complete",
            Some(500),
            0,
            0,
            1,
            Some(json!({"prompt": 30, "completion": 4, "cached": 9})),
        ),
        (192, "review", "complete", Some(700), 1, 0, 0, None),
    ] {
        let mut summary = JobSummary::queued(&job("head"));
        summary.queued_at = (now - chrono::Duration::hours(hours)).to_rfc3339();
        summary.kind = kind.into();
        summary.status = status.into();
        summary.duration_ms = duration;
        summary.findings = FindingCounts {
            total: high + medium + low,
            high,
            medium,
            low,
        };
        summary.tokens = tokens;
        entries.push(summary);
    }
    let oldest = entries.last().unwrap().queued_at.clone();
    let failed = entries[2].id.clone();
    for entry in entries {
        server
            .dashboard
            .lock()
            .await
            .jobs
            .insert(entry.id.clone(), entry);
    }
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let body = get_json(&client, &format!("{url}/api/overview"), &cookie).await;
    assert_eq!(
        body["last_24h"],
        json!({
            "reviews": 6, "failed": 2, "findings": 9,
            "severity": {"high": 1, "medium": 3, "low": 5},
            "tokens": 41, "prompt_tokens": 35, "cached_tokens": 13, "median_duration_ms": 201.0
        })
    );
    assert_eq!(
        body["last_7d"],
        json!({
            "reviews": 7, "failed": 2, "findings": 10,
            "severity": {"high": 1, "medium": 3, "low": 6},
            "tokens": 75, "prompt_tokens": 65, "cached_tokens": 22, "median_duration_ms": 250.5
        })
    );
    assert_eq!(body["history_since"], oldest);
    server
        .dashboard
        .lock()
        .await
        .jobs
        .get_mut(&failed)
        .unwrap()
        .duration_ms = None;
    let body = get_json(&client, &format!("{url}/api/overview"), &cookie).await;
    assert_eq!(body["last_24h"]["median_duration_ms"], 150.5);
    server.dashboard.lock().await.jobs.clear();
    let body = get_json(&client, &format!("{url}/api/overview"), &cookie).await;
    assert!(body["history_since"].is_null());
    for window in ["last_24h", "last_7d"] {
        assert_eq!(
            body[window],
            json!({
                "reviews": 0, "failed": 0, "findings": 0,
                "severity": {"high": 0, "medium": 0, "low": 0},
                "tokens": 0, "prompt_tokens": 0, "cached_tokens": 0, "median_duration_ms": null
            })
        );
    }
    task.abort();
}

#[tokio::test]
async fn jobs_search_matches_title_author_repo_pr_and_filtered_pagination() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let mut ids = Vec::new();
    for (index, (repo, title, author, pr, status)) in [
        ("owner/repo", "Fix Token Cache", "Alice", 123, "complete"),
        ("owner/repo", "Cache cleanup", "BOB", 124, "failed"),
        ("Other/Tools", "Refactor", "Alice", 125, "complete"),
        ("other/extra", "No match", "Someone", 123, "running"),
        ("other/extra", "Link 1234", "Someone", 9, "complete"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut summary = JobSummary::queued(&job("head"));
        summary.queued_at =
            (chrono::Utc::now() - chrono::Duration::minutes(index as i64)).to_rfc3339();
        summary.repo = repo.into();
        summary.title = Some(title.into());
        summary.author = Some(author.into());
        summary.pr = Some(pr);
        summary.status = status.into();
        ids.push(summary.id.clone());
        server
            .dashboard
            .lock()
            .await
            .jobs
            .insert(summary.id.clone(), summary);
    }
    let (url, task) = http(server).await;
    let client = reqwest::Client::new();
    for (q, expected) in [
        ("tOkEn", vec![0]),
        ("ALIce", vec![0, 2]),
        ("TOOLS", vec![2]),
        ("#123", vec![0, 3]),
        ("123", vec![0, 3, 4]),
        ("", vec![0, 1, 2, 3, 4]),
        ("absent", vec![]),
    ] {
        let mut search_url = url::Url::parse(&format!("{url}/api/jobs")).unwrap();
        search_url.query_pairs_mut().append_pair("q", q);
        let response = client
            .get(search_url)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|index| ids[index].clone())
            .collect();
        assert_eq!(
            body["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|job| job["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            expected,
            "q={q}"
        );
    }
    let filtered = get_json(
        &client,
        &format!("{url}/api/jobs?q=cache&repo=owner/repo&status=complete&kind=review"),
        &cookie,
    )
    .await;
    assert_eq!(filtered["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["jobs"][0]["id"], ids[0]);
    let unset = get_json(
        &client,
        &format!("{url}/api/jobs?repo=&status=&kind=&q="),
        &cookie,
    )
    .await;
    assert_eq!(unset["jobs"].as_array().unwrap().len(), 5);
    let first = get_json(&client, &format!("{url}/api/jobs?q=cache&limit=1"), &cookie).await;
    assert_eq!(first["jobs"][0]["id"], ids[0]);
    assert_eq!(first["next"], ids[0]);
    let second = get_json(
        &client,
        &format!("{url}/api/jobs?q=cache&limit=1&before={}", ids[0]),
        &cookie,
    )
    .await;
    assert_eq!(second["jobs"][0]["id"], ids[1]);
    assert!(second.get("next").is_none());
    task.abort();
}

#[tokio::test]
async fn job_detail_passes_through_stats_strategy_timing_and_ledger_when_present() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let summary = JobSummary::queued(&job("head"));
    let id = summary.id.clone();
    server
        .dashboard
        .lock()
        .await
        .jobs
        .insert(id.clone(), summary);
    let dir = crate::fsutil::ensure_dir(data.path(), &Path::new("jobs").join(&id)).unwrap();
    let report = json!({
        "strategy": "baseline",
        "stats": {"candidates": 3, "accepted": 1, "rejected": 1, "uncertain": 1,
            "unvalidated": 0, "files_read": 2, "retrieval": {"calls": 1},
            "validation": {"calls": 3}, "reused": 1, "resolved": 2, "reopened": 0,
            "tools": [{"name": "read_file", "calls": 2, "errors": 0, "latency_ms": 10}]},
        "timing": {"total_ms": 42, "phases": [], "skipped_phases": []},
        "ledger": {"requests": 4, "cached_prompt_tokens": 12, "by_route": []}
    });
    crate::fsutil::write_atomic_in(&dir, "report.json", &serde_json::to_vec(&report).unwrap())
        .unwrap();
    let (url, task) = http(server).await;
    let client = reqwest::Client::new();
    let body = get_json(&client, &format!("{url}/api/jobs/{id}"), &cookie).await;
    for field in ["stats", "strategy", "timing", "ledger"] {
        assert_eq!(body[field], report[field]);
    }
    crate::fsutil::write_atomic_in(&dir, "report.json", b"{}").unwrap();
    let body = get_json(&client, &format!("{url}/api/jobs/{id}"), &cookie).await;
    for field in ["stats", "strategy", "timing", "ledger"] {
        assert!(body.get(field).is_none());
    }
    task.abort();
}
