use super::validate::{recheck_prompt, validate_candidates, validator_prompt, validator_terminal};
use crate::config::{Config, Strategy};
use crate::diff::{DiffSet, parse_unified};
use crate::findings::{Finding, collapse};
use crate::git;
use crate::pipeline::anchor::{Placement, anchor, is_publishable};
use crate::provider::LedgerHandle;
use crate::report::{
    InlineComment, PublicationPlan, RunReport, RunStats, RunStatus, Summary, finding_body,
    ledger_report, summary_markdown,
};
use crate::state::{FindingState, ReviewState, recheck_transition, review_key};
use crate::timing::Recorder;
use crate::tools::ToolBox;
use crate::vera::VeraClient;
use anyhow::{Result, bail};
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
    /// Only trusted event metadata can establish public visibility. CLI is unknown.
    pub repository_public: bool,
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
    pub guidance: crate::guidance::Guidance,
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

    /// Findings still open after this run's rechecks. Forced re-reviews of
    /// the same head (mentions) otherwise rediscover them under a new
    /// defect_key, which is a new id and a second comment for one defect.
    pub fn tracked_block(&self, validate: bool) -> String {
        tracked_block(&self.state, validate)
    }
}

const MAX_TRACKED_LISTED: usize = 30;

fn tracked_block(state: &ReviewState, validate: bool) -> String {
    let open = state.open_findings();
    if !validate || open.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "\n\nAlready reported on this pull request and rechecked separately; do not submit these again, even reworded:\n",
    );
    for f in open.iter().take(MAX_TRACKED_LISTED) {
        let lines = match f.end_line {
            Some(end) if end > f.start_line => format!("{}-{end}", f.start_line),
            _ => f.start_line.to_string(),
        };
        let title: String = f.title.chars().take(160).collect();
        block.push_str(&format!(
            "- {}:{lines} `{}` {}\n",
            f.file,
            f.defect_key,
            crate::redact::text(&title)
        ));
    }
    block
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
        if !o.contains_key("file")
            && let Some(p) = o
                .remove("path")
                .and_then(|p| p.as_str().map(str::to_string))
        {
            match p
                .rsplit_once(':')
                .and_then(|(f, l)| Some((f, l.parse::<u32>().ok()?)))
            {
                Some((f, line)) => {
                    o.insert("start_line".into(), json!(line));
                    o.insert("file".into(), json!(f));
                }
                None => {
                    o.insert("file".into(), json!(p));
                }
            }
        }
        if !o.contains_key("claim")
            && let Some(d) = o.remove("description")
        {
            o.insert("claim".into(), d);
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

/// Scope the submitting agent explicitly reports as not checked.
pub fn parse_not_checked(args: &serde_json::Value) -> Vec<String> {
    args["not_checked"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| crate::text::excerpt_bytes(s.trim(), 300))
                .filter(|s| !s.is_empty())
                .take(20)
                .collect()
        })
        .unwrap_or_default()
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
            Ok(mut f) => {
                // verdict fields are owned by the validator, never the finder
                f.validation_status = None;
                f.rationale = None;
                f.counterevidence_checked.clear();
                f.validated_fix = None;
                f.suggestion = None;
                out.findings.push(f)
            }
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
    let mut parsed = parse_unified(&raw_diff);
    // credential-bearing files never reach a model, even as diff text
    let sensitive = crate::tools::glob_set(crate::config::SENSITIVE_GLOBS);
    let mut policy_gaps = vec![];
    parsed.files.retain(|f| {
        let hit = sensitive.is_match(&f.new_path) || sensitive.is_match(&f.old_path);
        if hit {
            policy_gaps.push(format!("`{}` excluded by the content policy", f.new_path));
        }
        !hit
    });
    let diff: Arc<DiffSet> = Arc::new(parsed);
    let patch_id = git::patch_id(&repo, &req.base, head_rev)
        .await
        .unwrap_or_default();
    let head_tree = git::tree_id(&repo, head_rev).await?;
    let key = review_key(
        &base_sha,
        &head_tree,
        &patch_id,
        &json!({"config": cfg.review_fingerprint(strategy_name), "repository_public": req.repository_public}),
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
        let plan_state = state.public_projection(cfg.review.publish_uncertain);
        let carried = crate::report::carried_findings(&state, &[]);
        let reason = "reused completed review of identical content";
        let summary = summary_markdown(&Summary {
            coverage: "",
            status: Some(RunStatus::Complete),
            strategy: strategy_name,
            publish_uncertain: cfg.review.publish_uncertain,
            reused: true,
            carried_open: carried.len(),
            ..Default::default()
        });
        let rep = RunReport {
            status: RunStatus::Complete,
            reason: Some(reason.into()),
            base: base_sha,
            head: head_sha,
            strategy: strategy_name.into(),
            findings: vec![],
            carried,
            plan: PublicationPlan {
                inline: vec![],
                summary_markdown: summary,
                state: plan_state,
                publish_uncertain: cfg.review.publish_uncertain,
            },
            ledger: ledger_report(&ledger.0.lock().unwrap(), wall.elapsed().as_millis() as u64),
            publication: Default::default(),
            coverage_gaps: vec![],
            timing: timing.finish(wall),
            stats: RunStats {
                reused: true,
                validation: "reused".into(),
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
    cfg.check_trust(false)?;
    let web = crate::web::WebResearch::from_config(
        &cfg.web_search,
        req.repository_public,
        cfg.review.validate,
        deadline,
    )?;
    let vera = Arc::new(VeraClient::from_config(&cfg.vera, &repo)?.with_deadline(deadline));
    let mut rerank = crate::vera::RerankState::Off;
    let vera_err = if !cfg.vera.enabled {
        tracing::info!("vera disabled by config; no index, no retrieval tools");
        timing.record("vera_index", "", Instant::now(), wall, "skipped");
        None
    } else {
        let t0 = Instant::now();
        let configured = match vera.configure().await {
            Ok(r) => Ok(r),
            Err(e) => {
                // the persistent Vera home may still carry another run's
                // reranker settings: do not search through it
                tracing::warn!("vera configuration failed: {e:#}");
                Err(e.context("vera configuration failed"))
            }
        };
        if let Ok(r) = &configured {
            rerank = *r;
            if *r == crate::vera::RerankState::Degraded {
                partial_reasons.push("vera reranker configured but not activated".into());
            }
        }
        let indexed = match configured {
            Ok(_) => vera.ensure_index().await,
            Err(e) => Err(e),
        };
        match indexed {
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
    )
    .with_head(&head_sha)
    .with_web(web);
    // a failed index leaves no usable vera_* tools; hiding them keeps the
    // tool note honest about lexical-only coverage
    if !cfg.vera.enabled || vera_err.is_some() {
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
        match rerank {
            crate::vera::RerankState::Enabled => "vera+rerank".to_string(),
            crate::vera::RerankState::Degraded => "vera (rerank degraded)".to_string(),
            crate::vera::RerankState::Off => "vera".to_string(),
        }
    };

    // ---- recheck prior open findings ----
    // validation-disabled runs are evaluation-only: no validator sessions,
    // and no prior finding may be re-accepted into this run's output
    let mut rechecks = if cfg.review.validate {
        recheck_candidates(&state)
    } else {
        vec![]
    };
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
            strategy_name,
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

    // the investigator never sees diffs omitted by max_diff_bytes: that is
    // unchecked scope, not a clean result
    let mut coverage_gaps = policy_gaps;
    let omitted = diff.omitted_files(cfg.review.max_diff_bytes);
    if !omitted.is_empty() {
        partial_reasons.push(format!(
            "{} omitted from the reviewed diff (max_diff_bytes={})",
            if omitted.len() == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", omitted.len())
            },
            cfg.review.max_diff_bytes
        ));
        for f in &omitted {
            coverage_gaps.push(format!("`{f}` diff omitted (exceeds max_diff_bytes)"));
        }
    }
    let changed: Vec<String> = diff.files.iter().map(|f| f.new_path.clone()).collect();
    let guidance = crate::guidance::load(
        &repo,
        &base_sha,
        &changed,
        cfg.review.guidance,
        cfg.review.guidance_max_bytes,
    )
    .await?;
    let stats = RunStats {
        guidance: (cfg.review.guidance != crate::config::GuidanceMode::Off)
            .then(|| guidance.clone()),
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
        guidance,
        coverage: String::new(),
        coverage_gaps,
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
    // Validation-disabled runs skip this so every candidate is scored.
    collapsed.retain(|f| {
        let id = f.id();
        !(cfg.review.validate && prep.state.is_tracked_open(&id) && !reenter_ids.contains(&id))
    });
    prep.stats.candidates = collapsed.len();

    // ---- validate ----
    // Publishability is Rust-owned: only a fresh validator verdict from this
    // run (or a recheck verdict below) can mark a candidate accepted.
    let validation_disabled = !cfg.review.validate;
    if validation_disabled {
        tracing::warn!(
            "review.validate=false: candidates reported unvalidated (evaluation-only, never published or stored)"
        );
        for c in &mut collapsed {
            c.validation_status = None;
            c.rationale = Some("not validated (review.validate=false, evaluation-only)".into());
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
            &prep.strategy_name,
            "validate",
        )
        .await;
        if let Some(r) = clean {
            prep.partial_reasons.push(r);
        }
    }

    collapsed.extend(reenter);
    // the validator may lower severity: the threshold applies to the
    // corrected finding, not the investigator's claim
    let before = collapsed.len();
    // Posted rechecks bypass `reenter`, but must obey the same threshold.
    let mut below_threshold_prior: std::collections::BTreeMap<_, _> = prep
        .rechecks
        .iter()
        .filter(|f| {
            f.validation_status == Some(crate::findings::ValidationStatus::Accepted)
                && f.severity < cfg.review.min_severity
        })
        .map(|f| (f.id(), f.clone()))
        .collect();
    // Unposted ones re-entered `collapsed` and are counted by the retain below.
    let rechecks_below = below_threshold_prior
        .keys()
        .filter(|id| !reenter_ids.contains(*id))
        .count();
    // Closed (resolved/rejected) findings stay closed; only still-open ones
    // are moved out of the carried set.
    let still_open = |id: &str| {
        prep.state
            .find(id)
            .is_some_and(|f| matches!(f.status, FindingState::Open | FindingState::Uncertain))
    };
    collapsed.retain(|f| {
        let keep = f.severity >= cfg.review.min_severity;
        if !keep && still_open(&f.id()) {
            below_threshold_prior.insert(f.id(), f.clone());
        }
        keep
    });
    prep.stats.below_threshold_after_validation = before - collapsed.len() + rechecks_below;

    // ---- anchor + plan ----
    // only publishable findings compete for inline slots; everything else
    // stays in the report (and state) without a placement
    let (publishable, unpublished): (Vec<Finding>, Vec<Finding>) =
        collapsed.into_iter().partition(is_publishable);
    let anchored = anchor(&prep.diff, publishable, cfg.review.max_findings);
    let mut inline: Vec<InlineComment> = vec![];
    let mut outside: Vec<&Finding> = vec![];
    let mut final_findings: Vec<Finding> = vec![];
    for a in &anchored {
        let f = &a.finding;
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
    final_findings.extend(unpublished);

    // ---- state update ----
    let mut state = prep.state.clone();
    for (id, f) in below_threshold_prior
        .into_iter()
        .filter(|_| !validation_disabled)
    {
        if state.find(&id).is_some() {
            // Preserve identity and posting history for later rechecks, but
            // exclude this finding from carried repair context now and later.
            state.upsert(&f, FindingState::Uncertain);
        }
    }
    let mut reopened_titles: Vec<String> = vec![];
    for f in final_findings.iter().filter(|_| !validation_disabled) {
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
    if !validation_disabled {
        state.record_outcome(
            &prep.base_sha,
            &prep.head_sha,
            &prep.patch_id,
            &prep.review_key,
            status,
        );
        state.save(&prep.repo)?;
    }

    let reason = if prep.partial_reasons.is_empty() {
        None
    } else {
        Some(prep.partial_reasons.join("; "))
    };
    let timing = prep.timing.finish(prep.wall);
    let carried = crate::report::carried_findings(&state, &final_findings);
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
    stats.uncertain = final_findings
        .iter()
        .filter(|f| f.validation_status == Some(crate::findings::ValidationStatus::Uncertain))
        .count();
    stats.unvalidated = final_findings
        .iter()
        .filter(|f| f.validation_status.is_none())
        .count();
    stats.validation = if validation_disabled {
        "disabled".into()
    } else {
        "fresh".into()
    };
    stats.files_read = prep.toolbox.files_read();
    stats.tools = prep.toolbox.tool_stats();
    let load = |n: &std::sync::atomic::AtomicUsize| n.load(std::sync::atomic::Ordering::Relaxed);
    let mut fallbacks = vec![];
    match load(&prep.vera.semantic_fallbacks) {
        0 => {}
        n => fallbacks.push(format!("keyword-only on {n} searches")),
    }
    match load(&prep.vera.rerank_fallbacks) {
        0 => {}
        n => fallbacks.push(format!("rerank fell back on {n} searches")),
    }
    if !fallbacks.is_empty() {
        stats.retrieval = format!("{} ({})", stats.retrieval, fallbacks.join("; "));
    }
    let routes = crate::report::footer_routes(&prep.ledger.0.lock().unwrap());
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
        carried_open: carried.len(),
        retrieval_unavailable: prep.retrieval_unavailable.as_deref(),
        resolved: &prep.resolved_titles,
        reopened: &reopened_titles,
        unvalidated: stats.unvalidated,
    });
    let public_state = state.public_projection(cfg.review.publish_uncertain);
    let rep = RunReport {
        status,
        reason,
        base: prep.base_sha.clone(),
        head: prep.head_sha.clone(),
        strategy: prep.strategy_name.clone(),
        findings: final_findings,
        carried,
        plan: PublicationPlan {
            inline,
            summary_markdown: summary,
            state: public_state,
            publish_uncertain: cfg.review.publish_uncertain,
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
    use super::*;
    use std::time::{Duration, Instant};

    fn carried_fixture() -> (tempfile::TempDir, Config, ReviewRequest) {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(dir.path())
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("bug.rs"), "bug\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(dir.path().join("bug.rs"), "still a bug\n").unwrap();
        git(&["commit", "-qam", "head"]);
        let cfg = Config::parse("vera: {enabled: false}\nmodels: {investigator: {protocol: openai-chat, base_url: 'https://api.example.com/v1', api_key_env: CARRIED_TEST_KEY, model: fixture}}", "test").unwrap();
        let req = ReviewRequest {
            repo: dir.path().into(),
            base: "HEAD~1".into(),
            head: None,
            title: None,
            body: String::new(),
            strategy_override: None,
            force: false,
            repository_public: false,
        };
        (dir, cfg, req)
    }

    fn carried_state() -> ReviewState {
        let mut state = ReviewState::default();
        for (key, status, posted) in [
            ("open", FindingState::Open, true),
            ("unposted", FindingState::Open, false),
            ("uncertain", FindingState::Uncertain, true),
            ("resolved", FindingState::Resolved, true),
            ("rejected", FindingState::Rejected, true),
        ] {
            let f: Finding = serde_json::from_value(json!({
                "defect_key":key,"severity":"high","file":"bug.rs","start_line":1,
                "title":key,"claim":"Bug remains","trigger":"Input","impact":"Wrong result",
                "supporting_evidence":[{"path":"bug.rs","start_line":1,"end_line":1,"note":"Evidence"}]
            })).unwrap();
            state.upsert(&f, status);
            if posted {
                state.mark_posted(&[f.id()]);
            }
        }
        state
    }

    fn assert_carried_report(report: &RunReport) {
        assert!(report.findings.is_empty());
        assert!(report.plan.inline.is_empty());
        assert_eq!(report.stats.accepted, 0);
        assert_eq!(report.carried.len(), 1);
        let f = &report.carried[0];
        assert_eq!(f.title, "open");
        assert_eq!(f.claim, "Bug remains");
        assert_eq!(f.trigger, "Input");
        assert_eq!(f.impact, "Wrong result");
        assert_eq!(f.evidence, ["bug.rs:1 Evidence"]);
        assert_eq!(f.to_finding().id(), f.id);
        assert!(!is_publishable(&f.to_finding()));
    }

    #[tokio::test]
    async fn fresh_report_carries_only_posted_open_findings() {
        let (_dir, cfg, req) = carried_fixture();
        let PrepareOut::Ready(mut prep) = prepare(&cfg, &req, "baseline").await.unwrap() else {
            panic!()
        };
        prep.state = carried_state();
        let (report, state) = finish(&cfg, prep, vec![], &[]).await.unwrap();
        assert_carried_report(&report);
        assert!(
            report
                .plan
                .summary_markdown
                .contains("1 still open from earlier runs")
        );
        // Even a current diagnostic with the same ID must suppress carrying.
        let current = state.findings[0].to_finding();
        assert!(crate::report::carried_findings(&state, &[current]).is_empty());
    }

    #[tokio::test]
    async fn reused_report_carries_only_posted_open_findings() {
        let (dir, cfg, req) = carried_fixture();
        let PrepareOut::Ready(prep) = prepare(&cfg, &req, "baseline").await.unwrap() else {
            panic!()
        };
        let mut state = carried_state();
        // An unposted open finding intentionally prevents the reuse path.
        state.findings.retain(|f| f.posted);
        state.record_outcome(
            &prep.base_sha,
            &prep.head_sha,
            &prep.patch_id,
            &prep.review_key,
            RunStatus::Complete,
        );
        state.save(dir.path()).unwrap();
        let PrepareOut::ShortCircuit(report, _) = prepare(&cfg, &req, "baseline").await.unwrap()
        else {
            panic!()
        };
        assert!(report.stats.reused);
        assert_carried_report(&report);
        assert!(
            report
                .plan
                .summary_markdown
                .contains("1 open finding carried forward")
        );
        assert!(!report.plan.summary_markdown.contains("2 open findings"));
    }

    #[tokio::test]
    async fn below_threshold_rechecks_become_uncertain_without_carrying_or_reposting() {
        for posted in [true, false] {
            let (dir, mut cfg, req) = carried_fixture();
            cfg.review.min_severity = crate::findings::Severity::Medium;
            let PrepareOut::Ready(mut prep) = prepare(&cfg, &req, "baseline").await.unwrap() else {
                panic!()
            };
            let PrepareOut::Ready(mut later_prep) = prepare(&cfg, &req, "baseline").await.unwrap()
            else {
                panic!()
            };
            let prior = carried_state().findings[0].to_finding();
            let mut corrected = prior.clone();
            corrected.severity = crate::findings::Severity::Low;
            corrected.validation_status = Some(crate::findings::ValidationStatus::Accepted);
            prep.state.upsert(&prior, FindingState::Open);
            if posted {
                prep.state.mark_posted(&[prior.id()]);
            }
            prep.rechecks = vec![corrected];
            let (report, state) = finish(&cfg, prep, vec![], &[]).await.unwrap();
            assert!(report.carried.is_empty());
            assert!(report.findings.is_empty());
            assert!(report.plan.inline.is_empty());
            let tracked = state.find(&prior.id()).unwrap();
            assert_eq!(tracked.status, FindingState::Uncertain);
            assert_eq!(tracked.severity, crate::findings::Severity::Low);
            assert_eq!(tracked.posted, posted);
            assert_eq!(report.stats.accepted, 0);
            assert_eq!(report.stats.uncertain, 0);
            assert_eq!(report.stats.below_threshold_after_validation, 1);
            assert!(
                report
                    .plan
                    .summary_markdown
                    .contains("no accepted findings")
            );
            assert!(
                !report
                    .plan
                    .summary_markdown
                    .contains("still open from earlier runs")
            );
            assert!(
                !report
                    .plan
                    .summary_markdown
                    .contains("Resolved since last review")
            );
            let saved = ReviewState::load(dir.path()).unwrap().unwrap();
            assert_eq!(
                saved.find(&prior.id()).unwrap().status,
                FindingState::Uncertain
            );
            assert_eq!(recheck_candidates(&saved).len(), 1);
            assert!(crate::report::carried_findings(&saved, &[]).is_empty());
            let PrepareOut::ShortCircuit(reused, _) =
                prepare(&cfg, &req, "baseline").await.unwrap()
            else {
                panic!()
            };
            assert!(reused.carried.is_empty());
            assert_eq!(reused.stats.accepted, 0);

            // A later above-threshold recheck reopens it without re-posting
            // an already published finding.
            later_prep.state = saved;
            later_prep.state.mark(
                &prior.id(),
                recheck_transition(crate::findings::ValidationStatus::Accepted),
            );
            later_prep.rechecks = vec![Finding {
                severity: crate::findings::Severity::High,
                validation_status: Some(crate::findings::ValidationStatus::Accepted),
                ..prior.clone()
            }];
            let (report, state) = finish(&cfg, later_prep, vec![], &[]).await.unwrap();
            assert_eq!(state.find(&prior.id()).unwrap().status, FindingState::Open);
            if posted {
                assert!(report.plan.inline.is_empty());
                assert!(report.findings.is_empty());
                assert_eq!(report.carried.len(), 1);
            } else {
                assert_eq!(report.findings.len(), 1);
                assert!(report.carried.is_empty());
            }
        }
    }

    #[test]
    fn tracked_block_lists_only_open_findings_when_validating() {
        use crate::state::{FindingState, ReviewState, StateFinding};
        let finding = |id: &str, status, end_line| StateFinding {
            id: id.into(),
            status,
            file: "src/a.rs".into(),
            start_line: 10,
            end_line,
            title: format!("title {id}"),
            posted: true,
            defect_key: format!("key_{id}"),
            severity: Default::default(),
            claim: String::new(),
            trigger: String::new(),
            impact: String::new(),
            evidence: vec![],
        };
        let mut state = ReviewState::default();
        assert_eq!(super::tracked_block(&state, true), "");
        state.findings = vec![
            finding("open", FindingState::Open, Some(12)),
            finding("unsure", FindingState::Uncertain, None),
            finding("fixed", FindingState::Resolved, None),
        ];
        let block = super::tracked_block(&state, true);
        assert!(
            block.contains("- src/a.rs:10-12 `key_open` title open\n"),
            "{block}"
        );
        assert!(
            block.contains("- src/a.rs:10 `key_unsure` title unsure\n"),
            "{block}"
        );
        assert!(!block.contains("key_fixed"), "{block}");
        assert_eq!(super::tracked_block(&state, false), "");
    }

    #[test]
    fn clamp_budget_uses_remaining_deadline() {
        let budget = clamp_budget(7, 100, Instant::now() + Duration::from_secs(5));
        assert!(budget.max_seconds <= 5);

        let expired = clamp_budget(7, 100, Instant::now() - Duration::from_secs(1));
        assert_eq!(expired.max_seconds, 0);
    }
}
