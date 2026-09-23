//! `revera review --preview`: what a review would look at and how it would
//! be staffed, computed from the diff and configuration alone. No model,
//! Vera or GitHub call is made and no state is written.

use crate::config::{Config, Strategy};
use crate::diff::{parse_unified, DiffLineKind, FileStatus};
use crate::pipeline::common::ReviewRequest;
use crate::triage::RiskTier;
use anyhow::Result;
use std::fmt::Write;

pub async fn preview(cfg: &Config, req: &ReviewRequest) -> Result<String> {
    let repo = crate::git::repo_root(&req.repo);
    let loaded = crate::pipeline::load::load_diff(&repo, req).await?;
    let t = crate::triage::triage(parse_unified(&loaded.raw_diff), &cfg.triage)?;
    let strategy = req.strategy_override.unwrap_or(cfg.review.strategy);
    let tier = (cfg.triage.risk_tiers && req.strategy_override.is_none()).then_some(t.tier);

    let mut s = String::new();
    writeln!(s, "revera preview (no model, Vera or GitHub calls)")?;
    writeln!(s, "base {} head {}", loaded.base_sha, loaded.head_sha)?;
    writeln!(s, "\nreviewed files ({}):", t.diff.files.len())?;
    for f in &t.diff.files {
        let (add, del) = f
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .fold((0, 0), |(a, d), l| match l.kind {
                DiffLineKind::Add => (a + 1, d),
                DiffLineKind::Del => (a, d + 1),
                DiffLineKind::Ctx => (a, d),
            });
        let status = match f.status {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
        };
        let path = if f.status == FileStatus::Deleted {
            &f.old_path
        } else {
            &f.new_path
        };
        writeln!(s, "  {status} {path} (+{add} -{del})")?;
    }
    if !t.skipped.is_empty() {
        writeln!(s, "\nskipped before review ({}):", t.skipped.len())?;
        for k in &t.skipped {
            writeln!(s, "  {}: {}", k.path, k.reason.as_str())?;
        }
    }
    let omitted = t.diff.omitted_by_budget(cfg.review.max_diff_bytes);
    if !omitted.is_empty() {
        writeln!(
            s,
            "\nnot in the reviewers' prompt (review.max_diff_bytes = {}):",
            cfg.review.max_diff_bytes
        )?;
        for p in &omitted {
            writeln!(s, "  {p}")?;
        }
    }

    writeln!(s)?;
    let strategy_name = format!("{strategy:?}").to_lowercase();
    let origin = if req.strategy_override.is_some() {
        "--strategy"
    } else {
        "config"
    };
    writeln!(s, "strategy: {strategy_name} ({origin})")?;
    match tier {
        Some(tier) => writeln!(
            s,
            "risk tier: {} ({} changed lines, {} files{})",
            tier.as_str(),
            t.changed_lines,
            t.diff.files.len(),
            if t.sensitive.is_empty() {
                String::new()
            } else {
                format!("; sensitive: {}", t.sensitive.join(", "))
            }
        )?,
        None => writeln!(s, "risk tier: not applied")?,
    }
    let trivial_swarm = tier == Some(RiskTier::Trivial) && strategy != Strategy::Baseline;
    if trivial_swarm {
        writeln!(s, "reviewers: single investigator (trivial tier)")?;
    } else if strategy == Strategy::Panel {
        let lanes = cfg
            .panel
            .effective_lanes(&cfg.models.investigator, cfg.models.scouts.as_deref())?;
        let cap = if tier == Some(RiskTier::Lite) {
            cfg.triage.lite_max_lanes
        } else {
            usize::MAX
        };
        let names: Vec<String> = lanes
            .iter()
            .take(cap)
            .map(|l| format!("{} ({})", l.name, l.route.model))
            .collect();
        writeln!(s, "reviewers: {}", names.join(", "))?;
        if lanes.len() > cap {
            writeln!(s, "  lite tier: {} lane(s) not run", lanes.len() - cap)?;
        }
        if let Some(rc) = &cfg.panel.lens_router {
            if t.sensitive.is_empty() {
                writeln!(
                    s,
                    "lens router: would ask {} ({}) which lanes apply before they run",
                    rc.model, rc.base_url
                )?;
            } else {
                writeln!(s, "lens router: bypassed (security-sensitive change)")?;
            }
        }
    } else {
        writeln!(s, "reviewers: {strategy_name} strategy as configured")?;
    }
    writeln!(
        s,
        "validation: {}",
        if cfg.review.validate {
            "fresh-context validator per candidate"
        } else {
            "OFF (review.validate = false)"
        }
    )?;
    Ok(s)
}
