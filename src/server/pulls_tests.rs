use super::*;
use dashboard::{FindingCounts, JobSummary, Mode, RepoInfo, Trigger};
use std::sync::Mutex as StdMutex;

fn open_pull(number: u64, head: &str) -> Value {
    let mut value = pr(head);
    value["number"] = json!(number);
    value["user"] = json!({"login": "automation[bot]", "type": "Bot"});
    value["created_at"] = json!("2026-10-01T00:00:00Z");
    value["updated_at"] = json!(format!("2026-10-{number:02}T00:00:00Z"));
    value["html_url"] = json!(format!("https://github.com/owner/repo/pull/{number}"));
    value["head"]["repo"] = json!({"id": 2, "full_name": "fork/repo"});
    value["base"]["repo"] = json!({"id": 1, "full_name": "owner/repo"});
    value["labels"] = json!([{"name": "bug"}]);
    value
}

async fn add_repo(server: &Server) -> events::Repo {
    let repo = job("").repo;
    server.dashboard.lock().await.repos.insert(
        repo.full_name.clone(),
        RepoInfo {
            repo: repo.clone(),
            private: true,
            html_url: "https://github.com/owner/repo".into(),
            has_override: false,
            open_prs: None,
        },
    );
    repo
}

async fn get(client: &reqwest::Client, url: &str, route: &str, cookie: &str) -> Value {
    let response = client
        .get(format!("{url}{route}"))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn webhook_pull(url: &str, action: &str, pull: Value) {
    let payload = json!({"action": action, "number": pull["number"], "repository": repository(), "installation": {"id": 9}, "pull_request": pull});
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
}

#[tokio::test]
async fn webhook_cache_upserts_all_supported_actions_and_removes_closed_and_uninstalled() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let repo = add_repo(&server).await;
    server.dashboard.lock().await.preferences.default_trigger = Trigger::Off;
    let (cookie, _) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    assert_eq!(
        get(&client, &url, "/api/pulls", &cookie).await,
        json!({"refreshed_at": null, "pulls": []})
    );
    // Before its first full listing a repository ignores webhook upserts.
    webhook_pull(&url, "opened", open_pull(7, "early")).await;
    assert_eq!(
        get(&client, &url, "/api/pulls", &cookie).await["pulls"],
        json!([])
    );
    server.pulls.lock().unwrap().listing(&repo, &[]);
    for (index, action) in [
        "opened",
        "reopened",
        "edited",
        "synchronize",
        "ready_for_review",
        "converted_to_draft",
    ]
    .iter()
    .enumerate()
    {
        let mut pull = open_pull(7, &format!("head-{index}"));
        pull["title"] = json!(action);
        pull["draft"] = json!(*action == "converted_to_draft");
        webhook_pull(&url, action, pull).await;
        let value = get(&client, &url, "/api/pulls", &cookie).await;
        assert!(
            chrono::DateTime::parse_from_rfc3339(value["refreshed_at"].as_str().unwrap()).is_ok()
        );
        let pulls = value["pulls"].as_array().unwrap();
        assert_eq!(pulls.len(), 1);
        let pull = &pulls[0];
        assert_eq!(pull["repo"], "owner/repo");
        assert_eq!(pull["repo_id"], 1);
        assert_eq!(pull["title"], *action);
        assert_eq!(pull["author_is_bot"], true);
        assert_eq!(pull["fork"], true);
        assert_eq!(pull["head_ref"], "topic");
        assert_eq!(pull["base_ref"], "main");
        assert_eq!(pull["head_sha"], format!("head-{index}"));
        assert_eq!(pull["draft"], *action == "converted_to_draft");
        assert_eq!(pull["labels"], json!(["bug"]));
        assert_eq!(pull["trigger"], "off");
    }
    webhook_pull(&url, "closed", open_pull(7, "head")).await;
    assert_eq!(
        get(&client, &url, "/api/pulls", &cookie).await["pulls"],
        json!([])
    );
    webhook_pull(&url, "opened", open_pull(7, "head")).await;
    server.remove_repo(repo).await.unwrap();
    // A late valid webhook cannot expose a repository absent from current inventory.
    webhook_pull(&url, "edited", open_pull(7, "late")).await;
    assert_eq!(
        get(&client, &url, "/api/pulls", &cookie).await["pulls"],
        json!([])
    );
    task.abort();
}

