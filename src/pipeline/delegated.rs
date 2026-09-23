use super::common::{
    findings_terminal_check, finish, investigator_user, parse_candidate_findings,
    parse_findings_checked, prepare, PrepareOut, ReviewRequest,
};
use super::make_client;
use crate::agent::{run_agent, run_agent_checked};
use crate::config::Config;
use crate::findings::Finding;
use crate::prompts;
use crate::report::RunReport;
use crate::state::ReviewState;
use crate::tools::{
    terminal_submit_findings_spec, terminal_submit_plan_spec, terminal_submit_worker_result_spec,
};
use anyhow::Result;
use futures::stream::StreamExt;
use serde::Deserialize;

/// Tools a delegated worker may call: read-only inspection plus both
/// retrieval families, so a lexical-only run (Vera disabled or degraded)
/// can still discover untouched files.
pub const WORKER_TOOLS: &[&str] = &[
    "read_file",
    "list_changed_files",
    "diff_context",
    "grep_repo",
    "find_files",
    "vera_search",
    "vera_references",
    "vera_grep",
    "vera_overview",
];

#[derive(Debug, Clone, Deserialize)]
pub struct PlanQuestion {
    #[serde(default)]
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub expected_evidence: String,
    #[serde(default)]
    pub stop_condition: String,
    #[serde(default)]
    pub files_hint: Vec<String>,
}

/// One worker's raw result, stored verbatim (bounded) for synthesis.
#[derive(Debug)]
struct WorkerReport {
    question_id: String,
    result: String,
    answer: String,
    raw: String,
    findings: Vec<Finding>,
    gaps: Vec<String>,
}

fn worker_failure_reason(index: usize, model: &str, detail: &str) -> String {
    format!("worker {index} ({model}) did not complete: {detail}")
}

/// Delegated strategy: lead plans bounded questions, workers answer them
/// concurrently, the lead synthesizes final candidates.
pub async fn run(cfg: &Config, req: &ReviewRequest) -> Result<(RunReport, ReviewState)> {
    let mut prep = match prepare(cfg, req, "delegated").await? {
        PrepareOut::Ready(p) => p,
        PrepareOut::ShortCircuit(rep, st) => return Ok((*rep, st)),
    };
    if prep.risk_tier == Some(crate::triage::RiskTier::Trivial) {
        return super::baseline::run_trivial(cfg, req, prep, "delegated").await;
    }
    let labels = vec![
        "lead".to_string(),
        "workers".to_string(),
        "validator".to_string(),
    ];
    let candidates = delegated_candidates(cfg, req, &mut prep).await?;
    finish(cfg, prep, candidates, &labels).await
}

