use super::make_client;
use crate::agent::{run_agent, AgentBudget, AgentRun};
use crate::config::Config;
use crate::diff::DiffSet;
use crate::findings::{Finding, ValidationStatus, Verdict};
use crate::prompts;
use crate::provider::{LedgerHandle, ToolSpec};
use crate::tools::{terminal_submit_verdict_spec, ToolBox};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Run one fresh-context validator agent per candidate, bounded by a
/// concurrency semaphore. Failures mark the candidate uncertain and flag the
/// run partial via the returned bool (false = had failures).
#[allow(clippy::too_many_arguments)]
pub async fn validate_candidates(
    cfg: &Config,
    ledger: LedgerHandle,
    toolbox: &Arc<ToolBox>,
    diff: &Arc<DiffSet>,
    candidates: &mut [Finding],
    system_prompt: &str,
    terminal: &ToolSpec,
    role: &str,
    recheck: bool,
) -> bool {
    let sem = Arc::new(Semaphore::new(cfg.review.concurrency.max(1)));
    let mut set = tokio::task::JoinSet::new();
    for (i, c) in candidates.iter().enumerate() {
        let sem = sem.clone();
        let cfg_models = cfg.models.validator.clone();
        let ledger = ledger.clone();
        let tb = toolbox.clone();
        let diff = diff.clone();
        let budget = cfg.budget.clone();
        let retries = cfg.budget.retries;
        let max_req = cfg.budget.run_max_requests;
        let system = system_prompt.to_string();
        let terminal = terminal.clone();
        let role = role.to_string();
        let cand = c.clone();
        let excerpt = diff.file_excerpt(&cand.file);
        set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let client =
                match make_client(&cfg_models, &role, ledger, max_req, retries, &terminal.name) {
                    Ok(c) => c,
                    Err(e) => return (i, Err(e)),
                };
            let user = if recheck {
                format!(
                    "PR diff for {}:\n\n{}\n\nPrior finding to recheck:\n{}",
                    cand.file,
                    excerpt,
                    serde_json::to_string_pretty(&cand).unwrap_or_default()
                )
            } else {
                format!(
                    "PR diff excerpt for {}:\n\n{}\n\nCandidate finding:\n{}",
                    cand.file,
                    excerpt,
                    serde_json::to_string_pretty(&cand).unwrap_or_default()
                )
            };
            let run = run_agent(
                client.as_ref(),
                &system,
                &user,
                &tb,
                &terminal,
                &AgentBudget {
                    max_tool_calls: budget.agent_max_tool_calls,
                    max_seconds: budget.agent_max_seconds,
                },
            )
            .await;
            (i, run)
        });
    }
    let mut clean = true;
    while let Some(res) = set.join_next().await {
        let (i, run) = res.expect("validator task panicked");
        let cand = &mut candidates[i];
        match run {
            Ok(AgentRun {
                final_call: Some(call),
                ..
            }) => match serde_json::from_value::<Verdict>(call.arguments) {
                Ok(v) => {
                    cand.validation_status = Some(v.validation_status);
                    cand.counterevidence_checked = v.counterevidence_checked;
                    if let Some(s) = v.severity {
                        cand.severity = s;
                    }
                    if let Some(l) = v.start_line {
                        cand.start_line = l;
                    }
                    if let Some(l) = v.end_line {
                        cand.end_line = Some(l);
                    }
                    cand.rationale = Some(v.rationale);
                }
                Err(e) => {
                    clean = false;
                    cand.validation_status = Some(ValidationStatus::Uncertain);
                    cand.rationale = Some(format!("validator returned malformed verdict: {e}"));
                }
            },
            Ok(AgentRun { stopped, .. }) => {
                clean = false;
                cand.validation_status = Some(ValidationStatus::Uncertain);
                cand.rationale = Some(format!("validator unavailable: {stopped:?}"));
            }
            Err(e) => {
                clean = false;
                cand.validation_status = Some(ValidationStatus::Uncertain);
                cand.rationale = Some(format!("validator unavailable: {e}"));
            }
        }
    }
    clean
}

pub fn validator_terminal() -> ToolSpec {
    terminal_submit_verdict_spec()
}

pub fn validator_prompt() -> &'static str {
    prompts::VALIDATOR
}

pub fn recheck_prompt() -> &'static str {
    prompts::RECHECK
}
