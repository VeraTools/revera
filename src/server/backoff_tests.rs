use super::*;
use chrono::{DateTime, Duration, Utc};

fn index_history(order: i64, status: &str, finished: DateTime<Utc>) -> dashboard::JobSummary {
    let job = events::Job::new(job("").repo, events::Kind::Index, "reconcile");
    let mut summary = dashboard::JobSummary::queued(&job);
    // IDs deliberately sort in the opposite order to the enqueue timestamps.
    summary.id = format!("{:032}", 1000 - order);
    summary.queued_at = (DateTime::from_timestamp(1_700_000_000, 0).unwrap()
        + Duration::seconds(order))
    .to_rfc3339();
    summary.set_state(status, None);
    if summary.terminal() {
        summary.finished_at = Some(finished.to_rfc3339());
    }
    summary
}

async fn inventory(mock: &MockServer, repo: Value, prs: Vec<Value>, times: u64) {
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 9, "account": {"login": "owner"}}
        ])))
        .expect(times)
        .mount(mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "inventory-token"})))
        .expect(times)
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"repositories": [repo]})))
        .expect(times)
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(prs)))
        .expect(times)
        .mount(mock)
        .await;
}

#[tokio::test]
async fn index_backoff_doubles_from_latest_finish_and_caps_at_one_day() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let base = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        server.dashboard.lock().await.index_retry_at("owner/repo"),
        None
    );
    let mut expected = None;
    for (offset, minutes) in [10, 20, 40, 80, 160, 320, 640, 1280, 1440, 1440, 1440]
        .into_iter()
        .enumerate()
    {
        let finished = base + Duration::days(offset as i64);
        let status = if offset % 2 == 0 { "failed" } else { "partial" };
        server
            .update_job(index_history(offset as i64, status, finished))
            .await
            .unwrap();
        expected = Some(finished + Duration::minutes(minutes));
        assert_eq!(
            server.dashboard.lock().await.index_retry_at("owner/repo"),
            expected
        );
    }
    // Unfinished, cancelled and skipped jobs neither add failures nor reset them.
    for (offset, status) in ["queued", "running", "cancelled", "skipped"]
        .into_iter()
        .enumerate()
    {
        server
            .update_job(index_history(20 + offset as i64, status, base))
            .await
            .unwrap();
    }
    let mut unrelated = index_history(30, "complete", base);
    unrelated.repo = "owner/other".into();
    server.update_job(unrelated).await.unwrap();
    let mut review = index_history(31, "complete", base);
    review.kind = "review".into();
    server.update_job(review).await.unwrap();
    assert_eq!(
        server.dashboard.lock().await.index_retry_at("owner/repo"),
        expected
    );
    let restarted = initialize(
        settings(data.path()),
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        restarted
            .dashboard
            .lock()
            .await
            .index_retry_at("owner/repo"),
        expected
    );
}

#[tokio::test]
async fn reconcile_waits_for_backoff_on_both_index_triggers_but_not_reviews() {
    for status in ["failed", "partial"] {
        for state in ["checkpoint-behind", "legacy-advanced", "first-inventory"] {
            let mock = MockServer::start().await;
            let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
            let mut repo = job("").repo;
            repo.pushed_at = "failed-push".into();
            if state != "first-inventory" {
                let mut previous = repo.clone();
                if state == "checkpoint-behind" {
                    previous.pushed_at = "last-complete-push".into();
                }
                server.seen.lock().await.observe(&previous, &[pr("before")]);
            }
            let before = server
                .seen
                .lock()
                .await
                .0
                .get(&repo.id)
                .map(|seen| seen.pushed_at.clone());
            let failed = index_history(1, status, Utc::now());
            server.update_job(failed.clone()).await.unwrap();
            let mut value = repository();
            value["pushed_at"] = json!(repo.pushed_at);
            inventory(&mock, value, vec![pr("after")], 2).await;
            reconcile::once(&server).await.unwrap();
            {
                let queue = server.queue.lock().await;
                assert!(
                    !queue.pending.contains_key(&(repo.id, 0)),
                    "{status}/{state}"
                );
                if state != "first-inventory" {
                    assert_eq!(queue.pending[&(repo.id, 7)].head(), "after");
                }
            }
            assert_eq!(
                server.seen.lock().await.0[&repo.id].pushed_at,
                before.unwrap_or_default()
            );
            let mut due = failed;
            due.finished_at = Some((Utc::now() - Duration::minutes(10)).to_rfc3339());
            server.update_job(due).await.unwrap();
            reconcile::once(&server).await.unwrap();
            let queue = server.queue.lock().await;
            let retry = &queue.pending[&(repo.id, 0)];
            assert_eq!(retry.trigger, "reconcile");
            assert_eq!(retry.repo.pushed_at, repo.pushed_at);
            assert!(matches!(retry.kind, events::Kind::Index));
        }
    }
}

