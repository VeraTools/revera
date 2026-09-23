//! Optional dynamic lens selection for the panel strategy through a TypeSafe
//! System One model: one request asks, per scout lane, whether the diff holds
//! changes that lane's reviewer should examine, and lanes below a probability
//! floor are not run. It only decides which scouts investigate; every
//! candidate still goes through fresh-context validation. Any failure runs
//! every lane (fail open), so a router outage never narrows a review.

use crate::config::EffectiveLane;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const QUESTION: &str =
    "Does `diff` contain changes that a code reviewer focused on `reviewer_focus` should examine?";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LensRouterConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Name of the environment variable holding the TypeSafe API key.
    pub api_key_env: String,
    /// Lanes whose relevance probability is below this are not run.
    #[serde(default = "default_min_probability")]
    pub min_probability: f64,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Cap on the diff text sent as state.
    #[serde(default = "default_max_state_bytes")]
    pub max_state_bytes: usize,
}

fn default_base_url() -> String {
    "https://api.typesafe.ai/v1".into()
}
fn default_model() -> String {
    "jev-latest".into()
}
fn default_min_probability() -> f64 {
    0.2
}
fn default_timeout_seconds() -> u64 {
    15
}
fn default_max_state_bytes() -> usize {
    60_000
}

impl LensRouterConfig {
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.min_probability) {
            bail!(
                "panel.lens_router.min_probability must be within 0..=1 (got {})",
                self.min_probability
            );
        }
        if self.api_key_env.trim().is_empty() {
            bail!("panel.lens_router.api_key_env must name an environment variable");
        }
        url::Url::parse(&self.base_url)
            .map_err(|e| anyhow::anyhow!("panel.lens_router.base_url: {e}"))?;
        Ok(())
    }

    pub fn check_credentials(&self) -> Result<()> {
        if std::env::var(&self.api_key_env).map_or(true, |v| v.is_empty()) {
            bail!(
                "panel.lens_router: api_key_env {} is not set (or empty) in the environment",
                self.api_key_env
            );
        }
        Ok(())
    }

    /// Review-identity view: everything that changes lane selection, never
    /// the key or its variable name.
    pub fn fingerprint(&self) -> Value {
        json!({
            "base_url": self.base_url,
            "model": self.model,
            "min_probability": self.min_probability,
            "max_state_bytes": self.max_state_bytes,
        })
    }
}

/// Which lanes run and why, in lane order.
#[derive(Debug, Clone, PartialEq)]
pub struct LensDecision {
    pub keep: Vec<bool>,
    /// Summary note; `None` when the router kept every lane.
    pub note: Option<String>,
}

fn lane_focus(lane: &EffectiveLane) -> &str {
    lane.focus.as_deref().unwrap_or(&lane.name)
}

/// Keep lanes at or above `min`; when none qualifies keep the most relevant
/// one, so a review never runs without a scout.
pub fn decide(probs: &[f64], min: f64) -> Vec<bool> {
    let mut keep: Vec<bool> = probs.iter().map(|p| *p >= min).collect();
    if !keep.iter().any(|k| *k) {
        if let Some((best, _)) = probs.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) {
            keep[best] = true;
        }
    }
    keep
}

fn request_body(cfg: &LensRouterConfig, diff: &str, lanes: &[EffectiveLane]) -> Value {
    let questions: serde_json::Map<String, Value> = lanes
        .iter()
        .enumerate()
        .map(|(i, lane)| {
            (
                format!("lane_{i}"),
                json!({
                    "type": "noul",
                    "instructions": {
                        "reviewer_focus": lane_focus(lane),
                        "question": QUESTION,
                    },
                    "criteria": {
                        "true": "The diff changes code or configuration this reviewer's focus covers.",
                        "false": "Nothing in the diff falls within this reviewer's focus.",
                    },
                }),
            )
        })
        .collect();
    json!({
        "state": { "diff": diff },
        "model": cfg.model,
        "questions": questions,
    })
}

/// Relevance probability per lane, in lane order; any missing or malformed
/// answer fails the whole response.
fn parse_answers(body: &Value, n: usize) -> Result<Vec<f64>, String> {
    (0..n)
        .map(|i| {
            let a = &body["answers"][format!("lane_{i}")];
            match a["noul"].as_f64() {
                Some(p) if (0.0..=1.0).contains(&p) => Ok(p),
                _ => Err(format!("no valid answer for lane {i}")),
            }
        })
        .collect()
}

async fn ask(
    cfg: &LensRouterConfig,
    diff: &str,
    lanes: &[EffectiveLane],
    deadline: Instant,
) -> Result<Vec<f64>, String> {
    let key = std::env::var(&cfg.api_key_env)
        .ok()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| format!("{} is not set", cfg.api_key_env))?;
    let timeout = Duration::from_secs(cfg.timeout_seconds)
        .min(deadline.saturating_duration_since(Instant::now()));
    if timeout.is_zero() {
        return Err("run deadline reached".into());
    }
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/systemone", cfg.base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(key)
        .json(&request_body(cfg, diff, lanes))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    parse_answers(&body, lanes.len())
}

/// Decide which `lanes` run for `diff`. Never fails: on any router error
/// every lane runs and the note says why.
pub async fn select_lanes(
    cfg: &LensRouterConfig,
    diff: &str,
    lanes: &[EffectiveLane],
    deadline: Instant,
) -> LensDecision {
    let all = LensDecision {
        keep: vec![true; lanes.len()],
        note: None,
    };
    if lanes.len() < 2 {
        return all;
    }
    let probs = match ask(cfg, diff, lanes, deadline).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("lens router unavailable, running every lane: {e}");
            return LensDecision {
                note: Some(format!("lens router unavailable ({e}); all lanes ran")),
                ..all
            };
        }
    };
    let keep = decide(&probs, cfg.min_probability);
    let skipped: Vec<String> = lanes
        .iter()
        .zip(&probs)
        .zip(&keep)
        .filter(|(_, k)| !**k)
        .map(|((lane, p), _)| format!("{} ({p:.2})", lane.name))
        .collect();
    LensDecision {
        note: (!skipped.is_empty()).then(|| {
            format!(
                "lens router skipped {} below {:.2}",
                skipped.join(", "),
                cfg.min_probability
            )
        }),
        keep,
    }
}
