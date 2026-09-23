use revera::config::GithubConfig;
use revera::findings::Finding;
use revera::github::api::GitHubApi;
use revera::github::event::{parse as parse_event, PrEvent};
use revera::github::publish::publish;
use revera::report::{LedgerReport, PublicationPlan, RunReport, RunStatus};
use revera::state::{FindingState, ReviewState};
use serde_json::{json, Value};
use std::path::Path;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn event() -> PrEvent {
    parse_event(Path::new("tests/fixtures/pr_event.json")).unwrap()
}

fn finding(file: &str) -> Finding {
    serde_json::from_value(json!({
        "defect_key": "k", "severity": "high", "file": file, "start_line": 3,
        "title": "t", "claim": "c", "validation_status": "accepted",
    }))
    .unwrap()
}

fn report() -> RunReport {
    RunReport {
        status: RunStatus::Complete,
        reason: None,
        base: "b".into(),
        head: HEAD.into(),
        strategy: "baseline".into(),
        findings: vec![],
        plan: PublicationPlan {
            inline: vec![],
            summary_markdown: "## Revera review\n".into(),
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

fn comment(login: &str, bot: bool, body: &str) -> Value {
    json!({"body": body, "author": {"login": login, "__typename": if bot { "Bot" } else { "User" }}})
}

fn thread(id: &str, resolved: bool, total: u64, comments: Vec<Value>) -> Value {
    json!({"id": id, "isResolved": resolved, "comments": {"totalCount": total, "nodes": comments}})
}

/// Mount the REST calls `publish` makes plus a GraphQL endpoint serving
/// `threads` (or `graphql_status` when it is not 200).
async fn server(threads: Vec<Value>, graphql_status: u16) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"head": {"sha": HEAD}})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&s)
        .await;
    for p in [
        "/repos/acme/widgets/pulls/42/comments",
        "/repos/acme/widgets/issues/42/comments",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&s)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 7})))
        .mount(&s)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/widgets/issues/comments/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 7})))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(move |req: &Request| {
            if graphql_status != 200 {
                return ResponseTemplate::new(graphql_status);
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            if body["query"]
                .as_str()
                .unwrap()
                .contains("resolveReviewThread")
            {
                ResponseTemplate::new(200).set_body_json(json!({"data": {"resolveReviewThread":
                    {"thread": {"id": body["variables"]["id"], "isResolved": true}}}}))
            } else {
                ResponseTemplate::new(200).set_body_json(
                    json!({"data": {"repository": {"pullRequest":
                    {"reviewThreads": {"pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": threads.clone()}}}}}),
                )
            }
        })
        .mount(&s)
        .await;
    s
}

async fn resolved_thread_ids(s: &MockServer) -> Vec<String> {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/graphql")
        .filter_map(|r| {
            let b: Value = serde_json::from_slice(&r.body).ok()?;
            b["query"]
                .as_str()?
                .contains("resolveReviewThread")
                .then(|| b["variables"]["id"].as_str().unwrap().to_string())
        })
        .collect()
}

fn state_with(resolved: &Finding, open: &Finding) -> ReviewState {
    let mut st = ReviewState::default();
    st.upsert(resolved, FindingState::Resolved);
    st.upsert(open, FindingState::Open);
    st
}

#[tokio::test]
async fn resolves_only_our_untouched_threads_for_validated_fixes() {
    let fixed = finding("src/fixed.rs");
    let open = finding("src/open.rs");
    let marker = |f: &Finding| format!("**[high] t**\n<!-- revera-id:{} -->", f.id());
    let ours = |b: &str| comment("github-actions", true, b);
    let threads = vec![
        thread("T-ok", false, 1, vec![ours(&marker(&fixed))]),
        thread(
            "T-human-reply",
            false,
            2,
            vec![
                ours(&marker(&fixed)),
                comment("alice", false, "not fixed yet"),
            ],
        ),
        thread(
            "T-forged",
            false,
            1,
            vec![comment("mallory", false, &marker(&fixed))],
        ),
        thread("T-still-open", false, 1, vec![ours(&marker(&open))]),
        thread("T-already", true, 1, vec![ours(&marker(&fixed))]),
        thread("T-partial", false, 60, vec![ours(&marker(&fixed))]),
        thread(
            "T-no-author",
            false,
            1,
            vec![json!({"body": marker(&fixed), "author": null})],
        ),
    ];
    let s = server(threads, 200).await;
    let api = GitHubApi::with_base(&s.uri(), "t");
    let mut rep = report();
    let mut st = state_with(&fixed, &open);
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        &GithubConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(resolved_thread_ids(&s).await, vec!["T-ok".to_string()]);
    assert_eq!(p.resolved_threads, 1);
    assert_eq!(p.thread_resolution_error, None);
}

#[tokio::test]
async fn graphql_failure_is_reported_not_fatal() {
    let fixed = finding("src/fixed.rs");
    let s = server(vec![], 502).await;
    let api = GitHubApi::with_base(&s.uri(), "t");
    let mut rep = report();
    let mut st = state_with(&fixed, &finding("src/open.rs"));
    let p = publish(
        &api,
        &event(),
        &mut rep,
        &mut st,
        10,
        &GithubConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.resolved_threads, 0);
    assert!(p.thread_resolution_error.unwrap().contains("502"));
    assert_eq!(p.summary_comment_id, Some(7));
}

#[tokio::test]
async fn disabled_or_nothing_resolved_makes_no_graphql_call() {
    for (resolve, resolved_state) in [(false, true), (true, false)] {
        let s = server(vec![], 200).await;
        let api = GitHubApi::with_base(&s.uri(), "t");
        let mut rep = report();
        let mut st = if resolved_state {
            state_with(&finding("src/fixed.rs"), &finding("src/open.rs"))
        } else {
            let mut st = ReviewState::default();
            st.upsert(&finding("src/open.rs"), FindingState::Open);
            st
        };
        let gh = GithubConfig {
            resolve_threads: resolve,
            ..Default::default()
        };
        publish(&api, &event(), &mut rep, &mut st, 10, &gh)
            .await
            .unwrap();
        let graphql = s
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/graphql")
            .count();
        assert_eq!(
            graphql, 0,
            "resolve={resolve} resolved_state={resolved_state}"
        );
    }
}
