use revera::findings::{Finding, Severity, ValidationStatus};
use revera::github::api::GitHubApi;
use revera::github::event::{parse as parse_event, PrEvent};
use revera::github::publish::{decode_state, encode_state, publish};
use revera::report::{InlineComment, LedgerReport, PublicationPlan, RunReport, RunStatus};
use revera::state::{FindingState, ReviewState};
use serde_json::json;
use std::path::Path;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn event() -> PrEvent {
    parse_event(Path::new("tests/fixtures/pr_event.json")).unwrap()
}

fn report_with_findings(inline: Vec<InlineComment>, findings: Vec<Finding>) -> RunReport {
    RunReport {
        status: RunStatus::Complete,
        reason: None,
        base: "b".into(),
        head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        strategy: "baseline".into(),
        findings,
        plan: PublicationPlan {
            inline,
            summary_markdown: "## Revera review\n\n- finding\n".into(),
            state: ReviewState::default(),
        },
        ledger: LedgerReport {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            by_route: vec![],
            wall_ms: 1,
        },
        publication: Default::default(),
        coverage_gaps: vec![],
        timing: Default::default(),
        stats: Default::default(),
    }
}

fn report(inline: Vec<InlineComment>) -> RunReport {
    report_with_findings(inline, vec![])
}

fn finding(id_body_file: &str) -> Finding {
    Finding {
        defect_key: "k".into(),
        severity: Severity::High,
        file: id_body_file.into(),
        start_line: 3,
        end_line: None,
        title: "t".into(),
        claim: "c".into(),
        trigger: "".into(),
        impact: "".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: Some(ValidationStatus::Accepted),
        suggested_fix: None,
        source: "investigator".into(),
        rationale: None,
        sources: vec![],
    }
}

#[test]
fn parses_pr_event() {
    let e = event();
    assert_eq!(e.repo_full_name, "acme/widgets");
    assert_eq!(e.number, 42);
    assert_eq!(e.head_sha, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(e.base_sha, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    assert_eq!(e.head_ref, "feat/discount-percent");
    assert_eq!(e.base_ref, "main");
    assert!(!e.is_fork());
    assert_eq!(e.owner_repo(), ("acme", "widgets"));
}

#[test]
fn fork_event_detected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("fork.json");
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pr_event.json")).unwrap();
    v["pull_request"]["head"]["repo"]["full_name"] = json!("contributor/widgets");
    std::fs::write(&p, v.to_string()).unwrap();
    let e = parse_event(&p).unwrap();
    assert!(e.is_fork());
    assert_eq!(e.head_repo_full_name, "contributor/widgets");
}

#[test]
fn unsupported_event_fails() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ev.json");
    std::fs::write(&p, r#"{"action":"push","ref":"main"}"#).unwrap();
    assert!(parse_event(&p).is_err());
}

#[test]
fn state_blob_roundtrip() {
    let mut s = ReviewState {
        reviewed_base: "b".into(),
        reviewed_head: "h".into(),
        patch_id: "pid".into(),
        ..Default::default()
    };
    s.upsert(&finding("src/x.rs"), FindingState::Open);
    s.mark_posted(&[s.findings[0].id.clone()]);
    let body = format!("## Revera review\n\n...\n{}", encode_state(&s));
    let back = decode_state(&body).unwrap();
    assert_eq!(back.reviewed_head, "h");
    assert_eq!(back.patch_id, "pid");
    assert_eq!(back.findings.len(), 1);
    assert!(back.findings[0].posted);
}

#[tokio::test]
async fn head_moved_refuses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": "cccccccccccccccccccccccccccccccccccccccc"}
        })))
        .mount(&server)
        .await;
    let api = GitHubApi::with_base(&server.uri(), "t");
    let mut rep = report(vec![]);
    let mut st = ReviewState::default();
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .unwrap();
    assert_eq!(
        p.skipped_reason.as_deref(),
        Some("head moved aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa -> cccccccccccccccccccccccccccccccccccccccc")
    );
    assert!(p.review_id.is_none());
    assert_eq!(rep.status, RunStatus::Partial);
    let pub_phase = rep
        .timing
        .phases
        .iter()
        .find(|p| p.phase == "publish")
        .expect("skipped publish phase recorded");
    assert_eq!(pub_phase.outcome, "skipped");
    assert_eq!(rep.timing.skipped_phases, 1);
    assert_eq!(rep.timing.incomplete_phases, 0);
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "only the head re-check may hit the API");
}

