use revera::provider::scripted::ScriptedClient;
use revera::provider::{ChatMessage, LedgerHandle, ModelClient, ProviderError};
use serde_json::json;
use std::io::Write;

fn script_file(body: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f
}

#[tokio::test]
async fn scripted_ordering_and_conversations() {
    let f = script_file(
        r#"{"roles": {"investigator": [
        [{"content": "first"}, {"tool_calls": [{"name": "submit_findings", "arguments": {"findings": [], "coverage": "x"}}]}],
        [{"content": "second"}]
    ]}}"#,
    );
    let ledger = LedgerHandle::new();
    let c1 =
        ScriptedClient::new(f.path(), "investigator", ledger.clone(), "submit_findings").unwrap();
    let m1 = c1.complete(&[], &[]).await.unwrap();
    assert_eq!(m1.message.content.as_deref(), Some("first"));
    let m2 = c1.complete(&[], &[]).await.unwrap();
    assert_eq!(m2.message.tool_calls[0].name, "submit_findings");
    // second session pops the second conversation
    let c2 =
        ScriptedClient::new(f.path(), "investigator", ledger.clone(), "submit_findings").unwrap();
    let m = c2.complete(&[], &[]).await.unwrap();
    assert_eq!(m.message.content.as_deref(), Some("second"));
    // third session exhausted -> terminal call
    let c3 = ScriptedClient::new(f.path(), "investigator", ledger, "submit_findings").unwrap();
    let m = c3.complete(&[], &[]).await.unwrap();
    assert_eq!(m.message.tool_calls[0].name, "submit_findings");
}

#[tokio::test]
async fn scripted_expectation_failure() {
    let f = script_file(
        r#"{"roles": {"investigator": [[
        {"tool_calls": [{"name": "vera_references", "arguments": {"symbol": "s"}}]},
        {"content": "next", "expect_tool_result_contains": "checkout.rs"}
    ]]}}"#,
    );
    let ledger = LedgerHandle::new();
    let c = ScriptedClient::new(f.path(), "investigator", ledger, "submit_findings").unwrap();
    c.complete(&[], &[]).await.unwrap();
    // feed a tool result NOT containing the needle
    let msgs = vec![
        ChatMessage::system("s"),
        ChatMessage::tool("1", "vera_references", "nothing relevant here"),
    ];
    let e = c.complete(&msgs, &[]).await.unwrap_err();
    assert!(
        matches!(e, ProviderError::ScriptExpectationFailed(_)),
        "{e}"
    );

    // and succeeds when it does contain the needle
    let f2 = script_file(
        r#"{"roles": {"investigator": [[
        {"tool_calls": [{"name": "x", "arguments": {}}]},
        {"content": "ok", "expect_tool_result_contains": "checkout.rs"}
    ]]}}"#,
    );
    let c2 = ScriptedClient::new(f2.path(), "investigator", LedgerHandle::new(), "t").unwrap();
    c2.complete(&[], &[]).await.unwrap();
    let msgs = vec![ChatMessage::tool("1", "x", "... checkout.rs ...")];
    c2.complete(&msgs, &[]).await.unwrap();
}

#[test]
fn summary_rendering() {
    use revera::findings::{Finding, Severity, ValidationStatus};
    let f = Finding {
        defect_key: "k".into(),
        severity: Severity::High,
        file: "src/a.rs".into(),
        start_line: 12,
        end_line: None,
        title: "bad thing".into(),
        claim: "c".into(),
        trigger: "t".into(),
        impact: "i".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: Some(ValidationStatus::Accepted),
        suggested_fix: None,
        source: "investigator".into(),
        rationale: None,
        sources: vec![],
        assurance: None,
        quoted_code: None,
        suggested_replacement: None,
        quote_anchored: false,
    };
    let md = revera::report::summary_markdown(&revera::report::Summary {
        findings: &[f],
        coverage: "checked callers",
        status: Some(revera::report::RunStatus::Complete),
        strategy: "baseline",
        routes: &["openai-chat:http://x:m".into()],
        ..Default::default()
    });
    assert!(md.contains("Revera review"));
    assert!(md.contains("**[high]** `src/a.rs`:12"));
    assert!(md.contains("Not checked: checked callers"));
    let body = revera::report::finding_body(&revera::findings::Finding {
        defect_key: "k".into(),
        severity: Severity::High,
        file: "src/a.rs".into(),
        start_line: 12,
        end_line: None,
        title: "bad thing".into(),
        claim: "c".into(),
        trigger: "t".into(),
        impact: "i".into(),
        introduced_by_change: true,
        supporting_evidence: vec![],
        counterevidence_checked: vec![],
        validation_status: Some(ValidationStatus::Accepted),
        suggested_fix: None,
        source: "investigator".into(),
        rationale: None,
        sources: vec![],
        assurance: None,
        quoted_code: None,
        suggested_replacement: None,
        quote_anchored: false,
    });
    assert!(body.contains("**[high] bad thing**"));
    assert!(body.contains("<!-- revera-id:"));
    let _ = json!(0);
}