#[tokio::test]
async fn startup_reconcile_caches_full_listing_replaces_and_evicts_without_extra_requests() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let pulls = Arc::new(StdMutex::new(vec![
        open_pull(7, "first"),
        open_pull(8, "second"),
    ]));
    let repos = Arc::new(StdMutex::new(vec![repository()]));
    Mock::given(method("GET")).and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 9, "account": {"login": "owner", "type": "User"}, "suspended_at": null,
            "permissions": {"contents":"read", "pull_requests":"write", "checks":"write", "metadata":"read", "issues":"read"}
        }]))).mount(&mock).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token":"pull-inventory-token"})),
        )
        .expect(3)
        .mount(&mock)
        .await;
    let values = repos.clone();
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"repositories": values.lock().unwrap().clone()}))
        })
        .expect(3)
        .mount(&mock)
        .await;
    let values = pulls.clone();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(values.lock().unwrap().clone())
        })
        .expect(2)
        .mount(&mock)
        .await;
    let (cookie, _) = administrator(&server).await;
    // The real loop must run immediately, not wait for the ten-minute interval.
    let mut events = server.events.subscribe();
    let reconcile_task = tokio::spawn(reconcile::loop_forever(server.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    reconcile_task.abort();
    let _ = reconcile_task.await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let first = get(&client, &url, "/api/pulls", &cookie).await;
    assert_eq!(first["pulls"][0]["number"], 8);
    assert_eq!(first["pulls"][1]["head_sha"], "first");
    assert_eq!(first["pulls"][1]["labels"], json!(["bug"]));
    *pulls.lock().unwrap() = vec![open_pull(7, "replacement")];
    reconcile::once(&server).await.unwrap();
    let second = get(&client, &url, "/api/pulls", &cookie).await;
    assert_eq!(second["pulls"].as_array().unwrap().len(), 1);
    assert_eq!(second["pulls"][0]["head_sha"], "replacement");
    repos.lock().unwrap().clear();
    reconcile::once(&server).await.unwrap();
    assert_eq!(
        get(&client, &url, "/api/pulls", &cookie).await,
        json!({"refreshed_at": null, "pulls": []})
    );
    task.abort();
}

fn summary(number: u64, head: &str, status: &str, order: u64) -> JobSummary {
    let mut job = job(head);
    job.kind = events::Kind::Review {
        number,
        pr: open_pull(number, head),
        force: true,
    };
    let mut summary = JobSummary::queued(&job);
    summary.queued_at = format!("2026-10-{order:02}T00:00:00Z");
    summary.set_state(status, None);
    summary
}

#[tokio::test]
async fn inbox_computes_every_review_state_and_filters_source_inventory_and_revocation() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let repo = add_repo(&server).await;
    let cases = [
        (1, None, "not_reviewed"),
        (2, Some("queued"), "reviewing"),
        (3, Some("running"), "reviewing"),
        (4, Some("complete"), "outdated"),
        (5, Some("failed"), "failed"),
        (6, Some("cancelled"), "failed"),
        (7, Some("partial"), "partial"),
        (8, Some("complete"), "findings"),
        (9, Some("complete"), "clean"),
        (10, Some("skipped"), "skipped"),
        (11, Some("complete"), "findings"),
    ];
    server.pulls.lock().unwrap().listing(
        &repo,
        &cases
            .iter()
            .map(|(n, _, _)| open_pull(*n, "current"))
            .collect::<Vec<_>>(),
    );
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.preferences.default_trigger = Trigger::Off;
        dashboard.preferences.default_mode = Mode::Quick;
        for (number, status, _) in cases {
            if let Some(status) = status {
                let mut job = summary(
                    number,
                    if number == 4 { "old" } else { "current" },
                    status,
                    2,
                );
                if number == 8 {
                    job.findings.total = 1;
                    job.findings.high = 1;
                }
                if number == 11 {
                    job.findings.carried = 2;
                }
                dashboard.jobs.insert(job.id.clone(), job);
            }
        }
        // A newer finished review does not hide any older active review.
        let finished = summary(2, "current", "complete", 3);
        dashboard.jobs.insert(finished.id.clone(), finished);
        // Latest is by queue time, not map/ID order or completion time.
        let old = summary(9, "old", "failed", 1);
        dashboard.jobs.insert(old.id.clone(), old);
        let mut external = summary(1, "current", "running", 4);
        external.source = events::Source::External;
        dashboard.jobs.insert(external.id.clone(), external);
        let mut index = summary(1, "current", "running", 5);
        index.kind = "index".into();
        dashboard.jobs.insert(index.id.clone(), index);
    }
    let (cookie, _) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let value = get(&client, &url, "/api/pulls", &cookie).await;
    assert_eq!(value["pulls"].as_array().unwrap().len(), cases.len());
    for (number, status, state) in cases {
        let pull = value["pulls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["number"] == number)
            .unwrap();
        assert_eq!(pull["review"]["state"], state);
        assert_eq!(pull["trigger"], "off");
        assert_eq!(pull["mode"], "quick");
        assert_eq!(pull["review"]["latest"].is_null(), status.is_none());
        if number == 11 {
            assert_eq!(pull["review"]["latest"]["findings"]["total"], 0);
            assert_eq!(pull["review"]["latest"]["findings"]["carried"], 2);
        }
        assert_eq!(
            pull["review"]["active_job"].is_string(),
            state == "reviewing"
        );
    }
    assert_eq!(value["pulls"][0]["number"], 11);
    server
        .dashboard
        .lock()
        .await
        .preferences
        .revoked_owners
        .push("owner".into());
    assert_eq!(
        get(&client, &url, "/api/pulls", &cookie).await["pulls"],
        json!([])
    );
    assert_eq!(
        client
            .get(format!("{url}/api/repos/owner/repo/pulls/7"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    task.abort();
}

#[test]
fn legacy_finding_counts_default_to_no_carried_findings() {
    let counts: FindingCounts =
        serde_json::from_value(json!({"total": 1, "high": 0, "medium": 1, "low": 0})).unwrap();
    assert_eq!(counts.total, 1);
    assert_eq!(counts.carried, 0);
}

fn conversation() -> Value {
    let author = json!({"login":"revera-test", "__typename":"Bot"});
    let hunk = (1..=15)
        .map(|n| format!("line-{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let comments = json!({"totalCount":101, "nodes":[{"id":"C1", "author":null, "body":"Comment<!-- revera-state:unfinished", "createdAt":"now", "url":"comment-url"}]});
    let reviews = json!({"totalCount":51, "nodes":[{"id":"R1", "author":author, "state":"COMMENTED", "body":"Review<!-- revera-state:one --><!-- revera-state:two -->", "submittedAt":"now", "url":"review-url"}]});
    let threads = json!({"totalCount":101, "nodes":[{
        "id":"T1", "isResolved":true, "isOutdated":true, "path":"src/main.rs", "line":null, "originalLine":42,
        "comments":{"totalCount":51, "nodes":[
            {"id":"TC1", "author":author, "body":"Thread<!-- revera-state:hidden -->", "createdAt":"now", "url":"thread-url", "diffHunk":hunk, "pullRequestReview":{"id":"R1"}},
            {"id":"TC2", "author":{"login":"other[bot]", "__typename":"Bot"}, "body":"Reply", "createdAt":"later", "url":"reply-url", "diffHunk":"not used", "pullRequestReview":{"id":"R2"}}
        ]}
    }, {"id":"T2", "isResolved":false, "isOutdated":false, "path":"file", "line":2, "originalLine":2, "comments":{"totalCount":0, "nodes":[]}}]});
    let pr = json!({
        "number":7, "title":"Title", "body":"Description<!-- revera-state:hidden --> done",
        "url":"https://github.com/owner/repo/pull/7", "state":"OPEN", "isDraft":false, "merged":false, "isCrossRepository":true,
        "createdAt":"2026-10-01T00:00:00Z", "updatedAt":"2026-10-02T00:00:00Z",
        "headRefName":"topic", "baseRefName":"main", "headRefOid":"current",
        "additions":4, "deletions":2, "changedFiles":1, "reviewDecision":"CHANGES_REQUESTED",
        "author":{"login":"human", "__typename":"User"},
        "labels":{"totalCount":21, "nodes":[{"name":"bug", "color":"ff0000"}]},
        "comments": comments, "reviews": reviews, "reviewThreads": threads
    });
    json!({"data":{"repository":{"pullRequest": pr}}})
}

async fn detail_mock(mock: &MockServer, response: ResponseTemplate, count: u64) {
    Mock::given(method("POST")).and(path("/app/installations/9/access_tokens"))
        .and(body_json(json!({"permissions":{"contents":"read", "pull_requests":"read", "metadata":"read"}, "repository_ids":[1]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token":"pull-detail-token"})))
        .expect(count).mount(mock).await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("Authorization", "Bearer pull-detail-token"))
        .and(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            body["variables"] == json!({"owner":"owner", "repo":"repo", "number":7})
                && body["query"]
                    .as_str()
                    .unwrap()
                    .contains("reviewThreads(first: 100)")
        })
        .respond_with(response)
        .expect(count)
        .mount(mock)
        .await;
}

async fn fix_fixture(data: &Path, repo: &events::Repo, number: u64, id: &str) {
    let root = crate::fsutil::ensure_dir(data, &Path::new("fixes").join(id)).unwrap();
    let record = json!({"id":id,"created_at":"2026-10-01T00:00:00Z","updated_at":"2026-10-02T00:00:00Z",
        "state":"discarded","reason":null,"source":"manual","requested_by":"admin","consent":null,
        "repo":repo.full_name,"repo_source":repo.source(),"pr":number,"pr_url":"pr-url","pr_title":"Title",
        "review_job":"job-id","base_sha":"base","head_sha":"current","findings":[{}],
        "attempt":1,"max_attempts":2,"progress":null,"generation":null,"verification":null,"tests":null,
        "patch":null,"usage":null,"delivery":null,"verify_review":null,"events":[]});
    crate::fsutil::write_atomic_in(&root, "fix.json", &serde_json::to_vec(&record).unwrap())
        .unwrap();
    let context = json!({"repo":repo,"findings":[],"policy":fixes::Policy::default(),"key":"key","preview_only":false});
    crate::fsutil::write_atomic_in(
        &root,
        "controller.json",
        &serde_json::to_vec(&context).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn detail_maps_graphql_threads_redacts_bodies_and_joins_live_jobs_fixes_and_policy() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let repo = add_repo(&server).await;
    detail_mock(
        &mock,
        ResponseTemplate::new(200).set_body_json(conversation()),
        2,
    )
    .await;
    fix_fixture(data.path(), &repo, 7, "aa").await;
    fix_fixture(data.path(), &repo, 8, "bb").await;
    let mut other = repo.clone();
    other.id = 2;
    fix_fixture(data.path(), &other, 7, "cc").await;
    *server.fixes.lock().await = fixes::Store::load(data.path()).unwrap();
    let (cookie, _) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let route = "/api/repos/owner/repo/pulls/7";
    let value = get(&client, &url, route, &cookie).await;
    assert_eq!(value["body"], "Description done");
    assert_eq!(value["comments"][0]["body"], "Comment");
    assert_eq!(
        value["comments"][0]["author"],
        json!({"login":null,"bot":false,"revera":false})
    );
    assert_eq!(value["reviews"][0]["body"], "Review");
    assert_eq!(
        value["reviews"][0]["author"],
        json!({"login":"revera-test","bot":true,"revera":true})
    );
    let thread = &value["threads"][0];
    assert_eq!(thread["review_id"], value["reviews"][0]["id"]);
    assert_eq!(thread["is_resolved"], true);
    assert_eq!(thread["is_outdated"], true);
    assert_eq!(thread["original_line"], 42);
    assert!(thread["line"].is_null());
    assert_eq!(thread["comments"][0]["body"], "Thread");
    assert_eq!(thread["comments"][1]["author"]["bot"], true);
    assert_eq!(thread["comments"][1]["author"]["revera"], false);
    let lines: Vec<_> = thread["diff_hunk"].as_str().unwrap().lines().collect();
    assert_eq!(lines.len(), 12);
    assert_eq!(lines[0], "line-4");
    assert_eq!(lines[11], "line-15");
    assert_eq!(thread["truncated"], true);
    assert_eq!(value["threads"][1]["truncated"], false);
    assert!(value["threads"][1]["review_id"].is_null());
    assert_eq!(
        value["truncated"],
        json!({"labels":true,"comments":true,"reviews":true,"threads":true})
    );
    assert!(!value.to_string().contains("revera-state"));
    assert_eq!(
        value["author"],
        json!({"login":"human","bot":false,"revera":false})
    );
    assert_eq!(value["state"], "open");
    assert_eq!(value["fork"], true);
    assert_eq!(value["head_sha"], "current");
    assert_eq!(value["changed_files"], 1);
    assert_eq!(value["fixes"].as_array().unwrap().len(), 1);
    assert_eq!(value["fixes"][0]["id"], "aa");
    assert_eq!(value["fixes"][0]["findings_count"], 1);
    let first = summary(7, "old", "complete", 1);
    let second = summary(7, "current", "running", 2);
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.jobs.insert(first.id.clone(), first.clone());
        dashboard.jobs.insert(second.id.clone(), second.clone());
        dashboard.preferences.default_mode = Mode::Deep;
    }
    let cached = get(&client, &url, route, &cookie).await;
    assert_eq!(cached["jobs"][0], json!(second));
    assert_eq!(cached["jobs"][1], json!(first));
    assert_eq!(cached["review"]["state"], "reviewing");
    assert_eq!(cached["mode"], "deep");
    // A PR webhook invalidates the GitHub snapshot, while jobs/fixes are never stale.
    webhook_pull(&url, "edited", open_pull(7, "current")).await;
    get(&client, &url, route, &cookie).await;
    task.abort();
}

#[tokio::test]
async fn detail_auth_unknown_repo_and_graphql_errors_are_safe() {
    for response in [ResponseTemplate::new(200).set_body_json(json!({"errors":[{"message":"private-upstream-diagnostic"}],"data":{"repository":null}})),
        ResponseTemplate::new(503).set_body_string("private-upstream-diagnostic"),
        ResponseTemplate::new(200).set_body_json(json!({"data":{"repository":{"pullRequest":null}}}))] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        add_repo(&server).await;
        detail_mock(&mock, response, 1).await;
        let (cookie, _) = administrator(&server).await;
        let (url, task) = http(server.clone()).await;
        let client = reqwest::Client::new();
        for route in ["/api/pulls", "/api/repos/owner/repo/pulls/7"] {
            assert_eq!(client.get(format!("{url}{route}")).send().await.unwrap().status(), 401);
        }
        assert_eq!(client.get(format!("{url}/api/repos/owner/unknown/pulls/7")).header("Cookie", &cookie).send().await.unwrap().status(), 404);
        let response = client.get(format!("{url}/api/repos/owner/repo/pulls/7")).header("Cookie", &cookie).send().await.unwrap();
        assert_eq!(response.status(), 502);
        assert_eq!(response.json::<Value>().await.unwrap(), json!({"error":"GitHub pull request unavailable"}));
        server.dashboard.lock().await.preferences.admins.clear();
        for route in ["/api/pulls", "/api/repos/owner/repo/pulls/7"] {
            assert_eq!(client.get(format!("{url}{route}")).header("Cookie", &cookie).send().await.unwrap().status(), 401);
        }
        task.abort();
    }
}

#[tokio::test]
async fn review_returns_real_queued_id_for_forced_same_head_and_rejects_failed_admission() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    add_repo(&server).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token":"pull-review-token"})),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(open_pull(7, "same")))
        .mount(&mock)
        .await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    let route = format!("{url}/api/repos/owner/repo/pulls/7/review");
    let mut previous = String::new();
    for index in 0..3 {
        let response = client
            .post(&route)
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&json!({"mode":"deep"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let value: Value = response.json().await.unwrap();
        let id = value["id"].as_str().unwrap();
        let queue = server.queue.lock().await;
        assert_eq!(queue.pending[&(1, 7, Installed)].id, id);
        assert_eq!(queue.pending[&(1, 7, Installed)].mode, Mode::Deep);
        drop(queue);
        assert_eq!(server.dashboard.lock().await.jobs[id].status, "queued");
        if index == 0 {
            server.queue.lock().await.take().unwrap();
        }
        if index == 2 {
            assert_eq!(
                server.dashboard.lock().await.jobs[&previous].status,
                "cancelled"
            );
        }
        previous = id.into();
    }
    server.stopping.send_replace(true);
    let response = client
        .post(&route)
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    task.abort();
}
