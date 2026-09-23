use super::common::{
    findings_terminal_check, finish, investigator_user, parse_findings_checked, prepare,
    PrepareOut, ReviewRequest,
};
use super::lens_router::select_lanes;
use super::make_client;
use crate::agent::run_agent_checked;
use crate::config::{Config, ModelRoute};
use crate::findings::Finding;
use crate::prompts;
use crate::report::RunReport;
use crate::state::ReviewState;
use crate::tools::terminal_submit_findings_spec;
use crate::triage::RiskTier;
use anyhow::Result;
use futures::stream::StreamExt;

/// Parse `## <focus>` sections out of prompts/scout_focus.md.
pub fn focus_addendum(focus: &str) -> Option<String> {
    let mut in_section = false;
    let mut out = String::new();
    for line in prompts::SCOUT_FOCUS.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            in_section = name.trim() == focus;
            continue;
        }
        if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    let out = out.trim();
    if out.is_empty() {
        None
    } else {
        Some(out.to_string())
    }
}

/// Resolve the system prompt addendum for an effective lane.
pub fn lane_addendum(lane: &crate::config::EffectiveLane) -> String {
    if let Some(prompt) = &lane.custom_prompt {
        prompt.clone()
    } else if let Some(focus) = &lane.focus {
        focus_addendum(focus).unwrap_or_default()
    } else {
        focus_addendum(&lane.name).unwrap_or_default()
    }
}

/// Build (focus, route) lane pairs: one scout repeated per focus when only one
/// scout route is configured; otherwise focuses.len() must == scouts.len().
pub fn scout_lanes(
    scouts: &[ModelRoute],
    focuses: &[String],
    default_route: &ModelRoute,
) -> Result<Vec<(String, ModelRoute)>> {
    if scouts.is_empty() {
        return Ok(focuses
            .iter()
            .map(|f| (f.clone(), default_route.clone()))
            .collect());
    }
    if scouts.len() == 1 {
        return Ok(focuses
            .iter()
            .map(|f| (f.clone(), scouts[0].clone()))
            .collect());
    }
    crate::config::check_lane_cardinality(focuses.len(), scouts.len())?;
    Ok(focuses
        .iter()
        .cloned()
        .zip(scouts.iter().cloned())
        .collect())
}

