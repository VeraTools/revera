use super::common::{
    PrepareOut, ReviewRequest, clamp_budget, findings_terminal_check, finish, investigator_user,
    parse_candidate_findings, parse_findings_checked, parse_not_checked, prepare,
};
use super::make_client;
use crate::agent::{run_agent, run_agent_checked};
use crate::config::Config;
use crate::findings::Finding;
use crate::prompts;
use crate::report::RunReport;
use crate::state::ReviewState;
use crate::tools::{submit_findings_spec, submit_worker_result_spec, terminal_submit_plan_spec};
use anyhow::Result;
use futures::stream::StreamExt;
use serde::Deserialize;

/// Tools a delegated worker may call: read-only inspection plus both
/// retrieval families, so a lexical-only run (Vera disabled or degraded)
/// can still discover untouched files.
pub const WORKER_TOOLS: &[&str] = &[
    "read_file",
    "read_skill",
    "list_changed_files",
    "diff_context",
    "grep_repo",
    "find_files",
    "vera_search",
    "vera_references",
    "vera_grep",
    "vera_overview",
    "web_search",
    "web_read",
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
        Some(super::cache_key(cfg, "delegated", "lead", None)),
        prep.ledger.clone(),
        cfg.budget.run_max_requests,
        cfg.budget.retries,
        &plan_terminal.name,
    )?;
    let mut user = investigator_user(req, &prep.diff, cfg.review.max_diff_bytes)
        + &prep.guidance.prompt_block()
        + &prep.tracked_block(cfg.review.validate);
    if !prep.rechecks.is_empty() {
        user.push_str("\n\nPrior findings under recheck:\n");
        for r in &prep.rechecks {
            user.push_str(&format!("- {}:{} {}\n", r.file, r.start_line, r.title));
        }
    }
    let system = prompts::LEAD_PLAN
        .replace("{max_questions}", &cfg.delegated.max_questions.to_string())
        + &cfg.playbook.investigator_block();
    let plan_start = std::time::Instant::now();
    let plan_tools = prep.toolbox.without_web();
    let run = run_agent(
        lead.as_ref(),
        &system,
        &user,
        &plan_tools,
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
    let worker_terminal = submit_worker_result_spec(&cfg.playbook);
    let worker_tb = std::sync::Arc::new(prep.toolbox.restricted(WORKER_TOOLS));
    let investigation_deadline = prep.investigation_deadline();
    // Create clients in lane order so scripted conversations pop deterministically.
    let mut worker_clients = Vec::new();
    for (i, _q) in questions.iter().enumerate() {
        let route = select_worker_route(worker_routes, &cfg.models.investigator, i);
        let c = match make_client(
            route,
            "workers",
            Some(super::cache_key(cfg, "delegated", "workers", None)),
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
    let worker_context = prep.guidance.prompt_block() + &prep.tracked_block(cfg.review.validate);
    let lane_futs = questions.iter().enumerate().map(|(i, q)| {
        let client = worker_clients[i].clone();
        let worker_model = select_worker_route(worker_routes, &cfg.models.investigator, i)
            .model
            .clone();
        let tb = worker_tb.clone();
        let terminal = worker_terminal.clone();
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
        ) + &worker_context;
        let worker_system = format!("{}\n\n{}", prompts::WORKER, cfg.playbook.classes_block());
        async move {
            let lane_start = std::time::Instant::now();
            if ledger.request_count() >= max_req || lane_start >= investigation_deadline {
                timing.record(
                    "lane",
                    &format!("worker:{qid}"),
                    lane_start,
                    wall,
                    "skipped",
                );
                return (i, qid, worker_model, None, lane_start);
            }
            let budget = clamp_budget(
                cfg.delegated.worker_max_tool_calls,
                cfg.delegated.worker_max_seconds,
                investigation_deadline,
            );
            let run = tokio::time::timeout_at(investigation_deadline.into(), run_agent(
                client.as_ref(),
                &worker_system,
                &qtext,
                &tb,
                &terminal,
                &budget,
            ))
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
            Ok(Ok(r)) => match r.final_call {
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
            Ok(Err(e)) => {
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
            Err(_) => {
                lane_outcome = "timeout";
                WorkerReport {
                    question_id: qid.clone(),
                    result: "blocked".into(),
                    answer: "worker reached investigation deadline".into(),
                    raw: String::new(),
                    findings: vec![],
                    gaps: vec![format!("question {qid}: investigation deadline reached")],
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
        prep.partial_reasons.push(format!(
            "investigation deadline or run budget exhausted before {skipped} lanes"
        ));
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
    if std::time::Instant::now() >= investigation_deadline {
        prep.timing.record(
            "synthesis",
            "lead",
            std::time::Instant::now(),
            prep.wall,
            "skipped",
        );
        prep.partial_reasons
            .push("investigation deadline reached before lead synthesis".into());
        return Ok(reports.into_iter().flat_map(|r| r.findings).collect());
    }
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
    ) + &prep.guidance.prompt_block()
        + &prep.tracked_block(cfg.review.validate);
    let synth_system = format!(
        "{}\n\n{}",
        prompts::LEAD_SYNTHESIZE,
        cfg.playbook.classes_block()
    );
    let synth_terminal = submit_findings_spec(&cfg.playbook);
    let lead2 = make_client(
        &lead_route,
        "lead",
        Some(super::cache_key(cfg, "delegated", "lead", None)),
        prep.ledger.clone(),
        cfg.budget.run_max_requests,
        cfg.budget.retries,
        &synth_terminal.name,
    )?;
    let read_only_tb = std::sync::Arc::new(prep.toolbox.restricted(&["read_file", "read_skill"]));
    let synth_start = std::time::Instant::now();
    let run = tokio::time::timeout_at(
        investigation_deadline.into(),
        run_agent_checked(
            lead2.as_ref(),
            &synth_system,
            &synth_user,
            &read_only_tb,
            &synth_terminal,
            &prep.budget(4, cfg.budget.agent_max_seconds),
            &findings_terminal_check,
        ),
    )
    .await;
    let run = match run {
        Ok(run) => run?,
        Err(_) => {
            prep.timing
                .record("synthesis", "lead", synth_start, prep.wall, "timeout");
            prep.partial_reasons
                .push("lead synthesis reached investigation deadline".into());
            return Ok(reports.into_iter().flat_map(|r| r.findings).collect());
        }
    };
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
                let unchecked = parse_not_checked(&serde_json::json!({
                    "not_checked": call.arguments.get("not_checked")
                        .or_else(|| call.arguments.get("unresolved"))
                }));
                if !unchecked.is_empty() {
                    prep.partial_reasons.push(format!(
                        "lead synthesis reported {} unchecked area(s)",
                        unchecked.len()
                    ));
                    prep.coverage_gaps.extend(unchecked);
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
        serde_saphyr::from_str(
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
        let mut c = cfg();
        let ledger = LedgerHandle::new();
        // The planner's one request exhausts the remaining budget before workers start.
        ledger.record(LedgerEntry {
            route: "r".into(),
            model: "m".into(),
            ..Default::default()
        });
        c.budget.run_max_requests = 2;
        let data = tempfile::tempdir().unwrap();
        let repo = data.path().to_path_buf();
        let script = serde_json::json!({"roles": {"lead": [
            [{"tool_calls": [{"name": "submit_plan", "arguments": {"questions":
                (0..3).map(|i| serde_json::json!({"id": format!("q{i}"), "question": "Check behavior"})).collect::<Vec<_>>()
            }}]}],
            [{"tool_calls": [{"name": "submit_findings", "arguments": {"findings": [], "coverage": "Workers skipped"}}]}]
        ]}});
        crate::fsutil::write_atomic_in(
            data.path(),
            "script.json",
            &serde_json::to_vec(&script).unwrap(),
        )
        .unwrap();
        c.models.investigator.protocol = crate::config::Protocol::Scripted;
        c.models.investigator.script = Some(repo.join("script.json"));
        c.models.workers = None;
        let req = ReviewRequest {
            repo: repo.clone(),
            base: "b".into(),
            head: Some("h".into()),
            title: None,
            body: String::new(),
            strategy_override: None,
            force: false,
            repository_public: false,
        };
        let vera = Arc::new(VeraClient {
            exe: "true".into(),
            repo_root: repo.clone(),
            env: vec![],
            backend: "local".into(),
            exclude: vec![],
            home: repo.clone(),
            rerank: None,
            embedding_pairs: vec![],
            index_key: String::new(),
            rerank_fallbacks: Default::default(),
            semantic_fallbacks: Default::default(),
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
            guidance: Default::default(),
            coverage: String::new(),
            coverage_gaps: vec![],
            report_note: None,
            deadline: Instant::now() + Duration::from_secs(60),
            reserve: Duration::ZERO,
            wall: Instant::now(),
            timing: crate::timing::Recorder::default(),
            intent: String::new(),
        };
        let findings = delegated_candidates(&c, &req, &mut prep).await.unwrap();
        assert!(findings.is_empty());
        assert_eq!(
            prep.partial_reasons,
            ["investigation deadline or run budget exhausted before 3 lanes"]
        );
        let timing = prep.timing.finish(prep.wall);
        let lanes: Vec<_> = timing.phases.iter().filter(|p| p.phase == "lane").collect();
        assert_eq!(lanes.len(), 3);
        assert!(lanes.iter().all(|p| p.outcome == "skipped"));
        assert!(
            ledger
                .0
                .lock()
                .unwrap()
                .entries
                .iter()
                .all(|e| e.role != "workers")
        );
    }
}
