use crate::findings::{Finding, Severity, ValidationStatus};
use crate::pipeline::anchor::is_publishable;
use crate::provider::RunLedger;
use crate::state::ReviewState;
use crate::timing::Timing;
use crate::tools::ToolStat;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineComment {
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationPlan {
    pub inline: Vec<InlineComment>,
    pub summary_markdown: String,
    pub state: ReviewState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteLedger {
    pub route: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub requested_reasoning: String,
    #[serde(default)]
    pub effective_reasoning: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LedgerReport {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    pub by_route: Vec<RouteLedger>,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Publication {
    /// "dry-run" | "comment"
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_comment_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

impl Default for Publication {
    fn default() -> Self {
        Self {
            mode: "dry-run".into(),
            review_id: None,
            summary_comment_id: None,
            skipped_reason: None,
        }
    }
}

/// Lightweight run telemetry (no transcripts, no source, no secrets).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunStats {
    /// Candidates after collapse/dedupe, before validation.
    pub candidates: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub uncertain: usize,
    /// Prior open findings resolved / reopened by this run.
    pub resolved: usize,
    pub reopened: usize,
    /// Completed prior review of identical content was reused.
    pub reused: bool,
    /// "vera" | "disabled" | "unavailable: <why>"
    pub retrieval: String,
    /// Distinct files read with `read_file` across all agents.
    pub files_read: usize,
    pub tools: Vec<ToolStat>,
    /// Malformed items dropped from terminal submissions.
    pub malformed_findings: usize,
    /// A terminal repair round was attempted.
    pub repaired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub base: String,
    pub head: String,
    pub strategy: String,
    pub findings: Vec<Finding>,
    pub plan: PublicationPlan,
    pub ledger: LedgerReport,
    #[serde(default)]
    pub publication: Publication,
    /// Things the run could not check (worker gaps/blocked items).
    #[serde(default)]
    pub coverage_gaps: Vec<String>,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default)]
    pub stats: RunStats,
}

pub fn surfaced_ids(report: &RunReport) -> Vec<String> {
    report
        .findings
        .iter()
        .filter(|f| is_publishable(f))
        .map(|f| f.id())
        .collect()
}

/// Strip `<!--` so model text cannot forge our HTML markers.
fn sanitize(t: &str) -> String {
    t.replace("<!--", "<!\u{200b}--")
}

/// Longest run of backticks in `s`.
fn backtick_run(s: &str) -> usize {
    s.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

static SECRET_REDACT_REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"(?x)
        \bAKIA[0-9A-Z]{16}\b |
        \bgh[pousr]_[A-Za-z0-9_]{36,255}\b |
        \bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b |
        -----BEGIN[A-Z\x20]*PRIVATE\x20KEY-----
    "#,
    )
    .unwrap()
});

/// Redact detected credentials and secrets from text.
pub fn redact_secrets(input: &str) -> String {
    SECRET_REDACT_REGEX
        .replace_all(input, "[REDACTED_CREDENTIAL]")
        .into_owned()
}

