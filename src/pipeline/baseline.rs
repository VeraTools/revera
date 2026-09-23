use super::common::{
    findings_terminal_check, finish, investigator_user, parse_findings_checked, prepare,
    PrepareOut, Prepared, ReviewRequest,
};
use super::make_client;
use crate::agent::run_agent_checked;
use crate::config::Config;
use crate::findings::Finding;
use crate::prompts;
use crate::provider::ToolSpec;
use crate::report::RunReport;
use crate::state::ReviewState;
use crate::tools::terminal_submit_findings_spec;
use anyhow::Result;

/// Baseline single-lane investigation.
pub async fn run(cfg: &Config, req: &ReviewRequest) -> Result<(RunReport, ReviewState)> {
    let mut prep = match prepare(cfg, req, "baseline").await? {
        PrepareOut::Ready(p) => p,
        PrepareOut::ShortCircuit(rep, st) => return Ok((*rep, st)),
    };
    let candidates = investigate_rounds(cfg, req, &mut prep).await?;
    finish(cfg, prep, candidates, &["investigator".to_string()]).await
}

/// `review.recall_rounds` investigator passes. Later rounds see what was
/// already reported and look for different defects; they stop as soon as a
/// round adds nothing new or the run deadline leaves no budget. Every
/// candidate still goes through validation.
pub(crate) async fn investigate_rounds(
    cfg: &Config,
    req: &ReviewRequest,
    prep: &mut Prepared,
) -> Result<Vec<Finding>> {
    let mut candidates = investigate(cfg, req, prep, &[]).await?;
    for round in 2..=cfg.review.recall_rounds {
        if prep
            .budget(
                cfg.budget.agent_max_tool_calls,
                cfg.budget.agent_max_seconds,
            )
            .max_seconds
            == 0
        {
            break;
        }
        let known = crate::findings::collapse(candidates.clone()).len();
        // round 1 covered the change; a later round that fails or times out
        // costs recall, not coverage, so it does not make the run partial
        let reasons = prep.partial_reasons.len();
        let more = investigate(cfg, req, prep, &candidates).await?;
        if prep.partial_reasons.len() > reasons {
            let dropped: Vec<String> = prep.partial_reasons.drain(reasons..).collect();
            tracing::warn!("recall round {round} incomplete: {}", dropped.join("; "));
        }
        candidates.extend(more);
        let now = crate::findings::collapse(candidates.clone()).len();
        tracing::info!("recall round {round}: {} new candidate(s)", now - known);
        if now == known {
            break;
        }
    }
    Ok(candidates)
}

/// Risk-tier downgrade for the swarm strategies: a trivial change gets one
/// investigator instead of the configured `swarm`.
pub(crate) async fn run_trivial(
    cfg: &Config,
    req: &ReviewRequest,
    mut prep: Box<Prepared>,
    swarm: &str,
) -> Result<(RunReport, ReviewState)> {
    prep.report_note = Some(format!(
        "risk tier trivial: single investigator instead of the {swarm} swarm"
    ));
    let candidates = investigate_rounds(cfg, req, &mut prep).await?;
    finish(cfg, prep, candidates, &["investigator".to_string()]).await
}

/// Run the investigator agent and return parsed candidate findings.
/// `already` lists findings of earlier rounds, which the agent must not
/// repeat.
pub(crate) async fn investigate(
    cfg: &Config,
    req: &ReviewRequest,
    prep: &mut Prepared,
    already: &[Finding],
) -> Result<Vec<Finding>> {
    let round = !already.is_empty();
    let investigator = make_client(
        &cfg.models.investigator,
        "investigator",
        prep.ledger.clone(),
        cfg.budget.run_max_requests,
        cfg.budget.retries,
        "submit_findings",
    )?;
    let user = investigator_user(
        req,
        &prep.diff,
        cfg.review.max_diff_bytes,
        &cfg.review.path_instructions,
        &cfg.review.knowledge_base,
        &prep.repo_guidance,
    );
    let user = if round {
        let listed: Vec<String> = already
            .iter()
            .map(|f| {
                format!(
                    "- {}:{} [{}] {}",
                    f.file,
                    f.start_line,
                    f.defect_key,
                    crate::text::excerpt_bytes(&f.title, 160)
                )
            })
            .collect();
        format!(
            "{user}\n\nAlready reported by an earlier pass (do not repeat these; look for different defects, and submit an empty list if there are none):\n{}",
            listed.join("\n")
        )
    } else {
        user
    };
    let terminal: ToolSpec = terminal_submit_findings_spec();
    let lane_start = std::time::Instant::now();
    let run = run_agent_checked(
        investigator.as_ref(),
        prompts::INVESTIGATOR,
        &user,
        &prep.toolbox,
        &terminal,
        &prep.budget(
            cfg.budget.agent_max_tool_calls,
            cfg.budget.agent_max_seconds,
        ),
        &findings_terminal_check,
    )
    .await?;
    prep.stats.repaired |= run.repaired;
    let mut candidates: Vec<Finding> = vec![];
    let outcome = match run.stopped {
        crate::agent::StopReason::Terminal if run.final_call.is_none() => {
            prep.partial_reasons
                .push("investigator ended without a submission".into());
            "error"
        }
        crate::agent::StopReason::Terminal => {
            let call = run.final_call.as_ref().expect("checked above");
            tracing::debug!(args = %call.arguments, "investigator terminal call");
            if !round {
                prep.coverage = call.arguments["coverage"]
                    .as_str()
                    .unwrap_or("(none)")
                    .to_string();
            }
            // a valid empty list is a complete clean result; a missing or
            // malformed list is not — keep what parsed, mark the run partial
            let parsed = parse_findings_checked(&call.arguments);
            candidates = parsed.findings;
            prep.stats.malformed_findings += parsed.dropped;
            for c in &mut candidates {
                c.source = "investigator".into();
                c.sources = vec!["investigator".into()];
            }
            match parsed.problem {
                Some(p) => {
                    prep.partial_reasons
                        .push(format!("investigator submission incomplete: {p}"));
                    "ok:malformed"
                }
                None if candidates.is_empty() => "ok",
                None => "ok:candidates",
            }
        }
        crate::agent::StopReason::TimeBudget => {
            prep.partial_reasons
                .push("investigator stopped early: TimeBudget".into());
            "timeout"
        }
        crate::agent::StopReason::ToolBudget => {
            prep.partial_reasons
                .push("investigator stopped early: ToolBudget".into());
            "tool_budget"
        }
        other => {
            prep.partial_reasons
                .push(format!("investigator stopped early: {other:?}"));
            "error"
        }
    };
    let label = if round {
        "investigator:recall"
    } else {
        "investigator"
    };
    prep.timing
        .record("lane", label, lane_start, prep.wall, outcome);
    Ok(candidates)
}
