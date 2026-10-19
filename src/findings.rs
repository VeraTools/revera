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
    /// The investigator's proposal. Never published: only `validated_fix`
    /// is rendered.
    #[serde(default)]
    pub suggested_fix: Option<String>,
    /// Remedy text the fresh validator approved or wrote in this run.
    #[serde(
        default,
        deserialize_with = "lenient_fix",
        skip_serializing_if = "Option::is_none"
    )]
    pub validated_fix: Option<String>,
    /// Exact replacement checked against the immutable reviewed head by Rust.
    #[serde(
        default,
        deserialize_with = "lenient_suggestion",
        skip_serializing_if = "Option::is_none"
    )]
    pub suggestion: Option<Suggestion>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Finding {
    /// hex(sha256(file + "\0" + defect_key))[..12]
    pub fn id(&self) -> String {
        finding_id(&self.file, &self.defect_key)
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
    #[serde(default)]
    pub rationale: String,
    /// Approved or replacement remedy. Anything but a non-blank string is
    /// treated as absent so a malformed optional field cannot discard the
    /// verdict itself.
    #[serde(default, deserialize_with = "lenient_fix")]
    pub fix: Option<String>,
    #[serde(default, deserialize_with = "lenient_suggestion")]
    pub suggestion: Option<Suggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub start_line: u32,
    pub end_line: u32,
    /// Exact bytes of the original range, including its final newline if present.
    pub original: String,
    pub replacement: String,
}

fn lenient_suggestion<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Suggestion>, D::Error> {
    Ok(serde_json::from_value(serde_json::Value::deserialize(d)?).ok())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedCounts {
    pub total: u64,
    pub high: u64,
    pub medium: u64,
    pub low: u64,
}

fn count_severities<'a>(severities: impl Iterator<Item = &'a str>) -> AcceptedCounts {
    let mut counts = AcceptedCounts::default();
    for severity in severities {
        counts.total += 1;
        match severity {
            "high" => counts.high += 1,
            "medium" => counts.medium += 1,
            "low" => counts.low += 1,
            _ => {}
        }
    }
    counts
}

pub fn accepted_counts(findings: &[Finding]) -> AcceptedCounts {
    count_severities(
        findings
            .iter()
            .filter(|f| f.validation_status == Some(ValidationStatus::Accepted))
            .map(|f| match f.severity {
                Severity::High => "high",
                Severity::Medium => "medium",
                Severity::Low => "low",
            }),
    )
}

pub fn accepted_values(report: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["validation_status"].as_str() == Some("accepted"))
}

pub fn accepted_counts_json(report: &serde_json::Value) -> AcceptedCounts {
    count_severities(accepted_values(report).map(|f| f["severity"].as_str().unwrap_or("")))
}

/// Fail closed unless this is an exact, small replacement of added RIGHT-side lines.
pub fn checked_suggestion(
    f: &Finding,
    suggestion: &Suggestion,
    diff: &crate::diff::DiffSet,
    head: &str,
) -> Option<Suggestion> {
    use crate::diff::DiffLineKind;
    let s = suggestion;
    if f.validation_status != Some(ValidationStatus::Accepted)
        || s.start_line == 0
        || s.end_line < s.start_line
        || s.end_line - s.start_line >= 10
        || s.start_line != f.start_line
        || s.end_line != f.end_line.unwrap_or(f.start_line)
        || s.replacement.is_empty()
        || s.replacement.len() > 2000
        || s.replacement.lines().count() > 20
        || [&s.original, &s.replacement].iter().any(|v| {
            v.contains(['\0', '\r'])
                || v.contains("<!--")
                || crate::redact::text(v).as_ref() != v.as_str()
        })
        || crate::tools::glob_set(crate::config::SENSITIVE_GLOBS).is_match(&f.file)
    {
        return None;
    }
    let file = diff.file(&f.file)?;
    if file.new_path != f.file
        || !file.hunks.iter().any(|h| {
            (s.start_line..=s.end_line).all(|n| {
                h.lines
                    .iter()
                    .any(|l| l.new_no == Some(n) && l.kind == DiffLineKind::Add)
            })
        })
    {
        return None;
    }
    let lines: Vec<_> = head.split_inclusive('\n').collect();
    let range = lines.get(s.start_line as usize - 1..s.end_line as usize)?;
    (range.concat() == s.original).then(|| s.clone())
}

fn lenient_fix<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    })
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
