use super::*;
use std::{collections::BTreeMap, sync::Mutex as StdMutex, time::Duration};

fn installation(id: u64, login: &str, account: u64) -> Value {
    json!({"id": id, "account": {"login": login, "id": account, "type": "Organization"},
        "repository_selection": "selected", "suspended_at": null,
        "permissions": {"contents": "read", "pull_requests": "write", "checks": "write", "metadata": "read", "issues": "read"},
        "html_url": format!("https://github.com/organizations/{login}/settings/installations/{id}")})
}

fn repo_value(id: u64, owner: &str, name: &str) -> Value {
    json!({"id": id, "full_name": format!("{owner}/{name}"), "owner": {"login": owner},
        "default_branch": "main", "pushed_at": "one", "private": true,
        "html_url": format!("https://github.com/{owner}/{name}")})
}

struct InventoryFixture {
    installations: Arc<StdMutex<Vec<Value>>>,
    repositories: Arc<StdMutex<BTreeMap<u64, Vec<Value>>>>,
}

impl InventoryFixture {
    async fn mount(mock: &MockServer, installations: Vec<Value>) -> Self {
        let result = Self {
            installations: Arc::new(StdMutex::new(installations)),
            repositories: Arc::new(StdMutex::new(BTreeMap::new())),
        };
        let values = result.installations.clone();
        Mock::given(method("GET"))
            .and(path("/app/installations"))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(values.lock().unwrap().clone())
            })
            .mount(mock)
            .await;
        for id in [9, 10, 11, 12] {
            Mock::given(method("POST"))
                .and(path(format!("/app/installations/{id}/access_tokens")))
                .respond_with(
                    ResponseTemplate::new(201)
                        .set_body_json(json!({"token": format!("inventory-{id}")})),
                )
                .mount(mock)
                .await;
            let repos = result.repositories.clone();
            Mock::given(method("GET")).and(path("/installation/repositories"))
                .and(header("Authorization", format!("Bearer inventory-{id}")))
                .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({"repositories": repos.lock().unwrap().get(&id).cloned().unwrap_or_default()})))
                .mount(mock).await;
        }
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(
                r"^/repos/[^/]+/[^/]+/pulls$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(mock)
            .await;
        result
    }
}

async fn api_get(url: &str, route: &str, cookie: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("{url}{route}"))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn owner_change(url: &str, login: &str, approve: bool, cookie: &str, csrf: &str) -> Value {
    let response = reqwest::Client::new()
        .request(
            if approve {
                reqwest::Method::PUT
            } else {
                reqwest::Method::DELETE
            },
            format!("{url}/api/owners/{login}"),
        )
        .header("Cookie", cookie)
        .header("X-Revera-CSRF", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["ok"], true);
    assert!(value["warning"].is_null(), "{value}");
    value
}

