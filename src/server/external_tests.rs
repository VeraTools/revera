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

fn tracked(number: u64) -> external::Tracked {
    external::Tracked {
        url: format!("https://github.com/community/project/pull/{number}"),
        repo: "community/project".into(),
        repo_id: 701,
        number,
        actor: "admin".into(),
        title: "Public contribution".into(),
        head_sha: String::new(),
        mode: dashboard::Mode::Quick,
        status: "tracking".into(),
        reason: None,
        etag: None,
        next_poll: 0,
    }
}

#[tokio::test]
async fn external_hourly_and_tracking_caps_persist_and_retry_without_losing_heads() {
    let mock = MockServer::start().await;
    no_writes(&mock).await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    metadata(&mock, public_repo(), public_pr(&"a".repeat(40))).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.preferences.external.tracking_enabled = true;
        dashboard.preferences.external.max_tracked = 1;
        dashboard.preferences.external.reviews_per_hour = 1;
    }
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let selection = tracked(7);
    // A different tracked PR fills the cap. Rejection never enrolls the new PR.
    let other = tracked(8);
    server
        .external
        .lock()
        .await
        .tracked
        .insert(other.url.clone(), other);
    let request = || {
        client
            .post(format!("{url}/api/external/tracking"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"url": selection.url, "mode": "quick"}))
    };
    assert_eq!(request().send().await.unwrap().status(), 409);
    assert!(
        !server
            .external
            .lock()
            .await
            .tracked
            .contains_key(&selection.url)
    );
    server.external.lock().await.tracked.clear();
    assert_eq!(request().send().await.unwrap().status(), 200);
    // A manual review consumes the shared external admission allowance.
    external::submit(&server, &selection.url, dashboard::Mode::Quick, "manual")
        .await
        .unwrap();
    let response = client
        .post(format!("{url}/api/external/reviews"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"url": selection.url}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert!(
        response.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 0
    );
    external::poll(&server).await.unwrap();
    let store = external::Store::load(data.path()).unwrap();
    assert_eq!(store.admissions.len(), 1);
    assert!(store.tracked[&selection.url].head_sha.is_empty());
    assert!(store.tracked[&selection.url].etag.is_none());
    assert!(
        store.tracked[&selection.url]
            .reason
            .as_deref()
            .unwrap()
            .contains("hourly limit")
    );
    assert_eq!(server.dashboard.lock().await.jobs.len(), 1);
    // Expired admissions are removed; the initial head is still due for review.
    {
        let mut store = server.external.lock().await;
        store.admissions[0] = chrono::Utc::now().timestamp() - 3601;
        store.tracked.get_mut(&selection.url).unwrap().next_poll = 0;
    }
    external::poll(&server).await.unwrap();
    let store = external::Store::load(data.path()).unwrap();
    assert_eq!(store.admissions.len(), 1);
    assert_eq!(store.tracked[&selection.url].head_sha, "a".repeat(40));
    assert!(store.tracked[&selection.url].reason.is_none());
    assert_eq!(server.dashboard.lock().await.jobs.len(), 2);
    task.abort();
}

#[tokio::test]
async fn external_tracking_new_head_then_deletion_or_visibility_loss_stops_and_cancels() {
    for scenario in ["repo-deleted", "pr-deleted", "private", "merged"] {
        let mock = MockServer::start().await;
        no_writes(&mock).await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let item = tracked(7);
        server
            .dashboard
            .lock()
            .await
            .preferences
            .external
            .tracking_enabled = true;
        server
            .external
            .lock()
            .await
            .tracked
            .insert(item.url.clone(), item.clone());
        metadata(&mock, public_repo(), public_pr(&"a".repeat(40))).await;
        external::poll(&server).await.unwrap();
        let first = server
            .queue
            .lock()
            .await
            .pending
            .values()
            .next()
            .unwrap()
            .clone();
        mock.reset().await;
        no_writes(&mock).await;
        metadata(&mock, public_repo(), public_pr(&"c".repeat(40))).await;
        server
            .external
            .lock()
            .await
            .tracked
            .get_mut(&item.url)
            .unwrap()
            .next_poll = 0;
        external::poll(&server).await.unwrap();
        let next = server
            .queue
            .lock()
            .await
            .pending
            .values()
            .next()
            .unwrap()
            .clone();
        assert_ne!(first.id, next.id);
        assert_eq!(next.head(), "c".repeat(40));
        assert_eq!(
            server.dashboard.lock().await.jobs[&first.id].status,
            "cancelled"
        );
        assert_eq!(server.external.lock().await.admissions.len(), 2);
        // A 200 response with the same head also must not enqueue a duplicate.
        server
            .external
            .lock()
            .await
            .tracked
            .get_mut(&item.url)
            .unwrap()
            .next_poll = 0;
        external::poll(&server).await.unwrap();
        assert_eq!(server.external.lock().await.admissions.len(), 2);
        let (_, cancel) = server.queue.lock().await.take().unwrap();
        mock.reset().await;
        no_writes(&mock).await;
        match scenario {
            "repo-deleted" => {
                Mock::given(method("GET"))
                    .respond_with(ResponseTemplate::new(404))
                    .mount(&mock)
                    .await;
            }
            "pr-deleted" => {
                Mock::given(method("GET"))
                    .and(path("/repos/community/project"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(public_repo()))
                    .mount(&mock)
                    .await;
                Mock::given(method("GET"))
                    .and(path("/repos/community/project/pulls/7"))
                    .respond_with(ResponseTemplate::new(404))
                    .mount(&mock)
                    .await;
            }
            "private" => {
                let mut repo = public_repo();
                repo["private"] = json!(true);
                metadata(&mock, repo, public_pr(&"c".repeat(40))).await;
            }
            "merged" => {
                let mut pr = public_pr(&"c".repeat(40));
                pr["merged"] = json!(true);
                metadata(&mock, public_repo(), pr).await;
            }
            _ => unreachable!(),
        }
        server
            .external
            .lock()
            .await
            .tracked
            .get_mut(&item.url)
            .unwrap()
            .next_poll = 0;
        external::poll(&server).await.unwrap();
        assert!(*cancel.borrow(), "{scenario}");
        let store = external::Store::load(data.path()).unwrap();
        assert_eq!(store.tracked[&item.url].status, "stopped");
        assert!(store.tracked[&item.url].reason.is_some());
        assert_eq!(
            server.dashboard.lock().await.jobs[&next.id].status,
            "cancelled"
        );
        let before = mock.received_requests().await.unwrap().len();
        external::poll(&server).await.unwrap();
        assert_eq!(mock.received_requests().await.unwrap().len(), before);
        assert!(
            mock.received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.headers.contains_key("authorization"))
        );
    }
}

