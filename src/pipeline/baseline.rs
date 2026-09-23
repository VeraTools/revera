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
    let candidates = investigate(cfg, req, &mut prep).await?;
    finish(cfg, prep, candidates, &["investigator".to_string()]).await
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
    let candidates = investigate(cfg, req, &mut prep).await?;
    finish(cfg, prep, candidates, &["investigator".to_string()]).await
}

/// Run the investigator agent and return parsed candidate findings.
pub(crate) async fn investigate(
    cfg: &Config,
    req: &ReviewRequest,
    prep: &mut Prepared,
) -> Result<Vec<Finding>> {
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
    );
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
            prep.coverage = call.arguments["coverage"]
                .as_str()
                .unwrap_or("(none)")
                .to_string();
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
    prep.timing
        .record("lane", "investigator", lane_start, prep.wall, outcome);
    Ok(candidates)
}
