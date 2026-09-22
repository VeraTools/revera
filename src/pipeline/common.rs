use super::validate::{recheck_prompt, validate_candidates, validator_prompt, validator_terminal};
use crate::config::{Config, Strategy};
use crate::diff::{parse_unified, DiffSet};
use crate::findings::{collapse, Finding};
use crate::git;
use crate::pipeline::anchor::{anchor, is_publishable, Placement};
use crate::provider::LedgerHandle;
use crate::report::{
    finding_body, ledger_report, summary_markdown, InlineComment, PublicationPlan, RunReport,
    RunStats, RunStatus, Summary,
};
use crate::state::{recheck_transition, review_key, FindingState, ReviewState};
use crate::timing::Recorder;
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
    /// Identity of this review (base, head tree, config); stored in state.
    pub review_key: String,
    pub vera: Arc<VeraClient>,
    pub toolbox: Arc<ToolBox>,
    pub ledger: LedgerHandle,
    pub state: ReviewState,
    /// Prior open/uncertain findings after recheck verdicts were applied.
    pub rechecks: Vec<Finding>,
    /// Titles of prior findings resolved by this run's rechecks.
    pub resolved_titles: Vec<String>,
    pub partial_reasons: Vec<String>,
    /// Why vera retrieval is unavailable (`None` when it works or is off).
    pub retrieval_unavailable: Option<String>,
    pub stats: RunStats,
    pub coverage: String,
    pub coverage_gaps: Vec<String>,
    /// Strategy-specific line appended to the summary (e.g. panel stats).
    pub report_note: Option<String>,
    /// wall-clock deadline for the whole run (start + run_max_seconds).
    pub deadline: Instant,
    /// Time held back from investigation so validation and the final
    /// report still fit before `deadline`.
    pub reserve: std::time::Duration,
    pub wall: Instant,
    pub timing: Recorder,
}

impl Prepared {
    /// AgentBudget for an investigation lane: capped by the per-agent
    /// limit and by the run deadline minus the validation reserve.
    pub fn budget(&self, max_tool_calls: u32, max_seconds: u64) -> crate::agent::AgentBudget {
        clamp_budget(
            max_tool_calls,
            max_seconds,
            self.deadline
                .checked_sub(self.reserve)
                .unwrap_or(self.deadline),
        )
    }
}

pub fn clamp_budget(
    max_tool_calls: u32,
    max_seconds: u64,
    deadline: Instant,
) -> crate::agent::AgentBudget {
    let left = deadline.saturating_duration_since(Instant::now());
    crate::agent::AgentBudget {
        max_tool_calls,
        max_seconds: max_seconds.min(left.as_secs()),
    }
}

