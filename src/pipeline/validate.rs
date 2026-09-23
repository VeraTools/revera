use super::make_client;
use crate::agent::{run_agent, AgentBudget, AgentRun, StopReason};
use crate::config::Config;
use crate::diff::DiffSet;
use crate::findings::{Finding, ValidationStatus, Verdict};
use crate::prompts;
use crate::provider::{LedgerHandle, ToolSpec};
use crate::timing::Recorder;
use crate::tools::{terminal_submit_verdict_spec, ToolBox};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

fn validator_budget_at(
    max_tool_calls: u32,
    max_seconds: u64,
    now: std::time::Instant,
    deadline: std::time::Instant,
) -> Option<AgentBudget> {
    let remaining = deadline.checked_duration_since(now)?;
    let max_seconds = max_seconds.min(remaining.as_secs());
    if max_seconds == 0 {
        return None;
    }
    Some(AgentBudget {
        max_tool_calls,
        max_seconds,
    })
}

/// Strict EVAL validation gate: verifies a candidate names a real location
/// before dispatching a validator agent. The file must be in the diff or be
/// a readable file of the reviewed tree: cross-file findings outside the diff
/// are legitimate and are published in the summary.
pub fn has_verifiable_evidence(finding: &Finding, diff: &DiffSet, repo_root: &Path) -> bool {
    if finding.file.trim().is_empty() || finding.start_line == 0 {
        return false;
    }
    if diff.file(&finding.file).is_none()
        && !ToolBox::is_safe_repo_path(repo_root, &finding.file).is_ok_and(|p| p.is_file())
    {
        return false;
    }
    for e in &finding.supporting_evidence {
        if e.path.contains("..") || e.path.starts_with('/') {
            return false;
        }
    }
    true
}

