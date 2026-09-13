use super::common::{
    finish, investigator_user, parse_findings, prepare, PrepareOut, ReviewRequest,
};
use super::make_client;
use crate::agent::run_agent;
use crate::config::{Config, ModelRoute};
use crate::findings::Finding;
use crate::prompts;
use crate::report::RunReport;
use crate::state::ReviewState;
use crate::tools::terminal_submit_findings_spec;
use anyhow::{bail, Result};
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
    if scouts.len() != focuses.len() {
        bail!(
            "panel: {} focuses but {} scouts (must be equal, or use a single scout)",
            focuses.len(),
            scouts.len()
        );
    }
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

    let scouts: Vec<ModelRoute> = cfg
        .models
        .scouts
        .as_ref()
        .map(|s| s.iter().map(|sc| sc.route.clone()).collect())
        .unwrap_or_default();
    let lanes = scout_lanes(&scouts, &cfg.panel.focuses, &cfg.models.investigator)?;
    let n_scouts = lanes.len();

    let terminal = terminal_submit_findings_spec();
    let user = investigator_user(req, &prep.diff, cfg.review.max_diff_bytes);
    let max_req = cfg.budget.run_max_requests;
    let ledger0 = prep.ledger.clone();
    let tb0 = prep.toolbox.clone();
    let lane_budget = prep.budget(cfg.panel.scout_max_tool_calls, cfg.budget.agent_max_seconds);
    let lane_futs = lanes.iter().map(|(focus, route)| {
        let ledger = ledger0.clone();
        let tb = tb0.clone();
        let user = user.clone();
        let terminal = terminal.clone();
        let focus = focus.clone();
        let route = route.clone();
        let budget = lane_budget.clone();
        let retries = cfg.budget.retries;
        async move {
            if ledger.request_count() >= max_req {
                return (focus, None);
            }
            let client = match make_client(&route, &focus, ledger, max_req, retries, &terminal.name)
            {
                Ok(c) => c,
                Err(e) => return (focus, Some(Err(e))),
            };
            let addendum = focus_addendum(&focus).unwrap_or_default();
            let system = format!(
                "{}\n\n# Focus: {}\n\n{}",
                prompts::INVESTIGATOR,
                focus,
                addendum
            );
            let run = run_agent(client.as_ref(), &system, &user, &tb, &terminal, &budget).await;
            (focus, Some(run))
        }
    });
    let mut stream =
        futures::stream::iter(lane_futs).buffer_unordered(cfg.review.concurrency.max(1));

    let mut raw: Vec<Finding> = vec![];
    let mut skipped = 0usize;
    while let Some((focus, run)) = stream.next().await {
        let Some(run) = run else {
            skipped += 1;
            continue;
        };
        match run {
            Ok(r) => match r.stopped {
                crate::agent::StopReason::Terminal => {
                    if let Some(call) = &r.final_call {
                        let mut fs = parse_findings(&call.arguments);
                        for f in &mut fs {
                            f.source = format!("panel:{focus}");
                            f.sources = vec![f.source.clone()];
                        }
                        raw.extend(fs);
                    }
                }
                other => {
                    prep.partial_reasons
                        .push(format!("scout {focus} stopped early: {other:?}"));
                }
            },
            Err(e) => {
                prep.partial_reasons
                    .push(format!("scout {focus} failed: {e}"));
            }
        }
    }
    if skipped > 0 {
        prep.partial_reasons
            .push(format!("run budget exhausted before {skipped} lanes"));
    }
    let n_raw = raw.len();
    let n_unique = crate::findings::collapse(raw.clone()).len();
    prep.report_note = Some(format!(
        "panel: {n_scouts} scouts, {n_raw} raw candidates → {n_unique} unique"
    ));
    prep.coverage = format!(
        "{n_scouts} scout lanes ({}); unioned and merged",
        cfg.panel.focuses.join(", ")
    );

    let labels: Vec<String> = lanes.iter().map(|(f, _)| format!("scout:{f}")).collect();
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
