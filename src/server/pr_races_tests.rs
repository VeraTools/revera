use super::*;
use dashboard::{JobSummary, Mode, RepoInfo, Trigger};

async fn deliver(server: &Arc<Server>, pull: Value, changes: Value) {
    let payload = json!({"action": "edited", "installation": {"id": 9},
        "repository": repository(), "number": 7, "pull_request": pull, "changes": changes});
    let body = serde_json::to_vec(&payload).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("X-GitHub-Event", "pull_request".parse().unwrap());
    headers.insert(
        "X-Hub-Signature-256",
        signature(&test_app().webhook_secret, &body)
            .parse()
            .unwrap(),
    );
    assert_eq!(
        webhook(State(server.clone()), headers, Bytes::from(body)).await,
        StatusCode::ACCEPTED
    );
}

fn retargeted(head: &str) -> Value {
    let mut pull = pr(head);
    pull["base"]["ref"] = json!("release");
    pull
}

#[tokio::test]
async fn double_retarget_keeps_replacement_for_cancelled_same_content_runner() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let original = job("same");
    server.enqueue(original.clone()).await;
    let (_, cancel) = server.queue.lock().await.take().unwrap();
    deliver(
        &server,
        retargeted("same"),
        json!({"base": {"ref": {"from": "main"}}}),
    )
    .await;
    let release_id = server.queue.lock().await.pending[&original.key()]
        .id
        .clone();
    deliver(
        &server,
        pr("same"),
        json!({"base": {"ref": {"from": "release"}}}),
    )
    .await;
    assert!(*cancel.borrow());
    let mut queue = server.queue.lock().await;
    let replacement = queue
        .pending
        .get(&original.key())
        .expect("main replacement must stay queued");
    assert_eq!(replacement.base_ref(), "main");
    assert_ne!(replacement.id, original.id);
    let replacement_id = replacement.id.clone();
    assert_eq!(
        server.dashboard.lock().await.jobs[&release_id].status,
        "cancelled"
    );
    queue.running.remove(&original.key());
    assert_eq!(queue.take().unwrap().0.id, replacement_id);
}

#[tokio::test]
async fn auto_retarget_preserves_explicit_requests_despite_automatic_review_filters() {
    for trigger in ["manual", "mention", "rerun"] {
        for running in [false, true] {
            for excluded in ["draft", "skipped_author"] {
                let mock = MockServer::start().await;
                let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
                let mut pull = pr("same");
                if excluded == "draft" {
                    pull["draft"] = json!(true);
                } else {
                    pull["user"]["login"] = json!("dependabot[bot]");
                }
                let mut requested = job("same");
                requested.trigger = trigger.into();
                requested.mode = Mode::Deep;
                requested.kind = events::Kind::Review {
                    number: 7,
                    pr: pull.clone(),
                    force: true,
                };
                server.enqueue(requested.clone()).await;
                if running {
                    server.queue.lock().await.take().unwrap();
                }
                pull["base"]["ref"] = json!("release");
                deliver(&server, pull, json!({"base": {"ref": {"from": "main"}}})).await;
                {
                    let queue = server.queue.lock().await;
                    let replacement = queue
                        .pending
                        .get(&requested.key())
                        .expect("explicit review must survive retarget");
                    assert_eq!(replacement.base_ref(), "release");
                    assert_eq!(replacement.trigger, trigger);
                    assert_eq!(replacement.mode, Mode::Deep);
                    assert!(matches!(
                        replacement.kind,
                        events::Kind::Review { force: true, .. }
                    ));
                    assert_eq!(
                        server.dashboard.lock().await.jobs[&requested.id].status,
                        "cancelled"
                    );
                }
                tokio::task::yield_now().await;
            }
        }
    }
}

#[tokio::test]
async fn purely_automatic_retarget_still_respects_review_filters() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let original = job("same");
    server.enqueue(original.clone()).await;
    let mut pull = retargeted("same");
    pull["draft"] = json!(true);
    deliver(&server, pull, json!({"base": {"ref": {"from": "main"}}})).await;
    assert!(server.queue.lock().await.pending.is_empty());
    assert_eq!(
        server.dashboard.lock().await.jobs[&original.id].status,
        "cancelled"
    );
}