pub fn finding_body(f: &Finding) -> String {
    let clean_title = redact_secrets(&sanitize(&f.title));
    let clean_claim = redact_secrets(&sanitize(&f.claim));
    let mut b = format!("**[{}] {}**\n\n{}\n", f.severity, clean_title, clean_claim);
    if !f.trigger.is_empty() {
        b.push_str(&format!(
            "\nTrigger: {}\n",
            redact_secrets(&sanitize(&f.trigger))
        ));
    }
    if !f.impact.is_empty() {
        b.push_str(&format!(
            "Impact: {}\n",
            redact_secrets(&sanitize(&f.impact))
        ));
    }
    if !f.supporting_evidence.is_empty() {
        let ev: Vec<String> = f
            .supporting_evidence
            .iter()
            .map(|e| format!("{}:{}", sanitize(&e.path), e.start_line))
            .collect();
        b.push_str(&format!("Evidence: {}\n", ev.join(", ")));
    }
    if let Some(fix) = &f.suggested_fix {
        let fix = redact_secrets(&sanitize(fix));
        let fence = "`".repeat(3.max(backtick_run(&fix) + 1));
        b.push_str(&format!("\nSuggested fix:\n{fence}\n{fix}\n{fence}\n"));
    }
    if f.sources.len() > 1 {
        let sources = f
            .sources
            .iter()
            .map(|s| sanitize(s))
            .collect::<Vec<_>>()
            .join(", ");
        b.push_str(&format!(
            "\n> **Consensus**: Flagged independently by {} review lenses: {}.\n",
            f.sources.len(),
            sources
        ));
    }
    let assurance = f.effective_assurance();
    b.push_str("\n<details>\n<summary>Assurance Trace</summary>\n\n");
    if let Some(rule) = &assurance.rule_id {
        b.push_str(&format!("- **Origin**: Static Rule `{}`\n", sanitize(rule)));
    } else {
        b.push_str(&format!("- **Origin**: Lens `{}`\n", sanitize(&f.source)));
    }
    if !assurance.trigger.is_empty() {
        b.push_str(&format!(
            "- **Trigger**: {}\n",
            redact_secrets(&sanitize(&assurance.trigger))
        ));
    }
    if !assurance.rationale.is_empty() {
        b.push_str(&format!(
            "- **Rationale**: {}\n",
            redact_secrets(&sanitize(&assurance.rationale))
        ));
    }
    if !assurance.counterevidence_checked.is_empty() {
        let ce = assurance
            .counterevidence_checked
            .iter()
            .map(|c| redact_secrets(&sanitize(c)))
            .collect::<Vec<_>>()
            .join("; ");
        b.push_str(&format!("- **Counter-evidence Checked**: {}\n", ce));
    }
    if let Some(rederivation) = &assurance.validator_rederivation {
        b.push_str(&format!(
            "- **Validator Re-derivation**: {}\n",
            redact_secrets(&sanitize(rederivation))
        ));
    }
    b.push_str("\n</details>\n");
    b.push_str(&format!("\n<!-- revera-id:{} -->", f.id()));
    b
}

/// Inputs for the human-facing summary.
#[derive(Default)]
pub struct Summary<'a> {
    pub findings: &'a [Finding],
    pub outside_diff: &'a [&'a Finding],
    pub coverage: &'a str,
    pub coverage_gaps: &'a [String],
    pub status: Option<RunStatus>,
    /// Why the run is partial/failed.
    pub reason: Option<&'a str>,
    pub strategy: &'a str,
    pub routes: &'a [String],
    pub publish_uncertain: bool,
    pub note: Option<&'a str>,
    pub timing: Option<&'a Timing>,
    /// Completed prior review of identical content reused.
    pub reused: bool,
    /// Open findings carried over from prior runs (already published).
    pub carried_open: usize,
    /// Retrieval degradation to surface (`None` when Vera worked or is off).
    pub retrieval_unavailable: Option<&'a str>,
    pub resolved: &'a [String],
    pub reopened: &'a [String],
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