#[tokio::test]
async fn external_retention_bounds_idle_checkouts_but_preserves_active_and_tracked_targets() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    server
        .dashboard
        .lock()
        .await
        .preferences
        .external
        .max_checkouts = 2;
    let now = chrono::Utc::now();
    for (id, days) in [(1, 0), (2, 1), (3, 2), (4, 8), (5, 9), (6, 9), (7, 9)] {
        crate::fsutil::ensure_dir(
            data.path(),
            &Path::new("external").join(id.to_string()).join("cache"),
        )
        .unwrap();
        let mut review = job(&"a".repeat(40));
        review.id = format!("retention-{id}");
        review.repo.id = id;
        review.repo.installation = None;
        let mut summary = dashboard::JobSummary::queued(&review);
        summary.queued_at = (now - chrono::Duration::days(days)).to_rfc3339();
        summary.set_state("complete", None);
        server.update_job(summary).await.unwrap();
        if id == 5 || id == 6 {
            server.queue.lock().await.enqueue(review);
        }
    }
    let _ = server.queue.lock().await.take().unwrap(); // protect both pending and running
    let mut item = tracked(7);
    item.repo_id = 7;
    server
        .external
        .lock()
        .await
        .tracked
        .insert(item.url.clone(), item);
    let installed =
        crate::fsutil::ensure_dir(data.path(), Path::new("repos/community/project")).unwrap();
    // Both count and age eviction run without tracking enabled and without API reads.
    external::poll(&server).await.unwrap();
    for id in [1, 2, 5, 6, 7] {
        assert!(data.path().join(format!("external/{id}")).is_dir(), "{id}");
    }
    for id in [3, 4] {
        assert!(!data.path().join(format!("external/{id}")).exists(), "{id}");
    }
    assert!(installed.is_dir());
    assert_eq!(
        server.dashboard.lock().await.jobs.len(),
        7,
        "checkout pruning retains reports"
    );
    assert!(mock.received_requests().await.unwrap().is_empty());
    // Even when the count allows it, an expired, no-longer-active target is removed.
    server.queue.lock().await.running.clear();
    server.queue.lock().await.pending.clear();
    server.external.lock().await.tracked.clear();
    server
        .dashboard
        .lock()
        .await
        .preferences
        .external
        .max_checkouts = 50;
    external::poll(&server).await.unwrap();
    for id in [5, 6, 7] {
        assert!(!data.path().join(format!("external/{id}")).exists());
    }
}

