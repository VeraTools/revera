//! Optional, removal-only comparison after independent validation. Any failure
//! leaves the entire accepted set unchanged.
use super::common::Prepared;
use crate::config::Config;
use crate::findings::{Evidence, Finding};
use crate::provider::{ChatMessage, ToolSpec};
use crate::report::CarriedFinding;
use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub async fn run(cfg: &Config, prep: &Prepared, findings: &mut [Finding]) -> usize {
    let carried = crate::report::carried_findings(&prep.state, findings);
    let items = items(findings, &carried);
    let new_count = items.iter().filter(|v| v["posted"] != true).count();
    if new_count == 0
        || items.len() < 2
        || prep.ledger.request_count() >= cfg.budget.run_max_requests
    {
        return 0;
    }
    // Validation is finished; only anchoring/publication still need a margin.
    let remaining = prep
        .deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(Duration::from_secs(10))
        .min(Duration::from_secs(60));
    if remaining.is_zero() {
        return 0;
    }
    let result = tokio::time::timeout(remaining, async {
        let client = super::make_client(
            cfg.effective_validator(), "dedup",
            Some(super::cache_key(cfg, &prep.strategy_name, "dedup", None)),
            prep.ledger.clone(), cfg.budget.run_max_requests, 0, "submit_groups",
        )?;
        let terminal = ToolSpec {
            name: "submit_groups".into(),
            description: "Group refs describing the same root cause and fix. Empty groups is valid.".into(),
            parameters: json!({"type":"object","properties":{"groups":{"type":"array","items":{"type":"array","items":{"type":"string"},"minItems":2}}},"required":["groups"],"additionalProperties":false}),
        };
        let user = crate::redact::text(&serde_json::to_string(&items)?).into_owned();
        let response = client.complete(&[
            ChatMessage::system(crate::redact::text(crate::prompts::DEDUP).into_owned()),
            ChatMessage::user(user),
        ], &[terminal]).await?;
        let calls = response.message.tool_calls;
        if calls.len() != 1 || calls[0].name != "submit_groups" {
            bail!("dedup must submit exactly one groups tool call");
        }
        apply(findings, &carried, &calls[0].arguments)
    }).await;
    match result {
        Ok(Ok(count)) => count,
        Ok(Err(error)) => {
            tracing::warn!(error = %crate::redact::diagnostic(&format!("{error:#}")),
                "optional deduplication failed; keeping all accepted findings");
            0
        }
        Err(error) => {
            tracing::warn!(error = %crate::redact::diagnostic(&error.to_string()),
                "optional deduplication timed out; keeping all accepted findings");
            0
        }
    }
}

fn items(findings: &[Finding], carried: &[CarriedFinding]) -> Vec<Value> {
    let mut items: Vec<_> = findings.iter().enumerate()
        .filter(|(_, f)| super::anchor::is_publishable(f))
        .map(|(i, f)| json!({"ref":format!("new:{i}"),"file":f.file,"start_line":f.start_line,"end_line":f.end_line,"category":f.category,"title":f.title,"claim":f.claim.chars().take(400).collect::<String>()}))
        .collect();
    items.extend(carried.iter().enumerate().map(|(i, f)| json!({"ref":format!("posted:{i}"),"posted":true,"file":f.file,"start_line":f.start_line,"end_line":f.end_line,"category":f.category,"title":f.title,"claim":f.claim.chars().take(400).collect::<String>()})));
    items
}

#[derive(Clone, Copy)]
enum Item {
    New(usize),
    Posted(usize),
}