#[test]
fn first_listing_keeps_completed_checkpoint_for_a_pr_excluded_by_observation() {
    let mut seen = reconcile::Seen::default();
    let completed = job("webhook-completed");
    seen.finished(&completed, None);
    let mut untouched = pr("baseline");
    untouched["number"] = json!(8);
    // PR 7 was touched during the listing, so only PR 8 is observed from it.
    assert!(seen.observe(&completed.repo, &[untouched]).is_empty());
    assert!(seen.review_current(&completed));
    assert!(
        seen.observe(&completed.repo, &[pr("webhook-completed")])
            .is_empty()
    );
}

#[tokio::test]
async fn unrelated_pr_webhook_during_listing_keeps_review_and_index_candidates() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let mut repo = job("old").repo;
    repo.pushed_at = "old-push".into();
    let mut other = pr("old");
    other["number"] = json!(8);
    let baseline = [pr("old"), other.clone()];
    assert!(
        server
            .seen
            .lock()
            .await
            .observe(&repo, &baseline)
            .is_empty()
    );
    server.pulls.lock().unwrap().listing(&repo, &baseline);
    other["head"]["sha"] = json!("listed");
    let listed = vec![pr("listed"), other.clone()];
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": 9, "account": {"login": "owner"}}])),
        )
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token": "listing-race-token"})),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/installation/repositories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"repositories": [repository()]})),
        )
        .mount(&mock)
        .await;
    let worker = server.clone();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(move |_: &wiremock::Request| {
            // Same observation/cache updates as a labeled webhook, while the
            // listing request is in flight. Only PR 8's candidate is stale.
            worker
                .queue
                .try_lock()
                .unwrap()
                .observe_pull((1, 8, Installed));
            worker
                .pulls
                .lock()
                .unwrap()
                .webhook(&json!({"action": "labeled", "repository": repository(),
                "installation": {"id": 9}, "number": 8, "pull_request": other}));
            ResponseTemplate::new(200).set_body_json(json!(listed))
        })
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/branches/main"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"commit": {"sha": "b".repeat(40)}})),
        )
        .mount(&mock)
        .await;
    reconcile::once(&server).await.unwrap();
    let queue = server.queue.lock().await;
    assert_eq!(
        queue
            .pending
            .get(&(1, 7, Installed))
            .expect("unrelated review must be admitted")
            .head(),
        "listed"
    );
    assert!(matches!(
        queue.pending.get(&(1, 0, Installed)).map(|job| &job.kind),
        Some(events::Kind::Index)
    ));
    assert!(!queue.contains((1, 8, Installed)));
}

#[tokio::test]
async fn retarget_auto_cancels_running_and_pending_then_reviews_new_base() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let old = job("same");
    server.enqueue(old.clone()).await;
    let (_, cancel) = server.queue.lock().await.take().unwrap();
    let mut pending = job("same");
    if let events::Kind::Review { force, .. } = &mut pending.kind {
        *force = true;
    }
    server.enqueue(pending.clone()).await;
    deliver(
        &server,
        retargeted("same"),
        json!({"base": {"ref": {"from": "main"}}}),
    )
    .await;
    assert!(*cancel.borrow());
    let queue = server.queue.lock().await;
    let replacement = &queue.pending[&old.key()];
    assert_eq!(replacement.base_ref(), "release");
    assert_eq!(replacement.head(), "same");
    assert_eq!(replacement.trigger, "auto");
    let dashboard = server.dashboard.lock().await;
    for id in [&old.id, &pending.id] {
        assert_eq!(dashboard.jobs[id].status, "cancelled");
        assert_eq!(
            dashboard.jobs[id].reason.as_deref(),
            Some("superseded: base changed")
        );
    }
    assert_eq!(
        dashboard.jobs[&replacement.id].base_ref.as_deref(),
        Some("release")
    );
}