fn local_git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn external_runner_full_path_uses_operator_config_exact_checkout_and_private_child() {
    let mock = MockServer::start().await;
    for verb in ["GET", "POST", "PATCH", "PUT", "DELETE"] {
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock)
            .await;
    }
    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join("child.py");
    std::fs::write(&script, r#"import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
assert args[0] == 'review'
get = lambda flag: args[args.index(flag) + 1]
assert '--private' in args and '--force' in args
assert get('--publish') == 'dry-run' and get('--profile') == 'quick'
cwd = pathlib.Path.cwd()
assert cwd == pathlib.Path(get('--repo')) and cwd.parts[-3:] == ('external', '701', 'pull-7')
assert pathlib.Path(get('--config')).is_file() and not pathlib.Path(get('--config')).is_relative_to(cwd)
assert os.environ['REVIEW_API_KEY'] == 'intended-model-key'
assert os.environ['HOME'] == str(cwd.parent / 'home')
assert os.environ['XDG_CACHE_HOME'] == str(cwd.parent / 'cache')
assert os.environ['REVERA_VERA_HOME'] == str(cwd.parent / 'cache/vera-home')
assert not any(key in os.environ for key in ['GITHUB_TOKEN', 'GH_TOKEN', 'UNREFERENCED_SECRET', 'REVERA_SETUP_TOKEN', 'SSH_AUTH_SOCK'])
assert not any('AUTHORIZATION' in value for value in os.environ.values())
event = json.loads(pathlib.Path(get('--event')).read_text())
pr = event['pull_request']
assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == pr['head']['sha']
assert subprocess.check_output(['git', 'cat-file', '-t', pr['base']['sha']], text=True).strip() == 'commit'
assert (cwd / 'file.txt').read_text() == 'pinned fork head\n'
pathlib.Path(get('--out')).write_text(json.dumps({'source': 'external', 'status': 'complete', 'publication': {'mode': 'private'}, 'findings': []}))
"#).unwrap();
    let runner = runner::Runner {
        executable: "/usr/bin/python3".into(),
        prefix: vec![script.display().to_string()],
    };
    let (data, server) = server(&mock, runner, true).await;
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard
            .secrets
            .insert("REVIEW_API_KEY".into(), "intended-model-key".into());
        dashboard
            .secrets
            .insert("UNREFERENCED_SECRET".into(), "not-for-child".into());
        dashboard
            .secrets
            .insert("GITHUB_TOKEN".into(), "never-for-child".into());
        dashboard
            .secrets
            .insert("REVERA_SETUP_TOKEN".into(), "never-for-child".into());
    }
    let source = crate::fsutil::ensure_dir(scratch.path(), Path::new("source")).unwrap();
    local_git(&source, &["init", "-q"]);
    std::fs::write(source.join("file.txt"), "base\n").unwrap();
    std::fs::write(source.join("revera.yaml"), "invalid repo config: [\n").unwrap();
    local_git(&source, &["add", "."]);
    local_git(&source, &["commit", "-qm", "base"]);
    let base = local_git(&source, &["rev-parse", "HEAD"]);
    std::fs::write(source.join("file.txt"), "pinned fork head\n").unwrap();
    local_git(&source, &["commit", "-qam", "head"]);
    let head = local_git(&source, &["rev-parse", "HEAD"]);
    std::fs::write(source.join("file.txt"), "newer fork head\n").unwrap();
    local_git(&source, &["commit", "-qam", "force push"]);
    let newer = local_git(&source, &["rev-parse", "HEAD"]);
    let bare = scratch.path().join("remote.git");
    local_git(
        scratch.path(),
        &[
            "clone",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    local_git(&bare, &["update-ref", "refs/pull/7/head", &newer]);
    let ws = crate::fsutil::ensure_dir(data.path(), Path::new("external/701/pull-7")).unwrap();
    local_git(&ws, &["init", "-q"]);
    local_git(
        &ws,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/community/project.git",
        ],
    );
    // Trusted fixture transport only: the production URL and anonymous environment
    // are unchanged, while this local git config rewrites HTTPS to the bare fixture.
    local_git(
        &ws,
        &[
            "config",
            &format!("url.file://{}.insteadOf", bare.display()),
            "https://github.com/community/project.git",
        ],
    );
    local_git(&ws, &["config", "protocol.file.allow", "always"]);
    let hook = ws.join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\ntouch hook-executed\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut pr = public_pr(&head);
    pr["base"]["sha"] = json!(base);
    let mut review = events::Job::new(
        events::Repo::parse(&public_repo(), None).unwrap(),
        events::Kind::Review {
            number: 7,
            pr,
            force: true,
        },
        "manual",
    );
    review.mode = dashboard::Mode::Quick;
    server.enqueue(review.clone()).await;
    let (running, cancel) = server.queue.lock().await.take().unwrap();
    runner::run(server.clone(), &running, cancel).await;
    let summary = server.dashboard.lock().await.jobs[&review.id].clone();
    assert_eq!(
        summary.status,
        "complete",
        "{:?}: {}",
        summary.reason,
        std::fs::read_to_string(data.path().join(format!("jobs/{}/log.txt", review.id))).unwrap()
    );
    assert_eq!(summary.source, events::Source::External);
    assert_eq!(summary.publication, events::Publication::Private);
    assert!(summary.check.is_none() && summary.check_run_url.is_none());
    assert!(!ws.join("hook-executed").exists());
    assert!(server.queue.lock().await.running.is_empty());
    assert!(server.seen.lock().await.0.is_empty());
    assert!(
        !data
            .path()
            .join(format!("jobs/{}/event.json", review.id))
            .exists()
    );
    assert!(mock.received_requests().await.unwrap().is_empty());
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