pub fn summary_markdown(inp: &Summary<'_>) -> String {
    let status = inp.status.unwrap_or(RunStatus::Complete);
    let mut s = String::from("## Revera review\n\n");
    let accepted: Vec<&Finding> = inp
        .findings
        .iter()
        .filter(|f| f.validation_status == Some(ValidationStatus::Accepted))
        .collect();
    let uncertain = inp
        .findings
        .iter()
        .filter(|f| f.validation_status == Some(ValidationStatus::Uncertain))
        .count();
    // headline: status first, so a partial run with zero findings can never
    // read like a clean pass
    let status_word = format!("{status:?}").to_lowercase();
    let count = if inp.reused {
        format!(
            "reused completed review of identical content; {} carried forward",
            plural(inp.carried_open, "open finding")
        )
    } else {
        let mut c = if accepted.is_empty() {
            "no new findings".to_string()
        } else {
            let high = accepted
                .iter()
                .filter(|f| f.severity == crate::findings::Severity::High)
                .count();
            let med = accepted
                .iter()
                .filter(|f| f.severity == crate::findings::Severity::Medium)
                .count();
            let low = accepted.len() - high - med;
            let mut parts = vec![];
            if high > 0 {
                parts.push(format!("{high} high"));
            }
            if med > 0 {
                parts.push(format!("{med} medium"));
            }
            if low > 0 {
                parts.push(format!("{low} low"));
            }
            format!(
                "{} ({})",
                plural(accepted.len(), "new finding"),
                parts.join(", ")
            )
        };
        if uncertain > 0 {
            c.push_str(&format!(", {} unconfirmed", uncertain));
        }
        if inp.carried_open > 0 {
            c.push_str(&format!(
                ", {} still open from earlier runs",
                inp.carried_open
            ));
        }
        c
    };
    s.push_str(&format!("**Status: {status_word}** \u{2014} {count}\n"));
    if status != RunStatus::Complete {
        s.push_str(&format!(
            "\n> Review incomplete{}. Absence of findings is not evidence the change is clean; rerun or inspect the parts listed as not checked.\n",
            inp.reason
                .filter(|r| !r.is_empty())
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        ));
    }
    if let Some(r) = inp.retrieval_unavailable {
        s.push_str(&format!(
            "\n> Semantic retrieval unavailable ({r}); repository-wide lookups used lexical search only.\n"
        ));
    }
    s.push('\n');
    if !accepted.is_empty() {
        for f in &accepted {
            s.push_str(&format!(
                "- **[{}]** `{}`:{} — {}\n",
                f.severity, f.file, f.start_line, f.title
            ));
        }
    }
    if inp.publish_uncertain {
        for f in inp
            .findings
            .iter()
            .filter(|f| f.validation_status == Some(ValidationStatus::Uncertain))
        {
            s.push_str(&format!(
                "- _(unconfirmed)_ **[{}]** `{}`:{} — {}\n",
                f.severity, f.file, f.start_line, f.title
            ));
        }
    }
    if !inp.outside_diff.is_empty() {
        s.push_str("\n### Findings outside the diff\n\n");
        for f in inp.outside_diff {
            s.push_str(&format!(
                "- **[{}]** `{}`:{} — {}\n",
                f.severity, f.file, f.start_line, f.title
            ));
        }
    }
    if !inp.reopened.is_empty() {
        s.push_str("\n### Reopened (previously resolved, reintroduced)\n\n");
        for t in inp.reopened {
            s.push_str(&format!("- {t}\n"));
        }
    }
    if !inp.resolved.is_empty() {
        s.push_str("\n### Resolved since last review\n\n");
        for t in inp.resolved {
            s.push_str(&format!("- {t}\n"));
        }
    }
    if !inp.reused {
        s.push_str(&format!("\nNot checked: {}\n", inp.coverage));
        for g in inp.coverage_gaps {
            s.push_str(&format!("- not checked: {g}\n"));
        }
    }
    if let Some(n) = inp.note {
        s.push_str(&format!("\n_{n}_\n"));
    }
    if let Some(t) = inp.timing {
        s.push('\n');
        s.push_str(&timing_line(t));
    }
    let routes = {
        let mut r = inp.routes.to_vec();
        r.sort();
        r.dedup();
        r.join(", ")
    };
    s.push_str(&format!(
        "\n<sub>revera · strategy {} · models: {routes}</sub>\n",
        inp.strategy
    ));
    s
}

/// "62s" for >=10s, "6.2s" below.
fn fmt_secs(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s >= 10.0 {
        format!("{s:.0}s")
    } else {
        format!("{s:.1}s")
    }
}

/// Italic summary line of wall-clock phases, e.g.
/// `_Timing: total 62s · index 4s · lanes 30s · validation 20s (p50 6s, first validated at 41s) · 1 phase incomplete_`
pub fn timing_line(t: &Timing) -> String {
    let mut seg = vec![format!("total {}", fmt_secs(t.total_ms))];
    if let Some(i) = t.vera_index_ms {
        seg.push(format!("index {}", fmt_secs(i)));
    }
    if let Some(l) = t.lanes_ms {
        seg.push(format!("lanes {}", fmt_secs(l)));
    }
    if let Some(v) = t.validate_ms {
        let mut inner: Vec<String> = vec![];
        if let Some(p) = t.validate_p50_ms {
            inner.push(format!("p50 {}", fmt_secs(p)));
        }
        if let Some(p) = t.validate_p95_ms {
            inner.push(format!("p95 {}", fmt_secs(p)));
        }
        if let Some(f) = t.first_candidate_ms {
            inner.push(format!("first candidate at {}", fmt_secs(f)));
        }
        if let Some(f) = t.first_validated_ms {
            inner.push(format!("first validated at {}", fmt_secs(f)));
        }
        if inner.is_empty() {
            seg.push(format!("validation {}", fmt_secs(v)));
        } else {
            seg.push(format!("validation {} ({})", fmt_secs(v), inner.join(", ")));
        }
    } else {
        if let Some(f) = t.first_candidate_ms {
            seg.push(format!("first candidate at {}", fmt_secs(f)));
        }
    }
    if t.incomplete_phases > 0 {
        seg.push(format!("{} phase(s) incomplete", t.incomplete_phases));
    }
    if t.skipped_phases > 0 {
        seg.push(format!("{} phase(s) skipped", t.skipped_phases));
    }
    format!("\n_Timing: {}_\n", seg.join(" \u{b7} "))
}

