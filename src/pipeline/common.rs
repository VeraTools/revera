use super::validate::{recheck_prompt, validate_candidates, validator_prompt, validator_terminal};
use crate::config::{Config, Strategy};
use crate::diff::{parse_unified, DiffSet};
use crate::findings::{collapse, Finding};
use crate::git;
use crate::pipeline::anchor::{anchor, is_publishable, Placement};
use crate::provider::LedgerHandle;
use crate::report::{
    finding_body, ledger_report, summary_markdown, InlineComment, PublicationPlan, RunReport,
    RunStatus,
};
use crate::state::{recheck_transition, FindingState, ReviewState};
use crate::tools::ToolBox;
use crate::vera::VeraClient;
use anyhow::{bail, Result};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub struct ReviewRequest {
    pub repo: PathBuf,
    pub base: String,
    pub head: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub strategy_override: Option<Strategy>,
    pub force: bool,
}

/// Shared pre-review state: resolved refs, diff, index, prior-state rechecks.
pub struct Prepared {
    pub repo: PathBuf,
    pub base_sha: String,
    pub head_sha: String,
    pub strategy_name: String,
    pub diff: Arc<DiffSet>,
    pub patch_id: String,
    pub vera: Arc<VeraClient>,
    pub toolbox: Arc<ToolBox>,
    pub ledger: LedgerHandle,
    pub state: ReviewState,
    /// Prior open/uncertain findings after recheck verdicts were applied.
    pub rechecks: Vec<Finding>,
    pub partial_reasons: Vec<String>,
    pub coverage: String,
    pub coverage_gaps: Vec<String>,
    /// Strategy-specific line appended to the summary (e.g. panel stats).
    pub report_note: Option<String>,
    /// wall-clock deadline for the whole run (start + run_max_seconds).
    pub deadline: Instant,
    pub wall: Instant,
}

impl Prepared {
    /// AgentBudget capped by both the per-agent limit and the run deadline.
    pub fn budget(&self, max_tool_calls: u32, max_seconds: u64) -> crate::agent::AgentBudget {
        let left = self.deadline.saturating_duration_since(Instant::now());
        crate::agent::AgentBudget {
            max_tool_calls,
            max_seconds: max_seconds.min(left.as_secs()),
        }
    }
}

pub enum PrepareOut {
    Ready(Box<Prepared>),
    ShortCircuit(Box<RunReport>, ReviewState),
}

/// Tolerate model-side field-name drift when parsing finding objects.
fn normalize_finding_json(v: &serde_json::Value) -> serde_json::Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        if !o.contains_key("file") {
            if let Some(p) = o
                .remove("path")
                .and_then(|p| p.as_str().map(str::to_string))
            {
                match p.rsplit_once(':') {
                    Some((f, l)) if l.parse::<u32>().is_ok() => {
                        o.insert("start_line".into(), json!(l.parse::<u32>().unwrap()));
                        o.insert("file".into(), json!(f));
                    }
                    _ => {
                        o.insert("file".into(), json!(p));
                    }
                }
            }
        }
        if !o.contains_key("claim") {
            if let Some(d) = o.remove("description") {
                o.insert("claim".into(), d);
            }
        }
    }
    v
}