#[tokio::test]
async fn push_webhook_with_new_pushed_at_bypasses_index_backoff() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    server
        .update_job(index_history(1, "failed", Utc::now()))
        .await
        .unwrap();
    assert!(
        server
            .dashboard
            .lock()
            .await
            .index_retry_at("owner/repo")
            .unwrap()
            > Utc::now()
    );
    let (url, task) = http(server.clone()).await;
    let mut repo = repository();
    repo["pushed_at"] = json!("new-push");
    let body = serde_json::to_vec(&json!({
        "installation": {"id": 9}, "repository": repo, "ref": "refs/heads/main"
    }))
    .unwrap();
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{url}/webhook"))
            .header("X-GitHub-Event", "push")
            .header(
                "X-Hub-Signature-256",
                signature(&test_app().webhook_secret, &body)
            )
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let queue = server.queue.lock().await;
    let index = &queue.pending[&(1, 0)];
    assert_eq!(index.trigger, "webhook");
    assert_eq!(index.repo.pushed_at, "new-push");
    assert!(matches!(index.kind, events::Kind::Index));
    task.abort();
}

#[tokio::test]
async fn manual_index_bypasses_backoff_and_repo_api_reports_retry_time() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    server
        .update_job(index_history(1, "failed", Utc::now()))
        .await
        .unwrap();
    inventory(&mock, repository(), vec![], 1).await;
    reconcile::once(&server).await.unwrap();
    assert!(server.queue.lock().await.pending.is_empty());
    let expected = server
        .dashboard
        .lock()
        .await
        .index_retry_at("owner/repo")
        .unwrap();
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let repos: Value = client
        .get(format!("{url}/api/repos"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let retry_at = DateTime::parse_from_rfc3339(repos[0]["index"]["retry_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(retry_at, expected);
    let response = client
        .post(format!("{url}/api/repos/owner/repo/index"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await.unwrap();
    let queue = server.queue.lock().await;
    let index = &queue.pending[&(1, 0)];
    assert_eq!(index.id, value["id"].as_str().unwrap());
    assert_eq!(index.trigger, "manual");
    assert!(matches!(index.kind, events::Kind::Index));
    task.abort();
}

#[tokio::test]
async fn completed_index_resets_backoff_and_clears_repo_retry_time() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let finished = Utc::now();
    for order in 1..=3 {
        server
            .update_job(index_history(order, "failed", finished))
            .await
            .unwrap();
    }
    server
        .update_job(index_history(4, "complete", finished))
        .await
        .unwrap();
    assert_eq!(
        server.dashboard.lock().await.index_retry_at("owner/repo"),
        None
    );
    inventory(&mock, repository(), vec![], 1).await;
    reconcile::once(&server).await.unwrap();
    assert!(server.queue.lock().await.pending.is_empty());
    let (url, task) = http(server.clone()).await;
    let repos: Value = reqwest::Client::new()
        .get(format!("{url}/api/repos"))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(repos[0]["index"].get("retry_at").is_none());
    server
        .update_job(index_history(5, "partial", finished))
        .await
        .unwrap();
    assert_eq!(
        server.dashboard.lock().await.index_retry_at("owner/repo"),
        Some(finished + Duration::minutes(10))
    );
    task.abort();
}

#[tokio::test]
async fn reconcile_tick_retests_hand_edited_config_and_keeps_unchanged_health() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    administrator(&server).await;
    server
        .dashboard
        .lock()
        .await
        .secrets
        .insert("REVIEW_API_KEY".into(), "reconcile-probe-key".into());
    let yaml = CONFIG.replace("https://api.example.com/v1", &format!("{}/v1", mock.uri()));
    crate::fsutil::write_atomic_in(data.path(), "revera.yaml", yaml.as_bytes()).unwrap();
    inventory(&mock, repository(), vec![], 3).await;
    for model in ["test-model", "edited-model"] {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer reconcile-probe-key"))
            .and(wiremock::matchers::body_partial_json(
                json!({"model": model}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "OK"}}]
            })))
            .expect(1)
            .mount(&mock)
            .await;
    }
    reconcile::tick(&server).await.unwrap();
    assert_eq!(
        server.dashboard.lock().await.health["investigator"]["model"],
        "test-model"
    );
    let changed = yaml.replace("model: test-model", "model: edited-model");
    crate::fsutil::write_atomic_in(data.path(), "revera.yaml", changed.as_bytes()).unwrap();
    // No restart, dashboard save or manual Test between the edit and this tick.
    reconcile::tick(&server).await.unwrap();
    let health = server.dashboard.lock().await.health.clone();
    for role in ["investigator", "validator"] {
        assert_eq!(health[role]["ok"], true);
        assert_eq!(health[role]["model"], "edited-model");
    }
    let persisted: Value = dashboard::read_json(data.path(), "health.json").unwrap();
    assert_eq!(persisted["models"], json!(health));
    reconcile::tick(&server).await.unwrap();
    assert_eq!(server.dashboard.lock().await.health, health);
    let (url, task) = http(server).await;
    let session: Value = reqwest::get(format!("{url}/api/session"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["setup"]["stage"], "done");
    task.abort();
}