/// Replace the `_Timing: ..._` line inside a summary built by
/// `summary_markdown` with a freshly rendered one (e.g. after the publish
/// phase was appended). Returns the summary unchanged when no timing line
/// is present.
pub fn refresh_timing_line(summary: &str, t: &Timing) -> String {
    let Some(beg) = summary.find("_Timing:") else {
        return summary.to_string();
    };
    let line_start = summary[..beg].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let Some(rel_end) = summary[beg..].find("_\n") else {
        return summary.to_string();
    };
    let line_end = beg + rel_end + 2; // consume the trailing "_\n"
    let mut out = String::with_capacity(summary.len());
    out.push_str(&summary[..line_start]);
    out.push_str(timing_line(t).trim_start_matches('\n'));
    out.push_str(&summary[line_end..]);
    out
}

/// `<role>=<route>:<model>@<requested>` (+ `-><effective>` when the sent
/// effort differs from the requested one, e.g. after a 400 step-down).
pub fn ledger_route_label(e: &crate::provider::LedgerEntry) -> String {
    if e.effective_reasoning != e.requested_reasoning {
        format!(
            "{}={}:{}@{}->{}",
            e.role, e.route, e.model, e.requested_reasoning, e.effective_reasoning
        )
    } else {
        format!(
            "{}={}:{}@{}",
            e.role, e.route, e.model, e.requested_reasoning
        )
    }
}

pub fn ledger_report(ledger: &RunLedger, wall_ms: u64) -> LedgerReport {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<(String, String, String, String, String), RouteLedger> = BTreeMap::new();
    let (mut pr, mut cr, mut rr) = (0u64, 0u64, 0u64);
    for e in &ledger.entries {
        let key = (
            e.role.clone(),
            e.route.clone(),
            e.model.clone(),
            e.requested_reasoning.clone(),
            e.effective_reasoning.clone(),
        );
        let r = by.entry(key).or_insert(RouteLedger {
            route: e.route.clone(),
            role: e.role.clone(),
            model: e.model.clone(),
            requested_reasoning: e.requested_reasoning.clone(),
            effective_reasoning: e.effective_reasoning.clone(),
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
        });
        r.requests += 1;
        r.prompt_tokens += e.prompt_tokens;
        r.completion_tokens += e.completion_tokens;
        r.reasoning_tokens += e.reasoning_tokens;
        pr += e.prompt_tokens;
        cr += e.completion_tokens;
        rr += e.reasoning_tokens;
    }
    LedgerReport {
        requests: ledger.entries.len() as u64,
        prompt_tokens: pr,
        completion_tokens: cr,
        reasoning_tokens: rr,
        by_route: by.into_values().collect(),
        wall_ms,
    }
}

