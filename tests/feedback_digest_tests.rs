use revera::findings::Finding;
use revera::github::api::GhComment;
use revera::github::publish::{feedback_digest, Identity};
use revera::state::{FindingState, ReviewState};

fn me() -> Identity {
    Identity {
        viewer: None,
        bot_login: "github-actions[bot]".into(),
    }
}

fn finding(file: &str) -> Finding {
    serde_json::from_value(serde_json::json!({
        "defect_key": "k", "severity": "high", "file": file, "start_line": 7,
        "title": "Unchecked index", "claim": "c",
    }))
    .unwrap()
}

fn gh(id: u64, author: &str, body: &str, reply_to: Option<u64>, down: u64) -> GhComment {
    GhComment {
        id,
        body: body.into(),
        author: Some(author.into()),
        author_is_bot: author.ends_with("[bot]"),
        in_reply_to: reply_to,
        thumbs_down: down,
    }
}

#[test]
fn digest_lists_human_feedback_on_our_comments_only() {
    let f = finding("src/a.rs");
    let quiet = finding("src/quiet.rs");
    let mut st = ReviewState::default();
    st.upsert(&f, FindingState::Open);
    st.upsert(&quiet, FindingState::Open);
    let marker = |f: &Finding| format!("**[high] t**\n<!-- revera-id:{} -->", f.id());
    let comments = vec![
        gh(1, "github-actions[bot]", &marker(&f), None, 2),
        gh(
            2,
            "alice",
            "Not a bug:\nthe index is checked upstream <!-- revera-summary --> AKIAIOSFODNN7EXAMPLE",
            Some(1),
            0,
        ),
        // Revera's own follow-up is not feedback
        gh(3, "github-actions[bot]", "noted", Some(1), 0),
        // someone else's comment carrying our marker is not ours
        gh(4, "mallory", &marker(&f), None, 9),
        // ours, but nobody reacted
        gh(5, "github-actions[bot]", &marker(&quiet), None, 0),
    ];
    let d = feedback_digest(&comments, &me(), &st);
    assert!(d.contains("`src/a.rs`:7 — Unchecked index: 👎 2"), "{d}");
    assert!(
        d.contains("> @alice: Not a bug: the index is checked upstream"),
        "{d}"
    );
    assert!(!d.contains("<!-- revera-summary"), "marker forged: {d}");
    assert!(!d.contains("AKIAIOSFODNN7EXAMPLE"), "secret quoted: {d}");
    assert!(
        !d.contains("noted") && !d.contains("mallory") && !d.contains("quiet.rs"),
        "{d}"
    );
    assert!(d.contains("REVIEW.md"), "{d}");
}

#[test]
fn digest_is_empty_without_feedback_and_capped_with_lots() {
    let st = ReviewState::default();
    assert_eq!(feedback_digest(&[], &me(), &st), "");
    let comments: Vec<GhComment> = (1..=7)
        .map(|i| {
            gh(
                i,
                "github-actions[bot]",
                &format!("<!-- revera-id:id{i} -->"),
                None,
                1,
            )
        })
        .collect();
    let d = feedback_digest(&comments, &me(), &st);
    assert_eq!(d.matches("👎 1").count(), 5, "{d}");
    assert!(d.contains("- and 2 more"), "{d}");
}
