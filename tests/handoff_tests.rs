use revera::{
    diff::parse_unified,
    findings::{
        Finding, Suggestion, ValidationStatus, accepted_counts, accepted_counts_json,
        checked_suggestion,
    },
    handoff::{self, Context},
    report::{self, RunStatus, Summary},
};
use serde_json::json;

fn finding(status: &str) -> Finding {
    serde_json::from_value(json!({"defect_key": "guard", "severity": "high", "file": "f.rs", "start_line": 2,
        "title": "Missing guard", "claim": "The changed path accepts invalid input.", "trigger": "An empty request", "impact": "A panic",
        "supporting_evidence": [{"path": "f.rs", "start_line": 2, "end_line": 2, "note": "Missing branch"}],
        "validation_status": status, "validated_fix": "Add a guard", "suggested_fix": "NEVER EXPOSE INVESTIGATOR REMEDY"})).unwrap()
}

fn context(stale: Option<bool>) -> Context<'static> {
    Context {
        repo: "owner/repo",
        pr_url: "https://github.com/owner/repo/pull/1",
        base: "bbbb",
        head: "aaaa",
        review_url: Some("https://review.example/reviews/1"),
        stale,
    }
}

#[test]
fn handoff_is_deterministic_accepted_only_and_contains_exact_working_rules() {
    let accepted = finding("accepted");
    let mut rejected = finding("rejected");
    rejected.title = "REJECTED CANDIDATE".into();
    let mut uncertain = finding("uncertain");
    uncertain.title = "UNCERTAIN CANDIDATE".into();
    let findings = [accepted.clone(), rejected, uncertain];
    let first = handoff::build(&context(Some(false)), &findings, None);
    assert_eq!(
        first,
        handoff::build(&context(Some(false)), &findings, None)
    );
    for expected in [
        handoff::WORKING_RULES,
        "owner/repo",
        "https://github.com/owner/repo/pull/1",
        "aaaa",
        "bbbb",
        "f.rs:2-2",
        "Missing branch",
        "An empty request",
        "A panic",
        "Validator's suggested remedy — not a tested patch",
        &accepted.id(),
    ] {
        assert!(first.contains(expected), "missing {expected}");
    }
    for hidden in [
        "REJECTED CANDIDATE",
        "UNCERTAIN CANDIDATE",
        "NEVER EXPOSE INVESTIGATOR REMEDY",
        "Potentially stale",
    ] {
        assert!(!first.contains(hidden));
    }
    assert!(handoff::build(&context(Some(true)), &findings, None).contains("Potentially stale"));
    assert!(!handoff::build(&context(None), &findings, None).contains("Potentially stale"));
    assert!(!handoff::build(&context(None), &findings, Some("unknown")).contains("Missing guard"));
}

#[test]
fn instructions_redact_every_field_and_bound_large_reviews_with_dashboard_pointer() {
    let secret = "handoff-fixture-secret-unique-71934372";
    revera::redact::register(secret);
    let mut f = finding("accepted");
    f.claim = format!("{secret}{}", "x".repeat(8000));
    f.validated_fix = Some(secret.into());
    f.supporting_evidence[0].note = format!("{secret}{}", "y".repeat(8000));
    let output = handoff::build(&context(None), &vec![f; 100], None);
    assert!(!output.contains(secret));
    assert!(output.len() <= handoff::MAX_INSTRUCTIONS_BYTES);
    assert!(output.contains("truncated"));
    assert!(output.contains("https://review.example/reviews/1"));
    assert!(output.contains(handoff::WORKING_RULES));
}

#[test]
fn mixed_status_counts_match_api_dashboard_and_github_summary() {
    let findings = [
        finding("accepted"),
        finding("rejected"),
        finding("uncertain"),
    ];
    let counts = accepted_counts(&findings);
    assert_eq!(counts, accepted_counts_json(&json!({"findings": findings})));
    assert_eq!(counts.total, 1);
    assert_eq!(counts.high, 1);
    let summary = report::summary_markdown(&Summary {
        findings: &findings,
        ..Default::default()
    });
    assert!(summary.contains("Accepted findings in this review: **1**."));
    for status in [RunStatus::Partial, RunStatus::Failed] {
        let summary = report::summary_markdown(&Summary {
            status: Some(status),
            ..Default::default()
        });
        assert!(summary.contains("Review incomplete"));
        assert!(!summary.contains("Status: complete"));
    }
}