#[tokio::test]
async fn listing_classifies_grants_suspension_and_absent_owners_without_private_repo_names() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let mut upgrade = installation(10, "upgrade", 110);
    upgrade["permissions"]["contents"] = json!("read");
    let mut suspended = installation(11, "paused", 111);
    suspended["suspended_at"] = json!("2026-10-18T00:00:00Z");
    let fixture = InventoryFixture::mount(
        &mock,
        vec![
            installation(9, "owner", 109),
            upgrade,
            suspended,
            installation(12, "outside", 112),
        ],
    )
    .await;
    fixture
        .repositories
        .lock()
        .unwrap()
        .insert(9, vec![repository()]);
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.preferences.allowed_owners =
            vec!["UPGRADE".into(), "paused".into(), "absent".into()];
    }
    Mock::given(method("GET")).and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"owner": {"type": "Organization", "login": "owner"}, "permissions": {"contents": "write"}}))).with_priority(1).mount(&mock).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/12/access_tokens"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    let (cookie, _) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    assert_eq!(
        reqwest::get(format!("{url}/api/installations"))
            .await
            .unwrap()
            .status(),
        401
    );
    let value = api_get(&url, "/api/installations", &cookie).await;
    let accounts: BTreeMap<_, _> = value["installations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["account"]["login"].as_str().unwrap(), v))
        .collect();
    assert_eq!(accounts["outside"]["status"], "awaiting_approval");
    assert!(accounts["outside"].get("repo_count").is_none());
    assert_eq!(accounts["upgrade"]["status"], "permission_update");
    assert_eq!(accounts["upgrade"]["permissions"]["contents"], "read");
    assert_eq!(accounts["paused"]["status"], "suspended");
    assert_eq!(accounts["absent"]["status"], "not_installed");
    assert_eq!(
        value["settings_url"],
        "https://github.com/organizations/owner/settings/apps/revera-test/permissions"
    );
    assert_eq!(
        api_get(&url, "/api/repos", &cookie)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(server.queue.lock().await.pending.is_empty());
    assert!(
        !value.to_string().contains("owner/repo"),
        "Account API contains metadata, not repository names"
    );
    task.abort();
}

#[tokio::test]
async fn approve_revoke_enforces_admin_csrf_cancels_jobs_and_removes_inventory_durably() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let fixture = InventoryFixture::mount(&mock, vec![installation(10, "second", 110)]).await;
    fixture
        .repositories
        .lock()
        .unwrap()
        .insert(10, vec![repo_value(2, "second", "private-repo")]);
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    for (cookie, csrf, expected) in [("", "", 401), (cookie.as_str(), "", 403)] {
        let response = reqwest::Client::new()
            .put(format!("{url}/api/owners/second"))
            .header("Cookie", cookie)
            .header("X-Revera-CSRF", csrf)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    reconcile::once(&server).await.unwrap();
    assert!(
        api_get(&url, "/api/repos", &cookie)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let payload = json!({"action": "opened", "installation": {"id": 10}, "repository": repo_value(2, "second", "private-repo"), "number": 7, "pull_request": pr("unapproved")});
    assert_eq!(post_webhook(&url, "pull_request", &payload).await, 202);
    assert!(server.queue.lock().await.pending.is_empty());
    owner_change(&url, "SECOND", true, &cookie, &csrf).await;
    assert_eq!(server.dashboard.lock().await.repos.len(), 1);
    let repo = server.dashboard.lock().await.repos["second/private-repo"]
        .repo
        .clone();
    let running = events::Job::new(
        repo.clone(),
        events::Kind::Review {
            number: 7,
            pr: pr("allowed"),
            force: true,
        },
        "manual",
    );
    server.enqueue(running.clone()).await;
    let (_, cancel) = server.queue.lock().await.take().unwrap();
    server.transition(&running, "running", None).await.unwrap();
    let pending = events::Job::new(repo, events::Kind::Index, "manual");
    server.enqueue(pending.clone()).await;
    owner_change(&url, "second", false, &cookie, &csrf).await;
    assert!(*cancel.borrow());
    for job in [&running, &pending] {
        assert_eq!(job_metadata(data.path(), job)["status"], "cancelled");
    }
    assert!(
        api_get(&url, "/api/repos", &cookie)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let inventory: Value =
        serde_json::from_slice(&std::fs::read(data.path().join("inventory.json")).unwrap())
            .unwrap();
    assert_eq!(inventory["repos"], json!({}));
    // A late detached lookup cannot re-admit work after revocation.
    server
        .enqueue(events::Job::new(
            running.repo.clone(),
            running.kind.clone(),
            "manual",
        ))
        .await;
    assert!(
        server
            .queue
            .lock()
            .await
            .pending
            .values()
            .all(|j| matches!(j.kind, events::Kind::Remove))
    );
    let preferences: dashboard::Preferences =
        serde_json::from_slice(&std::fs::read(data.path().join("settings.json")).unwrap()).unwrap();
    assert!(!preferences.owner_allowed("renamed", Some(110), false));
    task.abort();
}

async fn post_webhook(url: &str, event: &str, value: &Value) -> u16 {
    let body = serde_json::to_vec(value).unwrap();
    reqwest::Client::new()
        .post(format!("{url}/webhook"))
        .header("X-GitHub-Event", event)
        .header(
            "X-Hub-Signature-256",
            signature(&test_app().webhook_secret, &body),
        )
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn account_id_survives_rename_selected_repo_change_uninstall_and_permission_upgrade() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let fixture = InventoryFixture::mount(&mock, vec![installation(9, "owner", 109)]).await;
    fixture.repositories.lock().unwrap().insert(
        9,
        vec![
            repo_value(1, "owner", "first"),
            repo_value(2, "owner", "second"),
        ],
    );
    reconcile::once(&server).await.unwrap();
    assert_eq!(server.dashboard.lock().await.repos.len(), 2);
    let running_repo = server.dashboard.lock().await.repos["owner/second"]
        .repo
        .clone();
    let running = events::Job::new(
        running_repo,
        events::Kind::Review {
            number: 7,
            pr: pr("rename-running"),
            force: true,
        },
        "manual",
    );
    server.enqueue(running).await;
    let (_, cancelled) = server.queue.lock().await.take().unwrap();
    fixture.installations.lock().unwrap()[0]["account"]["login"] = json!("renamed");
    fixture
        .repositories
        .lock()
        .unwrap()
        .insert(9, vec![repo_value(2, "renamed", "second")]);
    reconcile::once(&server).await.unwrap();
    assert!(
        *cancelled.borrow(),
        "Renaming an owner cancels work using its old workspace identity"
    );
    assert!(
        server
            .allowed_account(&test_app(), "renamed", Some(109))
            .await
    );
    assert!(
        !server
            .allowed_account(&test_app(), "owner", Some(999))
            .await
    );
    assert!(
        server
            .dashboard
            .lock()
            .await
            .repos
            .contains_key("renamed/second")
    );
    assert!(
        !server
            .dashboard
            .lock()
            .await
            .repos
            .contains_key("owner/first")
    );
    assert!(matches!(
        server.queue.lock().await.pending[&(1, 0, Installed)].kind,
        events::Kind::Remove
    ));
    Mock::given(method("GET"))
        .and(path("/app"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"owner": {"type": "User"}, "permissions": {"contents": "write"}}),
        ))
        .with_priority(1)
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    assert_eq!(
        server.installations.lock().await.installations[0].status,
        "permission_update"
    );
    fixture.installations.lock().unwrap()[0]["permissions"]["contents"] = json!("write");
    reconcile::once(&server).await.unwrap();
    assert_eq!(
        server.installations.lock().await.installations[0].status,
        "allowed"
    );
    assert_eq!(
        server.installations.lock().await.installations[0].permissions["contents"],
        "write"
    );
    fixture.installations.lock().unwrap().clear();
    reconcile::once(&server).await.unwrap();
    assert!(server.dashboard.lock().await.repos.is_empty());
    assert!(
        server
            .installations
            .lock()
            .await
            .installations
            .iter()
            .all(|i| i.status == "not_installed")
    );
    assert_eq!(
        server.installations.lock().await.installations.len(),
        1,
        "Old login aliases do not produce ghost accounts"
    );
    assert!(matches!(
        server.queue.lock().await.pending[&(2, 0, Installed)].kind,
        events::Kind::Remove
    ));
}

#[tokio::test]
async fn access_denial_removes_inventory_but_temporary_outage_preserves_it() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let fixture = InventoryFixture::mount(&mock, vec![installation(9, "owner", 109)]).await;
    fixture
        .repositories
        .lock()
        .unwrap()
        .insert(9, vec![repository()]);
    reconcile::once(&server).await.unwrap();
    let response_status = Arc::new(std::sync::atomic::AtomicU16::new(503));
    let mock_status = response_status.clone();
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(mock_status.load(std::sync::atomic::Ordering::Relaxed))
        })
        .with_priority(1)
        .mount(&mock)
        .await;
    for status in [503, 403] {
        response_status.store(status, std::sync::atomic::Ordering::Relaxed);
        reconcile::once(&server).await.unwrap();
        assert_eq!(
            server.dashboard.lock().await.repos.is_empty(),
            status == 403
        );
        assert!(
            server.installations.lock().await.installations[0]
                .error
                .as_ref()
                .unwrap()
                .contains(&status.to_string())
        );
        // List refreshes retain an access error until an actual inventory retry succeeds.
        let listed = reconcile::discover(&server, &test_app()).await.unwrap();
        assert!(listed.installations[0].error.is_some());
    }
}

