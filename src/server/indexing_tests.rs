use super::*;
use super::{backoff_tests::inventory, regressions::deliver};
use std::time::Duration;

const INDEXED: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEXT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn record_index(seen: &mut reconcile::Seen, repo: &events::Repo, sha: &str) {
    seen.finished(
        &events::Job::new(repo.clone(), events::Kind::Index, "manual"),
        Some(sha),
    );
}

#[test]
fn old_seen_loads_and_first_inventory_preserves_only_successful_index_sha() {
    let repo = job("").repo;
    let old = json!({"1": {"repo": repo, "pushed_at": "one", "heads": {}}});
    let mut seen: reconcile::Seen = serde_json::from_value(old).unwrap();
    assert!(!seen.index_current(1, INDEXED));
    assert!(
        serde_json::to_value(&seen).unwrap()["1"]
            .get("indexed_sha")
            .is_none()
    );
    record_index(&mut seen, &repo, INDEXED);
    assert!(seen.observe(&repo, &[]).is_empty());
    assert!(seen.index_current(1, INDEXED));
    seen.finished(&job(NEXT), Some(NEXT));
    assert!(
        seen.index_current(1, INDEXED),
        "reviews never change indexed_sha"
    );
    assert_eq!(seen.0[&1].heads[&7], NEXT);
    let reloaded: reconcile::Seen =
        serde_json::from_value(serde_json::to_value(&seen).unwrap()).unwrap();
    assert!(reloaded.index_current(1, INDEXED));
}