/// Share of the run budget held back for validation and finalization when
/// validation is enabled: a quarter of the run, at most two minutes.
pub fn validation_reserve(run_max_seconds: u64, validate: bool) -> std::time::Duration {
    if !validate {
        return std::time::Duration::ZERO;
    }
    std::time::Duration::from_secs((run_max_seconds / 4).min(120))
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

/// Result of parsing a terminal `findings` submission. A valid empty list
/// is `Ok` with no findings; a missing/non-array `findings` or malformed
/// items make the submission incomplete (`problem` is set) while any valid
/// items are still kept.
#[derive(Debug, Default)]
pub struct ParsedFindings {
    pub findings: Vec<Finding>,
    pub dropped: usize,
    pub problem: Option<String>,
}

pub fn parse_findings_checked(args: &serde_json::Value) -> ParsedFindings {
    let Some(arr) = args["findings"].as_array() else {
        return ParsedFindings {
            problem: Some(if args.get("findings").is_none() {
                "submission has no `findings` array".into()
            } else {
                "`findings` is not an array".into()
            }),
            ..Default::default()
        };
    };
    let mut out = ParsedFindings::default();
    let mut errors: Vec<String> = vec![];
    for v in arr {
        match serde_json::from_value::<Finding>(normalize_finding_json(v)) {
            Ok(f) => out.findings.push(f),
            Err(e) => {
                tracing::warn!("dropping malformed finding: {e}");
                out.dropped += 1;
                if errors.len() < 3 {
                    errors.push(e.to_string());
                }
            }
        }
    }
    if out.dropped > 0 {
        out.problem = Some(format!(
            "{} of {} finding(s) malformed and dropped ({})",
            out.dropped,
            arr.len(),
            errors.join("; ")
        ));
    }
    out
}

/// Terminal check for `submit_findings`: rejects submissions that would
/// parse incompletely so the agent gets one repair round.
pub fn findings_terminal_check(args: &serde_json::Value) -> Result<(), String> {
    match parse_findings_checked(args).problem {
        Some(p) => Err(p),
        None => Ok(()),
    }
}

pub fn parse_findings(args: &serde_json::Value) -> Vec<Finding> {
    parse_findings_checked(args).findings
}

/// Parse a worker's `candidate_findings` array (same schema).
pub fn parse_candidate_findings(v: &serde_json::Value) -> Vec<Finding> {
    parse_findings(&json!({"findings": v}))
}

/// Resolve refs, build the diff, load prior state, handle the unchanged
/// short-circuit, ensure the Vera index, and recheck prior open findings.
pub async fn prepare(cfg: &Config, req: &ReviewRequest, strategy_name: &str) -> Result<PrepareOut> {
    let wall = Instant::now();
    let timing = Recorder::default();
    let deadline = wall + std::time::Duration::from_secs(cfg.budget.run_max_seconds);
    let repo = git::repo_root(&req.repo);
    if !git::is_repo(&repo).await {
        bail!("{} is not a git repository", repo.display());
    }
    let base_sha = git::rev_parse(&repo, &req.base).await?;
    let head_rev = req.head.as_deref().unwrap_or("HEAD");
    let head_sha = git::rev_parse(&repo, head_rev).await?;
    let current_head = git::current_head(&repo).await?;
    if head_sha != current_head {
        bail!(
            "head {head_sha} is not the checked-out tree (HEAD is {current_head}); reviewer tools read the working tree, so check out the PR head first (GitHub Actions: actions/checkout with ref: ${{{{ github.event.pull_request.head.sha }}}})"
        );
    }
    if git::tracked_dirty(&repo).await? {
        bail!(
            "working tree has uncommitted changes to tracked files; commit or stash them so the reviewed tree matches {head_sha} (HEAD is {current_head})"
        );
    }
    let raw_diff = git::diff(&repo, &req.base, head_rev).await?;
    let diff: Arc<DiffSet> = Arc::new(parse_unified(&raw_diff));
    let patch_id = git::patch_id(&repo, &req.base, head_rev)
        .await
        .unwrap_or_default();
    let head_tree = git::tree_id(&repo, head_rev).await?;
    let key = review_key(
        &base_sha,
        &head_tree,
        &patch_id,
        &cfg.review_fingerprint(strategy_name),
    );
    let ledger = LedgerHandle::new();

    let mut state = ReviewState::load(&repo)?.unwrap_or_default();

    // short-circuit: a *completed* review of exactly this content and
    // configuration — but not while any accepted finding is still unposted
    // (it must publish on this run). Partial runs, legacy state and any
    // identity drift fall through to a fresh review.
    let unposted = state
        .findings
        .iter()
        .any(|f| f.status == FindingState::Open && !f.posted);
    if !req.force && !unposted && state.can_reuse(&key) {
        state.reviewed_head = head_sha.clone();
        state.save(&repo)?;
        let plan_state = state.clone();
        let carried = state.open_findings().len();
        let reason = "reused completed review of identical content";
        let summary = summary_markdown(&Summary {
            coverage: "",
            status: Some(RunStatus::Complete),
            strategy: strategy_name,
            publish_uncertain: cfg.review.publish_uncertain,
            reused: true,
            carried_open: carried,
            ..Default::default()
        });
        let rep = RunReport {
            status: RunStatus::Complete,
            reason: Some(reason.into()),
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
            timing: timing.finish(wall),
            stats: RunStats {
                reused: true,
                retrieval: "not needed".into(),
                ..Default::default()
            },
        };
        return Ok(PrepareOut::ShortCircuit(Box::new(rep), state));
    }
    if state.version != 0 && state.version != crate::state::STATE_VERSION {
        tracing::warn!(
            "state version {} differs from {}; re-reviewing (publication ids retained)",
            state.version,
            crate::state::STATE_VERSION
        );
    }

    let mut partial_reasons: Vec<String> = Vec::new();
    let vera = Arc::new(VeraClient::from_config(&cfg.vera, &repo)?.with_deadline(deadline));
    let vera_err = if !cfg.vera.enabled {
        tracing::info!("vera disabled by config; no index, no retrieval tools");
        timing.record("vera_index", "", Instant::now(), wall, "skipped");
        None
    } else {
        let t0 = Instant::now();
        match vera.ensure_index().await {
            Ok(_) => {
                timing.record("vera_index", "", t0, wall, "ok");
                None
            }
            Err(e) => {
                timing.record(
                    "vera_index",
                    "",
                    t0,
                    wall,
                    &format!("error:{}", crate::text::excerpt_bytes(&e.to_string(), 60)),
                );
                tracing::warn!("vera index failed (continuing without retrieval): {e}");
                partial_reasons.push("semantic retrieval unavailable".into());
                Some(crate::text::excerpt_bytes(&e.to_string(), 200))
            }
        }
    };
    let mut tb = ToolBox::new(
        repo.clone(),
        diff.clone(),
        vera.clone(),
        cfg.review.max_tool_output_bytes,
    );
    if !cfg.vera.enabled {
        tb.hide_vera_tools();
    }
    let toolbox = Arc::new(tb);
    let retrieval_unavailable = vera_err.clone();
    if let Some(r) = vera_err {
        toolbox.disable_vera(r);
    }
    let retrieval = if !cfg.vera.enabled {
        "lexical-only".to_string()
    } else if let Some(r) = &retrieval_unavailable {
        format!("unavailable: {r}")
    } else {
        "vera".to_string()
    };

    // ---- recheck prior open findings ----
    let mut rechecks = recheck_candidates(&state);
    let mut resolved_titles = vec![];
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
            &timing,
            wall,
            "recheck",
        )
        .await;
        if let Some(r) = clean {
            partial_reasons.push(r.replace("validations", "rechecks"));
        }
        for rc in &rechecks {
            if let Some(vs) = rc.validation_status {
                let st = recheck_transition(vs);
                if st == FindingState::Resolved {
                    resolved_titles.push(rc.title.clone());
                }
                state.mark(&rc.id(), st);
            }
        }
    }

    let stats = RunStats {
        retrieval,
        resolved: resolved_titles.len(),
        ..Default::default()
    };
    Ok(PrepareOut::Ready(Box::new(Prepared {
        repo,
        base_sha,
        head_sha,
        strategy_name: strategy_name.into(),
        diff,
        patch_id,
        review_key: key,
        vera,
        toolbox,
        ledger,
        state,
        rechecks,
        resolved_titles,
        partial_reasons,
        retrieval_unavailable,
        stats,
        coverage: String::new(),
        coverage_gaps: vec![],
        report_note: None,
        deadline,
        reserve: validation_reserve(cfg.budget.run_max_seconds, cfg.review.validate),
        wall,
        timing,
    })))
}