/// Generate OASIS SARIF 2.1.0 JSON representation of the run findings.
pub fn to_sarif(report: &RunReport) -> serde_json::Value {
    let rules: Vec<serde_json::Value> = report
        .findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "id": f.source,
                "name": f.title,
                "shortDescription": {
                    "text": f.title
                },
                "fullDescription": {
                    "text": f.claim
                },
                "defaultConfiguration": {
                    "level": match f.severity {
                        Severity::High => "error",
                        Severity::Medium => "warning",
                        Severity::Low => "note",
                    }
                }
            })
        })
        .collect();

    let results: Vec<serde_json::Value> = report
        .findings
        .iter()
        .map(|f| {
            let mut result = serde_json::json!({
                "ruleId": f.source,
                "level": match f.severity {
                    Severity::High => "error",
                    Severity::Medium => "warning",
                    Severity::Low => "note",
                },
                "message": {
                    "text": format!("{}\n\nTrigger: {}", f.claim, f.trigger)
                },
                "locations": [
                    {
                        "physicalLocation": {
                            "artifactLocation": {
                                "uri": f.file,
                                "uriBaseId": "%SRCROOT%"
                            },
                            "region": {
                                "startLine": f.start_line,
                                "endLine": f.end_line.unwrap_or(f.start_line)
                            }
                        }
                    }
                ]
            });
            if let Some(fix) = &f.suggested_fix {
                result["fixes"] = serde_json::json!([
                    {
                        "description": {
                            "text": "Suggested fix"
                        },
                        "replacement": fix
                    }
                ]);
            }
            result
        })
        .collect();

    serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "revera",
                        "version": env!("CARGO_PKG_VERSION"),
                        "informationUri": "https://github.com/citron07r/revera",
                        "rules": rules
                    }
                },
                "results": results
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{LedgerEntry, RunLedger};

    fn entry(role: &str, requested: &str, effective: &str) -> LedgerEntry {
        LedgerEntry {
            role: role.into(),
            route: "openai-chat:https://api.example.com/v1".into(),
            model: "m".into(),
            requested_reasoning: requested.into(),
            effective_reasoning: effective.into(),
            ..Default::default()
        }
    }

    #[test]
    fn roles_distinct_routes_and_efforts() {
        // same route+model, different roles/efforts -> two ledger groups and
        // two distinct footer route strings
        let mut l = RunLedger::default();
        l.entries.push(entry("investigator", "max", "max"));
        l.entries.push(entry("validator", "high", "high"));
        let rep = ledger_report(&l, 0);
        assert_eq!(rep.by_route.len(), 2);
        let labels: Vec<String> = l.entries.iter().map(ledger_route_label).collect();
        assert_eq!(
            labels,
            vec![
                "investigator=openai-chat:https://api.example.com/v1:m@max",
                "validator=openai-chat:https://api.example.com/v1:m@high",
            ]
        );
    }

    #[test]
    fn step_down_renders_arrow() {
        let e = entry("investigator", "max", "high");
        assert_eq!(
            ledger_route_label(&e),
            "investigator=openai-chat:https://api.example.com/v1:m@max->high"
        );
        let mut l = RunLedger::default();
        l.entries.push(e);
        let rep = ledger_report(&l, 0);
        assert_eq!(rep.by_route[0].requested_reasoning, "max");
        assert_eq!(rep.by_route[0].effective_reasoning, "high");
    }

    #[test]
    fn refresh_timing_line_replaces_in_place() {
        let t0 = Timing {
            total_ms: 1000,
            ..Default::default()
        };
        let mut summary = summary_markdown(&Summary {
            coverage: "nothing",
            status: Some(RunStatus::Complete),
            strategy: "baseline",
            timing: Some(&t0),
            ..Default::default()
        });
        assert!(summary.contains("_Timing: total 1.0s_"));
        let t1 = Timing {
            total_ms: 2000,
            skipped_phases: 2,
            ..Default::default()
        };
        summary = refresh_timing_line(&summary, &t1);
        assert!(
            summary.contains("_Timing: total 2.0s \u{b7} 2 phase(s) skipped_"),
            "{summary}"
        );
        assert!(!summary.contains("total 1.0s"));
        // untouched text stays
        assert!(summary.contains("**Status: complete**"));
        // no timing line -> unchanged
        let plain = "## Revera review\n\nNo findings.\n";
        assert_eq!(refresh_timing_line(plain, &t1), plain);
    }

    #[test]
    fn sarif_export_generates_valid_schema() {
        let f = Finding {
            defect_key: "k".into(),
            severity: Severity::High,
            file: "src/lib.rs".into(),
            start_line: 10,
            end_line: Some(15),
            title: "Test Title".into(),
            claim: "Test Claim".into(),
            trigger: "Test Trigger".into(),
            impact: "Test Impact".into(),
            introduced_by_change: true,
            supporting_evidence: vec![],
            counterevidence_checked: vec![],
            validation_status: None,
            suggested_fix: Some("let x = 1;".into()),
            source: "test-rule".into(),
            rationale: None,
            sources: vec!["test-rule".into()],
            assurance: None,
        };

        let rep = RunReport {
            status: RunStatus::Complete,
            reason: None,
            base: "base".into(),
            head: "head".into(),
            strategy: "baseline".into(),
            findings: vec![f],
            plan: PublicationPlan {
                inline: vec![],
                summary_markdown: String::new(),
                state: Default::default(),
            },
            ledger: Default::default(),
            publication: Default::default(),
            coverage_gaps: vec![],
            timing: Default::default(),
            stats: Default::default(),
        };

        let sarif = to_sarif(&rep);
        assert_eq!(sarif["version"], "2.1.0");
        assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "revera");
        assert_eq!(sarif["runs"][0]["results"].as_array().unwrap().len(), 1);
        assert_eq!(sarif["runs"][0]["results"][0]["level"], "error");
        assert_eq!(
            sarif["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "src/lib.rs"
        );
    }
}
