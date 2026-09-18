use revera::findings::{Finding, Severity, ValidationStatus};
use revera::report::{surfaced_ids, LedgerReport, PublicationPlan, RunReport, RunStatus};
use revera::state::{recheck_transition, FindingState, ReviewState};

fn finding(file: &str, key: &str, line: u32) -> Finding {
    Finding {
        defect_key: key.into(),
        severity: Severity::High,
        file: file.into(),
        start_line: line,
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
fn state_roundtrip_and_transitions() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = ReviewState::default();
    let f = finding("src/x.rs", "k", 5);
    s.upsert(&f, FindingState::Open);
    s.mark_posted(&[f.id()]);
    s.record_outcome("b", "h", "abc", "key-1", RunStatus::Complete);
    s.save(dir.path()).unwrap();

    let loaded = ReviewState::load(dir.path()).unwrap().unwrap();
    assert_eq!(loaded.findings.len(), 1);
    assert_eq!(loaded.findings[0].status, FindingState::Open);
    assert!(loaded.findings[0].posted);
    assert!(loaded.can_reuse("key-1"));
    assert!(!loaded.can_reuse("key-2"));

    let mut s = loaded;
    s.mark(&f.id(), FindingState::Resolved);
    s.save(dir.path()).unwrap();
    let re = ReviewState::load(dir.path()).unwrap().unwrap();
    assert_eq!(re.findings[0].status, FindingState::Resolved);
    assert!(re.open_findings().is_empty());
}

#[test]
fn upsert_preserves_posted() {
    let mut s = ReviewState::default();
    let f = finding("f.rs", "k", 1);
    s.upsert(&f, FindingState::Open);
    s.mark_posted(&[f.id()]);
    // re-review of the same still-open finding keeps posted=true
    s.upsert(&f, FindingState::Open);
    assert!(s.findings[0].posted);
}

#[test]
fn recheck_transitions() {
    assert_eq!(
        recheck_transition(ValidationStatus::Rejected),
        FindingState::Resolved
    );
    assert_eq!(
        recheck_transition(ValidationStatus::Accepted),
        FindingState::Open
    );
    assert_eq!(
        recheck_transition(ValidationStatus::Uncertain),
        FindingState::Uncertain
    );
}

#[test]
fn surfaced_ids_only_returns_accepted_findings() {
    let accepted = finding("accepted.rs", "accepted", 1);
    let mut rejected = finding("rejected.rs", "rejected", 2);
    rejected.validation_status = Some(ValidationStatus::Rejected);
    let report = RunReport {
        status: RunStatus::Complete,
        reason: None,
        base: "b".into(),
        head: "h".into(),
        strategy: "baseline".into(),
        findings: vec![accepted.clone(), rejected],
        plan: PublicationPlan {
            inline: vec![],
            summary_markdown: String::new(),
            state: ReviewState::default(),
        },
        stats: Default::default(),
        ledger: LedgerReport {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            by_route: vec![],
            wall_ms: 0,
        },
        publication: Default::default(),
        coverage_gaps: vec![],
        timing: Default::default(),
    };
    assert_eq!(surfaced_ids(&report), vec![accepted.id()]);
}