#[tokio::test]
async fn retarget_mention_only_requeues_requested_work_and_keeps_its_mode() {
    for state in [
        "none",
        "pending",
        "running",
        "automatic_pending",
        "automatic_running",
    ] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let mut old = job("same");
        old.trigger = if state.starts_with("automatic") {
            "webhook"
        } else {
            "manual"
        }
        .into();
        old.mode = Mode::Deep;
        server
            .pulls
            .lock()
            .unwrap()
            .listing(&old.repo, &[pr("same")]);
        if state != "none" {
            server.enqueue(old.clone()).await;
        }
        let cancel = if state.ends_with("running") {
            Some(server.queue.lock().await.take().unwrap().1)
        } else {
            None
        };
        // Existing automatic work was admitted before mention-only was selected.
        server.dashboard.lock().await.preferences.default_trigger = Trigger::Mention;
        deliver(
            &server,
            retargeted("same"),
            json!({"base": {"ref": {"from": "main"}}}),
        )
        .await;
        let queue = server.queue.lock().await;
        if state == "none" || state.starts_with("automatic") {
            assert!(queue.pending.is_empty());
            if state == "none" {
                assert!(server.dashboard.lock().await.jobs.is_empty());
            } else {
                assert_eq!(
                    server.dashboard.lock().await.jobs[&old.id].status,
                    "cancelled"
                );
            }
        } else {
            let replacement = &queue.pending[&old.key()];
            assert_eq!(replacement.base_ref(), "release");
            assert_eq!(replacement.mode, Mode::Deep);
            assert_eq!(replacement.trigger, "manual");
            assert_eq!(
                server.dashboard.lock().await.jobs[&old.id].status,
                "cancelled"
            );
        }
        if let Some(cancel) = cancel {
            assert!(*cancel.borrow());
        }
        drop(queue);
    }
}

#[tokio::test]
async fn retarget_admits_replacement_when_cancellation_metadata_cannot_be_written() {
    for running in [false, true] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let old = job("same");
        server.enqueue(old.clone()).await;
        let cancel = running.then(|| server.queue.try_lock().unwrap().take().unwrap().1);
        // A directory at meta.json makes this job's atomic metadata write fail,
        // without preventing persistence of the replacement job.
        let metadata = data.path().join("jobs").join(&old.id).join("meta.json");
        std::fs::remove_file(&metadata).unwrap();
        std::fs::create_dir(&metadata).unwrap();
        assert!(server.transition(&old, "cancelled", None).await.is_err());
        server
            .retarget(events::Job::new(
                old.repo.clone(),
                events::Kind::Review {
                    number: 7,
                    pr: retargeted("same"),
                    force: false,
                },
                "webhook",
            ))
            .await
            .unwrap();
        let queue = server.queue.lock().await;
        let replacement = &queue.pending[&old.key()];
        assert_eq!(replacement.base_ref(), "release");
        assert_ne!(replacement.id, old.id);
        assert!(
            server
                .dashboard
                .lock()
                .await
                .jobs
                .contains_key(&replacement.id)
        );
        if let Some(cancel) = cancel {
            assert!(*cancel.borrow());
        }
    }
}

#[test]
fn reconcile_tracks_base_ref_but_not_base_sha_and_keeps_first_discovery_baseline() {
    let mut seen = reconcile::Seen::default();
    let mut current = job("head");
    assert!(seen.observe(&current.repo, &[pr("head")]).is_empty());
    let mut drift = pr("head");
    drift["base"]["sha"] = json!("c".repeat(40));
    assert!(seen.observe(&current.repo, &[drift]).is_empty());
    let next = retargeted("head");
    let jobs = seen.observe(&current.repo, std::slice::from_ref(&next));
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].base_ref(), "release");
    current.kind = events::Kind::Review {
        number: 7,
        pr: next.clone(),
        force: false,
    };
    seen.finished(&current, None);
    assert!(seen.observe(&current.repo, &[next]).is_empty());
}

#[test]
fn queue_same_head_different_base_supersedes_instead_of_coalescing() {
    let mut queue = queue::Queue::default();
    let old = job("head");
    queue.enqueue(old.clone());
    let (_, cancel) = queue.take().unwrap();
    let mut next = job("head");
    next.kind = events::Kind::Review {
        number: 7,
        pr: retargeted("head"),
        force: false,
    };
    queue.enqueue(next.clone());
    assert!(*cancel.borrow());
    assert_eq!(queue.pending[&old.key()].id, next.id);
}

