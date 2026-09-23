use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Low,
    Medium,
    High,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationStatus {
    Accepted,
    Rejected,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    #[serde(default)]
    pub start_line: u32,
    #[serde(default)]
    pub end_line: u32,
    #[serde(default)]
    pub note: String,
}

fn default_confidence() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssuranceCase {
    #[serde(default)]
    pub rule_id: Option<String>,
    pub trigger: String,
    pub rationale: String,
    #[serde(default)]
    pub counterevidence_checked: Vec<String>,
    #[serde(default)]
    pub validator_rederivation: Option<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub defect_key: String,
    pub severity: Severity,
    pub file: String,
    pub start_line: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    pub title: String,
    pub claim: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub impact: String,
    #[serde(default)]
    pub introduced_by_change: bool,
    #[serde(default)]
    pub supporting_evidence: Vec<Evidence>,
    #[serde(default)]
    pub counterevidence_checked: Vec<String>,
    #[serde(default)]
    pub validation_status: Option<ValidationStatus>,
    #[serde(default)]
    pub suggested_fix: Option<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub assurance: Option<AssuranceCase>,
    /// Verbatim head-side lines the finding is about. Anchoring derives the
    /// line range from a unique match in the diff instead of trusting counted
    /// line numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted_code: Option<String>,
    /// Exact replacement text for `quoted_code`; published as a GitHub
    /// suggestion only when the quote anchored uniquely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_replacement: Option<String>,
    /// Set by anchoring when `quoted_code` matched exactly once in the diff.
    #[serde(skip)]
    pub quote_anchored: bool,
}

impl Finding {
    /// hex(sha256(file + "\0" + defect_key))[..12]
    pub fn id(&self) -> String {
        finding_id(&self.file, &self.defect_key)
    }

    /// Number of distinct review lenses or scouts that flagged this defect.
    pub fn consensus_count(&self) -> usize {
        self.sources.len().max(1)
    }

    /// Returns the effective assurance case or constructs a baseline one from finding fields.
    pub fn effective_assurance(&self) -> AssuranceCase {
        if let Some(a) = &self.assurance {
            a.clone()
        } else {
            AssuranceCase {
                rule_id: if self.source.starts_with("static:") {
                    Some(self.source.trim_start_matches("static:").to_string())
                } else {
                    None
                },
                trigger: self.trigger.clone(),
                rationale: self.rationale.clone().unwrap_or_else(|| self.claim.clone()),
                counterevidence_checked: self.counterevidence_checked.clone(),
                validator_rederivation: None,
                confidence: 1.0,
            }
        }
    }
}

pub fn finding_id(file: &str, defect_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(file.as_bytes());
    h.update([0u8]);
    h.update(defect_key.as_bytes());
    hex::encode(h.finalize())[..12].to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub validation_status: ValidationStatus,
    #[serde(default)]
    pub counterevidence_checked: Vec<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub start_line: Option<u32>,
    #[serde(default)]
    pub end_line: Option<u32>,
    /// Corrected quote of the head-side lines, when the anchor should move.
    #[serde(default)]
    pub quoted_code: Option<String>,
    #[serde(default)]
    pub rationale: String,
}

fn normalize_title(t: &str) -> Vec<String> {
    t.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

fn lines_overlap(a: &Finding, b: &Finding) -> bool {
    let a_end = a.end_line.unwrap_or(a.start_line);
    let b_end = b.end_line.unwrap_or(b.start_line);
    a.start_line <= b_end && b.start_line <= a_end
}

fn titles_similar(a: &str, b: &str) -> bool {
    let ta = normalize_title(a);
    let tb = normalize_title(b);
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let shared = ta.iter().filter(|w| tb.contains(w)).count();
    let denom = ta.len().max(tb.len()) as f64;
    (shared as f64 / denom) >= 0.6
}

/// Rank candidate findings prior to validation or publication.
/// Order priority:
/// 1. `consensus_count()` descending (corroborated by multiple independent scouts)
/// 2. `severity` descending (High > Medium > Low)
/// 3. `supporting_evidence.len()` descending (grounded in more code references)
/// 4. `file` and `start_line` ascending for deterministic tie-breaking.
pub fn rank_candidates(mut candidates: Vec<Finding>) -> Vec<Finding> {
    candidates.sort_by(|a, b| {
        b.consensus_count()
            .cmp(&a.consensus_count())
            .then_with(|| b.severity.cmp(&a.severity))
            .then_with(|| {
                b.supporting_evidence
                    .len()
                    .cmp(&a.supporting_evidence.len())
            })
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    candidates
}

/// Merge candidates: same file and (same defect_key OR overlapping line ranges
/// with >=60% shared normalized title tokens). Keep highest severity, union
/// evidence, record all sources.
pub fn collapse(candidates: Vec<Finding>) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for mut c in candidates {
        c.sources = {
            let mut s = c.sources.clone();
            if !c.source.is_empty() && !s.contains(&c.source) {
                s.push(c.source.clone());
            }
            s
        };
        let dup = out.iter().position(|o| {
            o.file == c.file
                && (o.defect_key == c.defect_key
                    || (lines_overlap(o, &c) && titles_similar(&o.title, &c.title)))
        });
        match dup {
            Some(i) => {
                let o = &mut out[i];
                if c.severity > o.severity {
                    o.severity = c.severity;
                }
                for e in c.supporting_evidence {
                    if !o.supporting_evidence.iter().any(|x| {
                        x.path == e.path && x.start_line == e.start_line && x.note == e.note
                    }) {
                        o.supporting_evidence.push(e);
                    }
                }
                for s in c.sources {
                    if !o.sources.contains(&s) {
                        o.sources.push(s);
                    }
                }
                // keep longer claim/title
                if c.claim.len() > o.claim.len() {
                    o.claim = c.claim;
                }
                if c.suggested_fix.is_some() && o.suggested_fix.is_none() {
                    o.suggested_fix = c.suggested_fix;
                }
            }
            None => out.push(c),
        }
    }
    out
}