pub fn parse_findings(args: &serde_json::Value) -> Vec<Finding> {
    args["findings"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    match serde_json::from_value::<Finding>(normalize_finding_json(v)) {
                        Ok(f) => Some(f),
                        Err(e) => {
                            tracing::warn!("dropping malformed finding: {e}");
                            None
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a worker's `candidate_findings` array (same schema).
pub fn parse_candidate_findings(v: &serde_json::Value) -> Vec<Finding> {
    parse_findings(&json!({"findings": v}))
}

/// Resolve refs, build the diff, load prior state, handle the unchanged
/// short-circuit, ensure the Vera index, and recheck prior open findings.
pub async fn prepare(cfg: &Config, req: &ReviewRequest, strategy_name: &str) -> Result<PrepareOut> {
    let wall = Instant::now();
    let repo = git::repo_root(&req.repo);
    if !git::is_repo(&repo).await {
        bail!("{} is not a git repository", repo.display());
    }
    let base_sha = git::rev_parse(&repo, &req.base).await?;
    let head_rev = req.head.as_deref().unwrap_or("HEAD");
    let head_sha = git::rev_parse(&repo, head_rev).await?;

    let raw_diff = git::diff(&repo, &req.base, head_rev).await?;
    let diff: Arc<DiffSet> = Arc::new(parse_unified(&raw_diff));
    let patch_id = git::patch_id(&repo, &req.base, head_rev)
        .await
        .unwrap_or_default();
    let ledger = LedgerHandle::new();

    let mut state = ReviewState::load(&repo)?.unwrap_or_default();
    let had_prior = !state.findings.is_empty() || !state.patch_id.is_empty();

    // short-circuit: identical patch content already reviewed — but not while
    // any accepted finding is still unposted (it must publish on this run)
    let unposted = state
        .findings
        .iter()
        .any(|f| f.status == FindingState::Open && !f.posted);
    if !req.force && had_prior && !unposted && state.is_unchanged(&base_sha, &patch_id) {
        state.reviewed_head = head_sha.clone();
        state.save(&repo)?;
        let plan_state = state.clone();
        let summary = summary_markdown(
            &[],
            &[],
            "unchanged since last review (identical patch-id)",
            &[],
            RunStatus::Complete,
            strategy_name,
            &[],
            cfg.review.publish_uncertain,
            None,
        );
        let rep = RunReport {
            status: RunStatus::Complete,
            reason: Some("unchanged since last review".into()),
            base: base_sha,
            head: head_sha,
            strategy: strategy_name.into(),
            findings: vec![],
            plan: PublicationPlan {
                inline: vec![],
                summary_markdown: summary,
                state: plan_state,
            },
            ledger: ledger_report(&ledger.0.lock().unwrap(), wall.elapsed().as_millis() as u64),
            publication: Default::default(),
            coverage_gaps: vec![],
        };
        return Ok(PrepareOut::ShortCircuit(Box::new(rep), state));
    }

    let mut partial_reasons: Vec<String> = Vec::new();
    let vera = Arc::new(VeraClient::from_config(&cfg.vera, &repo)?);
    let vera_err = match vera.ensure_index().await {
        Ok(_) => None,
        Err(e) => {
            let r = format!("retrieval unavailable: {e}");
            tracing::warn!("vera index failed (continuing without retrieval): {e}");
            partial_reasons.push(r.clone());
            Some(r)
        }
    };
    let toolbox = Arc::new(ToolBox::new(
        repo.clone(),
        diff.clone(),
        vera.clone(),
        cfg.review.max_tool_output_bytes,
    ));
    if let Some(r) = vera_err {
        toolbox.disable_vera(r);
    }

    let deadline = Instant::now() + std::time::Duration::from_secs(cfg.budget.run_max_seconds);
    // ---- recheck prior open findings ----
    let mut rechecks = recheck_candidates(&state);
    if !rechecks.is_empty() {
        let clean = validate_candidates(
            cfg,
            ledger.clone(),
            &toolbox,
            &diff,
            &mut rechecks,
            recheck_prompt(),
            &validator_terminal(),
            "validator",
            true,
            deadline,
        )
        .await;
        if let Some(r) = clean {
            partial_reasons.push(r.replace("validations", "rechecks"));
        }
        for rc in &rechecks {
            if let Some(vs) = rc.validation_status {
                let st = recheck_transition(vs);
                state.mark(&rc.id(), st);
            }
        }
    }

    Ok(PrepareOut::Ready(Box::new(Prepared {
        repo,
        base_sha,
        head_sha,
        strategy_name: strategy_name.into(),
        diff,
        patch_id,
        vera,
        toolbox,
        ledger,
        state,
        rechecks,
        partial_reasons,
        coverage: String::new(),
        coverage_gaps: vec![],
        report_note: None,
        deadline: Instant::now() + std::time::Duration::from_secs(cfg.budget.run_max_seconds),
        wall,
    })))
}

/// Prior open/uncertain findings -> recheck validator verdicts.
fn recheck_candidates(state: &ReviewState) -> Vec<Finding> {
    state
        .open_findings()
        .iter()
        .map(|f| Finding {
            defect_key: f.defect_key.clone(),
            severity: crate::findings::Severity::Medium,
            file: f.file.clone(),
            start_line: f.start_line,
            end_line: None,
            title: f.title.clone(),
            claim: String::new(),
            trigger: String::new(),
            impact: String::new(),
            introduced_by_change: true,
            supporting_evidence: vec![],
            counterevidence_checked: vec![],
            validation_status: None,
            suggested_fix: None,
            source: "prior".into(),
            rationale: None,
            sources: vec!["prior".into()],
        })
        .collect()
}

/// Shared tail: collapse -> min_severity -> dedupe vs prior -> validate ->
/// anchor -> state -> report. `lane_labels` are informational (unused today:
/// model attribution comes from the ledger).
pub async fn finish(
    cfg: &Config,
    mut prep: Box<Prepared>,
    candidates: Vec<Finding>,
    _lane_labels: &[String],
) -> Result<(RunReport, ReviewState)> {
    // ---- collapse + severity gate ----
    let mut collapsed = collapse(candidates);
    collapsed.retain(|f| f.severity >= cfg.review.min_severity);
    // accepted rechecks that were never posted re-enter the final set so
    // they anchor and publish on this run
    let reenter: Vec<Finding> = prep
        .rechecks
        .iter()
        .filter(|r| {
            r.validation_status == Some(crate::findings::ValidationStatus::Accepted)
                && !prep.state.has_posted(&r.id())
        })
        .cloned()
        .collect();
    let reenter_ids: std::collections::HashSet<String> = reenter.iter().map(|f| f.id()).collect();
    // dedupe against prior posted/open/uncertain ids
    collapsed.retain(|f| {
        let id = f.id();
        !(prep.state.has_posted(&id)
            || (!reenter_ids.contains(&id)
                && prep.state.findings.iter().any(|p| {
                    p.id == id && matches!(p.status, FindingState::Open | FindingState::Uncertain)
                })))
    });

    // ---- validate ----
    if !collapsed.is_empty() {
        let clean = validate_candidates(
            cfg,
            prep.ledger.clone(),
            &prep.toolbox,
            &prep.diff,
            &mut collapsed,
            validator_prompt(),
            &validator_terminal(),
            "validator",
            false,
            prep.deadline,
        )
        .await;
        if let Some(r) = clean {
            prep.partial_reasons.push(r);
        }
    }

    collapsed.extend(reenter);
    // ---- anchor + plan ----
    let anchored = anchor(&prep.diff, collapsed.clone(), cfg.review.max_findings);
    let mut inline: Vec<InlineComment> = vec![];
    let mut outside: Vec<&Finding> = vec![];
    let mut final_findings: Vec<Finding> = vec![];
    for a in &anchored {
        let f = &a.finding;
        if !is_publishable(f) {
            final_findings.push(f.clone());
            continue;
        }
        match a.placement {
            Placement::Inline => inline.push(InlineComment {
                file: f.file.clone(),
                line: f.start_line,
                end_line: f.end_line,
                body: finding_body(f),
            }),
            Placement::Summary => outside.push(f),
        }
        final_findings.push(f.clone());
    }

    // ---- state update ----
    let mut state = prep.state.clone();
    for f in &final_findings {
        // `posted` is only ever set by the publisher via mark_posted().
        let st = match f.validation_status {
            Some(crate::findings::ValidationStatus::Accepted) => FindingState::Open,
            Some(crate::findings::ValidationStatus::Rejected) => FindingState::Rejected,
            _ => FindingState::Uncertain,
        };
        state.upsert(f, st);
    }
    state.reviewed_base = prep.base_sha.clone();
    state.reviewed_head = prep.head_sha.clone();
    state.patch_id = prep.patch_id.clone();
    state.save(&prep.repo)?;

    let status = if prep.partial_reasons.is_empty() {
        RunStatus::Complete
    } else {
        RunStatus::Partial
    };
    let reason = if prep.partial_reasons.is_empty() {
        None
    } else {
        Some(prep.partial_reasons.join("; "))
    };
    let routes: Vec<String> = prep
        .ledger
        .0
        .lock()
        .unwrap()
        .entries
        .iter()
        .map(|e| format!("{}:{}", e.route, e.model))
        .collect();
    let coverage = if prep.coverage.is_empty() {
        "(none reported)".to_string()
    } else {
        prep.coverage.clone()
    };
    let summary = summary_markdown(
        &final_findings,
        &outside,
        &coverage,
        &prep.coverage_gaps,
        status,
        &prep.strategy_name,
        &routes,
        cfg.review.publish_uncertain,
        prep.report_note.as_deref(),
    );
    let rep = RunReport {
        status,
        reason,
        base: prep.base_sha.clone(),
        head: prep.head_sha.clone(),
        strategy: prep.strategy_name.clone(),
        findings: final_findings,
        plan: PublicationPlan {
            inline,
            summary_markdown: summary,
            state: state.clone(),
        },
        ledger: ledger_report(
            &prep.ledger.0.lock().unwrap(),
            prep.wall.elapsed().as_millis() as u64,
        ),
        publication: Default::default(),
        coverage_gaps: prep.coverage_gaps.clone(),
    };
    Ok((rep, state))
}

/// Render the standard investigator-style user message.
pub fn investigator_user(req: &ReviewRequest, diff: &DiffSet, max_diff_bytes: usize) -> String {
    let changed: Vec<String> = diff
        .files
        .iter()
        .map(|f| format!("{:?} {}", f.status, f.new_path))
        .collect();
    format!(
        "PR title: {}\n\nPR body:\n{}\n\nChanged files:\n{}\n\nDiff:\n{}",
        req.title.as_deref().unwrap_or("(untitled)"),
        req.body,
        changed.join("\n"),
        diff.render_truncated(max_diff_bytes),
    )
}
