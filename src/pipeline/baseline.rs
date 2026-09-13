use super::make_client;
use super::validate::validate_candidates;
use crate::agent::{run_agent, AgentBudget};
use crate::config::{Config, Strategy};
use crate::diff::{parse_unified, DiffSet};
use crate::findings::{collapse, Finding, Severity};
use crate::git;
use crate::pipeline::anchor::{anchor, is_publishable, Placement};
use crate::pipeline::validate::{recheck_prompt, validator_prompt, validator_terminal};
use crate::prompts;
use crate::provider::{LedgerHandle, ToolSpec};
use crate::report::{
    finding_body, ledger_report, summary_markdown, InlineComment, PublicationPlan, RunReport,
    RunStatus,
};
use crate::state::{recheck_transition, FindingState, ReviewState};
use crate::tools::{terminal_submit_findings_spec, ToolBox};
use crate::vera::VeraClient;
use anyhow::{bail, Result};
use std::sync::Arc;
use std::time::Instant;

pub struct ReviewRequest {
    pub repo: std::path::PathBuf,
    pub base: String,
    pub head: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub strategy_override: Option<Strategy>,
    pub force: bool,
}

fn parse_findings(args: &serde_json::Value) -> Vec<Finding> {
    args["findings"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| serde_json::from_value::<Finding>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Prior open findings -> recheck validator verdicts, updating state.
/// Returns the candidates-as-Finding for verdict mapping.
fn recheck_candidates(state: &ReviewState) -> Vec<Finding> {
    state
        .open_findings()
        .iter()
        .map(|f| Finding {
            defect_key: f.defect_key.clone(),
            severity: Severity::Medium,
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

pub async fn run(cfg: &Config, req: &ReviewRequest) -> Result<(RunReport, ReviewState)> {
    let wall = Instant::now();
    let repo = git::repo_root(&req.repo);
    if !git::is_repo(&repo).await {
        bail!("{} is not a git repository", repo.display());
    }
    let base_sha = git::rev_parse(&repo, &req.base).await?;
    let head_rev = req.head.as_deref().unwrap_or("HEAD");
    let head_sha = git::rev_parse(&repo, head_rev).await?;
    let strategy = req.strategy_override.unwrap_or(cfg.review.strategy);
    if strategy != Strategy::Baseline {
        bail!("strategy {strategy:?} not implemented yet");
    }

    let raw_diff = git::diff(&repo, &req.base, head_rev).await?;
    let diff: Arc<DiffSet> = Arc::new(parse_unified(&raw_diff));
    let patch_id = git::patch_id(&repo, &req.base, head_rev)
        .await
        .unwrap_or_default();
    let ledger = LedgerHandle::new();

    let mut state = ReviewState::load(&repo)?.unwrap_or_default();
    let had_prior = !state.findings.is_empty() || !state.patch_id.is_empty();

    // short-circuit: identical patch content already reviewed
    if !req.force && had_prior && state.is_unchanged(&base_sha, &patch_id) {
        let mut plan_state = state.clone();
        plan_state.reviewed_head = head_sha.clone();
        state.save(&repo)?;
        let summary = summary_markdown(
            &[],
            &[],
            "unchanged since last review (identical patch-id)",
            RunStatus::Complete,
            "baseline",
            &[],
            cfg.review.publish_uncertain,
        );
        let rep = RunReport {
            status: RunStatus::Complete,
            reason: Some("unchanged since last review".into()),
            base: base_sha,
            head: head_sha,
            strategy: "baseline".into(),
            findings: vec![],
            plan: PublicationPlan {
                inline: vec![],
                summary_markdown: summary,
                state: plan_state,
            },
            ledger: ledger_report(&ledger.0.lock().unwrap(), wall.elapsed().as_millis() as u64),
        };
        return Ok((rep, state));
    }

    let vera = Arc::new(VeraClient::from_config(&cfg.vera, &repo)?);
    if let Err(e) = vera.ensure_index().await {
        tracing::warn!("vera index failed (continuing without retrieval): {e}");
    }
    let toolbox = Arc::new(ToolBox::new(
        repo.clone(),
        diff.clone(),
        vera,
        cfg.review.max_tool_output_bytes,
    ));
    let budget = AgentBudget {
        max_tool_calls: cfg.budget.agent_max_tool_calls,
        max_seconds: cfg.budget.agent_max_seconds,
    };
    let mut partial_reasons: Vec<String> = Vec::new();

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
        )
        .await;
        if !clean {
            partial_reasons.push("one or more rechecks were inconclusive".into());
        }
        for rc in &rechecks {
            if let Some(vs) = rc.validation_status {
                let st = recheck_transition(vs);
                state.mark(&rc.id(), st);
            }
        }
    }

    // ---- investigate ----
    let investigator = make_client(
        &cfg.models.investigator,
        "investigator",
        ledger.clone(),
        cfg.budget.run_max_requests,
        cfg.budget.retries,
        "submit_findings",
    )?;
    let changed: Vec<String> = diff
        .files
        .iter()
        .map(|f| format!("{:?} {}", f.status, f.new_path))
        .collect();
    let user = format!(
        "PR title: {}\n\nPR body:\n{}\n\nChanged files:\n{}\n\nDiff:\n{}",
        req.title.as_deref().unwrap_or("(untitled)"),
        req.body,
        changed.join("\n"),
        diff.render_truncated(cfg.review.max_diff_bytes),
    );
    let terminal: ToolSpec = terminal_submit_findings_spec();
    let run = run_agent(
        investigator.as_ref(),
        prompts::INVESTIGATOR,
        &user,
        &toolbox,
        &terminal,
        &budget,
    )
    .await?;
    let mut coverage = String::from("(none reported)");
    let mut candidates: Vec<Finding> = vec![];
    match run.stopped {
        crate::agent::StopReason::Terminal => {
            if let Some(call) = &run.final_call {
                coverage = call.arguments["coverage"]
                    .as_str()
                    .unwrap_or("(none)")
                    .to_string();
                candidates = parse_findings(&call.arguments);
                for c in &mut candidates {
                    c.source = "investigator".into();
                    c.sources = vec!["investigator".into()];
                }
            }
        }
        other => {
            partial_reasons.push(format!("investigator stopped early: {other:?}"));
        }
    }

    // ---- collapse + severity gate ----
    let mut collapsed = collapse(candidates);
    collapsed.retain(|f| f.severity >= cfg.review.min_severity);
    // dedupe against prior posted/open ids
    collapsed.retain(|f| {
        let id = f.id();
        !(state.has_posted(&id)
            || state
                .findings
                .iter()
                .any(|p| p.id == id && p.status == FindingState::Open))
    });

    // ---- validate ----
    if !collapsed.is_empty() {
        let clean = validate_candidates(
            cfg,
            ledger.clone(),
            &toolbox,
            &diff,
            &mut collapsed,
            validator_prompt(),
            &validator_terminal(),
            "validator",
            false,
        )
        .await;
        if !clean {
            partial_reasons.push("one or more validations were inconclusive".into());
        }
    }

    // ---- anchor + plan ----
    let anchored = anchor(&diff, collapsed.clone(), cfg.review.max_findings);
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
    for f in &final_findings {
        let st = match f.validation_status {
            Some(crate::findings::ValidationStatus::Accepted) => FindingState::Open,
            Some(crate::findings::ValidationStatus::Rejected) => FindingState::Rejected,
            _ => FindingState::Open,
        };
        let posted = st == FindingState::Open
            && f.validation_status == Some(crate::findings::ValidationStatus::Accepted);
        state.upsert(f, st, posted);
    }
    state.reviewed_base = base_sha.clone();
    state.reviewed_head = head_sha.clone();
    state.patch_id = patch_id;
    state.save(&repo)?;

    let status = if partial_reasons.is_empty() {
        RunStatus::Complete
    } else {
        RunStatus::Partial
    };
    let reason = if partial_reasons.is_empty() {
        None
    } else {
        Some(partial_reasons.join("; "))
    };
    let routes: Vec<String> = ledger
        .0
        .lock()
        .unwrap()
        .entries
        .iter()
        .map(|e| format!("{}:{}", e.route, e.model))
        .collect();
    let summary = summary_markdown(
        &final_findings,
        &outside,
        &coverage,
        status,
        "baseline",
        &routes,
        cfg.review.publish_uncertain,
    );
    let rep = RunReport {
        status,
        reason,
        base: base_sha,
        head: head_sha,
        strategy: "baseline".into(),
        findings: final_findings,
        plan: PublicationPlan {
            inline,
            summary_markdown: summary,
            state: state.clone(),
        },
        ledger: ledger_report(&ledger.0.lock().unwrap(), wall.elapsed().as_millis() as u64),
    };
    Ok((rep, state))
}
