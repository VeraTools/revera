use revera::diff::parse_unified;
use revera::findings::{Finding, Severity, ValidationStatus};
use revera::pipeline::anchor::{anchor, Placement};
use revera::state::{recheck_transition, FindingState, ReviewState};

fn make_finding(file: &str, defect_key: &str, line: u32) -> Finding {
    Finding {
        defect_key: defect_key.into(),
        severity: Severity::High,
        file: file.into(),
        start_line: line,
        end_line: None,
        title: format!("Unchecked division at line {line}"),
        claim: "Potential divide-by-zero when denominator is 0".into(),
        trigger: "denom = 0 passed to divide()".into(),
        impact: "Panic in production service".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: Some(ValidationStatus::Accepted),
        suggested_fix: Some("if denom == 0 { return Err(ZeroDivision); }".into()),
        source: "panel:security".into(),
        rationale: None,
        sources: vec!["panel:security".into(), "panel:concurrency".into()],
        assurance: None,
    }
}

#[test]
fn multi_commit_anchor_shift_and_deduplication() {
    // --- Commit 1: Introduce defect at line 6 ---
    let diff_c1_str = r#"diff --git a/src/calc.rs b/src/calc.rs
index 0000000..1111111 100644
--- a/src/calc.rs
+++ b/src/calc.rs
@@ -1,1 +1,7 @@
 fn add(a: i32, b: i32) -> i32 { a + b }
+
+pub fn divide(a: i32, b: i32) -> i32 {
+    // line 4
+    // line 5
+    a / b
+}
"#;
    let diff_c1 = parse_unified(diff_c1_str);
    let f_c1 = make_finding("src/calc.rs", "div_by_zero", 6);
    let anchored_c1 = anchor(&diff_c1, vec![f_c1.clone()], 10);
    assert_eq!(anchored_c1.len(), 1);
    assert_eq!(anchored_c1[0].placement, Placement::Inline);
    assert_eq!(anchored_c1[0].finding.start_line, 6);

    let mut state = ReviewState::default();
    let finding_id = f_c1.id();
    state.upsert(&f_c1, FindingState::Open);
    assert_eq!(state.open_findings().len(), 1);
    // Mark as posted to GitHub
    state.mark_posted(std::slice::from_ref(&finding_id));
    assert!(state.has_posted(&finding_id));

    // --- Commit 2: Line shift - 15 lines of imports/comments inserted at top ---
    // Defect shifts from line 6 to line 21
    let diff_c2_str = r#"diff --git a/src/calc.rs b/src/calc.rs
index 0000000..2222222 100644
--- a/src/calc.rs
+++ b/src/calc.rs
@@ -1,1 +1,22 @@
+// Line 1: Comment
+// Line 2: Comment
+// Line 3: Comment
+// Line 4: Comment
+// Line 5: Comment
+// Line 6: Comment
+// Line 7: Comment
+// Line 8: Comment
+// Line 9: Comment
+// Line 10: Comment
+// Line 11: Comment
+// Line 12: Comment
+// Line 13: Comment
+// Line 14: Comment
+// Line 15: Comment
 fn add(a: i32, b: i32) -> i32 { a + b }
+
+pub fn divide(a: i32, b: i32) -> i32 {
+    // line 19
+    // line 20
+    a / b
+}
"#;
    let diff_c2 = parse_unified(diff_c2_str);
    let f_c2 = make_finding("src/calc.rs", "div_by_zero", 21);
    let anchored_c2 = anchor(&diff_c2, vec![f_c2.clone()], 10);
    assert_eq!(anchored_c2.len(), 1);
    assert_eq!(anchored_c2[0].placement, Placement::Inline);
    assert_eq!(anchored_c2[0].finding.start_line, 21);

    // Finding ID must be identical across line shifts!
    assert_eq!(f_c2.id(), finding_id);

    // Verify deduplication invariant: already posted -> do NOT post duplicate!
    assert!(state.has_posted(&f_c2.id()));

    // Update state to reflect shifted line
    state.upsert(&f_c2, FindingState::Open);
    let updated = state.find(&finding_id).unwrap();
    assert_eq!(updated.start_line, 21);
    assert!(updated.posted); // posted flag preserved across shifts
}

#[test]
fn multi_commit_resolution_lifecycle() {
    let f = make_finding("src/calc.rs", "div_by_zero", 6);
    let id = f.id();
    let mut state = ReviewState::default();
    state.upsert(&f, FindingState::Open);
    state.mark_posted(std::slice::from_ref(&id));
    assert_eq!(state.open_findings().len(), 1);

    // Author patches defect: recheck validator rejects defect as no longer present -> Resolved
    let recheck_verdict = ValidationStatus::Rejected;
    let next_state = recheck_transition(recheck_verdict);
    assert_eq!(next_state, FindingState::Resolved);

    state.mark(&id, next_state);
    assert_eq!(state.open_findings().len(), 0);
    assert_eq!(state.resolved_findings().len(), 1);
    assert_eq!(state.resolved_findings()[0].id, id);

    // Verify reintroduction: if same bug reappears in a future commit, it re-opens
    let reopened = state.upsert(&f, FindingState::Open);
    assert!(reopened);
    assert_eq!(state.open_findings().len(), 1);
    assert_eq!(state.resolved_findings().len(), 0);
    // Posted flag must be reset so comment posts again on reintroduction!
    let current = state.find(&id).unwrap();
    assert!(!current.posted);
}