async fn reconcile_candidate(server: &Server) -> (events::Job, queue::ReviewAdmission) {
    let current = job("old");
    let admission = server
        .queue
        .lock()
        .await
        .review_admission((1, 0, Installed));
    let mut seen = server.seen.lock().await;
    assert!(seen.observe(&current.repo, &[pr("old")]).is_empty());
    let candidate = seen.observe(&current.repo, &[pr("listed")]).remove(0);
    server
        .pulls
        .lock()
        .unwrap()
        .listing(&current.repo, &[pr("listed")]);
    (candidate, admission)
}

#[tokio::test]
async fn reconcile_atomic_admission_rejects_newer_webhook_even_after_it_finishes() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (candidate, admission) = reconcile_candidate(&server).await;
    assert!(!server.queue.lock().await.contains(candidate.key()));
    // Reconcile has built the old job, but has not acquired its admission lock.
    let newer = job("webhook");
    {
        let mut queue = server.queue.lock().await;
        queue.observe_pull((1, 7, Installed));
        queue.enqueue(newer.clone());
    }
    assert!(
        !server
            .enqueue_reconcile(candidate.clone(), &admission)
            .await
    );
    assert_eq!(
        server.queue.lock().await.pending[&candidate.key()].id,
        newer.id
    );
    // Even once the new job leaves the queue, the observation still invalidates
    // the stale candidate; checking pending/running alone would admit it.
    server.queue.lock().await.pending.clear();
    assert!(!server.enqueue_reconcile(candidate, &admission).await);
}

#[tokio::test]
async fn reconcile_does_not_readmit_content_completed_after_jobs_were_built() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (candidate, admission) = reconcile_candidate(&server).await;
    {
        let _queue = server.queue.lock().await;
        server.seen.lock().await.finished(&candidate, None);
    }
    assert!(!server.enqueue_reconcile(candidate, &admission).await);
    assert!(server.queue.lock().await.pending.is_empty());
}

#[tokio::test]
async fn close_invalidates_listing_admission_even_for_a_pr_unknown_at_listing_start() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (candidate, admission) = reconcile_candidate(&server).await;
    server.close(candidate.key()).await.unwrap();
    assert!(
        !server
            .enqueue_reconcile(candidate.clone(), &admission)
            .await
    );
    let fresh = server
        .queue
        .lock()
        .await
        .review_admission((1, 0, Installed));
    assert!(server.enqueue_reconcile(candidate, &fresh).await);
}

#[tokio::test]
async fn close_during_check_rerequest_or_manual_fetch_rejects_late_review() {
    for manual in [false, true] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let repo = job("head").repo;
        server.dashboard.lock().await.repos.insert(
            repo.full_name.clone(),
            RepoInfo {
                repo: repo.clone(),
                private: true,
                html_url: String::new(),
                has_override: false,
                open_prs: None,
            },
        );
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"token": "close-fetch-token"})),
            )
            .mount(&mock)
            .await;
        let worker = server.clone();
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/7"))
            .respond_with(move |_: &wiremock::Request| {
                worker.queue.try_lock().unwrap().close((1, 7, Installed));
                ResponseTemplate::new(200).set_body_json(pr("old-open-response"))
            })
            .expect(1)
            .mount(&mock)
            .await;
        if manual {
            let (cookie, csrf) = administrator(&server).await;
            let (url, task) = http(server.clone()).await;
            let response = reqwest::Client::new()
                .post(format!("{url}/api/repos/owner/repo/pulls/7/review"))
                .header("Cookie", cookie)
                .header("X-Revera-CSRF", csrf)
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 409);
            task.abort();
        } else {
            server
                .handle(&test_app(), events::Event::Rerequest(repo, 7))
                .await
                .unwrap();
        }
        assert!(server.queue.lock().await.pending.is_empty());
        assert!(server.dashboard.lock().await.jobs.is_empty());
    }
}