fn diff() -> revera::diff::DiffSet {
    parse_unified(
        "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,4 @@\n ctx\n-old\n+new\n+second\n tail\n",
    )
}
fn suggestion() -> Suggestion {
    Suggestion {
        start_line: 2,
        end_line: 2,
        original: "new\n".into(),
        replacement: "fixed\n".into(),
    }
}

#[test]
fn exact_suggestion_is_right_anchored_and_backticks_cannot_escape_fence() {
    let mut f = finding("accepted");
    let mut s = suggestion();
    s.replacement = "fixed(````)\n\n".into();
    f.suggestion = checked_suggestion(&f, &s, &diff(), "ctx\nnew\nsecond\ntail\n");
    assert_eq!(f.suggestion, Some(s));
    let body = report::finding_body(&f);
    assert!(
        body.contains("`````suggestion\nfixed(````)\n\n`````"),
        "{body}"
    );
    let anchored = revera::pipeline::anchor::anchor(&diff(), vec![f], 1);
    assert_eq!(anchored[0].finding.start_line, 2);
    assert_eq!(anchored[0].finding.end_line, None);
    assert_eq!(
        anchored[0].placement,
        revera::pipeline::anchor::Placement::Inline
    );
}

#[test]
fn suggestions_reject_every_unsafe_or_inexact_path() {
    let f = finding("accepted");
    let head = "ctx\nnew\nsecond\ntail\n";
    let mut cases = vec![];
    for status in [ValidationStatus::Rejected, ValidationStatus::Uncertain] {
        let mut bad = f.clone();
        bad.validation_status = Some(status);
        cases.push(("not accepted", bad, suggestion(), diff(), head.to_owned()));
    }
    for (label, start, end) in [
        ("zero", 0, 0),
        ("reversed", 3, 2),
        ("over ten", 2, 12),
        ("anchor mismatch", 3, 3),
        ("context", 1, 1),
        ("past EOF", 8, 8),
    ] {
        let mut s = suggestion();
        s.start_line = start;
        s.end_line = end;
        let mut bad = f.clone();
        if label != "anchor mismatch" {
            bad.start_line = start;
            bad.end_line = Some(end);
        }
        cases.push((label, bad, s, diff(), head.to_owned()));
    }
    for (label, original, replacement) in [
        ("original mismatch", "wrong\n".into(), "ok".into()),
        ("missing original newline", "new".into(), "ok".into()),
        (
            "too many replacement lines",
            "new\n".into(),
            "x\n".repeat(21),
        ),
        ("too many bytes", "new\n".into(), "x".repeat(2001)),
        ("NUL", "new\n".into(), "a\0b".into()),
        ("CR", "new\n".into(), "a\rb".into()),
        ("original CR", "new\r\n".into(), "ok".into()),
        ("original NUL", "new\0\n".into(), "ok".into()),
    ] {
        let mut s = suggestion();
        s.original = original;
        s.replacement = replacement;
        cases.push((label, f.clone(), s, diff(), head.to_owned()));
    }
    let mut missing = f.clone();
    missing.file = "absent.rs".into();
    cases.push((
        "absent file",
        missing,
        suggestion(),
        diff(),
        head.to_owned(),
    ));
    let mut sensitive = f.clone();
    sensitive.file = ".env".into();
    cases.push((
        "sensitive",
        sensitive,
        suggestion(),
        diff(),
        head.to_owned(),
    ));
    let mut multi = f.clone();
    multi.end_line = Some(3);
    let mut s = suggestion();
    s.end_line = 3;
    s.original = "new\nsecond\n".into();
    let separate = parse_unified(
        "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +2 @@\n-old\n+new\n@@ -2 +3 @@\n-old2\n+second\n",
    );
    cases.push(("multiple hunks", multi, s, separate, head.into()));
    cases.push(("file unavailable", f, suggestion(), diff(), "".into()));
    for (label, f, s, diff, head) in cases {
        assert_eq!(checked_suggestion(&f, &s, &diff, &head), None, "{label}");
    }
}