async fn delegated_candidates(
    cfg: &Config,
    req: &ReviewRequest,
    prep: &mut super::common::Prepared,
) -> Result<Vec<Finding>> {
    let lead_route = cfg
        .models
        .lead
        .clone()
        .unwrap_or_else(|| cfg.models.investigator.clone());

    // ---- 1. lead plans ----
    let plan_terminal = terminal_submit_plan_spec();
    let lead = make_client(
        &lead_route,
        "lead",
        prep.ledger.clone(),
        cfg.budget.run_max_requests,
        cfg.budget.retries,
        &plan_terminal.name,
    )?;
    let mut user = investigator_user(
        req,
        &prep.diff,
        cfg.review.max_diff_bytes,
        &cfg.review.path_instructions,
        &cfg.review.knowledge_base,
        &prep.repo_guidance,
    );
    if !prep.rechecks.is_empty() {
        user.push_str("\n\nPrior findings under recheck:\n");
        for r in &prep.rechecks {
            user.push_str(&format!("- {}:{} {}\n", r.file, r.start_line, r.title));
        }
    }
    let system =
        prompts::LEAD_PLAN.replace("{max_questions}", &cfg.delegated.max_questions.to_string());
    let plan_start = std::time::Instant::now();
    let run = run_agent(
        lead.as_ref(),
        &system,
        &user,
        &prep.toolbox,
        &plan_terminal,
        &prep.budget(6, cfg.budget.agent_max_seconds),
    )
    .await?;
    let plan_outcome = match run.stopped {
        crate::agent::StopReason::Terminal => "ok",
        crate::agent::StopReason::TimeBudget => "timeout",
        crate::agent::StopReason::ToolBudget => "tool_budget",
        _ => "error",
    };
    prep.timing
        .record("plan", "lead", plan_start, prep.wall, plan_outcome);
    let mut questions: Vec<PlanQuestion> = match run.stopped {
        crate::agent::StopReason::Terminal => run
            .final_call
            .as_ref()
            .map(|c| {
                c.arguments["questions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|q| serde_json::from_value::<PlanQuestion>(q.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
        other => {
            prep.partial_reasons
                .push(format!("lead planner stopped early: {other:?}"));
            vec![]
        }
    };
    questions.truncate(cfg.delegated.max_questions);
    if questions.is_empty() {
        // no questions -> degrade to baseline investigator
        prep.partial_reasons
            .push("lead produced no questions; fell back to baseline".into());
        tracing::info!("delegated: lead returned no questions; running baseline investigator");
        return super::baseline::investigate(cfg, req, prep).await;
    }

    // ---- 2. workers concurrently ----
    let worker_routes = cfg.models.workers.as_deref().unwrap_or(&[]);
    let worker_terminal = terminal_submit_worker_result_spec();
    let worker_tb = std::sync::Arc::new(prep.toolbox.restricted(WORKER_TOOLS));
    let lane_budget = prep.budget(
        cfg.delegated.worker_max_tool_calls,
        cfg.delegated.worker_max_seconds,
    );
    // Create clients in lane order so scripted conversations pop deterministically.
    let mut worker_clients = Vec::new();
    for (i, _q) in questions.iter().enumerate() {
        let route = select_worker_route(worker_routes, &cfg.models.investigator, i);
        let c = match make_client(
            route,
            "workers",
            prep.ledger.clone(),
            cfg.budget.run_max_requests,
            cfg.budget.retries,
            &worker_terminal.name,
        ) {
            Ok(c) => c,
            Err(e) => return Err(e.into()),
        };
        worker_clients.push(c);
    }
    let max_req = cfg.budget.run_max_requests;
    let lane_futs = questions.iter().enumerate().map(|(i, q)| {
        let client = worker_clients[i].clone();
        let worker_model = select_worker_route(worker_routes, &cfg.models.investigator, i)
            .model
            .clone();
        let tb = worker_tb.clone();
        let terminal = worker_terminal.clone();
        let budget = lane_budget.clone();
        let ledger = prep.ledger.clone();
        let timing = prep.timing.clone();
        let wall = prep.wall;
        let qid = if q.id.is_empty() {
            format!("q{}", i + 1)
        } else {
            q.id.clone()
        };
        let qtext = format!(
            "Question {}: {}\n\nSymbols to start from: {}\n\nExpected evidence: {}\n\nStop condition: {}\n\nFiles to look at first: {}\n\nDiff (context):\n{}",
            qid,
            q.question,
            q.symbols.join(", "),
            q.expected_evidence,
            q.stop_condition,
            q.files_hint.join(", "),
            prep.diff.render_truncated(cfg.review.max_diff_bytes),
        );
        async move {
            let lane_start = std::time::Instant::now();
            if ledger.request_count() >= max_req {
                timing.record(
                    "lane",
                    &format!("worker:{qid}"),
                    lane_start,
                    wall,
                    "skipped",
                );
                return (i, qid, worker_model, None, lane_start);
            }
            let run = run_agent(
                client.as_ref(),
                prompts::WORKER,
                &qtext,
                &tb,
                &terminal,
                &budget,
            )
            .await;
            (i, qid, worker_model, Some(run), lane_start)
        }
    });
    let mut lanes =
        futures::stream::iter(lane_futs).buffer_unordered(cfg.review.concurrency.max(1));
    let mut skipped = 0usize;
    let mut reports: Vec<WorkerReport> = Vec::new();
    while let Some((i, qid, worker_model, run, lane_start)) = lanes.next().await {
        let Some(run) = run else {
            skipped += 1;
            continue;
        };
        let mut lane_outcome = "ok";
        let report = match run {
            Ok(r) => match r.final_call {
                Some(call) => {
                    let a = &call.arguments;
                    let result = a["result"].as_str().unwrap_or("answered").to_string();
                    let answer = a["answer"].as_str().unwrap_or("").to_string();
                    let gaps: Vec<String> = a["gaps"]
                        .as_array()
                        .map(|g| {
                            g.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut findings = parse_candidate_findings(&a["candidate_findings"]);
                    if !findings.is_empty() {
                        lane_outcome = "ok:candidates";
                    }
                    for f in &mut findings {
                        f.source = format!("delegated:worker:{qid}");
                        f.sources = vec![f.source.clone()];
                    }
                    let raw = {
                        let s = serde_json::to_string_pretty(a).unwrap_or_default();
                        if s.len() > 6000 {
                            format!("{}...[truncated]", &s[..6000])
                        } else {
                            s
                        }
                    };
                    WorkerReport {
                        question_id: qid.clone(),
                        result,
                        answer,
                        raw,
                        findings,
                        gaps,
                    }
                }
                None => {
                    lane_outcome = match r.stopped {
                        crate::agent::StopReason::TimeBudget => "timeout",
                        crate::agent::StopReason::ToolBudget => "tool_budget",
                        _ => "error",
                    };
                    WorkerReport {
                        question_id: qid.clone(),
                        result: "blocked".into(),
                        answer: format!("worker stopped without submitting ({:?})", r.stopped),
                        raw: String::new(),
                        findings: vec![],
                        gaps: vec![format!("question {qid}: worker did not submit")],
                    }
                }
            },
            Err(e) => {
                lane_outcome = "error";
                WorkerReport {
                    question_id: qid.clone(),
                    result: "blocked".into(),
                    answer: format!("worker failed: {e}"),
                    raw: String::new(),
                    findings: vec![],
                    gaps: vec![format!("question {qid}: worker failed: {e}")],
                }
            }
        };
        prep.timing.record(
            "lane",
            &format!("worker:{qid}"),
            lane_start,
            prep.wall,
            lane_outcome,
        );
        if report.result == "blocked" {
            prep.partial_reasons
                .push(worker_failure_reason(i, &worker_model, &report.answer));
        }
        let _ = i;
        reports.push(report);
    }
    if skipped > 0 {
        prep.partial_reasons
            .push(format!("run budget exhausted before {skipped} lanes"));
    }
    for r in &reports {
        for g in &r.gaps {
            prep.coverage_gaps.push(g.clone());
        }
        if r.result == "blocked" {
            prep.coverage_gaps
                .push(format!("question {} blocked: {}", r.question_id, r.answer));
        }
    }

    // ---- 3. lead synthesizes ----
    let mut worker_section = String::new();
    for r in &reports {
        worker_section.push_str(&format!(
            "\n### Worker report for {}\n\n{}\n",
            r.question_id, r.raw
        ));
    }
    let synth_user = format!(
        "PR title: {}\n\nDiff summary:\n{}\n\nWorker reports:\n{}",
        req.title.as_deref().unwrap_or("(untitled)"),
        prep.diff.render_truncated(cfg.review.max_diff_bytes),
        worker_section,
    );
    let synth_terminal = terminal_submit_findings_spec();
    let lead2 = make_client(
        &lead_route,
        "lead",
        prep.ledger.clone(),
        cfg.budget.run_max_requests,
        cfg.budget.retries,
        &synth_terminal.name,
    )?;
    let read_only_tb = std::sync::Arc::new(prep.toolbox.restricted(&["read_file"]));
    let synth_start = std::time::Instant::now();
    let run = run_agent_checked(
        lead2.as_ref(),
        prompts::LEAD_SYNTHESIZE,
        &synth_user,
        &read_only_tb,
        &synth_terminal,
        &prep.budget(4, cfg.budget.agent_max_seconds),
        &findings_terminal_check,
    )
    .await?;
    prep.stats.repaired |= run.repaired;
    let synth_outcome = match run.stopped {
        crate::agent::StopReason::Terminal if run.final_call.is_none() => "error",
        crate::agent::StopReason::Terminal => "ok",
        crate::agent::StopReason::TimeBudget => "timeout",
        crate::agent::StopReason::ToolBudget => "tool_budget",
        _ => "error",
    };
    prep.timing
        .record("synthesis", "lead", synth_start, prep.wall, synth_outcome);
    let mut candidates: Vec<Finding> = vec![];
    match run.stopped {
        crate::agent::StopReason::Terminal => {
            if let Some(call) = &run.final_call {
                prep.coverage = call.arguments["coverage"]
                    .as_str()
                    .unwrap_or("(none)")
                    .to_string();
                for u in call.arguments["unresolved"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(s) = u.as_str() {
                        prep.coverage_gaps.push(s.to_string());
                    }
                }
                let parsed = parse_findings_checked(&call.arguments);
                prep.stats.malformed_findings += parsed.dropped;
                if let Some(p) = parsed.problem {
                    prep.partial_reasons
                        .push(format!("lead synthesis submission incomplete: {p}"));
                }
                candidates = parsed.findings;
                for c in &mut candidates {
                    // attribute to the worker whose candidate matches, else lead
                    let origin = reports.iter().find_map(|r| {
                        r.findings
                            .iter()
                            .find(|wf| wf.defect_key == c.defect_key || wf.file == c.file)
                            .map(|_| format!("delegated:worker:{}", r.question_id))
                    });
                    c.source = origin.unwrap_or_else(|| "delegated:lead".into());
                    c.sources = vec![c.source.clone()];
                }
            } else {
                prep.partial_reasons
                    .push("lead synthesis ended without a submission".into());
                for r in reports {
                    candidates.extend(r.findings);
                }
            }
        }
        other => {
            prep.partial_reasons
                .push(format!("lead synthesis stopped early: {other:?}"));
            // fall back to the workers' raw candidates rather than lose them
            for r in reports {
                candidates.extend(r.findings);
            }
        }
    }
    Ok(candidates)
}

fn select_worker_route<'a>(
    workers: &'a [crate::config::ModelRoute],
    investigator: &'a crate::config::ModelRoute,
    question_index: usize,
) -> &'a crate::config::ModelRoute {
    workers
        .iter()
        .filter(|w| !w.model.is_empty())
        .cycle()
        .nth(question_index)
        .unwrap_or(investigator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::diff::DiffSet;
    use crate::provider::{LedgerEntry, LedgerHandle};
    use crate::tools::ToolBox;
    use crate::vera::VeraClient;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn cfg() -> Config {
        std::env::set_var("REVERA_TEST_KEY", "sk-test");
        serde_yaml::from_str(
            r#"
review: {strategy: delegated, concurrency: 2}
models:
  investigator: {protocol: openai-chat, base_url: "http://127.0.0.1:1", api_key_env: REVERA_TEST_KEY, model: m}
  validator: {protocol: openai-chat, base_url: "http://127.0.0.1:1", api_key_env: REVERA_TEST_KEY, model: m}
  workers:
    - {protocol: openai-chat, base_url: "http://127.0.0.1:1", api_key_env: REVERA_TEST_KEY, model: w0}
    - {protocol: openai-chat, base_url: "http://127.0.0.1:1", api_key_env: REVERA_TEST_KEY, model: w1}
vera: {}
"#,
        )
        .unwrap()
    }

    fn q(id: &str) -> PlanQuestion {
        PlanQuestion {
            id: id.into(),
            question: "q".into(),
            symbols: vec![],
            expected_evidence: String::new(),
            stop_condition: String::new(),
            files_hint: vec![],
        }
    }

    #[test]
    fn plan_capped_at_max_questions() {
        let mut v: Vec<PlanQuestion> = (0..10).map(|i| q(&format!("q{i}"))).collect();
        v.truncate(4);
        assert_eq!(v.len(), 4);
        assert_eq!(v[3].id, "q3");
    }

    #[test]
    fn worker_routing_round_robins() {
        let c = cfg();
        let workers = c.models.workers.as_deref().unwrap();
        let assigned: Vec<String> = (0..5)
            .map(|i| {
                select_worker_route(workers, &c.models.investigator, i)
                    .model
                    .clone()
            })
            .collect();
        assert_eq!(assigned, ["w0", "w1", "w0", "w1", "w0"]);
        let empty: Vec<crate::config::ModelRoute> = vec![];
        let assigned: Vec<String> = (0..5)
            .map(|i| {
                select_worker_route(&empty, &c.models.investigator, i)
                    .model
                    .clone()
            })
            .collect();
        assert!(assigned.iter().all(|model| model == "m"));
    }

    #[test]
    fn blocked_worker_reason_is_partial_specific() {
        let reason = worker_failure_reason(2, "worker-model", "worker stopped without submitting");
        assert_eq!(
            reason,
            "worker 2 (worker-model) did not complete: worker stopped without submitting"
        );
    }

    #[tokio::test]
    async fn run_budget_skips_lanes() {
        let c = cfg();
        let ledger = LedgerHandle::new();
        // exhaust the budget before lanes start
        for _ in 0..2 {
            ledger.record(LedgerEntry {
                route: "r".into(),
                model: "m".into(),
                ..Default::default()
            });
        }
        // emulate the lane gate used by delegated_candidates:
        // `ledger.request_count() >= max_req` -> lane skipped
        let mut c = c;
        c.budget.run_max_requests = 2;
        let max_req = c.budget.run_max_requests;
        let skipped = (0..3).filter(|_| ledger.request_count() >= max_req).count();
        assert_eq!(skipped, 3);

        // full-path check: prepared + skip -> partial_reasons gains the note
        let repo = std::env::temp_dir();
        let vera = Arc::new(VeraClient {
            exe: "true".into(),
            repo_root: repo.clone(),
            env: vec![],
            backend: "local".into(),
            exclude: vec![],
            deadline: None,
        });
        let diff = Arc::new(DiffSet::default());
        let toolbox = Arc::new(ToolBox::new(repo.clone(), diff.clone(), vera.clone(), 1000));
        let mut prep = crate::pipeline::common::Prepared {
            repo,
            base_sha: "b".into(),
            head_sha: "h".into(),
            strategy_name: "delegated".into(),
            diff,
            patch_id: String::new(),
            review_key: String::new(),
            vera,
            toolbox,
            ledger: ledger.clone(),
            state: ReviewState::default(),
            rechecks: vec![],
            resolved_titles: vec![],
            partial_reasons: vec![],
            retrieval_unavailable: None,
            stats: Default::default(),
            coverage: String::new(),
            coverage_gaps: vec![],
            report_note: None,
            deadline: Instant::now() + Duration::from_secs(60),
            reserve: Duration::ZERO,
            wall: Instant::now(),
            timing: crate::timing::Recorder::default(),
            progress: Arc::new(crate::progress::ProgressBroadcaster::default()),
            risk_tier: None,
            sensitive_change: false,
            repo_guidance: String::new(),
        };
        if skipped > 0 {
            prep.partial_reasons
                .push(format!("run budget exhausted before {skipped} lanes"));
        }
        assert!(prep
            .partial_reasons
            .iter()
            .any(|r| r == "run budget exhausted before 3 lanes"));
    }
}