#[tokio::test]
async fn push_indexing_is_opt_in_and_skips_the_already_indexed_sha() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    record_index(&mut *server.seen.lock().await, &job("").repo, INDEXED);
    for (enabled, after, reference, deleted, queued) in [
        (false, NEXT, "refs/heads/main", false, false),
        (true, NEXT, "refs/heads/main", false, true),
        (true, INDEXED, "refs/heads/main", false, false),
        (true, NEXT, "refs/heads/topic", false, false),
        (true, NEXT, "refs/tags/main", false, false),
        (true, NEXT, "refs/heads/main", true, false),
    ] {
        server.dashboard.lock().await.preferences.index_on_push = enabled;
        server.queue.lock().await.pending.clear();
        let payload = json!({"installation": {"id": 9}, "repository": repository(),
            "ref": reference, "after": after, "deleted": deleted});
        assert_eq!(
            deliver(&server, "push", payload).await,
            StatusCode::ACCEPTED
        );
        let queue = server.queue.lock().await;
        assert_eq!(
            queue.pending.len(),
            usize::from(queued),
            "{enabled}/{after}/{reference}/{deleted}"
        );
        if queued {
            let index = &queue.pending[&(1, 0, Installed)];
            assert!(matches!(index.kind, events::Kind::Index));
            assert_eq!(index.trigger, "webhook");
        }
        assert!(server.seen.lock().await.index_current(1, INDEXED));
    }
    assert!(mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn mention_pr_warmup_uses_default_base_and_reviewability_not_push_preference() {
    for index_on_push in [false, true] {
        for (trigger, action, base, sha, draft, author, expected) in [
            (
                dashboard::Trigger::Mention,
                "opened",
                "main",
                NEXT,
                false,
                "human",
                "index",
            ),
            (
                dashboard::Trigger::Mention,
                "reopened",
                "main",
                NEXT,
                false,
                "human",
                "index",
            ),
            (
                dashboard::Trigger::Mention,
                "ready_for_review",
                "main",
                NEXT,
                false,
                "human",
                "index",
            ),
            (
                dashboard::Trigger::Mention,
                "opened",
                "main",
                INDEXED,
                false,
                "human",
                "none",
            ),
            (
                dashboard::Trigger::Mention,
                "synchronize",
                "main",
                NEXT,
                false,
                "human",
                "none",
            ),
            (
                dashboard::Trigger::Mention,
                "opened",
                "release",
                NEXT,
                false,
                "human",
                "none",
            ),
            (
                dashboard::Trigger::Mention,
                "opened",
                "main",
                NEXT,
                true,
                "human",
                "none",
            ),
            (
                dashboard::Trigger::Mention,
                "opened",
                "main",
                NEXT,
                false,
                "dependabot[bot]",
                "none",
            ),
            (
                dashboard::Trigger::Auto,
                "opened",
                "main",
                NEXT,
                false,
                "human",
                "review",
            ),
            (
                dashboard::Trigger::Off,
                "opened",
                "main",
                NEXT,
                false,
                "human",
                "none",
            ),
        ] {
            let mock = MockServer::start().await;
            let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
            {
                let mut dashboard = server.dashboard.lock().await;
                dashboard.preferences.default_trigger = trigger;
                dashboard.preferences.index_on_push = index_on_push;
            }
            record_index(&mut *server.seen.lock().await, &job("").repo, INDEXED);
            let mut pull = pr(NEXT);
            pull["base"] = json!({"ref": base, "sha": sha});
            pull["draft"] = json!(draft);
            pull["user"]["login"] = json!(author);
            let payload = json!({"installation": {"id": 9}, "repository": repository(),
                "number": 7, "action": action, "pull_request": pull});
            assert_eq!(
                deliver(&server, "pull_request", payload).await,
                StatusCode::ACCEPTED
            );
            let queue = server.queue.lock().await;
            assert_eq!(
                queue.pending.len(),
                usize::from(expected != "none"),
                "{trigger:?}/{action}/{base}/{sha}/{draft}/{author}"
            );
            if expected == "index" {
                let index = &queue.pending[&(1, 0, Installed)];
                assert!(matches!(index.kind, events::Kind::Index));
                assert_eq!(index.trigger, "pull_request");
            } else if expected == "review" {
                assert!(matches!(
                    queue.pending[&(1, 7, Installed)].kind,
                    events::Kind::Review { .. }
                ));
            }
            assert!(mock.received_requests().await.unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn reconcile_checks_default_head_only_for_opted_in_push_candidates() {
    for (enabled, pushed_at, response, queued, lookups) in [
        (false, "two", Some(INDEXED), false, 0),
        (true, "two", Some(INDEXED), false, 1),
        (true, "two", Some(NEXT), true, 1),
        (true, "two", None, true, 1),
        (true, "two", Some("invalid"), true, 1),
        (true, "one", Some(NEXT), false, 0),
    ] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        server.dashboard.lock().await.preferences.index_on_push = enabled;
        let mut value = repository();
        value["default_branch"] = json!("release/with#percent%?name");
        let previous = events::Repo::parse(&value, 9).unwrap();
        {
            let mut seen = server.seen.lock().await;
            seen.observe(&previous, &[]);
            record_index(&mut seen, &previous, INDEXED);
            // Production persists every seen change as it is made.
            reconcile::save(&server, &seen).await.unwrap();
        }
        value["pushed_at"] = json!(pushed_at);
        inventory(&mock, value, vec![], 1).await;
        Mock::given(method("GET"))
            .and(path(
                "/repos/owner/repo/branches/release%2Fwith%23percent%25%3Fname",
            ))
            .respond_with(match response {
                Some(sha) => {
                    ResponseTemplate::new(200).set_body_json(json!({"commit": {"sha": sha}}))
                }
                None => ResponseTemplate::new(503),
            })
            .expect(lookups)
            .mount(&mock)
            .await;
        reconcile::once(&server).await.unwrap();
        let queue = server.queue.lock().await;
        assert_eq!(
            queue.pending.len(),
            usize::from(queued),
            "{enabled}/{pushed_at}/{response:?}"
        );
        if queued {
            assert!(matches!(
                queue.pending[&(1, 0, Installed)].kind,
                events::Kind::Index
            ));
        }
        let saved: reconcile::Seen = dashboard::read_json(data.path(), "seen.json").unwrap();
        assert_eq!(
            saved.0[&1].pushed_at,
            if queued { "one" } else { pushed_at }
        );
        assert!(saved.index_current(1, INDEXED));
    }
}

#[tokio::test]
async fn reconcile_rewrites_seen_state_only_when_a_repository_changed() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    inventory(&mock, repository(), vec![], 2).await;
    reconcile::once(&server).await.unwrap();
    let path = data.path().join("seen.json");
    let saved: reconcile::Seen = dashboard::read_json(data.path(), "seen.json").unwrap();
    assert!(saved.0[&1].initialized);
    std::fs::remove_file(&path).unwrap();
    reconcile::once(&server).await.unwrap();
    assert!(
        !path.exists(),
        "an unchanged pass must not rewrite seen.json"
    );
}

#[tokio::test]
async fn reconcile_retries_a_failed_seen_write_even_without_new_changes() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    inventory(&mock, repository(), vec![], 2).await;
    reconcile::once(&server).await.unwrap();
    let path = data.path().join("seen.json");
    std::fs::remove_file(&path).unwrap();
    // A directory in its place makes the atomic rename fail.
    std::fs::create_dir(&path).unwrap();
    {
        let seen = server.seen.lock().await;
        assert!(reconcile::save(&server, &seen).await.is_err());
    }
    std::fs::remove_dir(&path).unwrap();
    reconcile::once(&server).await.unwrap();
    let saved: reconcile::Seen = dashboard::read_json(data.path(), "seen.json").unwrap();
    assert!(saved.0[&1].initialized);
}

#[tokio::test]
async fn disabled_push_indexing_advances_checkpoints_and_hides_all_retries() {
    for minutes in [0, 20] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        server.dashboard.lock().await.preferences.index_on_push = false;
        let previous = job("").repo;
        server.seen.lock().await.observe(&previous, &[]);
        let mut summary = dashboard::JobSummary::queued(&events::Job::new(
            previous,
            events::Kind::Index,
            "reconcile",
        ));
        summary.set_state("failed", None);
        summary.finished_at =
            Some((chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339());
        server.update_job(summary).await.unwrap();
        let mut value = repository();
        value["pushed_at"] = json!("two");
        inventory(&mock, value, vec![], 2).await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/branches/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"commit": {"sha": NEXT}})),
            )
            .expect(0)
            .mount(&mock)
            .await;
        reconcile::once(&server).await.unwrap();
        assert!(server.queue.lock().await.pending.is_empty());
        let saved: reconcile::Seen = dashboard::read_json(data.path(), "seen.json").unwrap();
        assert_eq!(saved.0[&1].pushed_at, "two");
        let (cookie, _) = administrator(&server).await;
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
        // With no failure history, turning on after the off checkpoint does not backfill.
        server.dashboard.lock().await.jobs.clear();
        server.dashboard.lock().await.preferences.index_on_push = true;
        reconcile::once(&server).await.unwrap();
        assert!(server.queue.lock().await.pending.is_empty());
        task.abort();
    }
}