#[test]
fn investigator_cannot_seed_suggestions_or_validated_remedies() {
    let mut f = serde_json::to_value(finding("accepted")).unwrap();
    f["suggestion"] = json!(suggestion());
    let parsed = revera::pipeline::common::parse_findings(&json!({"findings": [f]}));
    assert_eq!(parsed.len(), 1);
    assert!(parsed[0].suggestion.is_none());
    assert!(parsed[0].validated_fix.is_none());
    assert!(parsed[0].validation_status.is_none());
    assert!(
        revera::pipeline::validate::validator_terminal().parameters["properties"]["suggestion"]
            .is_object()
    );
}

#[tokio::test]
async fn validator_checks_suggestions_against_pinned_head_not_dirty_worktree() {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(repo.path().join("f.rs"), "ctx\nnew\nsecond\ntail\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-qm", "head"]] {
        assert!(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let head = revera::git::current_head(repo.path()).await.unwrap();
    std::fs::write(repo.path().join("f.rs"), "ctx\nDIRTY\nsecond\ntail\n").unwrap();
    let script = repo.path().join("validator.json");
    let verdict = |original: &str, status: &str| {
        json!({"tool_calls": [{"name": "submit_verdict", "arguments": {
            "validation_status": status, "rationale": "Checked", "suggestion": {"start_line": 2, "end_line": 2, "original": original, "replacement": "fixed\n"}
        }}]})
    };
    std::fs::write(&script, serde_json::to_vec(&json!({"roles": {"validator": [[verdict("new\n", "accepted")], [verdict("DIRTY\n", "accepted")], [verdict("new\n", "rejected")]]}})).unwrap()).unwrap();
    let cfg = revera::config::Config::parse(&format!("models:\n  investigator: {{protocol: scripted, model: fixture, script: {}}}\nvera: {{enabled: false}}\nreview: {{concurrency: 1}}\n", script.display()), "fixture").unwrap();
    let diff = Arc::new(diff());
    let vera = Arc::new(revera::vera::VeraClient::from_config(&cfg.vera, repo.path()).unwrap());
    let toolbox = Arc::new(
        revera::tools::ToolBox::new(repo.path().to_owned(), diff.clone(), vera, 12000)
            .with_head(&head),
    );
    let mut candidates = vec![finding("uncertain"); 3];
    let now = Instant::now();
    let reason = revera::pipeline::validate::validate_candidates(
        &cfg,
        revera::provider::LedgerHandle::new(),
        &toolbox,
        &diff,
        &mut candidates,
        "Validate the finding",
        &revera::pipeline::validate::validator_terminal(),
        "validator",
        false,
        now + Duration::from_secs(30),
        &revera::timing::Recorder::default(),
        now,
        "baseline",
        "validate",
    )
    .await;
    assert_eq!(reason, None);
    assert_eq!(candidates[0].suggestion, Some(suggestion()));
    assert!(
        candidates[1].suggestion.is_none(),
        "dirty working-tree bytes cannot approve a replacement"
    );
    assert!(
        candidates[2].suggestion.is_none(),
        "rejected verdict cannot approve a replacement"
    );
    assert!(report::finding_body(&candidates[0]).contains("```suggestion\nfixed\n```"));
    assert!(!report::finding_body(&candidates[1]).contains("```suggestion"));
}

#[test]
fn summary_and_inline_escape_untrusted_links_mentions_and_markers() {
    let mut f = finding("accepted");
    f.title = "[click](https://evil.invalid) @everyone <!-- revera:fix-status -->".into();
    f.claim = f.title.clone();
    f.file = "x` [click](https://evil.invalid) @everyone".into();
    for output in [
        report::finding_body(&f),
        report::summary_markdown(&Summary {
            findings: &[f],
            ..Default::default()
        }),
    ] {
        assert!(!output.contains("[click](https://evil.invalid)"));
        assert!(!output.contains("@everyone"));
        assert!(!output.contains("<!-- revera:fix-status -->"));
    }
}
