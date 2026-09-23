use revera::config::Config;
use revera::diff::parse_unified;
use revera::findings::{Finding, ValidationStatus};
use revera::pipeline::anchor::{anchor, resolve_quote, Placement, QuoteMatch};
use revera::pipeline::common::ReviewRequest;
use revera::report::finding_body;
use std::path::Path;
use std::process::Command;

const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,4 +10,5 @@
 fn f(xs: &[u8]) -> u8 {
-    xs[0]
+    let i = xs.len() - 1;
+    xs[i]
 }
@@ -40,2 +41,3 @@
 fn g() {
+    let i = xs.len() - 1;
 }
";

fn finding(line: u32, quote: Option<&str>) -> Finding {
    let mut f: Finding = serde_json::from_value(serde_json::json!({
        "defect_key": "k", "severity": "high", "file": "src/a.rs", "start_line": line,
        "title": "t", "claim": "c",
    }))
    .unwrap();
    f.quoted_code = quote.map(str::to_string);
    f.validation_status = Some(ValidationStatus::Accepted);
    f
}

#[test]
fn unique_quote_resolves_to_head_side_range() {
    let d = parse_unified(DIFF);
    assert_eq!(
        resolve_quote(&d, "src/a.rs", "+    let i = xs.len() - 1;\n  +  xs[i]\n"),
        QuoteMatch::Unique(11, 12)
    );
    // context lines are head-side too
    assert_eq!(
        resolve_quote(&d, "src/a.rs", "fn g() {"),
        QuoteMatch::Unique(41, 41)
    );
}

#[test]
fn repeated_deleted_or_absent_quotes_do_not_anchor() {
    let d = parse_unified(DIFF);
    // the same line was added in both hunks
    assert_eq!(
        resolve_quote(&d, "src/a.rs", "let i = xs.len() - 1;"),
        QuoteMatch::Ambiguous
    );
    // deleted code has no head-side line to comment on
    assert_eq!(resolve_quote(&d, "src/a.rs", "xs[0]"), QuoteMatch::NotFound);
    // a window may not span two hunks
    assert_eq!(
        resolve_quote(&d, "src/a.rs", "}\nfn g() {"),
        QuoteMatch::NotFound
    );
    assert_eq!(resolve_quote(&d, "src/b.rs", "xs[i]"), QuoteMatch::NotFound);
    assert_eq!(resolve_quote(&d, "src/a.rs", "  \n"), QuoteMatch::NotFound);
}

#[test]
fn anchoring_trusts_the_quote_over_counted_lines() {
    let d = parse_unified(DIFF);
    let out = anchor(
        &d,
        vec![
            // model miscounted: line 13 is the closing brace
            finding(13, Some("let i = xs.len() - 1;\nxs[i]")),
            finding(12, Some("let i = xs.len() - 1;")),
            finding(12, None),
        ],
        10,
    );
    assert_eq!(out[0].placement, Placement::Inline);
    assert_eq!(
        (out[0].finding.start_line, out[0].finding.end_line),
        (11, Some(12))
    );
    assert!(out[0].finding.quote_anchored);
    // an ambiguous quote cannot be placed honestly, whatever the line says
    assert_eq!(out[1].placement, Placement::Summary);
    assert!(!out[1].finding.quote_anchored);
    // no quote: the counted line is still used
    assert_eq!(out[2].placement, Placement::Inline);
    assert_eq!(out[2].finding.start_line, 12);
}

#[test]
fn suggestion_blocks_only_for_quote_anchored_clean_replacements() {
    let mut f = finding(11, Some("xs[i]"));
    f.suggested_replacement = Some("    xs.last().copied().unwrap_or(0)".into());
    f.quote_anchored = true;
    let body = finding_body(&f);
    assert!(
        body.contains("```suggestion\n    xs.last().copied().unwrap_or(0)\n```"),
        "{body}"
    );

    f.quote_anchored = false;
    let body = finding_body(&f);
    assert!(!body.contains("suggestion\n"), "{body}");
    assert!(body.contains("Suggested replacement:"), "{body}");

    // text that redaction must alter is never offered as a one-click commit
    f.quote_anchored = true;
    f.suggested_replacement = Some("let k = \"AKIAIOSFODNN7EXAMPLE\";".into());
    let body = finding_body(&f);
    assert!(!body.contains("```suggestion"), "{body}");
    assert!(!body.contains("AKIAIOSFODNN7EXAMPLE"), "{body}");
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[tokio::test]
async fn pipeline_places_a_miscounted_finding_by_its_quote() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn last(xs: &[u8]) -> u8 {\n    xs[0]\n}\n",
    )
    .unwrap();
    std::fs::write(repo.join(".gitignore"), ".revera/\nscript.json\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn last(xs: &[u8]) -> u8 {\n    let i = xs.len() - 1;\n    xs[i]\n}\n",
    )
    .unwrap();
    git(repo, &["commit", "-q", "-am", "change"]);

    let finding = serde_json::json!({
        "defect_key": "last_underflows_on_empty", "severity": "high", "file": "src/lib.rs",
        "start_line": 4, "title": "Underflow on empty slice",
        "claim": "len() - 1 underflows for an empty slice", "trigger": "last(&[])",
        "quoted_code": "    let i = xs.len() - 1;",
        "suggested_replacement": "    let i = xs.len().saturating_sub(1);",
    });
    let script = repo.join("script.json");
    std::fs::write(
        &script,
        serde_json::json!({"roles": {"investigator": [[{"tool_calls": [{"name": "submit_findings",
            "arguments": {"findings": [finding], "coverage": "src/lib.rs"}}]}]]}})
        .to_string(),
    )
    .unwrap();
    let cfg: Config = serde_yaml::from_str(&format!(
        "review: {{validate: false}}\nmodels:\n  investigator: {{protocol: scripted, script: {}, model: m}}\n",
        script.display()
    ))
    .unwrap();
    let req = ReviewRequest {
        repo: repo.to_path_buf(),
        base,
        head: None,
        title: None,
        body: String::new(),
        strategy_override: None,
        force: true,
        uncommitted: false,
        progress: None,
    };
    let (rep, _) = revera::pipeline::run(&cfg, &req).await.unwrap();
    assert_eq!(rep.plan.inline.len(), 1, "{:#?}", rep.plan);
    let c = &rep.plan.inline[0];
    assert_eq!((c.file.as_str(), c.line), ("src/lib.rs", 2));
    assert!(
        c.body
            .contains("```suggestion\n    let i = xs.len().saturating_sub(1);\n```"),
        "{}",
        c.body
    );
}