#[tokio::test]
async fn happy_path_posts_review_and_summary() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 777})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 555})))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let f = finding("src/x.rs");
    let body = revera::report::finding_body(&f);
    let mut rep = report(vec![InlineComment {
        file: "src/x.rs".into(),
        line: 3,
        end_line: None,
        body,
    }]);
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .unwrap();
    assert_eq!(p.review_id, Some(777));
    assert_eq!(p.summary_comment_id, Some(555));
    assert!(st.findings[0].posted);

    let reqs = server.received_requests().await.unwrap();
    let review_req = reqs
        .iter()
        .find(|r| r.url.path() == "/repos/acme/widgets/pulls/42/reviews")
        .expect("review was posted");
    let v: serde_json::Value = serde_json::from_slice(&review_req.body).unwrap();
    assert_eq!(v["event"], "COMMENT");
    assert_eq!(v["comments"][0]["path"], "src/x.rs");
    assert_eq!(v["comments"][0]["side"], "RIGHT");
    assert!(!v["body"].as_str().unwrap().contains("revera-state"));
    let comment_req = reqs
        .iter()
        .find(|r| r.url.path() == "/repos/acme/widgets/issues/42/comments" && r.method == "POST")
        .expect("summary comment posted");
    let v: serde_json::Value = serde_json::from_slice(&comment_req.body).unwrap();
    let body = v["body"].as_str().unwrap();
    assert!(body.contains("<!-- revera-summary -->"));
    assert!(decode_state(body).is_some());
}

#[tokio::test]
async fn second_run_updates_summary_and_posts_only_unposted() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    // existing managed comment carrying a state blob where f1 is posted
    let f1 = finding("src/x.rs");
    let mut prev = ReviewState::default();
    prev.upsert(&f1, FindingState::Open);
    prev.mark_posted(&[f1.id()]);
    let existing_body = format!("<!-- revera-summary -->\nold\n{}", encode_state(&prev));
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 555, "body": existing_body}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/issues/comments/555"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 555})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 778})))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    // inline plan has the already-posted f1 and a new f2
    let f2 = {
        let mut f = finding("src/y.rs");
        f.defect_key = "k2".into();
        f.start_line = 9;
        f
    };
    let mut rep = report(vec![
        InlineComment {
            file: f1.file.clone(),
            line: f1.start_line,
            end_line: None,
            body: revera::report::finding_body(&f1),
        },
        InlineComment {
            file: f2.file.clone(),
            line: f2.start_line,
            end_line: None,
            body: revera::report::finding_body(&f2),
        },
    ]);
    let mut st = prev.clone();
    st.upsert(&f2, FindingState::Open);
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .unwrap();
    assert_eq!(p.review_id, Some(778));
    assert_eq!(p.summary_comment_id, Some(555));

    let reqs = server.received_requests().await.unwrap();
    let review_req = reqs
        .iter()
        .find(|r| r.url.path() == "/repos/acme/widgets/pulls/42/reviews")
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&review_req.body).unwrap();
    let comments = v["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1, "only the unposted id is commented");
    assert_eq!(comments[0]["path"], "src/y.rs");
    assert!(st.findings.iter().all(|f| f.posted));
}

#[tokio::test]
async fn summary_only_finding_marked_posted() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 555})))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let f = finding("src/outside.rs");
    let mut rep = report_with_findings(vec![], vec![f.clone()]);
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .unwrap();
    assert!(p.review_id.is_none());
    assert_eq!(p.summary_comment_id, Some(555));
    assert!(st.findings[0].posted);
    assert!(!st
        .findings
        .iter()
        .any(|f| f.status == FindingState::Open && !f.posted));

    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs
        .iter()
        .any(|r| r.url.path() == "/repos/acme/widgets/pulls/42/reviews"));
    let summary_req = reqs
        .iter()
        .find(|r| r.url.path() == "/repos/acme/widgets/issues/42/comments" && r.method == "POST")
        .expect("summary comment posted");
    let v: serde_json::Value = serde_json::from_slice(&summary_req.body).unwrap();
    let persisted = decode_state(v["body"].as_str().unwrap()).unwrap();
    assert!(persisted.findings[0].posted);
}

