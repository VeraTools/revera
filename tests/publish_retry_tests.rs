use revera::config::GithubConfig;
use revera::findings::Finding;
use revera::github::api::{GitHubApi, ReviewComment};
use revera::github::event::{parse as parse_event, PrEvent};
use revera::github::publish::{placeable, publish, pull_files_diff};
use revera::report::{InlineComment, LedgerReport, PublicationPlan, RunReport, RunStatus};
use revera::state::{FindingState, ReviewState};
use serde_json::{json, Value};
use std::path::Path;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// GitHub's diff: src/a.rs head-side lines 1..=3 in one hunk.
const PATCH: &str = "@@ -1,2 +1,3 @@\n fn a() {\n+    let x = 1;\n }";

fn event() -> PrEvent {
    parse_event(Path::new("tests/fixtures/pr_event.json")).unwrap()
}

fn inline(file: &str, line: u32, id: &str) -> InlineComment {
    InlineComment {
        file: file.into(),
        line,
        end_line: None,
        body: format!("**[high] t**\n<!-- revera-id:{id} -->"),
    }
}

/// State holding open findings for `good` and `stale`; returns their ids.
fn seeded() -> (ReviewState, String, String) {
    let mk = |file: &str| -> Finding {
        serde_json::from_value(json!({"defect_key": "k", "severity": "high", "file": file,
            "start_line": 2, "title": "t", "claim": "c", "validation_status": "accepted"}))
        .unwrap()
    };
    let (good, stale) = (mk("src/a.rs"), mk("src/b.rs"));
    let mut st = ReviewState::default();
    st.upsert(&good, FindingState::Open);
    st.upsert(&stale, FindingState::Open);
    (st, good.id(), stale.id())
}

fn report(inline: Vec<InlineComment>) -> RunReport {
    RunReport {
        status: RunStatus::Complete,
        reason: None,
        base: "b".into(),
        head: HEAD.into(),
        strategy: "baseline".into(),
        findings: vec![],
        plan: PublicationPlan {
            inline,
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

#[test]
fn placement_follows_githubs_own_hunks() {
    let d = pull_files_diff(&[
        ("src/a.rs".into(), Some(PATCH.into())),
        ("logo.png".into(), None),
    ]);
    let c = |path: &str, line, end| ReviewComment {
        path: path.into(),
        line,
        end_line: end,
        body: String::new(),
    };
    assert!(placeable(&d, &c("src/a.rs", 2, None)));
    assert!(placeable(&d, &c("src/a.rs", 1, Some(3))));
    assert!(!placeable(&d, &c("src/a.rs", 9, None)));
    assert!(!placeable(&d, &c("src/a.rs", 2, Some(9))));
    assert!(!placeable(&d, &c("logo.png", 1, None)));
    assert!(!placeable(&d, &c("src/other.rs", 1, None)));
}

/// A GitHub stub whose review endpoint rejects any batch holding a comment
/// off its diff with `reject_body`, and accepts clean batches.
async fn server(reject_body: &'static str) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"head": {"sha": HEAD}})))
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
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/42/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"filename": "src/a.rs", "patch": PATCH}])),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/widgets/pulls/42/reviews"))
        .respond_with(move |req: &Request| {
            let b: Value = serde_json::from_slice(&req.body).unwrap();
            let off_diff = b["comments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["line"].as_u64() == Some(9));
            if off_diff {
                ResponseTemplate::new(422).set_body_string(reject_body)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"id": 99}))
            }
        })
        .mount(&s)
        .await;
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
    s
}

async fn review_posts(s: &MockServer) -> Vec<usize> {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/repos/acme/widgets/pulls/42/reviews")
        .map(|r| {
            let b: Value = serde_json::from_slice(&r.body).unwrap();
            b["comments"].as_array().unwrap().len()
        })
        .collect()
}

#[tokio::test]
async fn placement_422_resends_only_placeable_comments() {
    let s = server(r#"{"message":"Unprocessable Entity","errors":["Line could not be resolved"]}"#)
        .await;
    let api = GitHubApi::with_base(&s.uri(), "t");
    let (mut st, good, stale) = seeded();
    let mut rep = report(vec![
        inline("src/a.rs", 2, &good),
        inline("src/a.rs", 9, &stale),
    ]);
    let gh = GithubConfig {
        resolve_threads: false,
        ..Default::default()
    };
    let p = publish(&api, &event(), &mut rep, &mut st, 10, &gh)
        .await
        .unwrap();
    assert_eq!(review_posts(&s).await, vec![2, 1]);
    assert_eq!(p.review_id, Some(99));
    assert!(p.skipped_reason.unwrap().contains("1 inline comment(s)"));
    assert!(st.has_posted(&good));
    assert!(!st.has_posted(&stale));
}

#[tokio::test]
async fn other_422_is_not_retried() {
    let s = server(r#"{"message":"You have triggered an abuse detection mechanism"}"#).await;
    let api = GitHubApi::with_base(&s.uri(), "t");
    let (mut st, good, stale) = seeded();
    let mut rep = report(vec![
        inline("src/a.rs", 2, &good),
        inline("src/a.rs", 9, &stale),
    ]);
    let gh = GithubConfig {
        resolve_threads: false,
        ..Default::default()
    };
    let p = publish(&api, &event(), &mut rep, &mut st, 10, &gh)
        .await
        .unwrap();
    assert_eq!(review_posts(&s).await, vec![2]);
    assert_eq!(p.review_id, None);
    assert!(!st.has_posted(&good));
    assert!(!st.has_posted(&stale));
}
