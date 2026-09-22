use crate::findings::{Finding, ValidationStatus};
use crate::report::RunStatus;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bump when the meaning of stored fields changes. Older blobs are still
/// read (findings and publication ids are kept) but never trusted as a
/// completed review of the current content.
pub const STATE_VERSION: u32 = 2;
/// Upper bound on findings retained in state (resolved/rejected are dropped
/// first) so the embedded blob stays small.
pub const MAX_STATE_FINDINGS: usize = 200;
const MAX_TEXT: usize = 400;
const MAX_EVIDENCE: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingState {
    Open,
    Uncertain,
    Resolved,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFinding {
    pub id: String,
    pub status: FindingState,
    pub file: String,
    pub start_line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    pub title: String,
    #[serde(default)]
    pub posted: bool,
    /// defect_key retained so recheck prompts can name the defect.
    #[serde(default)]
    pub defect_key: String,
    #[serde(default)]
    pub severity: crate::findings::Severity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claim: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trigger: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub impact: String,
    /// Compact `path:line note` references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewState {
    #[serde(default)]
    pub version: u32,
    pub reviewed_head: String,
    pub reviewed_base: String,
    /// `git patch-id --stable` of base...head. Informational: identical
    /// patch-ids do not prove identical code (whitespace/string changes).
    #[serde(default)]
    pub patch_id: String,
    /// Identity of the last review (base, exact head tree, review-affecting
    /// config). Reuse requires an exact match *and* `last_status == Complete`.
    #[serde(default)]
    pub review_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<RunStatus>,
    /// Id of the managed summary comment, when one was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_comment_id: Option<u64>,
    #[serde(default)]
    pub findings: Vec<StateFinding>,
}

fn clip(s: &str) -> String {
    crate::text::truncate_bytes(s, MAX_TEXT)
}

impl StateFinding {
    fn from_finding(f: &Finding, status: FindingState) -> Self {
        Self {
            id: f.id(),
            status,
            file: f.file.clone(),
            start_line: f.start_line,
            end_line: f.end_line,
            title: f.title.clone(),
            posted: false,
            defect_key: f.defect_key.clone(),
            severity: f.severity,
            claim: clip(&f.claim),
            trigger: clip(&f.trigger),
            impact: clip(&f.impact),
            evidence: f
                .supporting_evidence
                .iter()
                .take(MAX_EVIDENCE)
                .map(|e| {
                    if e.note.is_empty() {
                        format!("{}:{}", e.path, e.start_line)
                    } else {
                        format!("{}:{} {}", e.path, e.start_line, clip(&e.note))
                    }
                })
                .collect(),
        }
    }

    /// Rebuild a `Finding` for rechecking, from the retained detail.
    pub fn to_finding(&self) -> Finding {
        Finding {
            defect_key: self.defect_key.clone(),
            severity: self.severity,
            file: self.file.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            title: self.title.clone(),
            claim: self.claim.clone(),
            trigger: self.trigger.clone(),
            impact: self.impact.clone(),
            introduced_by_change: true,
            supporting_evidence: self
                .evidence
                .iter()
                .map(|e| {
                    let (loc, note) = e.split_once(' ').unwrap_or((e.as_str(), ""));
                    let (path, line) = loc.rsplit_once(':').unwrap_or((loc, "0"));
                    crate::findings::Evidence {
                        path: path.to_string(),
                        start_line: line.parse().unwrap_or(0),
                        end_line: 0,
                        note: note.to_string(),
                    }
                })
                .collect(),
            counterevidence_checked: vec![],
            validation_status: None,
            suggested_fix: None,
            source: "prior".into(),
            rationale: None,
            sources: vec!["prior".into()],
            assurance: None,
        }
    }
}

impl ReviewState {
    pub fn path(repo_root: &Path) -> PathBuf {
        repo_root.join(".revera/state.json")
    }

    /// Loads local state. A corrupt file is set aside (`state.json.corrupt`)
    /// and treated as absent, so a bad blob cannot wedge every later run.
    pub fn load(repo_root: &Path) -> Result<Option<Self>> {
        let p = Self::path(repo_root);
        if !p.exists() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(&p)?;
        match serde_json::from_str::<Self>(&s) {
            Ok(st) => Ok(Some(st)),
            Err(e) => {
                tracing::warn!("ignoring corrupt state {}: {e}", p.display());
                let _ = std::fs::rename(&p, p.with_extension("json.corrupt"));
                Ok(None)
            }
        }
    }

    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let dir = repo_root.join(".revera");
        std::fs::create_dir_all(&dir)?;
        let target = Self::path(repo_root);
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        serde_json::to_writer_pretty(tmp.as_file_mut(), self)?;
        tmp.as_file_mut().sync_all()?;
        tmp.persist(&target).map_err(|e| e.error)?;
        Ok(())
    }

    /// True when a rerun would review the same content with the same
    /// configuration and the previous run completed. Legacy (unversioned)
    /// state, partial runs, and any identity drift force a fresh review.
    pub fn can_reuse(&self, review_key: &str) -> bool {
        self.version == STATE_VERSION
            && !self.review_key.is_empty()
            && self.review_key == review_key
            && self.last_status == Some(RunStatus::Complete)
    }

    /// Record the outcome of this run.
    pub fn record_outcome(
        &mut self,
        base: &str,
        head: &str,
        patch_id: &str,
        review_key: &str,
        status: RunStatus,
    ) {
        self.version = STATE_VERSION;
        self.reviewed_base = base.to_string();
        self.reviewed_head = head.to_string();
        self.patch_id = patch_id.to_string();
        self.review_key = review_key.to_string();
        self.last_status = Some(status);
        self.prune();
    }

    /// Downgrade the recorded outcome when publication did not finish:
    /// the review may have completed, but the PR does not carry it, so the
    /// next run must redo (not reuse) it.
    pub fn mark_publication_incomplete(&mut self) {
        if self.last_status == Some(RunStatus::Complete) {
            self.last_status = Some(RunStatus::Partial);
        }
    }

    fn prune(&mut self) {
        while self.findings.len() > MAX_STATE_FINDINGS {
            match self
                .findings
                .iter()
                .position(|f| matches!(f.status, FindingState::Resolved | FindingState::Rejected))
            {
                Some(i) => {
                    self.findings.remove(i);
                }
                None => {
                    self.findings.remove(0);
                }
            }
        }
    }

    pub fn open_findings(&self) -> Vec<&StateFinding> {
        self.findings
            .iter()
            .filter(|f| matches!(f.status, FindingState::Open | FindingState::Uncertain))
            .collect()
    }

    pub fn resolved_findings(&self) -> Vec<&StateFinding> {
        self.findings
            .iter()
            .filter(|f| f.status == FindingState::Resolved)
            .collect()
    }

    pub fn find(&self, id: &str) -> Option<&StateFinding> {
        self.findings.iter().find(|f| f.id == id)
    }

    /// Posted *and still open/uncertain*: a resolved or rejected finding that
    /// reappears is a reintroduction and must surface again.
    pub fn has_posted(&self, id: &str) -> bool {
        self.findings.iter().any(|f| {
            f.id == id
                && f.posted
                && matches!(f.status, FindingState::Open | FindingState::Uncertain)
        })
    }

    /// Currently tracked as open/uncertain (posted or not).
    pub fn is_tracked_open(&self, id: &str) -> bool {
        self.findings
            .iter()
            .any(|f| f.id == id && matches!(f.status, FindingState::Open | FindingState::Uncertain))
    }

    pub fn mark(&mut self, id: &str, status: FindingState) {
        if let Some(f) = self.findings.iter_mut().find(|f| f.id == id) {
            f.status = status;
        }
    }

    /// Upsert a finding into state. An existing `posted` flag is preserved
    /// while the finding stays open; a resolved/rejected finding that comes
    /// back open is a reopen and needs publishing again. Returns true when
    /// the call reopened a previously closed finding.
    pub fn upsert(&mut self, f: &Finding, status: FindingState) -> bool {
        let id = f.id();
        if let Some(e) = self.findings.iter_mut().find(|x| x.id == id) {
            let reopened = matches!(e.status, FindingState::Resolved | FindingState::Rejected)
                && matches!(status, FindingState::Open | FindingState::Uncertain);
            let posted = e.posted && !reopened;
            *e = StateFinding {
                posted,
                ..StateFinding::from_finding(f, status)
            };
            return reopened;
        }
        self.findings.push(StateFinding::from_finding(f, status));
        false
    }

    /// Called by the publisher after a successful post; marks ids as posted.
    pub fn mark_posted(&mut self, ids: &[String]) {
        for f in self.findings.iter_mut() {
            if ids.contains(&f.id) {
                f.posted = true;
            }
        }
    }
}

/// Map a validation verdict onto a prior-state status transition.
pub fn recheck_transition(v: ValidationStatus) -> FindingState {
    match v {
        ValidationStatus::Accepted => FindingState::Open,
        ValidationStatus::Rejected => FindingState::Resolved,
        ValidationStatus::Uncertain => FindingState::Uncertain,
    }
}

/// sha256 over the review identity: base, exact head tree, patch-id and the
/// review-affecting configuration. Never includes secrets.
pub fn review_key(
    base_sha: &str,
    head_tree: &str,
    patch_id: &str,
    cfg: &serde_json::Value,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in [base_sha, head_tree, patch_id] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    h.update(cfg.to_string().as_bytes());
    hex::encode(h.finalize())[..24].to_string()
}