#[tokio::test]
async fn recovery_rechecks_activity_and_patches_only_check_fields() {
    for becomes_active in [true, false] {
        let mock = MockServer::start().await;
        let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let current = job("head");
        let mut summary = JobSummary::queued(&current);
        summary.check = Some(dashboard::CheckRecovery {
            id: 100,
            repo: current.repo.clone(),
            pending: true,
        });
        server.update_job(summary).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"token": "recovery-race-token"})),
            )
            .mount(&mock)
            .await;
        let worker = server.clone();
        let candidate = current.clone();
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/check-runs/100"))
            .respond_with(move |_: &wiremock::Request| {
                if becomes_active {
                    let mut queue = worker.queue.try_lock().unwrap();
                    queue.enqueue(candidate.clone());
                    queue.take().unwrap();
                }
                let mut dashboard = worker.dashboard.try_lock().unwrap();
                let live = dashboard.jobs.get_mut(&candidate.id).unwrap();
                live.set_state(
                    if becomes_active {
                        "running"
                    } else {
                        "complete"
                    },
                    None,
                );
                live.findings.total = 3;
                live.tokens = Some(json!({"prompt": 123}));
                live.check_run_url = Some("https://github.com/check/100".into());
                ResponseTemplate::new(200).set_body_json(json!({"status": "in_progress"}))
            })
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/repos/owner/repo/check-runs/100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 100})))
            .expect(if becomes_active { 0 } else { 1 })
            .mount(&mock)
            .await;
        server.recover_checks().await.unwrap();
        let dashboard = server.dashboard.lock().await;
        let live = &dashboard.jobs[&current.id];
        assert_eq!(
            live.status,
            if becomes_active {
                "running"
            } else {
                "complete"
            }
        );
        assert_eq!(live.findings.total, 3);
        assert_eq!(live.tokens, Some(json!({"prompt": 123})));
        assert_eq!(
            live.check_run_url.as_deref(),
            Some("https://github.com/check/100")
        );
        assert_eq!(live.check.as_ref().unwrap().pending, becomes_active);
    }
}

#[tokio::test]
async fn field_patches_and_transitions_keep_cancellation_and_newer_metadata() {
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let current = job("head");
    server.enqueue(current.clone()).await;
    server
        .transition(
            &current,
            "cancelled",
            Some("superseded: base changed".into()),
        )
        .await
        .unwrap();
    server
        .patch_job(&current.id, |summary| {
            summary.findings.total = 5;
            summary.check_run_url = Some("new-url".into());
            summary.set_state("complete", None);
        })
        .await
        .unwrap();
    server.transition(&current, "running", None).await.unwrap();
    let dashboard = server.dashboard.lock().await;
    let summary = &dashboard.jobs[&current.id];
    assert_eq!(summary.status, "cancelled");
    assert_eq!(summary.reason.as_deref(), Some("superseded: base changed"));
    assert_eq!(summary.findings.total, 5);
    assert_eq!(summary.check_run_url.as_deref(), Some("new-url"));
}

#[tokio::test]
async fn detail_fetch_invalidated_by_webhook_cannot_refill_cache() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mock = MockServer::start().await;
    let (_data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let repo = job("head").repo;
    server.dashboard.lock().await.repos.insert(
        repo.full_name.clone(),
        RepoInfo {
            repo: repo.clone(),
            private: true,
            html_url: String::new(),
            has_override: false,
            open_prs: None,
        },
    );
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"token": "detail-race-token"})),
        )
        .mount(&mock)
        .await;
    let calls = AtomicUsize::new(0);
    let worker = server.clone();
    Mock::given(method("POST")).and(path("/graphql"))
        .respond_with(move |_: &wiremock::Request| {
            let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                worker.pulls.lock().unwrap().webhook(&json!({"action": "synchronize",
                    "repository": repository(), "installation": {"id": 9}, "pull_request": pr("new")}));
            }
            ResponseTemplate::new(200).set_body_json(json!({"data": {"repository": {"pullRequest": {
                "number": 7, "headRefOid": if first { "old" } else { "new" }, "baseRefName": "main", "baseRefOid": "base"
            }}}}))
        }).expect(2).mount(&mock).await;
    for head in ["old", "new", "new"] {
        let value = pulls::detail(
            State(server.clone()),
            axum::extract::Path(("owner".into(), "repo".into(), 7)),
        )
        .await
        .map_err(|error| (error.0, error.1))
        .unwrap()
        .0;
        assert_eq!(value["head_sha"], head);
        assert_eq!(value["base_sha"], "base");
    }
}