/// Validate all groups before applying any removals, so errors cannot leave a
/// partially deduplicated result. The only mutable field of a dropped finding
/// is duplicate_of; its validator verdict stays visible in the report.
fn apply(findings: &mut [Finding], carried: &[CarriedFinding], args: &Value) -> Result<usize> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Groups {
        groups: Vec<Vec<String>>,
    }
    let groups: Groups = serde_json::from_value(args.clone())?;
    let mut refs = HashMap::new();
    for (i, f) in findings
        .iter()
        .enumerate()
        .filter(|(_, f)| super::anchor::is_publishable(f))
    {
        let _ = f;
        refs.insert(format!("new:{i}"), Item::New(i));
    }
    for i in 0..carried.len() {
        refs.insert(format!("posted:{i}"), Item::Posted(i));
    }
    let mut seen = HashSet::new();
    let mut checked = Vec::new();
    for group in groups.groups {
        if group.len() < 2 {
            bail!("dedup group needs at least two refs");
        }
        let mut members = Vec::new();
        for r in group {
            if !seen.insert(r.clone()) {
                bail!("repeated dedup ref");
            }
            members.push(
                *refs
                    .get(&r)
                    .ok_or_else(|| anyhow::anyhow!("unknown dedup ref"))?,
            );
        }
        checked.push(members);
    }
    let mut count = 0;
    for group in checked {
        let posted = group
            .iter()
            .filter_map(|m| match m {
                Item::Posted(i) => Some(*i),
                _ => None,
            })
            .min();
        let mut new: Vec<_> = group
            .iter()
            .filter_map(|m| match m {
                Item::New(i) => Some(*i),
                _ => None,
            })
            .collect();
        new.sort_unstable();
        let winner = if posted.is_some() {
            None
        } else {
            new.iter().copied().reduce(|a, b| {
                if findings[b].severity > findings[a].severity {
                    b
                } else {
                    a
                }
            })
        };
        let id = match (posted, winner) {
            (Some(i), _) => carried[i].id.clone(),
            (_, Some(i)) => findings[i].id(),
            _ => continue,
        };
        for i in new.into_iter().filter(|i| Some(*i) != winner) {
            if let Some(keep) = winner {
                let mut evidence = findings[i].supporting_evidence.clone();
                evidence.push(Evidence {
                    path: findings[i].file.clone(),
                    start_line: findings[i].start_line,
                    end_line: findings[i].end_line.unwrap_or(findings[i].start_line),
                    note: findings[i].title.clone(),
                });
                for e in evidence {
                    if !findings[keep].supporting_evidence.iter().any(|old| {
                        old.path == e.path
                            && old.start_line == e.start_line
                            && old.end_line == e.end_line
                    }) {
                        findings[keep].supporting_evidence.push(e);
                    }
                }
            }
            findings[i].duplicate_of = Some(id.clone());
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::ValidationStatus;
    use crate::state::{FindingState, ReviewState};

    fn finding(key: &str, severity: &str, line: u32) -> Finding {
        serde_json::from_value(json!({"defect_key":key,"severity":severity,"file":"bug.rs","start_line":line,"title":key,"claim":"claim","category":"logic","validation_status":"accepted"})).unwrap()
    }

    #[test]
    fn severity_then_candidate_order_wins_and_evidence_is_merged() {
        let mut fs = vec![
            finding("first", "medium", 1),
            finding("second", "high", 2),
            finding("third", "high", 3),
        ];
        assert_eq!(
            apply(&mut fs, &[], &json!({"groups":[["new:2","new:0","new:1"]]})).unwrap(),
            2
        );
        assert_eq!(fs[0].duplicate_of, Some(fs[1].id()));
        assert_eq!(fs[2].duplicate_of, Some(fs[1].id()));
        assert_eq!(fs[1].supporting_evidence.len(), 2);
        assert_eq!(fs[0].validation_status, Some(ValidationStatus::Accepted));
        assert!(!super::super::anchor::is_publishable(&fs[0]));
        let mut state = ReviewState::default();
        state.upsert(&fs[0], FindingState::Open);
        state.upsert(&fs[1], FindingState::Open);
        assert_eq!(state.findings.len(), 1);
    }

    #[test]
    fn carried_wins_and_invalid_groups_are_atomic() {
        let mut state = ReviewState::default();
        let prior = finding("prior", "low", 4);
        state.upsert(&prior, FindingState::Open);
        state.mark_posted(&[prior.id()]);
        let carried = crate::report::carried_findings(&state, &[]);
        let mut fs = vec![finding("new", "high", 1), finding("another", "medium", 2)];
        for args in [
            json!({"groups":[["new:0","posted:0"],["new:1","bad"]]}),
            json!({"groups":[["new:0","new:1"],["new:0","posted:0"]]}),
            json!({"groups":"bad"}),
        ] {
            assert!(apply(&mut fs, &carried, &args).is_err());
            assert!(fs.iter().all(|f| f.duplicate_of.is_none()));
        }
        assert_eq!(
            apply(
                &mut fs,
                &carried,
                &json!({"groups":[["new:0","posted:0","new:1"]]})
            )
            .unwrap(),
            2
        );
        assert!(fs.iter().all(|f| f.duplicate_of == Some(prior.id())));
    }
}
