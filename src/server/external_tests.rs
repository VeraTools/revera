use super::super::external;
use super::*;
use wiremock::matchers::query_param;

fn public_repo() -> Value {
    json!({"id": 701, "full_name": "community/project", "private": false, "size": 10, "default_branch": "main"})
}
fn public_pr(head: &str) -> Value {
    json!({"number": 7, "title": "Public contribution", "state": "open", "merged": false,
        "user": {"login": "admin"}, "html_url": "https://github.com/community/project/pull/7",
        "head": {"sha": head, "ref": "topic", "repo": {"full_name": "contributor/project", "private": false}},
        "base": {"sha": "b".repeat(40), "ref": "main", "repo": public_repo()}})
}
async fn metadata(mock: &MockServer, repo: Value, pr: Value) {
    Mock::given(method("GET"))
        .and(path("/repos/community/project"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo))
        .mount(mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/community/project/pulls/7"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"head-v1\"")
                .set_body_json(pr),
        )
        .mount(mock)
        .await;
}
async fn no_writes(mock: &MockServer) {
    for verb in ["POST", "PATCH", "PUT", "DELETE"] {
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(mock)
            .await;
    }
}

#[test]
fn external_url_parser_rejects_ssrf_and_normalization_tricks() {
    use external::PrUrl;
    assert_eq!(
        PrUrl::parse("https://github.com/Owner/repo.name-_/pull/0007")
            .unwrap()
            .url(),
        "https://github.com/Owner/repo.name-_/pull/7"
    );
    for bad in [
        "http://github.com/o/r/pull/1",
        "https://evil.example/o/r/pull/1",
        "https://gist.github.com/o/r/pull/1",
        "https://github.com:443/o/r/pull/1",
        "https://github.com:123/o/r/pull/1",
        "https://github.com@evil.example/o/r/pull/1",
        "https://user@github.com/o/r/pull/1",
        "https://github.com/o%2Fr/r/pull/1",
        "https://github.com/o/r%2fother/pull/1",
        "https://github.com/../r/pull/1",
        "https://github.com/o/../pull/1",
        "https://github.com/o/x/../r/pull/1",
        "https://github.com/o/r/issues/1",
        "https://github.com/o/r/pull/0",
        "https://github.com/o/r/pull/-1",
        "https://github.com/o/r/pull/1?x=1",
        "https://github.com/o/r/pull/1#fragment",
        "https://github.com/o/r/pull/1/",
        "https://github.com/o/r/pull/18446744073709551616",
        "https://github.com/o\\r/r/pull/1",
        " https://github.com/o/r/pull/1",
        "https://github.com/o/r/pull/1\n",
        "https://github.com//r/pull/1",
        "https://github.com/o/r/pull/1@127.0.0.1",
    ] {
        assert!(PrUrl::parse(bad).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn external_resolve_private_missing_closed_and_size_cap_never_write() {
    for scenario in [
        "private",
        "missing",
        "closed",
        "large",
        "unknown-visibility",
    ] {
        let mock = MockServer::start().await;
        no_writes(&mock).await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        if scenario == "missing" {
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&mock)
                .await;
        } else {
            let mut repo = public_repo();
            let mut pr = public_pr(&"a".repeat(40));
            match scenario {
                "private" => repo["private"] = json!(true),
                "unknown-visibility" => {
                    repo.as_object_mut().unwrap().remove("private");
                }
                "closed" => pr["state"] = json!("closed"),
                "large" => repo["size"] = json!(1048577),
                _ => unreachable!(),
            }
            metadata(&mock, repo, pr).await;
        }
        let result = external::submit(
            &server,
            "https://github.com/community/project/pull/7",
            dashboard::Mode::Quick,
            "manual",
        )
        .await
        .unwrap_err();
        assert!(
            result.message.contains(if scenario == "closed" {
                "closed"
            } else if scenario == "large" {
                "size limit"
            } else {
                "access/installation"
            }),
            "{scenario}: {result}"
        );
        assert!(server.queue.lock().await.pending.is_empty());
    }
}

#[tokio::test]
async fn external_api_fork_force_push_rerun_and_staleness_are_private() {
    let mock = MockServer::start().await;
    no_writes(&mock).await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    server.dashboard.lock().await.preferences.default_trigger = dashboard::Trigger::Off;
    let (cookie, csrf) = administrator(&server).await;
    metadata(&mock, public_repo(), public_pr(&"a".repeat(40))).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let route = format!("{url}/api/external/reviews");
    assert_eq!(
        client
            .post(&route)
            .json(&json!({"url": "https://github.com/community/project/pull/7"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&route)
            .header("Cookie", &cookie)
            .json(&json!({"url": "https://github.com/community/project/pull/7"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(&route)
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"url": "https://github.com/community/project/pull/7", "mode": "quick"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let id = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = server.dashboard.lock().await.jobs[&id].clone();
    assert_eq!(json!(first)["source"], "external");
    assert_eq!(json!(first)["publication"], "private");
    assert_eq!(first.mode, dashboard::Mode::Quick);
    assert!(first.check.is_none());
    assert!(first.check_run_url.is_none());
    let pending = server
        .queue
        .lock()
        .await
        .pending
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(pending.repo.installation.is_none());
    assert_eq!(
        pending.event()["pull_request"]["head"]["repo"]["full_name"],
        "contributor/project"
    );
    assert_eq!(external::stale(&server, &first).await, Some(false));
    mock.reset().await;
    no_writes(&mock).await;
    metadata(&mock, public_repo(), public_pr(&"c".repeat(40))).await;
    server.external_heads.lock().await.clear();
    assert_eq!(external::stale(&server, &first).await, Some(true));
    let response = client
        .post(format!("{url}/api/jobs/{id}/rerun"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let rerun: Value = response.json().await.unwrap();
    let queue = server.queue.lock().await;
    let job = queue.pending.values().next().unwrap();
    assert_eq!(job.id, rerun["id"]);
    assert_eq!(job.head(), "c".repeat(40));
    assert_eq!(job.mode, dashboard::Mode::Quick);
    assert_eq!(job.trigger, "rerun");
    drop(queue);
    task.abort();
}

#[test]
fn external_queue_keys_and_concurrency_are_isolated_from_installations() {
    let installed = job(&"a".repeat(40));
    let mut external = installed.clone();
    external.id = "external".into();
    external.repo.installation = None;
    assert_ne!(installed.key(), external.key());
    let mut queue = queue::Queue::default();
    queue.enqueue(installed.clone());
    queue.enqueue(external.clone());
    assert_eq!(queue.pending.len(), 2);
    let _ = queue.take().unwrap();
    let _ = queue.take().unwrap();
    assert_eq!(queue.running.len(), 2);
    let mut other_external = external.clone();
    other_external.repo.id = 2;
    queue.enqueue(other_external);
    assert!(
        queue.take().is_none(),
        "one external at a time even across repositories"
    );
    queue.close(installed.key());
    assert!(!*queue.running[&external.key()].cancel.borrow());
    queue.remove_repo(installed.repo);
    assert!(!*queue.running[&external.key()].cancel.borrow());
}

#[tokio::test]
async fn external_tracking_discover_choose_ignore_etag_close_untrack_and_caps() {
    let mock = MockServer::start().await;
    no_writes(&mock).await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    metadata(&mock, public_repo(), public_pr(&"a".repeat(40))).await;
    Mock::given(method("GET"))
        .and(path("/search/issues"))
        .and(query_param("q", "is:pr is:open is:public author:admin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"html_url": "https://github.com/community/project/pull/7", "title": "Public"},
            {"html_url": "https://github.com/owner/repo/pull/9", "title": "Installed"}
        ]})))
        .expect(2)
        .mount(&mock)
        .await;
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.repos.insert(
            "owner/repo".into(),
            dashboard::RepoInfo {
                repo: job("").repo,
                private: false,
                html_url: String::new(),
                has_override: false,
                open_prs: None,
            },
        );
    }
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let response: Value = client
        .get(format!("{url}/api/external/discover"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["prs"].as_array().unwrap().len(), 1);
    assert!(
        server.external.lock().await.tracked.is_empty(),
        "discovery never selects PRs"
    );
    let selection = json!({"url": "https://github.com/community/project/pull/7", "mode": "deep"});
    let request = || {
        client
            .post(format!("{url}/api/external/tracking"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&selection)
    };
    assert_eq!(
        request().send().await.unwrap().status(),
        409,
        "off by default"
    );
    server
        .dashboard
        .lock()
        .await
        .preferences
        .external
        .tracking_enabled = true;
    assert_eq!(request().send().await.unwrap().status(), 200);
    assert_eq!(
        request().send().await.unwrap().status(),
        200,
        "track is idempotent"
    );
    external::poll(&server).await.unwrap();
    let key = selection["url"].as_str().unwrap();
    assert_eq!(
        server.external.lock().await.tracked[key].head_sha,
        "a".repeat(40)
    );
    let pending = server
        .queue
        .lock()
        .await
        .pending
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(pending.mode, dashboard::Mode::Deep);
    assert_eq!(pending.trigger, "tracking");
    // Not due: no network, no repeated admission.
    let before = mock.received_requests().await.unwrap().len();
    external::poll(&server).await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), before);
    assert_eq!(
        client
            .post(format!("{url}/api/external/ignore"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"url": key}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let response: Value = client
        .get(format!("{url}/api/external/discover"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["prs"], json!([]));
    mock.verify().await;
    Mock::given(method("GET"))
        .and(path("/repos/community/project/pulls/7"))
        .and(header("if-none-match", "\"head-v1\""))
        .respond_with(ResponseTemplate::new(304))
        .with_priority(1)
        .expect(1)
        .mount(&mock)
        .await;
    server
        .external
        .lock()
        .await
        .tracked
        .get_mut(key)
        .unwrap()
        .next_poll = 0;
    external::poll(&server).await.unwrap();
    assert_eq!(server.external.lock().await.admissions.len(), 1);
    mock.verify().await;
    mock.reset().await;
    no_writes(&mock).await;
    let mut closed = public_pr(&"a".repeat(40));
    closed["state"] = json!("closed");
    metadata(&mock, public_repo(), closed).await;
    server
        .external
        .lock()
        .await
        .tracked
        .get_mut(key)
        .unwrap()
        .next_poll = 0;
    external::poll(&server).await.unwrap();
    assert_eq!(server.external.lock().await.tracked[key].status, "stopped");
    assert!(server.queue.lock().await.pending.is_empty());
    let persisted = external::Store::load(data.path()).unwrap();
    assert!(
        persisted.tracked[key]
            .reason
            .as_deref()
            .unwrap()
            .contains("closed")
    );
    let before = mock.received_requests().await.unwrap().len();
    external::poll(&server).await.unwrap();
    assert_eq!(mock.received_requests().await.unwrap().len(), before);
    assert_eq!(
        client
            .delete(format!("{url}/api/external/tracking/701/7"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        server.external.lock().await.tracked[key].reason.as_deref(),
        Some("untracked manually")
    );
    task.abort();
}

#[tokio::test]
async fn external_rate_limits_back_off_and_persist() {
    for (status, retry, remaining) in [
        (429, Some("120"), false),
        (403, None, true),
        (403, None, false),
    ] {
        let mock = MockServer::start().await;
        no_writes(&mock).await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let mut response = ResponseTemplate::new(status);
        if let Some(retry) = retry {
            response = response.insert_header("retry-after", retry);
        }
        if remaining {
            response = response
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header(
                    "x-ratelimit-reset",
                    (chrono::Utc::now().timestamp() + 180).to_string(),
                );
        }
        Mock::given(method("GET"))
            .respond_with(response)
            .expect(1)
            .mount(&mock)
            .await;
        for _ in 0..2 {
            let error = external::submit(
                &server,
                "https://github.com/community/project/pull/7",
                dashboard::Mode::Standard,
                "manual",
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, 429);
            assert!(error.retry_after.unwrap() >= 59);
        }
        assert!(
            external::Store::load(data.path()).unwrap().retry_at > chrono::Utc::now().timestamp()
        );
    }
}

#[test]
fn external_legacy_jobs_repo_check_recovery_and_seen_load() {
    let mut summary = serde_json::to_value(dashboard::JobSummary::queued(&job("legacy"))).unwrap();
    for field in ["source", "publication", "repo_id", "base_sha"] {
        summary.as_object_mut().unwrap().remove(field);
    }
    let legacy_repo = repository();
    let mut repo = legacy_repo.clone();
    repo["installation"] = json!(9);
    summary["check"] = json!({"id": 100, "repo": repo, "pending": true});
    let loaded: dashboard::JobSummary = serde_json::from_value(summary).unwrap();
    assert_eq!(loaded.source, events::Source::Installed);
    assert_eq!(loaded.publication, events::Publication::Github);
    assert_eq!(loaded.check.unwrap().repo.installation, Some(9));
    let seen: reconcile::Seen = serde_json::from_value(
        json!({"1": {"repo": repo, "pushed_at": "one", "heads": {"7": "old"}}}),
    )
    .unwrap();
    assert_eq!(seen.0[&1].repo.installation_id().unwrap(), 9);
    let preferences: dashboard::Preferences =
        serde_json::from_value(json!({"admins": ["admin"], "default_enabled": true})).unwrap();
    assert!(!preferences.external.tracking_enabled);
    assert_eq!(preferences.external.poll_minutes, 60);
    let external_repo = events::Repo::parse(&public_repo(), None).unwrap();
    assert!(external_repo.installation_id().is_err());
}
