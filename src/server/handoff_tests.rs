use super::*;

#[tokio::test]
async fn accepted_detail_diagnostics_download_and_cached_staleness() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let reviewed = "a".repeat(40);
    let mut summary = dashboard::JobSummary::queued(&job(&reviewed));
    summary.set_state("complete", None);
    let id = summary.id.clone();
    let info: dashboard::RepoInfo = serde_json::from_value(
        json!({"repo": job("").repo, "private": false, "html_url": "", "has_override": false}),
    )
    .unwrap();
    {
        let mut dashboard = server.dashboard.lock().await;
        dashboard.repos.insert("owner/repo".into(), info);
        dashboard.jobs.insert(id.clone(), summary);
    }
    Mock::given(method("POST"))
        .and(path("/app/installations/9/access_tokens"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"token": "handoff-installation-token"})),
        )
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/7"))
        .and(header("authorization", "Bearer handoff-installation-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr(&"c".repeat(40))))
        .expect(1)
        .mount(&mock)
        .await;
    let secret = "api-handoff-fixture-secret-79461372";
    crate::redact::register(secret);
    let accepted = json!({"defect_key": "guard", "severity": "high", "title": "Accepted defect", "file": "src/a b.rs", "start_line": 2, "end_line": 3,
        "claim": format!("Claim {secret}"), "trigger": "Empty input", "impact": "Panic", "supporting_evidence": [{"path": "src/a b.rs", "start_line": 2, "end_line": 3, "note": "Evidence"}],
        "validation_status": "accepted", "validated_fix": "Add a guard", "suggested_fix": "DO NOT PUBLISH"});
    let report = json!({"head": reviewed, "base": "b".repeat(40), "status": "complete", "plan": {"summary_markdown": "Review<!-- revera-state:hidden -->"}, "findings": [accepted,
        {"defect_key": "rejected", "severity": "high", "file": "x", "title": "Rejected candidate", "validation_status": "rejected", "rationale": "Already guarded", "suggested_fix": "unsafe", "validated_fix": "unsafe"},
        {"defect_key": "uncertain", "severity": "low", "file": "x", "title": "Uncertain candidate", "validation_status": "uncertain"}]});
    let dir = dashboard::job_dir(data.path(), &id).unwrap();
    crate::fsutil::ensure_dir(data.path(), &Path::new("jobs").join(&id)).unwrap();
    crate::fsutil::write_atomic_in(&dir, "report.json", &serde_json::to_vec(&report).unwrap())
        .unwrap();
    let (url, task) = http(server).await;
    let client = reqwest::Client::new();
    let endpoint = format!("{url}/api/jobs/{id}");
    let response = client.get(&endpoint).send().await.unwrap();
    assert_eq!(response.status(), 401);
    let detail: Value = client
        .get(&endpoint)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["findings"].as_array().unwrap().len(), 1);
    assert!(detail["carried"].as_array().unwrap().is_empty());
    assert!(detail.get("diagnostics").is_none());
    assert_eq!(
        detail["counts"],
        json!({"total": 1, "high": 1, "medium": 0, "low": 0})
    );
    assert_eq!(detail["job"]["findings"], detail["counts"]);
    assert_eq!(detail["stale"], true);
    let f = &detail["findings"][0];
    assert_eq!(f["id"], crate::findings::finding_id("src/a b.rs", "guard"));
    assert_eq!(
        f["html_url"],
        format!("https://github.com/owner/repo/blob/{reviewed}/src/a%20b.rs#L2-L3")
    );
    for field in [
        "defect_key",
        "severity",
        "title",
        "claim",
        "file",
        "start_line",
        "end_line",
        "trigger",
        "impact",
        "supporting_evidence",
        "validated_fix",
        "reviewed_head_sha",
        "reviewed_base_sha",
        "prompt",
    ] {
        assert!(!f[field].is_null(), "{field}");
    }
    assert_eq!(f["reviewed_head_sha"], reviewed);
    assert_eq!(f["reviewed_base_sha"], "b".repeat(40));
    assert_eq!(f["claim"], format!("Claim {}", crate::redact::text(secret)));
    assert!(f.get("suggested_fix").is_none());
    let text = detail.to_string();
    for forbidden in [
        secret,
        "DO NOT PUBLISH",
        "Rejected candidate",
        "Uncertain candidate",
        "revera-state",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    assert!(
        detail["instructions"]
            .as_str()
            .unwrap()
            .contains("Potentially stale")
    );
    assert!(
        detail["instructions"]
            .as_str()
            .unwrap()
            .contains(crate::handoff::WORKING_RULES)
    );
    let diagnostics: Value = client
        .get(format!("{endpoint}?diagnostics=1"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diagnostics["diagnostics"].as_array().unwrap().len(), 2);
    assert_eq!(diagnostics["diagnostics"][0]["status"], "rejected");
    assert_eq!(diagnostics["diagnostics"][1]["status"], "uncertain");
    assert!(diagnostics["diagnostics"][0].get("validated_fix").is_none());
    for (query, expected) in [
        (String::new(), detail["instructions"].as_str().unwrap()),
        (
            format!("?finding={}", &f["id"].as_str().unwrap()[..6]),
            f["prompt"].as_str().unwrap(),
        ),
    ] {
        let response = client
            .get(format!("{endpoint}/instructions.md{query}"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "text/markdown; charset=utf-8"
        );
        assert!(
            response.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .contains("attachment")
        );
        assert_eq!(response.text().await.unwrap(), expected);
    }
    for (query, status) in [("?finding=no", 422), ("?finding=000000", 404)] {
        let response = client
            .get(format!("{endpoint}/instructions.md{query}"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    task.abort();
}

#[tokio::test]
async fn carried_detail_and_downloads_are_separate_from_accepted_counts() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, _) = administrator(&server).await;
    let head = "a".repeat(40);
    let mut summary = dashboard::JobSummary::queued(&job(&head));
    summary.set_state("complete", None);
    let id = summary.id.clone();
    server
        .dashboard
        .lock()
        .await
        .jobs
        .insert(id.clone(), summary);
    let secret = "carried-api-secret-786456354";
    crate::redact::register(secret);
    let f = serde_json::from_value(json!({"defect_key":"old-guard","severity":"high","file":"bug.rs","start_line":2,"end_line":3,
        "title":"Earlier finding","claim":format!("Claim {secret}"),"trigger":"Empty input","impact":"Panic",
        "supporting_evidence":[{"path":"caller.rs","start_line":8,"end_line":8,"note":"Caller"}],"validation_status":"accepted"})).unwrap();
    let mut state = crate::state::ReviewState::default();
    state.upsert(&f, crate::state::FindingState::Open);
    state.mark_posted(&[f.id()]);
    let carried = crate::report::carried_findings(&state, &[]);
    let report = json!({"head":head,"base":"b".repeat(40),"status":"complete","findings":[],"carried":carried});
    let dir = dashboard::job_dir(data.path(), &id).unwrap();
    crate::fsutil::ensure_dir(data.path(), &Path::new("jobs").join(&id)).unwrap();
    crate::fsutil::write_atomic_in(&dir, "report.json", &serde_json::to_vec(&report).unwrap())
        .unwrap();
    let (url, task) = http(server).await;
    let client = reqwest::Client::new();
    let endpoint = format!("{url}/api/jobs/{id}");
    let detail: Value = client
        .get(&endpoint)
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(detail["findings"].as_array().unwrap().is_empty());
    assert_eq!(detail["counts"]["total"], 0);
    assert_eq!(detail["job"]["findings"], detail["counts"]);
    assert_eq!(detail["carried"].as_array().unwrap().len(), 1);
    let carried = &detail["carried"][0];
    assert_eq!(carried["id"], f.id());
    assert_eq!(carried["carried"], true);
    assert!(carried["validation_status"].is_null());
    assert_eq!(carried["supporting_evidence"][0]["path"], "caller.rs");
    assert!(
        carried["prompt"]
            .as_str()
            .unwrap()
            .contains(crate::handoff::CARRIED_NOTE)
    );
    assert!(!detail.to_string().contains(secret));
    for query in [String::new(), format!("?finding={}", &f.id()[..6])] {
        let response = client
            .get(format!("{endpoint}/instructions.md{query}"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let text = response.text().await.unwrap();
        assert!(text.contains("Still open from earlier reviews"));
        assert!(text.contains("line numbers may have moved"));
        assert!(text.contains(&f.id()));
        assert!(!text.contains(secret));
    }
    task.abort();
}

#[test]
fn carried_api_rejects_duplicates_current_verdicts_and_mismatched_identities() {
    let f: crate::findings::Finding = serde_json::from_value(json!({"defect_key":"old","file":"bug.rs","start_line":1,"severity":"low","title":"Old","claim":"Claim"})).unwrap();
    let mut state = crate::state::ReviewState::default();
    state.upsert(&f, crate::state::FindingState::Open);
    state.mark_posted(&[f.id()]);
    let carried = crate::report::carried_findings(&state, &[]).remove(0);
    assert_eq!(
        api::report_carried(&json!({"findings":[],"carried":[carried,carried]})).len(),
        1
    );
    assert!(api::report_carried(&json!({"findings":[f],"carried":[carried]})).is_empty());
    let mut mismatch = carried;
    mismatch.id = "000000000000".into();
    assert!(api::report_carried(&json!({"findings":[],"carried":[mismatch]})).is_empty());
}

#[test]
fn finding_detail_exposes_exact_suggestion_shape() {
    let suggestion =
        json!({"start_line":2,"end_line":3,"original":"old\ntext\n","replacement":"new\ntext\n"});
    let finding = serde_json::from_value(json!({"defect_key":"guard","severity":"high","file":"src/a.rs","start_line":2,"end_line":3,
        "title":"Missing guard","claim":"Empty input panics","validation_status":"accepted","suggestion":suggestion})).unwrap();
    let head = "a".repeat(40);
    let base = "b".repeat(40);
    let context = crate::handoff::Context {
        repo: "owner/repo",
        pr_url: "https://github.com/owner/repo/pull/7",
        base: &base,
        head: &head,
        review_url: None,
        stale: Some(false),
    };
    let detail = api::finding_detail(&finding, &context);
    assert_eq!(detail["suggestion"], suggestion);
    assert_eq!(detail["reviewed_head_sha"], head);
    assert_eq!(detail["reviewed_base_sha"], base);
}

#[tokio::test]
async fn staleness_false_and_unknown_are_distinct_and_github_errors_are_cached() {
    for (status, expected) in [(200, json!(false)), (403, Value::Null)] {
        let mock = MockServer::start().await;
        let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
        let (cookie, _) = administrator(&server).await;
        let head = "a".repeat(40);
        let summary = dashboard::JobSummary::queued(&job(&head));
        let id = summary.id.clone();
        let info: dashboard::RepoInfo = serde_json::from_value(
            json!({"repo": job("").repo, "private": false, "html_url": "", "has_override": false}),
        )
        .unwrap();
        {
            let mut dashboard = server.dashboard.lock().await;
            dashboard.repos.insert("owner/repo".into(), info);
            dashboard.jobs.insert(id.clone(), summary);
        }
        Mock::given(method("POST"))
            .and(path("/app/installations/9/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"token": "stale-token"})))
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/7"))
            .respond_with(ResponseTemplate::new(status).set_body_json(pr(&head)))
            .expect(1)
            .mount(&mock)
            .await;
        let dir = crate::fsutil::ensure_dir(data.path(), &Path::new("jobs").join(&id)).unwrap();
        crate::fsutil::write_atomic_in(
            &dir,
            "report.json",
            &serde_json::to_vec(&json!({"head": head, "findings": []})).unwrap(),
        )
        .unwrap();
        let (url, task) = http(server).await;
        for _ in 0..2 {
            let response: Value = reqwest::Client::new()
                .get(format!("{url}/api/jobs/{id}"))
                .header("Cookie", &cookie)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(response["stale"], expected);
        }
        task.abort();
    }
}
