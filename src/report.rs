use crate::findings::{Finding, ValidationStatus};
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
    /// Public projection of the state (see `ReviewState::public_projection`).
    pub state: ReviewState,
    /// Whether uncertain findings are surfaced (and so may be projected).
    #[serde(default)]
    pub publish_uncertain: bool,
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
    pub cached_prompt_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerReport {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_prompt_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    pub by_route: Vec<RouteLedger>,
    #[serde(default)]
    pub fallback_events: Vec<crate::provider::FallbackEvent>,
    #[serde(default)]
    pub unavailable_routes: Vec<crate::provider::UnavailableRoute>,
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
    /// "lexical-only" | "vera" | "vera+rerank" | "vera (rerank degraded)" |
    /// "unavailable: <why>" | "not needed", with an optional fallback suffix.
    pub retrieval: String,
    /// Distinct files read with `read_file` across all agents.
    pub files_read: usize,
    pub tools: Vec<ToolStat>,
    /// Malformed items dropped from terminal submissions.
    pub malformed_findings: usize,
    /// A terminal repair round was attempted.
    pub repaired: bool,
    /// Findings without any validator verdict (validation disabled).
    #[serde(default)]
    pub unvalidated: usize,
    /// "fresh" (per-candidate validator) | "reused" (identical content) |
    /// "not needed" (nothing reviewable) | "disabled" (evaluation-only).
    #[serde(default)]
    pub validation: String,
    /// Findings dropped because validator-corrected severity fell below
    /// `min_severity`.
    #[serde(default)]
    pub below_threshold_after_validation: usize,
    /// Base-commit guidance given to the investigator (sources + digest;
    /// never rendered into public comments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<crate::guidance::Guidance>,
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
    /// Previously published findings still open, not newly validated or publishable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedFinding>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarriedFinding {
    pub id: String,
    /// Retained to reconstruct the stable finding identity, not a new verdict.
    pub defect_key: String,
    pub severity: crate::findings::Severity,
    pub title: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: Option<u32>,
    pub claim: String,
    pub trigger: String,
    pub impact: String,
    pub evidence: Vec<String>,
}

impl CarriedFinding {
    pub fn from_state(f: &crate::state::StateFinding) -> Self {
        Self {
            id: f.id.clone(),
            defect_key: f.defect_key.clone(),
            severity: f.severity,
            title: f.title.clone(),
            file: f.file.clone(),
            start_line: f.start_line,
            end_line: f.end_line,
            claim: f.claim.clone(),
            trigger: f.trigger.clone(),
            impact: f.impact.clone(),
            evidence: f.evidence.clone(),
        }
    }

    /// Repair context only: deliberately has no current-run validator verdict.
    pub fn to_finding(&self) -> Finding {
        let state = crate::state::StateFinding {
            id: self.id.clone(),
            defect_key: self.defect_key.clone(),
            severity: self.severity,
            title: self.title.clone(),
            file: self.file.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            claim: self.claim.clone(),
            trigger: self.trigger.clone(),
            impact: self.impact.clone(),
            evidence: self.evidence.clone(),
            status: crate::state::FindingState::Open,
            posted: true,
        };
        Finding {
            source: "carried".into(),
            ..state.to_finding()
        }
    }
}

/// The same eligibility rule for fresh and identical-content reviews.
pub fn carried_findings(state: &ReviewState, findings: &[Finding]) -> Vec<CarriedFinding> {
    let ids: std::collections::HashSet<_> = findings.iter().map(Finding::id).collect();
    state
        .findings
        .iter()
        .filter(|f| {
            f.status == crate::state::FindingState::Open && f.posted && !ids.contains(&f.id)
        })
        .map(CarriedFinding::from_state)
        .collect()
}

impl RunReport {
    pub fn actionable_findings(&self) -> Vec<Finding> {
        self.findings
            .iter()
            .filter(|f| f.validation_status == Some(ValidationStatus::Accepted))
            .cloned()
            .chain(self.carried.iter().map(CarriedFinding::to_finding))
            .collect()
    }
}

pub fn surfaced_ids(report: &RunReport) -> Vec<String> {
    report
        .findings
        .iter()
        .filter(|f| is_publishable(f))
        .map(|f| f.id())
        .collect()
}