#[tokio::test]
async fn revoking_implicit_owner_survives_legacy_settings_saves_and_github_outage() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let fixture = InventoryFixture::mount(&mock, vec![installation(9, "owner", 109)]).await;
    fixture
        .repositories
        .lock()
        .unwrap()
        .insert(9, vec![repository()]);
    reconcile::once(&server).await.unwrap();
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&mock)
        .await;
    let response: Value = reqwest::Client::new()
        .delete(format!("{url}/api/owners/owner"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    assert!(response["warning"].as_str().unwrap().contains("503"));
    assert!(!server.allowed_owner(&test_app(), "owner").await);
    assert!(server.dashboard.lock().await.repos.is_empty());
    let mut legacy = serde_json::to_value(&server.dashboard.lock().await.preferences).unwrap();
    for field in ["owner_accounts", "revoked_owners", "revoked_owner_ids"] {
        legacy.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(
        reqwest::Client::new()
            .put(format!("{url}/api/settings/server"))
            .header("Cookie", &cookie)
            .header("X-Revera-CSRF", &csrf)
            .json(&legacy)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(!server.allowed_owner(&test_app(), "owner").await);
    let loaded: dashboard::Preferences =
        serde_json::from_slice(&std::fs::read(data.path().join("settings.json")).unwrap()).unwrap();
    assert!(!loaded.owner_allowed("owner", Some(109), true));
    task.abort();
}

#[tokio::test]
async fn lifecycle_webhooks_are_verified_coalesced_and_do_not_enqueue_unapproved_jobs() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let fixture = InventoryFixture::mount(&mock, vec![installation(12, "outside", 112)]).await;
    fixture
        .repositories
        .lock()
        .unwrap()
        .insert(12, vec![repo_value(12, "outside", "never-disclose")]);
    Mock::given(method("POST"))
        .and(path("/app/installations/12/access_tokens"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&mock)
        .await;
    let worker = tokio::spawn(reconcile::loop_forever(server.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.installations.lock().await.install_url.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (url, task) = http(server.clone()).await;
    let generation = *server.reconcile_request.borrow();
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{url}/webhook"))
            .header("X-GitHub-Event", "installation")
            .header("X-Hub-Signature-256", "sha256=00")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(*server.reconcile_request.borrow(), generation);
    for (event, actions) in [
        (
            "installation",
            vec![
                "created",
                "deleted",
                "suspend",
                "unsuspend",
                "new_permissions_accepted",
            ],
        ),
        ("installation_repositories", vec!["added", "removed"]),
        ("installation_target", vec!["renamed"]),
    ] {
        for action in actions {
            let payload = json!({"action": action, "installation": installation(12, "outside", 112), "account": {"id": 112, "login": "outside"}, "repositories_added": [repo_value(12, "outside", "never-disclose")]});
            assert_eq!(post_webhook(&url, event, &payload).await, 202);
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = mock
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path() == "/app/installations")
                .count();
            if count >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    let count = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/app/installations")
        .count();
    assert_eq!(count, 2, "startup plus one coalesced refresh");
    assert!(server.queue.lock().await.pending.is_empty());
    assert!(server.dashboard.lock().await.repos.is_empty());
    worker.abort();
    task.abort();
}

#[tokio::test]
async fn suspension_and_uninstall_cancel_immediately_and_block_late_jobs() {
    for action in ["suspend", "deleted"] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let fixture = InventoryFixture::mount(&mock, vec![installation(9, "owner", 109)]).await;
        fixture
            .repositories
            .lock()
            .unwrap()
            .insert(9, vec![repository()]);
        reconcile::once(&server).await.unwrap();
        let running = job("head");
        server.enqueue(running.clone()).await;
        let (_, cancel) = server.queue.lock().await.take().unwrap();
        let (url, task) = http(server.clone()).await;
        assert_eq!(
            post_webhook(
                &url,
                "installation",
                &json!({"action": action, "installation": installation(9, "owner", 109)})
            )
            .await,
            202
        );
        assert!(*cancel.borrow());
        assert_eq!(job_metadata(data.path(), &running)["status"], "cancelled");
        assert!(server.dashboard.lock().await.repos.is_empty());
        let mut late = job("late");
        late.kind = events::Kind::Review {
            number: 8,
            pr: pr("late"),
            force: false,
        };
        server.enqueue(late.clone()).await;
        assert!(!server.queue.lock().await.pending.contains_key(&late.key()));
        if action == "suspend" {
            fixture.installations.lock().unwrap()[0]["suspended_at"] =
                json!("2026-10-18T00:00:00Z");
        } else {
            fixture.installations.lock().unwrap().clear();
        }
        reconcile::once(&server).await.unwrap();
        assert!(server.dashboard.lock().await.repos.is_empty());
        task.abort();
    }
}

#[tokio::test]
async fn manifest_setup_choices_are_explicit_and_default_to_private_read_only() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), false).await;
    let token = server.setup.lock().await.token.clone().unwrap();
    let (url, task) = http(server).await;
    for (query, public, contents) in [
        ("", false, "read"),
        ("&public=true&publish_fixes=true", true, "write"),
    ] {
        let value: Value = reqwest::get(format!("{url}/api/setup/manifest?token={token}{query}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(value["manifest"]["public"], public);
        assert_eq!(
            value["manifest"]["default_permissions"]["contents"],
            contents
        );
        assert_eq!(
            value["manifest"]["setup_url"],
            "https://revera.example.com/repositories"
        );
        assert_eq!(value["manifest"]["setup_on_update"], true);
    }
    task.abort();
}

#[tokio::test]
async fn legacy_single_account_settings_and_inventory_load_unchanged() {
    let mock = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let legacy = json!({"admins": ["admin"], "allowed_owners": ["SECOND"], "default_trigger": "mention", "default_mode": "standard", "repos": {}, "review_drafts": false, "skip_authors": []});
    crate::fsutil::write_atomic_in(
        data.path(),
        "settings.json",
        &serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    crate::fsutil::write_atomic_in(data.path(), "inventory.json", &serde_json::to_vec(&json!({"installations": 1, "repos": {"owner/repo": {"repo": job("head").repo, "private": true, "html_url": "https://github.com/owner/repo", "has_override": false}}})).unwrap()).unwrap();
    crate::fsutil::write_secret_atomic(
        data.path(),
        "app.json",
        &serde_json::to_vec(&test_app()).unwrap(),
    )
    .unwrap();
    let server = initialize(
        settings(data.path()),
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    assert!(server.allowed_owner(&test_app(), "owner").await);
    assert!(server.allowed_owner(&test_app(), "second").await);
    assert!(!server.allowed_owner(&test_app(), "outside").await);
    assert_eq!(server.dashboard.lock().await.repos.len(), 1);
    let loaded: Value =
        serde_json::from_slice(&std::fs::read(data.path().join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        loaded, legacy,
        "Loading 0.7 settings requires no rewrite or manual migration"
    );
}