/// Run one fresh-context validator agent per candidate, bounded by a
/// concurrency semaphore. Failures mark the candidate uncertain; returns
/// Some(partial_reason) when any candidate could not be conclusively validated.
#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
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
    deadline: std::time::Instant,
    timing: &Recorder,
    wall: std::time::Instant,
    phase_name: &str,
) -> Option<String> {
    let sem = Arc::new(Semaphore::new(cfg.review.concurrency.max(1)));
    let mut set = tokio::task::JoinSet::new();
    let mut skipped_from = None;
    for i in 0..candidates.len() {
        if std::time::Instant::now() >= deadline {
            skipped_from = Some(i);
            break;
        }
        let sem = sem.clone();
        let cfg_models = cfg.effective_validator().clone();
        let ledger = ledger.clone();
        let tb = toolbox.clone();
        let diff = diff.clone();
        let budget = cfg.budget.clone();
        let retries = cfg.budget.retries;
        let max_req = cfg.budget.run_max_requests;
        let system = system_prompt.to_string();
        let terminal = terminal.clone();
        let role = role.to_string();
        let cand = candidates[i].clone();
        let cand_id = cand.id();
        let excerpt = diff.file_excerpt(&cand.file);
        // rechecks re-validate findings already published; only the
        // validator may resolve them, never this pre-filter
        if !recheck && !has_verifiable_evidence(&cand, &diff, &tb.repo_root) {
            candidates[i].validation_status = Some(ValidationStatus::Rejected);
            candidates[i].rationale =
                Some("failed EVAL gate: missing or ungrounded code evidence".into());
            timing.record(
                phase_name,
                &format!("validator:{}", cand_id),
                std::time::Instant::now(),
                wall,
                "eval_gate:rejected",
            );
            continue;
        }
        // create the client in candidate order (before the semaphore race)
        // so scripted validators are matched to candidates deterministically
        let client = make_client(&cfg_models, &role, ledger, max_req, retries, &terminal.name);
        set.spawn(async move {
            let queued_at = std::time::Instant::now();
            let _permit = sem.acquire().await.unwrap();
            let exec_start = std::time::Instant::now();
            let queue_ms = exec_start.duration_since(queued_at).as_millis() as u64;
            let agent_budget = match validator_budget_at(
                budget.agent_max_tool_calls,
                budget.agent_max_seconds,
                std::time::Instant::now(),
                deadline,
            ) {
                Some(budget) => budget,
                None => {
                    return (
                        i,
                        Ok(AgentRun {
                            final_call: None,
                            transcript_len: 0,
                            tool_calls: 0,
                            stopped: StopReason::TimeBudget,
                            repaired: false,
                        }),
                        cand_id,
                        exec_start,
                        queue_ms,
                        true,
                    )
                }
            };
            let client = match client {
                Ok(c) => c,
                Err(e) => return (i, Err(e), cand_id, exec_start, queue_ms, false),
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
                &agent_budget,
            )
            .await;
            (i, run, cand_id, exec_start, queue_ms, false)
        });
    }
    if let Some(from) = skipped_from {
        for c in candidates.iter_mut().skip(from) {
            timing.record(
                phase_name,
                &c.id(),
                std::time::Instant::now(),
                wall,
                "skipped",
            );
            c.validation_status = Some(ValidationStatus::Uncertain);
            c.rationale = Some("run time budget exhausted".into());
        }
    }
    let mut clean = skipped_from.is_none();
    let mut reason = skipped_from.map(|_| "run time budget exhausted".to_string());
    while let Some(res) = set.join_next().await {
        let (i, run, cand_id, exec_start, queue_ms, skipped) =
            res.expect("validator task panicked");
        let cand = &mut candidates[i];
        let outcome: String = if skipped {
            // the deadline passed while this task queued on the semaphore
            clean = false;
            reason = Some("run time budget exhausted".into());
            cand.validation_status = Some(ValidationStatus::Uncertain);
            cand.rationale = Some("run time budget exhausted".into());
            "skipped".to_string()
        } else {
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
                        if v.validation_status == ValidationStatus::Accepted {
                            "ok:accepted".to_string()
                        } else {
                            "ok".to_string()
                        }
                    }
                    Err(e) => {
                        clean = false;
                        cand.validation_status = Some(ValidationStatus::Uncertain);
                        cand.rationale = Some(format!("validator returned malformed verdict: {e}"));
                        "error:malformed verdict".to_string()
                    }
                },
                Ok(AgentRun {
                    stopped: StopReason::TimeBudget,
                    ..
                }) => {
                    clean = false;
                    reason = Some("run time budget exhausted".into());
                    cand.validation_status = Some(ValidationStatus::Uncertain);
                    cand.rationale = Some("run time budget exhausted".into());
                    "timeout".to_string()
                }
                Ok(AgentRun {
                    stopped: StopReason::ToolBudget,
                    ..
                }) => {
                    clean = false;
                    cand.validation_status = Some(ValidationStatus::Uncertain);
                    cand.rationale = Some(format!(
                        "validator unavailable: {:?}",
                        StopReason::ToolBudget
                    ));
                    "tool_budget".to_string()
                }
                Ok(AgentRun { stopped, .. }) => {
                    clean = false;
                    cand.validation_status = Some(ValidationStatus::Uncertain);
                    cand.rationale = Some(format!("validator unavailable: {stopped:?}"));
                    format!("error:{stopped:?}")
                }
                Err(e) => {
                    clean = false;
                    cand.validation_status = Some(ValidationStatus::Uncertain);
                    cand.rationale = Some(format!("validator unavailable: {e}"));
                    format!("error:{}", crate::text::excerpt_bytes(&e.to_string(), 60))
                }
            }
        };
        timing.record_queued(phase_name, &cand_id, exec_start, queue_ms, wall, &outcome);
    }
    if clean {
        None
    } else {
        Some(reason.unwrap_or_else(|| "one or more validations were inconclusive".into()))
    }
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

#[cfg(test)]
mod tests {
    use super::validator_budget_at;
    use std::time::{Duration, Instant};

    #[test]
    fn validator_budget_is_none_after_deadline() {
        let now = Instant::now();
        assert!(validator_budget_at(4, 30, now, now - Duration::from_secs(1)).is_none());
    }

    #[test]
    fn validator_budget_is_none_under_one_second() {
        let now = Instant::now();
        assert!(validator_budget_at(4, 30, now, now + Duration::from_millis(400)).is_none());
        let b = validator_budget_at(4, 30, now, now + Duration::from_secs(5)).unwrap();
        assert_eq!(b.max_seconds, 5);
    }
}