/// Neutralize active links, mentions and HTML without damaging inline code or
/// multiline prose. Markers are neutralized even inside code: Rust reads them
/// without a Markdown parser when recovering publication state.
pub fn public_text(t: &str) -> String {
    let t = crate::redact::text(t).replace("<!--", "<!\u{200b}--");
    let mut out = String::new();
    let mut rest = t.as_str();
    let mut line_start = true;
    while !rest.is_empty() {
        if line_start {
            let indent = rest.len() - rest.trim_start_matches(' ').len();
            let after = &rest[indent..];
            if indent <= 3 && (after.starts_with("```") || after.starts_with("~~~")) {
                out.push_str(&rest[..indent]);
                out.push('\\');
                out.push_str(&after[..1]);
                rest = &after[1..];
            }
            line_start = false;
        }
        // Preserve Markdown escapes, but do not let them undo neutralization.
        if rest.starts_with('\\') && rest.as_bytes().get(1).is_some_and(u8::is_ascii_punctuation) {
            match rest.as_bytes()[1] {
                b'@' => out.push_str("@\u{200b}"),
                b'<' => out.push_str("&lt;"),
                _ => out.push_str(&rest[..2]),
            }
            rest = &rest[2..];
            continue;
        }
        if rest.starts_with('`') {
            let run = rest.bytes().take_while(|&c| c == b'`').count();
            let line = rest.split('\n').next().unwrap();
            let mut end = run;
            let mut matched = false;
            while let Some(next) = line[end..].find('`') {
                let start = end + next;
                end = start + line[start..].bytes().take_while(|&c| c == b'`').count();
                if end - start == run {
                    out.push_str(&rest[..end]);
                    rest = &rest[end..];
                    matched = true;
                    break;
                }
            }
            if !matched {
                // Unmatched delimiters are literal text, not a safe code span.
                out.push_str(&rest[..run]);
                rest = &rest[run..];
            }
            continue;
        }
        let c = rest.chars().next().unwrap();
        // GitHub also autolinks bare URLs, even after Markdown links are escaped.
        if ["https://", "http://", "ftp://", "www."]
            .iter()
            .any(|prefix| {
                rest.get(..prefix.len())
                    .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
            })
        {
            out.push(c);
            out.push('\u{200b}');
            rest = &rest[c.len_utf8()..];
            continue;
        }
        match c {
            '@' => out.push_str("@\u{200b}"),
            '<' => out.push_str("&lt;"),
            '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
        line_start = c == '\n';
        rest = &rest[c.len_utf8()..];
    }
    out
}

pub fn public_line(t: &str) -> String {
    public_text(&t.replace(['\r', '\n'], " "))
}

/// A file path is literal code, not prose. A longer delimiter (and padding
/// when backticks touch its edges) keeps arbitrary paths inside the span.
pub fn public_code_span(t: &str) -> String {
    let t = crate::redact::text(t)
        .replace(['\r', '\n'], " ")
        .replace("<!--", "<!\u{200b}--");
    let fence = "`".repeat(backtick_run(&t) + 1);
    let padding = if t.starts_with('`')
        || t.ends_with('`')
        || (t.starts_with(' ') && t.ends_with(' ') && !t.trim().is_empty())
    {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{t}{padding}{fence}")
}

/// Longest run of backticks in `s`.
fn backtick_run(s: &str) -> usize {
    s.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

pub fn finding_body(f: &Finding) -> String {
    let mut b = format!(
        "**[{}] {}**\n\n{}\n",
        f.severity,
        public_line(&f.title),
        public_text(&f.claim)
    );
    if !f.trigger.is_empty() {
        b.push_str(&format!("\nTrigger: {}\n", public_text(&f.trigger)));
    }
    if !f.impact.is_empty() {
        b.push_str(&format!("Impact: {}\n", public_text(&f.impact)));
    }
    if !f.supporting_evidence.is_empty() {
        let ev: Vec<String> = f
            .supporting_evidence
            .iter()
            .map(|e| format!("{}:{}", public_code_span(&e.path), e.start_line))
            .collect();
        b.push_str(&format!("Evidence: {}\n", ev.join(", ")));
    }
    if let Some(fix) = &f.validated_fix {
        let fix = crate::redact::text(fix).replace("<!--", "<!\u{200b}--");
        let fence = "`".repeat(3.max(backtick_run(&fix) + 1));
        b.push_str(&format!(
            "\nValidator's suggested remedy — not a tested patch:\n{fence}\n{fix}\n{fence}\n"
        ));
    }
    if f.validation_status == Some(ValidationStatus::Accepted)
        && let Some(s) = &f.suggestion
        && s.start_line == f.start_line
        && s.end_line == f.end_line.unwrap_or(f.start_line)
    {
        let fence = "`".repeat(3.max(backtick_run(&s.replacement) + 1));
        b.push_str(&format!(
            "\n{fence}suggestion\n{}\n{fence}\n",
            s.replacement.strip_suffix('\n').unwrap_or(&s.replacement)
        ));
    }
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
    /// Candidates reported without validation (evaluation-only runs).
    pub unvalidated: usize,
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
    let counts = crate::findings::accepted_counts(inp.findings);
    let uncertain = inp
        .findings
        .iter()
        .filter(|f| f.validation_status == Some(ValidationStatus::Uncertain))
        .count();
    // headline: status first, so a partial run with zero findings can never
    // read like a clean pass
    let status_word = format!("{status:?}").to_lowercase();
    let mut count = if counts.total == 0 {
        "no accepted findings".to_string()
    } else {
        let mut parts = vec![];
        if counts.high > 0 {
            parts.push(format!("{} high", counts.high));
        }
        if counts.medium > 0 {
            parts.push(format!("{} medium", counts.medium));
        }
        if counts.low > 0 {
            parts.push(format!("{} low", counts.low));
        }
        format!(
            "{} ({})",
            plural(counts.total as usize, "accepted finding"),
            parts.join(", ")
        )
    };
    if inp.reused {
        count.push_str(&format!(
            "; reused completed review of identical content; {} carried forward",
            plural(inp.carried_open, "open finding")
        ));
    } else {
        if uncertain > 0 {
            count.push_str(&format!(", {} unconfirmed", uncertain));
        }
        if inp.carried_open > 0 {
            count.push_str(&format!(
                ", {} still open from earlier runs",
                inp.carried_open
            ));
        }
    }
    s.push_str(&format!("**Status: {status_word}** \u{2014} {count}\n"));
    if status != RunStatus::Complete {
        s.push_str(&format!(
            "\n> Review incomplete{}. Absence of findings is not evidence the change is clean; rerun or inspect the parts listed as not checked.\n",
            inp.reason
                .filter(|r| !r.is_empty())
                .map(|r| format!(": {}", public_line(r)))
                .unwrap_or_default()
        ));
    }
    if inp.unvalidated > 0 {
        s.push_str(&format!(
            "\n> Validation disabled (evaluation-only): {} not validated and not publishable.\n",
            plural(inp.unvalidated, "candidate")
        ));
    }
    if let Some(r) = inp.retrieval_unavailable {
        s.push_str(&format!(
            "\n> Semantic retrieval unavailable ({r}); repository-wide lookups used lexical search only.\n"
        ));
    }
    s.push('\n');
    // findings that cannot anchor inline are listed once, in their own section
    let outside_ids: std::collections::HashSet<String> =
        inp.outside_diff.iter().map(|f| f.id()).collect();
    for f in accepted.iter().filter(|f| !outside_ids.contains(&f.id())) {
        s.push_str(&format!(
            "- **[{}]** {}:{} — {}\n",
            f.severity,
            public_code_span(&f.file),
            f.start_line,
            public_line(&f.title)
        ));
    }
    if inp.publish_uncertain {
        for f in inp
            .findings
            .iter()
            .filter(|f| f.validation_status == Some(ValidationStatus::Uncertain))
        {
            s.push_str(&format!(
                "- _(unconfirmed)_ **[{}]** {}:{} — {}\n",
                f.severity,
                public_code_span(&f.file),
                f.start_line,
                public_line(&f.title)
            ));
        }
    }
    if !inp.outside_diff.is_empty() {
        s.push_str("\n### Findings outside the diff\n\n");
        for f in inp.outside_diff {
            s.push_str(&format!(
                "- **[{}]** {}:{} — {}\n",
                f.severity,
                public_code_span(&f.file),
                f.start_line,
                public_line(&f.title)
            ));
        }
    }
    if !inp.reopened.is_empty() {
        s.push_str("\n### Reopened (previously resolved, reintroduced)\n\n");
        for t in inp.reopened {
            s.push_str(&format!("- {}\n", public_line(t)));
        }
    }
    if !inp.resolved.is_empty() {
        s.push_str("\n### Resolved since last review\n\n");
        for t in inp.resolved {
            s.push_str(&format!("- {}\n", public_line(t)));
        }
    }
    if !inp.reused {
        s.push_str(&format!("\nChecked: {}\n", public_text(inp.coverage)));
        for g in inp.coverage_gaps {
            s.push_str(&format!("- not checked: {}\n", public_line(g)));
        }
    }
    if let Some(n) = inp.note {
        s.push_str(&format!("\n_{}_\n", public_text(n)));
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
    if routes.is_empty() {
        s.push_str(&format!(
            "\n<sub>revera · strategy {}</sub>\n",
            inp.strategy
        ));
    } else {
        s.push_str(&format!(
            "\n<sub>revera · strategy {} · models: {routes}</sub>\n",
            inp.strategy
        ));
    }
    s
}

fn dashboard_link(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(
        parsed
            .to_string()
            .replace(['<', '>'], "")
            .replace('(', "%28")
            .replace(')', "%29"),
    )
}

/// CLI decoration, after the pipeline has pinned the reviewed SHAs.
pub fn append_handoff(
    report: &mut RunReport,
    context: &crate::handoff::Context<'_>,
    fix_url: Option<&str>,
    bot_login: &str,
) {
    if report.status != RunStatus::Complete
        || (crate::findings::accepted_counts(&report.findings).total == 0
            && report.carried.is_empty())
    {
        return;
    }
    let instructions = crate::handoff::build(context, &report.actionable_findings(), None);
    let s = &mut report.plan.summary_markdown;
    s.push_str("\n<details><summary>Fix with your coding agent</summary>\n\n");
    if let Some(link) = context.review_url.and_then(dashboard_link) {
        s.push_str(&format!(
            "[Open this review and download instructions](<{link}>).\n\n"
        ));
    }
    if instructions.chars().count() <= 3000 {
        let instructions = instructions.replace("<!--", "<!\u{200b}--");
        let fence = "`".repeat(3.max(backtick_run(&instructions) + 1));
        s.push_str(&format!("{fence}text\n{instructions}\n{fence}\n\n"));
    }
    s.push_str("</details>\n");
    if let Some(link) = fix_url.and_then(dashboard_link) {
        s.push_str(&format!(
            "\n**Want Revera to fix these?** [Fix with Revera](<{link}>)"
        ));
        let slug = bot_login.strip_suffix("[bot]").unwrap_or(bot_login);
        if !slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            s.push_str(&format!(" · or comment `@{slug} fix`"));
        }
        s.push_str(".\n");
    }
}

pub fn mark_incomplete_summary(report: &mut RunReport) {
    let s = &mut report.plan.summary_markdown;
    if let Some(start) = s.find("\n<details><summary>Fix with your coding agent</summary>") {
        s.truncate(start);
    }
    *s = s.replace("**Status: complete**", "**Status: partial**");
    s.push_str("\n> Review incomplete: publication did not finish. Absence of findings is not evidence the change is clean.\n");
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

/// `<role>=<model>@<requested>` (+ `-><effective>` when the sent effort
/// differs from the requested one, e.g. after a 400 step-down). Public
/// output, so it never carries the protocol or endpoint.
pub fn footer_route_label(e: &crate::provider::LedgerEntry) -> String {
    if e.effective_reasoning != e.requested_reasoning {
        format!(
            "{}={}@{}->{}",
            e.role, e.model, e.requested_reasoning, e.effective_reasoning
        )
    } else {
        format!("{}={}@{}", e.role, e.model, e.requested_reasoning)
    }
}

/// Only successful requests identify the model that actually served a role.
/// Preserve the initial model in a switch chain, but never expose endpoints.
pub fn footer_routes(ledger: &RunLedger) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut roles: BTreeMap<&str, Vec<&crate::provider::LedgerEntry>> = BTreeMap::new();
    for entry in ledger.entries.iter().filter(|e| e.error.is_none()) {
        roles.entry(&entry.role).or_default().push(entry);
    }
    roles
        .into_iter()
        .map(|(role, entries)| {
            let last = entries.last().unwrap();
            let label = if let Some(first) = ledger.fallback_events.iter().find(|e| e.role == role)
            {
                let mut models = vec![first.from_model.as_str()];
                for entry in &entries {
                    if models.last().copied() != Some(entry.model.as_str()) {
                        models.push(&entry.model);
                    }
                }
                let mut display = (*last).clone();
                display.model = models.join("→");
                footer_route_label(&display)
            } else {
                footer_route_label(last)
            };
            crate::redact::text(&label).into_owned()
        })
        .collect()
}

pub fn ledger_report(ledger: &RunLedger, wall_ms: u64) -> LedgerReport {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<(String, String, String, String, String), RouteLedger> = BTreeMap::new();
    let (mut pr, mut cr, mut cpr, mut cwr, mut rr) = (0u64, 0u64, 0u64, 0u64, 0u64);
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
            cached_prompt_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        });
        r.requests += 1;
        r.prompt_tokens += e.prompt_tokens;
        r.completion_tokens += e.completion_tokens;
        r.cached_prompt_tokens += e.cached_prompt_tokens;
        r.cache_write_tokens += e.cache_write_tokens;
        r.reasoning_tokens += e.reasoning_tokens;
        pr += e.prompt_tokens;
        cr += e.completion_tokens;
        cpr += e.cached_prompt_tokens;
        cwr += e.cache_write_tokens;
        rr += e.reasoning_tokens;
    }
    LedgerReport {
        requests: ledger.entries.len() as u64,
        prompt_tokens: pr,
        completion_tokens: cr,
        cached_prompt_tokens: cpr,
        cache_write_tokens: cwr,
        reasoning_tokens: rr,
        by_route: by.into_values().collect(),
        fallback_events: ledger.fallback_events.clone(),
        unavailable_routes: ledger.unavailable_routes.clone(),
        wall_ms,
    }
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
        let footer_labels: Vec<String> = l.entries.iter().map(footer_route_label).collect();
        assert_eq!(
            footer_labels,
            vec!["investigator=m@max", "validator=m@high",]
        );
    }

    #[test]
    fn ledger_sums_cached_prompt_and_write_tokens() {
        let mut l = RunLedger::default();
        let mut e = entry("investigator", "max", "max");
        e.prompt_tokens = 20;
        e.cached_prompt_tokens = 7;
        e.cache_write_tokens = 3;
        l.entries.push(e);
        let rep = ledger_report(&l, 0);
        assert_eq!(rep.prompt_tokens, 20);
        assert_eq!(rep.cached_prompt_tokens, 7);
        assert_eq!(rep.cache_write_tokens, 3);
        assert_eq!(rep.by_route[0].cached_prompt_tokens, 7);
        assert_eq!(rep.by_route[0].cache_write_tokens, 3);
    }

    #[test]
    fn old_ledger_reports_default_cache_token_fields_to_zero() {
        let rep: LedgerReport = serde_json::from_str(
            r#"{"requests":0,"prompt_tokens":0,"completion_tokens":0,"reasoning_tokens":0,"by_route":[],"wall_ms":0}"#,
        )
        .unwrap();
        assert_eq!(rep.cached_prompt_tokens, 0);
        assert_eq!(rep.cache_write_tokens, 0);

        let route: RouteLedger = serde_json::from_str(
            r#"{"route":"r","role":"worker","model":"m","requested_reasoning":"none","effective_reasoning":"none","requests":1,"prompt_tokens":0,"completion_tokens":0,"reasoning_tokens":0}"#,
        )
        .unwrap();
        assert_eq!(route.cached_prompt_tokens, 0);
        assert_eq!(route.cache_write_tokens, 0);
    }

    #[test]
    fn step_down_renders_arrow() {
        let e = entry("investigator", "max", "high");
        assert_eq!(footer_route_label(&e), "investigator=m@max->high");
        let mut l = RunLedger::default();
        l.entries.push(e);
        let rep = ledger_report(&l, 0);
        assert_eq!(rep.by_route[0].requested_reasoning, "max");
        assert_eq!(rep.by_route[0].effective_reasoning, "high");
    }

    #[test]
    fn summary_omits_empty_models_segment() {
        let summary = summary_markdown(&Summary {
            strategy: "baseline",
            ..Default::default()
        });
        assert!(summary.contains("<sub>revera · strategy baseline</sub>"));
        assert!(!summary.contains(" · models:"));
    }

    #[test]
    fn summary_footer_uses_endpoint_free_route_labels() {
        let e = entry("investigator", "max", "high");
        let route = footer_route_label(&e);
        let summary = summary_markdown(&Summary {
            strategy: "baseline",
            routes: &[route],
            ..Default::default()
        });
        assert!(summary.contains("investigator=m@max->high"));
        assert!(!summary.contains("https://"));
        assert!(!summary.contains("openai-chat:"));
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
}
