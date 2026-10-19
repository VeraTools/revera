use super::deliver::*;
use super::workspace::{FileChange, Status};
use crate::github::api::GitHubApi;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, body_partial_json, method, path},
};

fn repo() -> Repository {
    Repository {
        owner: "owner".into(),
        name: "repo".into(),
    }
}
fn sha(c: char) -> String {
    c.to_string().repeat(40)
}
fn ref_body(branch: &str, sha: &str) -> Value {
    json!({"ref":format!("refs/heads/{branch}"),"object":{"type":"commit","sha":sha}})
}
async fn mock(server: &MockServer, verb: &str, route: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}
#[tokio::test]
async fn blobs_tree_commit_ref_and_draft_never_force() {
    let s = MockServer::start().await;
    let api = GitHubApi::with_base(&s.uri(), "installation-token-test");
    let parent = sha('a');
    let base_tree = sha('b');
    let blob = sha('c');
    let tree = sha('d');
    let commit = sha('e');
    mock(
        &s,
        "GET",
        &format!("/repos/owner/repo/git/commits/{parent}"),
        200,
        json!({"sha":parent,"tree":{"sha":base_tree}}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/git/blobs"))
        .and(body_json(json!({"content":"fixed\n","encoding":"utf-8"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha":blob})))
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("POST")).and(path("/repos/owner/repo/git/trees")).and(body_json(json!({"base_tree":base_tree,"tree":[{"path":"bug.py","mode":"100644","type":"blob","sha":blob},{"path":"obsolete.txt","mode":"100644","type":"blob","sha":null}]}))).respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha":tree}))).expect(1).mount(&s).await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/git/commits"))
        .and(body_json(
            json!({"message":"Fix arithmetic","parents":[parent],"tree":tree}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"sha":commit})))
        .expect(1)
        .mount(&s)
        .await;
    let changes = vec![
        FileChange {
            path: "bug.py".into(),
            status: Status::Modified,
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            binary: false,
            additions: 1,
            deletions: 1,
            size: 6,
            old_content: Some("bug\n".into()),
            new_content: Some("fixed\n".into()),
        },
        FileChange {
            path: "obsolete.txt".into(),
            status: Status::Deleted,
            old_mode: Some(0o100644),
            new_mode: None,
            binary: false,
            additions: 0,
            deletions: 1,
            size: 0,
            old_content: Some("old\n".into()),
            new_content: None,
        },
    ];
    assert_eq!(
        create_commit(&api, &repo(), &parent, &changes, "Fix arithmetic")
            .await
            .unwrap(),
        commit
    );
    let branch = format!("revera/fix-{}", &commit[..12]);
    let next = format!("{branch}-1");
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/git/refs"))
        .and(body_json(
            json!({"ref":format!("refs/heads/{branch}"),"sha":commit}),
        ))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({"message":"Reference already exists"})),
        )
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/git/refs"))
        .and(body_json(
            json!({"ref":format!("refs/heads/{next}"),"sha":commit}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(ref_body(&next, &commit)))
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls"))
        .and(body_json(
            json!({"head":next,"base":"main","title":"Fix","body":"Approved patch","draft":true}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(
            json!({"number":42,"draft":true,"html_url":"https://github.com/owner/repo/pull/42"}),
        ))
        .expect(1)
        .mount(&s)
        .await;
    let pr = open_draft_pr(
        &api,
        &repo(),
        &commit,
        "revera/fix",
        "main",
        "Fix",
        "Approved patch",
    )
    .await
    .unwrap();
    assert_eq!(pr.branch, next);
    assert_eq!(pr.number, 42);
    for r in s.received_requests().await.unwrap() {
        assert_ne!(r.method.as_str(), "PATCH");
        assert!(!String::from_utf8_lossy(&r.body).contains("force"));
    }
}
#[tokio::test]
async fn moved_head_refuses_write_and_fast_forward_is_non_force() {
    let parent = sha('a');
    let moved = sha('b');
    let commit = sha('c');
    for fresh in [false, true] {
        let s = MockServer::start().await;
        let api = GitHubApi::with_base(&s.uri(), "installation-token-test");
        mock(
            &s,
            "GET",
            &format!("/repos/owner/repo/git/commits/{commit}"),
            200,
            json!({"sha":commit,"parents":[{"sha":parent}]}),
        )
        .await;
        mock(
            &s,
            "GET",
            "/repos/owner/repo/git/ref/heads/feature/fix",
            200,
            ref_body("feature/fix", if fresh { &parent } else { &moved }),
        )
        .await;
        if fresh {
            Mock::given(method("PATCH"))
                .and(path("/repos/owner/repo/git/refs/heads/feature/fix"))
                .and(body_json(json!({"sha":commit,"force":false})))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(ref_body("feature/fix", &commit)),
                )
                .expect(1)
                .mount(&s)
                .await;
        } else {
            Mock::given(method("PATCH"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&s)
                .await;
        }
        let result = fast_forward_branch(&api, &repo(), "feature/fix", &parent, &commit).await;
        if fresh {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err(), Error::MovedHead);
        }
    }
}
#[tokio::test]
async fn github_permission_protection_and_validation_are_typed_and_not_retried() {
    let parent = sha('a');
    let commit = sha('c');
    for (status, message, expected) in [
        (
            403,
            "Resource not accessible by integration",
            Error::Permission,
        ),
        (404, "Not Found", Error::NotFound),
        (409, "Conflict", Error::Conflict),
        (422, "Update is not a fast forward", Error::MovedHead),
        (
            403,
            "Protected branch update failed",
            Error::ProtectedBranch,
        ),
        (
            422,
            "Repository rule violations found",
            Error::ProtectedBranch,
        ),
    ] {
        let s = MockServer::start().await;
        let api = GitHubApi::with_base(&s.uri(), "installation-token-test");
        mock(
            &s,
            "GET",
            &format!("/repos/owner/repo/git/commits/{commit}"),
            200,
            json!({"sha":commit,"parents":[{"sha":parent}]}),
        )
        .await;
        mock(
            &s,
            "GET",
            "/repos/owner/repo/git/ref/heads/main",
            200,
            ref_body("main", &parent),
        )
        .await;
        Mock::given(method("PATCH"))
            .and(path("/repos/owner/repo/git/refs/heads/main"))
            .and(body_json(json!({"sha":commit,"force":false})))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({"message":message})))
            .expect(1)
            .mount(&s)
            .await;
        assert_eq!(
            fast_forward_branch(&api, &repo(), "main", &parent, &commit)
                .await
                .unwrap_err(),
            expected
        );
    }
    let s = MockServer::start().await;
    let api = GitHubApi::with_base(&s.uri(), "installation-token-test");
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/git/refs"))
        .and(body_partial_json(json!({"sha":commit})))
        .respond_with(ResponseTemplate::new(422).set_body_json(
            json!({"message":"Validation failed, or the endpoint has been spammed"}),
        ))
        .expect(1)
        .mount(&s)
        .await;
    assert!(matches!(
        open_draft_pr(&api, &repo(), &commit, "fix", "main", "Fix", "body").await,
        Err(Error::Validation(_))
    ));
}
#[tokio::test]
async fn draft_failure_retains_branch_for_reconciliation_and_wrong_parent_blocks() {
    let commit = sha('c');
    let parent = sha('a');
    let s = MockServer::start().await;
    let api = GitHubApi::with_base(&s.uri(), "installation-token-test");
    let branch = format!("fix-{}", &commit[..12]);
    mock(
        &s,
        "POST",
        "/repos/owner/repo/git/refs",
        201,
        ref_body(&branch, &commit),
    )
    .await;
    mock(
        &s,
        "POST",
        "/repos/owner/repo/pulls",
        403,
        json!({"message":"Forbidden"}),
    )
    .await;
    assert_eq!(
        open_draft_pr(&api, &repo(), &commit, "fix", "main", "Fix", "body")
            .await
            .unwrap_err(),
        Error::DraftFailed {
            branch,
            cause: Box::new(Error::Permission)
        }
    );
    mock(
        &s,
        "GET",
        &format!("/repos/owner/repo/git/commits/{commit}"),
        200,
        json!({"sha":commit,"parents":[{"sha":sha('b')}]}),
    )
    .await;
    assert!(matches!(
        fast_forward_branch(&api, &repo(), "main", &parent, &commit).await,
        Err(Error::Invalid(_))
    ));
    for r in s.received_requests().await.unwrap() {
        assert_ne!(r.method.as_str(), "PATCH");
        assert_ne!(r.method.as_str(), "DELETE");
    }
}

#[tokio::test]
async fn malformed_created_pr_retains_branch_for_reconciliation() {
    let commit = sha('c');
    let branch = format!("fix-{}", &commit[..12]);
    for response in [
        json!({"number":42,"html_url":"https://github.com/owner/repo/pull/42","draft":false}),
        json!({"number":0,"html_url":"https://github.com/owner/repo/pull/42","draft":true}),
        json!({"number":42,"html_url":"invalid","draft":true}),
    ] {
        let s = MockServer::start().await;
        let api = GitHubApi::with_base(&s.uri(), "installation-token-test");
        mock(
            &s,
            "POST",
            "/repos/owner/repo/git/refs",
            201,
            ref_body(&branch, &commit),
        )
        .await;
        mock(&s, "POST", "/repos/owner/repo/pulls", 201, response).await;
        assert!(
            matches!(open_draft_pr(&api, &repo(), &commit, "fix", "main", "Fix", "body").await,
            Err(Error::DraftFailed { branch: actual, cause }) if actual == branch && matches!(*cause, Error::Malformed(_)))
        );
    }
}