#[tokio::test]
async fn due_index_retry_checks_sha_even_without_a_new_push() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let repo = job("").repo;
    {
        let mut seen = server.seen.lock().await;
        seen.observe(&repo, &[]);
        record_index(&mut seen, &repo, INDEXED);
    }
    let mut summary =
        dashboard::JobSummary::queued(&events::Job::new(repo, events::Kind::Index, "reconcile"));
    summary.set_state("failed", None);
    summary.finished_at = Some((chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339());
    server.update_job(summary).await.unwrap();
    inventory(&mock, repository(), vec![], 1).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/branches/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"commit": {"sha": INDEXED}})))
        .expect(1)
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    assert!(server.queue.lock().await.pending.is_empty());
    assert_eq!(server.seen.lock().await.0[&1].pushed_at, "one");
}

#[tokio::test]
async fn index_on_push_defaults_false_and_settings_api_round_trips_old_files() {
    assert!(!dashboard::Preferences::default().index_on_push);
    let mock = MockServer::start().await;
    let (data, _) = server(&mock, runner::Runner::current().unwrap(), true).await;
    crate::fsutil::write_atomic_in(
        data.path(),
        "settings.json",
        br#"{"admins":["admin"],"default_trigger":"mention","review_drafts":true}"#,
    )
    .unwrap();
    let server = initialize(
        settings(data.path()),
        runner::Runner::current().unwrap(),
        app::Api::new(mock.uri()).unwrap(),
    )
    .await
    .unwrap();
    assert!(!server.dashboard.lock().await.preferences.index_on_push);
    assert!(server.dashboard.lock().await.preferences.review_drafts);
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let client = reqwest::Client::new();
    for enabled in [false, true, false] {
        let mut preferences: Value = client
            .get(format!("{url}/api/settings/server"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if !enabled {
            assert!(preferences["index_on_push"].is_boolean());
        }
        preferences["index_on_push"] = json!(enabled);
        assert_eq!(
            client
                .put(format!("{url}/api/settings/server"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .json(&preferences)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let settings: Value = client
            .get(format!("{url}/api/settings"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let get: Value = client
            .get(format!("{url}/api/settings/server"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(get["index_on_push"], enabled);
        assert_eq!(settings["server"]["index_on_push"], enabled);
        assert!(get["review_drafts"].as_bool().unwrap());
        let reloaded =
            dashboard::Dashboard::load(data.path(), &reconcile::Seen::default()).unwrap();
        assert_eq!(reloaded.preferences.index_on_push, enabled);
    }
    task.abort();
}

#[tokio::test]
async fn manual_index_runs_with_push_indexing_off_and_even_when_sha_is_current() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    record_index(&mut *server.seen.lock().await, &job("").repo, INDEXED);
    inventory(&mock, repository(), vec![], 1).await;
    reconcile::once(&server).await.unwrap();
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    for enabled in [false, true] {
        server.dashboard.lock().await.preferences.index_on_push = enabled;
        server.queue.lock().await.pending.clear();
        assert_eq!(
            reqwest::Client::new()
                .post(format!("{url}/api/repos/owner/repo/index"))
                .header("Cookie", &cookie)
                .header("X-Revera-CSRF", &csrf)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let queue = server.queue.lock().await;
        assert!(matches!(
            queue.pending[&(1, 0, Installed)].kind,
            events::Kind::Index
        ));
        assert_eq!(queue.pending[&(1, 0, Installed)].trigger, "manual");
    }
    task.abort();
}

#[tokio::test]
async fn only_successful_index_records_actual_fetch_sha_and_stale_checkout_locks_recover() {
    for outcome in ["success", "no-index", "failure", "cancelled"] {
        let mock = MockServer::start().await;
        let scratch = tempfile::tempdir().unwrap();
        let marker = scratch.path().join("indexed-head");
        let command = format!(
            "git rev-parse HEAD > '{}.tmp' && mv '{}.tmp' '{}'; {}",
            marker.display(),
            marker.display(),
            marker.display(),
            match outcome {
                "success" => "mkdir -p .vera && echo '{}' > .vera/vectors.manifest; exit 0",
                "no-index" => "exit 0",
                "failure" => "exit 1",
                _ => "sleep 60",
            }
        );
        let runner = runner::Runner {
            executable: "/bin/sh".into(),
            prefix: vec!["-c".into(), command, "fixture".into()],
        };
        let (data, server) = server(&mock, runner, true).await;
        let source = scratch.path().join("source");
        std::fs::create_dir(&source).unwrap();
        git(&source, &["init", "-qb", "main"]);
        std::fs::write(source.join("file"), "indexed content").unwrap();
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
        let ws = crate::fsutil::ensure_dir(data.path(), Path::new("repos/owner/repo")).unwrap();
        git(
            &ws,
            &["clone", "--no-checkout", source.to_str().unwrap(), "."],
        );
        for name in ["index.lock", "HEAD.lock", "config.lock", "packed-refs.lock"] {
            std::fs::write(ws.join(".git").join(name), "keep non-checkout locks").unwrap();
        }
        let previous = job("").repo;
        {
            let mut seen = server.seen.lock().await;
            seen.observe(&previous, &[]);
            record_index(&mut seen, &previous, INDEXED);
            reconcile::save(&server, &seen).await.unwrap();
        }
        let mut repo = previous;
        repo.pushed_at = "two".into();
        let current = events::Job::new(repo, events::Kind::Index, "manual");
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "index-token"})))
            .expect(1)
            .mount(&mock)
            .await;
        server.enqueue(current.clone()).await;
        let (running, cancel) = server.queue.lock().await.take().unwrap();
        let worker = server.clone();
        let task = tokio::spawn(async move {
            runner::run(worker, &running, cancel).await;
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while !marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if outcome == "cancelled" {
            let queue = server.queue.lock().await;
            queue.running[&current.key()].cancel.send(true).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), head);
        let success = matches!(outcome, "success" | "no-index");
        let seen: reconcile::Seen = dashboard::read_json(data.path(), "seen.json").unwrap();
        // Success records the fetched commit only when an index exists (Vera
        // disabled exits 0 without one); a failed run may have discarded the
        // previous index, so it forgets the commit; cancellation keeps it.
        match outcome {
            "success" => assert!(seen.index_current(1, &head)),
            "no-index" | "cancelled" => assert!(seen.index_current(1, INDEXED)),
            _ => assert_eq!(seen.0[&1].indexed_sha, None),
        }
        assert_eq!(seen.0[&1].pushed_at, if success { "two" } else { "one" });
        assert_eq!(
            job_metadata(data.path(), &current)["status"],
            match outcome {
                "success" | "no-index" => "complete",
                "failure" => "failed",
                _ => "cancelled",
            }
        );
        for name in ["index.lock", "HEAD.lock"] {
            assert!(!ws.join(".git").join(name).exists());
        }
        for name in ["config.lock", "packed-refs.lock"] {
            assert_eq!(
                std::fs::read_to_string(ws.join(".git").join(name)).unwrap(),
                "keep non-checkout locks"
            );
        }
    }
}
