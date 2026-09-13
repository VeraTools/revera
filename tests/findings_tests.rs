use revera::diff::parse_unified;
use revera::findings::{collapse, finding_id, Finding, Severity, ValidationStatus};
use revera::pipeline::anchor::{anchor, Placement};

fn finding(file: &str, key: &str, line: u32, sev: Severity) -> Finding {
    Finding {
        defect_key: key.into(),
        severity: sev,
        file: file.into(),
        start_line: line,
        end_line: None,
        title: format!("title {key}"),
        claim: "claim".into(),
        trigger: "t".into(),
        impact: "i".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: None,
        suggested_fix: None,
        source: "investigator".into(),
        rationale: None,
        sources: vec![],
    }
}

#[test]
fn id_is_stable_and_12_hex() {
    let a = finding_id("src/x.rs", "some_defect");
    let b = finding_id("src/x.rs", "some_defect");
    let c = finding_id("src/x.rs", "other_defect");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 12);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn collapse_same_defect_key() {
    let mut a = finding("f.rs", "k1", 10, Severity::Low);
    a.supporting_evidence.push(revera::findings::Evidence {
        path: "f.rs".into(),
        start_line: 10,
        end_line: 10,
        note: "n1".into(),
    });
    let mut b = finding("f.rs", "k1", 40, Severity::High);
    b.title = "different wording entirely".into();
    b.source = "scout:x".into();
    let out = collapse(vec![a, b]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].severity, Severity::High);
    assert_eq!(out[0].sources.len(), 2);
}

#[test]
fn collapse_overlapping_lines_similar_titles() {
    let mut a = finding("f.rs", "k1", 10, Severity::Medium);
    a.end_line = Some(15);
    a.title = "null pointer dereference in parse".into();
    let mut b = finding("f.rs", "k2", 12, Severity::Low);
    b.title = "parse has null pointer dereference".into();
    let out = collapse(vec![a, b]);
    assert_eq!(out.len(), 1);
}

#[test]
fn no_collapse_different_files() {
    let a = finding("a.rs", "k1", 10, Severity::Low);
    let b = finding("b.rs", "k1", 10, Severity::Low);
    assert_eq!(collapse(vec![a, b]).len(), 2);
}

const DIFF: &str = "\
diff --git a/src/x.rs b/src/x.rs
--- a/src/x.rs
+++ b/src/x.rs
@@ -10,3 +10,4 @@
 ctx
-old
+new
+line12
 ctx
";

#[test]
fn anchor_inline_vs_summary() {
    let d = parse_unified(DIFF);
    let mut inside = finding("src/x.rs", "k", 12, Severity::High);
    inside.validation_status = Some(ValidationStatus::Accepted);
    let mut outside = finding("src/x.rs", "k2", 50, Severity::High);
    outside.validation_status = Some(ValidationStatus::Accepted);
    let mut other = finding("other.rs", "k3", 12, Severity::High);
    other.validation_status = Some(ValidationStatus::Accepted);
    let out = anchor(&d, vec![inside, outside, other], 10);
    assert_eq!(out[0].placement, Placement::Inline);
    assert_eq!(out[1].placement, Placement::Summary);
    assert_eq!(out[2].placement, Placement::Summary);
}

#[test]
fn anchor_caps_inline() {
    let d = parse_unified(DIFF);
    let mk = |line: u32, sev: Severity, key: &str| {
        let mut f = finding("src/x.rs", key, line, sev);
        f.validation_status = Some(ValidationStatus::Accepted);
        f
    };
    let out = anchor(
        &d,
        vec![mk(12, Severity::Low, "a"), mk(11, Severity::High, "b")],
        1,
    );
    let inline: Vec<_> = out
        .iter()
        .filter(|a| a.placement == Placement::Inline)
        .collect();
    assert_eq!(inline.len(), 1);
    assert_eq!(inline[0].finding.severity, Severity::High);
}