/// Panel strategy: concurrent scout lanes (investigator prompt + focus
/// addendum), union of all findings, collapse, shared validate+finish.
pub async fn run(cfg: &Config, req: &ReviewRequest) -> Result<(RunReport, ReviewState)> {
    let mut prep = match prepare(cfg, req, "panel").await? {
        PrepareOut::Ready(p) => p,
        PrepareOut::ShortCircuit(rep, st) => return Ok((*rep, st)),
    };
    if prep.risk_tier == Some(RiskTier::Trivial) {
        return super::baseline::run_trivial(cfg, req, prep, "panel").await;
    }

    let mut lanes = cfg
        .panel
        .effective_lanes(&cfg.models.investigator, cfg.models.scouts.as_deref())?;
    // security-sensitive changes always get every lane
    let mut router_note = String::new();
    if let (Some(rc), false) = (&cfg.panel.lens_router, prep.sensitive_change) {
        let t0 = std::time::Instant::now();
        let d = select_lanes(
            rc,
            &prep.diff.render_truncated(rc.max_state_bytes),
            &lanes,
            prep.deadline,
        )
        .await;
        let outcome = if d.keep.iter().all(|k| *k) {
            "ok"
        } else {
            "narrowed"
        };
        prep.timing
            .record("lens_router", "", t0, prep.wall, outcome);
        let mut keep = d.keep.into_iter();
        lanes.retain(|_| keep.next().unwrap_or(true));
        if let Some(n) = d.note {
            router_note = format!("; {n}");
        }
    }
    let tier_note =
        if prep.risk_tier == Some(RiskTier::Lite) && lanes.len() > cfg.triage.lite_max_lanes {
            let dropped = lanes.len() - cfg.triage.lite_max_lanes;
            lanes.truncate(cfg.triage.lite_max_lanes);
            format!("; risk tier lite: {dropped} lane(s) not run")
        } else {
            String::new()
        };
    let n_scouts = lanes.len();

    let terminal = terminal_submit_findings_spec();
    let user = investigator_user(
        req,
        &prep.diff,
        cfg.review.max_diff_bytes,
        &cfg.review.path_instructions,
        &cfg.review.knowledge_base,
        &prep.repo_guidance,
    );
    let max_req = cfg.budget.run_max_requests;
    let ledger0 = prep.ledger.clone();
    let tb0 = prep.toolbox.clone();
    let timing0 = prep.timing.clone();
    let wall = prep.wall;
    let progress0 = prep.progress.clone();
    let lane_budget = prep.budget(cfg.panel.scout_max_tool_calls, cfg.budget.agent_max_seconds);
    let lane_futs = lanes.iter().map(|lane| {
        let ledger = ledger0.clone();
        let tb = tb0.clone();
        let user = user.clone();
        let terminal = terminal.clone();
        let lane_name = lane.name.clone();
        let route = lane.route.clone();
        let budget = lane_budget.clone();
        let retries = cfg.budget.retries;
        let timing = timing0.clone();
        let progress = progress0.clone();
        let addendum = lane_addendum(lane);
        async move {
            let lane_start = std::time::Instant::now();
            progress.emit(crate::progress::ProgressEvent::ScoutDispatched {
                lane: lane_name.clone(),
                model: route.model.clone(),
            });
            if ledger.request_count() >= max_req {
                timing.record(
                    "lane",
                    &format!("panel:{lane_name}"),
                    lane_start,
                    wall,
                    "skipped",
                );
                return (lane_name, None, lane_start);
            }
            let client =
                match make_client(&route, &lane_name, ledger, max_req, retries, &terminal.name) {
                    Ok(c) => c,
                    Err(e) => return (lane_name, Some(Err(e)), lane_start),
                };
            let system = if addendum.is_empty() {
                format!("{}\n\n# Focus: {}", prompts::INVESTIGATOR, lane_name)
            } else {
                format!(
                    "{}\n\n# Focus: {}\n\n{}",
                    prompts::INVESTIGATOR,
                    lane_name,
                    addendum
                )
            };
            let run = run_agent_checked(
                client.as_ref(),
                &system,
                &user,
                &tb,
                &terminal,
                &budget,
                &findings_terminal_check,
            )
            .await;
            (lane_name, Some(run), lane_start)
        }
    });
    let mut stream =
        futures::stream::iter(lane_futs).buffer_unordered(cfg.review.concurrency.max(1));

    let mut raw: Vec<Finding> = vec![];
    let mut skipped = 0usize;
    while let Some((lane_name, run, lane_start)) = stream.next().await {
        let Some(run) = run else {
            skipped += 1;
            continue;
        };
        let mut candidates_count = 0usize;
        let outcome = match run {
            Ok(r) => match r.stopped {
                crate::agent::StopReason::Terminal => {
                    prep.stats.repaired |= r.repaired;
                    let Some(call) = &r.final_call else {
                        prep.partial_reasons
                            .push(format!("scout {lane_name} ended without a submission"));
                        prep.timing.record(
                            "lane",
                            &format!("panel:{lane_name}"),
                            lane_start,
                            prep.wall,
                            "error",
                        );
                        continue;
                    };
                    let parsed = parse_findings_checked(&call.arguments);
                    prep.stats.malformed_findings += parsed.dropped;
                    let mut fs = parsed.findings;
                    let n = fs.len();
                    candidates_count = n;
                    for f in &mut fs {
                        f.source = format!("panel:{lane_name}");
                        f.sources = vec![f.source.clone()];
                    }
                    raw.extend(fs);
                    match parsed.problem {
                        Some(p) => {
                            prep.partial_reasons
                                .push(format!("scout {lane_name} submission incomplete: {p}"));
                            "ok:malformed".to_string()
                        }
                        None if n > 0 => "ok:candidates".to_string(),
                        None => "ok".to_string(),
                    }
                }
                crate::agent::StopReason::TimeBudget => {
                    prep.partial_reasons
                        .push(format!("scout {lane_name} stopped early: TimeBudget"));
                    "timeout".into()
                }
                crate::agent::StopReason::ToolBudget => {
                    prep.partial_reasons
                        .push(format!("scout {lane_name} stopped early: ToolBudget"));
                    "tool_budget".into()
                }
                other => {
                    prep.partial_reasons
                        .push(format!("scout {lane_name} stopped early: {other:?}"));
                    "error".into()
                }
            },
            Err(e) => {
                prep.partial_reasons
                    .push(format!("scout {lane_name} failed: {e}"));
                format!("error:{}", crate::text::excerpt_bytes(&e.to_string(), 60))
            }
        };
        prep.timing.record(
            "lane",
            &format!("panel:{lane_name}"),
            lane_start,
            prep.wall,
            &outcome,
        );
        prep.progress
            .emit(crate::progress::ProgressEvent::LaneCompleted {
                lane: lane_name.clone(),
                candidates: candidates_count,
                status: outcome.clone(),
            });
    }
    if skipped > 0 {
        prep.partial_reasons
            .push(format!("run budget exhausted before {skipped} lanes"));
    }
    let n_raw = raw.len();
    let collapsed = crate::findings::collapse(raw.clone());
    let n_unique = collapsed.len();
    let n_consensus = collapsed.iter().filter(|f| f.sources.len() > 1).count();
    prep.report_note = Some(format!(
        "panel: {n_scouts} scouts, {n_raw} raw candidates → {n_unique} unique ({n_consensus} multi-lens consensus){router_note}{tier_note}"
    ));
    let lane_names: Vec<String> = lanes.iter().map(|l| l.name.clone()).collect();
    prep.coverage = format!(
        "{n_scouts} scout lanes ({}); unioned and merged",
        lane_names.join(", ")
    );

    let labels: Vec<String> = lanes.iter().map(|l| format!("scout:{}", l.name)).collect();
    finish(cfg, prep, raw, &labels).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelRoute, Protocol};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn route(model: &str) -> ModelRoute {
        ModelRoute {
            protocol: Protocol::OpenaiChat,
            base_url: Some("http://127.0.0.1:1".into()),
            api_key_env: Some("REVERA_TEST_KEY".into()),
            model: model.into(),
            max_output_tokens: 100,
            temperature: 0.0,
            extra_headers: HashMap::new(),
            session_header: None,
            script: None,
            reasoning: crate::config::Reasoning::default(),
        }
    }

    #[test]
    fn focus_addendum_parses_sections() {
        let g = focus_addendum("general").expect("general section");
        assert!(!g.is_empty());
        let c = focus_addendum("cross-file").expect("cross-file section");
        assert_ne!(g, c);
        let arch = focus_addendum("architecture").expect("architecture section");
        assert!(arch.contains("architectural layers"));
        let reg = focus_addendum("regression").expect("regression section");
        assert!(reg.contains("backward compatibility"));
        let tq = focus_addendum("test-quality").expect("test-quality section");
        assert!(tq.contains("Vacuous assertions"));
        assert!(focus_addendum("nonexistent-focus").is_none());
    }

    #[test]
    fn scout_lanes_rules() {
        let focuses = vec!["general".to_string(), "cross-file".to_string()];
        // empty -> repeat default per focus
        let lanes = scout_lanes(&[], &focuses, &route("inv")).unwrap();
        assert_eq!(lanes.len(), 2);
        assert!(lanes.iter().all(|(_, r)| r.model == "inv"));
        // single scout -> repeated
        let lanes = scout_lanes(&[route("s0")], &focuses, &route("inv")).unwrap();
        assert_eq!(lanes.len(), 2);
        assert!(lanes.iter().all(|(_, r)| r.model == "s0"));
        // equal counts -> zipped
        let lanes = scout_lanes(&[route("a"), route("b")], &focuses, &route("inv")).unwrap();
        assert_eq!(lanes[0].0, "general");
        assert_eq!(lanes[0].1.model, "a");
        assert_eq!(lanes[1].1.model, "b");
        // mismatch -> error
        assert!(scout_lanes(
            &[route("a"), route("b"), route("c")],
            &focuses,
            &route("inv")
        )
        .is_err());
    }

    #[test]
    fn no_std_pathbuf() {
        let _ = PathBuf::new();
    }
}