#[tokio::test]
async fn summary_failure_leaves_finding_unposted() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let f = finding("src/outside.rs");
    let mut rep = report_with_findings(vec![], vec![f.clone()]);
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    assert!(publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .is_err());
    assert!(!st.findings[0].posted);
}

/// Inline review posted, then the summary upsert fails before the state blob
/// records it. A retry with a fresh (unrecorded) state must not re-post the
/// inline comment: the comment already on the PR is the durable record.
#[tokio::test]
async fn retry_after_summary_failure_does_not_duplicate_inline() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let f = finding("src/x.rs");
    let body = revera::report::finding_body(&f);
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    // first run: no inline comments yet
    let first = Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .up_to_n_times(1)
        .mount_as_scoped(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 778})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let inline = || {
        vec![InlineComment {
            file: f.file.clone(),
            line: f.start_line,
            end_line: None,
            body: body.clone(),
        }]
    };
    let mut rep = report_with_findings(inline(), vec![f.clone()]);
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    assert!(publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .is_err());
    assert!(st.findings[0].posted, "in-memory state records the post");
    drop(first);

    // retry: GitHub now lists the inline comment, the state blob never
    // recorded it (summary failed), local state was lost
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1,
            "body": body,
            "user": {"login": "github-actions[bot]", "type": "Bot"}
        }])))
        .mount(&server)
        .await;
    let mut rep2 = report_with_findings(inline(), vec![f.clone()]);
    let mut st2 = ReviewState::default();
    st2.upsert(&f, FindingState::Open);
    let _ = publish(
        &api,
        &event(),
        &mut rep2,
        &mut st2,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await;
    assert!(st2.findings[0].posted, "reconciled from the PR's comments");
    let reviews = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/repos/acme/widgets/pulls/42/reviews")
        .count();
    assert_eq!(
        reviews, 1,
        "inline review posted exactly once across both runs"
    );
}

/// A contributor who posts a predicted `revera-id` marker before Revera runs
/// must not suppress the real inline comment: only our own comments count.
#[tokio::test]
async fn forged_inline_marker_by_other_author_is_ignored() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let f = finding("src/x.rs");
    let body = revera::report::finding_body(&f);
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "body": body, "user": {"login": "mallory", "type": "User"}},
            {"id": 2, "body": body, "user": {"login": "other-app[bot]", "type": "Bot"}}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 778})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 900})))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/issues/comments/900"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 900})))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let inline = vec![InlineComment {
        file: f.file.clone(),
        line: f.start_line,
        end_line: None,
        body: body.clone(),
    }];
    let mut rep = report_with_findings(inline, vec![f.clone()]);
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .unwrap();
    assert_eq!(p.review_id, Some(778), "inline review still posted");
}

#[tokio::test]
async fn inline_review_422_degrades_to_summary_only() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(422).set_body_string("line is not part of the diff"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 555})))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let f = finding("src/x.rs");
    let mut rep = report_with_findings(
        vec![InlineComment {
            file: f.file.clone(),
            line: f.start_line,
            end_line: None,
            body: revera::report::finding_body(&f),
        }],
        vec![f.clone()],
    );
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .unwrap();
    assert!(p.review_id.is_none());
    assert_eq!(p.summary_comment_id, Some(555));
    assert_eq!(
        p.skipped_reason.as_deref(),
        Some("inline review rejected by GitHub (422); findings listed in summary only")
    );
    assert!(st.findings.iter().all(|f| f.posted));
}

#[tokio::test]
async fn inline_review_500_still_fails_and_leaves_unposted() {
    let server = MockServer::start().await;
    let head_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "head": {"sha": head_sha}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
        .mount(&server)
        .await;

    let api = GitHubApi::with_base(&server.uri(), "t");
    let f = finding("src/x.rs");
    let mut rep = report_with_findings(
        vec![InlineComment {
            file: f.file.clone(),
            line: f.start_line,
            end_line: None,
            body: revera::report::finding_body(&f),
        }],
        vec![f.clone()],
    );
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    assert!(publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        "<!-- revera-summary -->",
        "github-actions[bot]",
    )
    .await
    .is_err());
    assert!(!st.findings[0].posted);
}