/// Prior open/uncertain findings -> recheck validator verdicts.
fn recheck_candidates(state: &ReviewState) -> Vec<Finding> {
    state
        .open_findings()
        .iter()
        .map(|f| f.to_finding())
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
    collapsed = crate::findings::rank_candidates(collapsed);
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
    // dedupe against findings still tracked as open/uncertain (posted or
    // awaiting recheck). Resolved/rejected ids are *not* filtered: the same
    // defect coming back is a reintroduction and must be validated again.
    collapsed.retain(|f| {
        let id = f.id();
        !(prep.state.is_tracked_open(&id) && !reenter_ids.contains(&id))
    });
    prep.stats.candidates = collapsed.len();

    // ---- validate ----
    if !cfg.review.validate {
        // eval-only knob: candidates treated as accepted, no validation pass
        tracing::warn!("review.validate=false: skipping validation (eval-only)");
        for c in &mut collapsed {
            c.validation_status = Some(crate::findings::ValidationStatus::Accepted);
        }
    } else if !collapsed.is_empty() {
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
            &prep.timing,
            prep.wall,
            "validate",
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
    let mut reopened_titles: Vec<String> = vec![];
    for f in &final_findings {
        // `posted` is only ever set by the publisher via mark_posted().
        let st = match f.validation_status {
            Some(crate::findings::ValidationStatus::Accepted) => FindingState::Open,
            Some(crate::findings::ValidationStatus::Rejected) => FindingState::Rejected,
            _ => FindingState::Uncertain,
        };
        if state.upsert(f, st) && st == FindingState::Open {
            reopened_titles.push(format!("`{}`:{} — {}", f.file, f.start_line, f.title));
        }
    }
    let status = if prep.partial_reasons.is_empty() {
        RunStatus::Complete
    } else {
        RunStatus::Partial
    };
    state.record_outcome(
        &prep.base_sha,
        &prep.head_sha,
        &prep.patch_id,
        &prep.review_key,
        status,
    );
    state.save(&prep.repo)?;

    let reason = if prep.partial_reasons.is_empty() {
        None
    } else {
        Some(prep.partial_reasons.join("; "))
    };
    let timing = prep.timing.finish(prep.wall);
    let final_ids: std::collections::HashSet<String> =
        final_findings.iter().map(|f| f.id()).collect();
    let carried_open = state
        .findings
        .iter()
        .filter(|f| f.status == FindingState::Open && f.posted && !final_ids.contains(&f.id))
        .count();
    let mut stats = prep.stats.clone();
    stats.reopened = reopened_titles.len();
    stats.accepted = final_findings
        .iter()
        .filter(|f| f.validation_status == Some(crate::findings::ValidationStatus::Accepted))
        .count();
    stats.rejected = final_findings
        .iter()
        .filter(|f| f.validation_status == Some(crate::findings::ValidationStatus::Rejected))
        .count();
    stats.uncertain = final_findings.len() - stats.accepted - stats.rejected;
    stats.files_read = prep.toolbox.files_read();
    stats.tools = prep.toolbox.tool_stats();
    let routes: Vec<String> = prep
        .ledger
        .0
        .lock()
        .unwrap()
        .entries
        .iter()
        .map(crate::report::ledger_route_label)
        .collect();
    let coverage = if prep.coverage.is_empty() {
        "(none reported)".to_string()
    } else {
        prep.coverage.clone()
    };
    let summary = summary_markdown(&Summary {
        findings: &final_findings,
        outside_diff: &outside,
        coverage: &coverage,
        coverage_gaps: &prep.coverage_gaps,
        status: Some(status),
        reason: reason.as_deref(),
        strategy: &prep.strategy_name,
        routes: &routes,
        publish_uncertain: cfg.review.publish_uncertain,
        note: prep.report_note.as_deref(),
        timing: Some(&timing),
        reused: false,
        carried_open,
        retrieval_unavailable: prep.retrieval_unavailable.as_deref(),
        resolved: &prep.resolved_titles,
        reopened: &reopened_titles,
    });
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
        timing,
        stats,
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

#[cfg(test)]
mod tests {
    use super::clamp_budget;
    use std::time::{Duration, Instant};

    #[test]
    fn clamp_budget_uses_remaining_deadline() {
        let budget = clamp_budget(7, 100, Instant::now() + Duration::from_secs(5));
        assert!(budget.max_seconds <= 5);

        let expired = clamp_budget(7, 100, Instant::now() - Duration::from_secs(1));
        assert_eq!(expired.max_seconds, 0);
    }
}
