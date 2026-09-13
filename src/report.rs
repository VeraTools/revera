use crate::findings::{Finding, ValidationStatus};
use crate::pipeline::anchor::is_publishable;
use crate::provider::RunLedger;
use crate::state::ReviewState;
use crate::timing::Timing;
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
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

pub fn finding_body(f: &Finding) -> String {
    let mut b = format!(
        "**[{}] {}**\n\n{}\n",
        f.severity,
        sanitize(&f.title),
        sanitize(&f.claim)
    );
    if !f.trigger.is_empty() {
        b.push_str(&format!("\nTrigger: {}\n", sanitize(&f.trigger)));
    }
    if !f.impact.is_empty() {
        b.push_str(&format!("Impact: {}\n", sanitize(&f.impact)));
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
        let fix = sanitize(fix);
        let fence = "`".repeat(3.max(backtick_run(&fix) + 1));
        b.push_str(&format!("\nSuggested fix:\n{fence}\n{fix}\n{fence}\n"));
    }
    b.push_str(&format!("\n<!-- revera-id:{} -->", f.id()));
    b
}

#[allow(clippy::too_many_arguments)]
pub fn summary_markdown(
    findings: &[Finding],
    outside_diff: &[&Finding],
    coverage: &str,
    coverage_gaps: &[String],
    status: RunStatus,
    strategy: &str,
    routes: &[String],
    publish_uncertain: bool,
    note: Option<&str>,
    timing: Option<&Timing>,
) -> String {
    let mut s = String::from("## Revera review\n\n");
    let accepted: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.validation_status == Some(ValidationStatus::Accepted))
        .collect();
    if accepted.is_empty() {
        s.push_str("No findings.\n");
    } else {
        for f in &accepted {
            s.push_str(&format!(
                "- **[{}]** `{}`:{} — {}\n",
                f.severity, f.file, f.start_line, f.title
            ));
        }
    }
    if publish_uncertain {
        for f in findings
            .iter()
            .filter(|f| f.validation_status == Some(ValidationStatus::Uncertain))
        {
            s.push_str(&format!(
                "- _(unconfirmed)_ **[{}]** `{}`:{} — {}\n",
                f.severity, f.file, f.start_line, f.title
            ));
        }
    }
    if !outside_diff.is_empty() {
        s.push_str("\n### Findings outside the diff\n\n");
        for f in outside_diff {
            s.push_str(&format!(
                "- **[{}]** `{}`:{} — {}\n",
                f.severity, f.file, f.start_line, f.title
            ));
        }
    }
    s.push_str(&format!("\nNot checked: {}\n", coverage));
    for g in coverage_gaps {
        s.push_str(&format!("- not checked: {g}\n"));
    }
    if let Some(n) = note {
        s.push_str(&format!("\n_{n}_\n"));
    }
    s.push_str(&format!("\nStatus: {:?}\n", status).to_lowercase());
    if let Some(t) = timing {
        s.push_str(&timing_line(t));
    }
    let routes = {
        let mut r = routes.to_vec();
        r.sort();
        r.dedup();
        r.join(", ")
    };
    s.push_str(&format!(
        "\n<sub>revera · strategy {strategy} · models: {routes}</sub>\n"
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
    format!("\n_Timing: {}_\n", seg.join(" \u{b7} "))
}

pub fn ledger_report(ledger: &RunLedger, wall_ms: u64) -> LedgerReport {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<String, RouteLedger> = BTreeMap::new();
    let (mut pr, mut cr, mut rr) = (0u64, 0u64, 0u64);
    for e in &ledger.entries {
        let r = by.entry(e.route.clone()).or_insert(RouteLedger {
            route: e.route.clone(),
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
