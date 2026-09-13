use crate::findings::{Finding, ValidationStatus};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    pub title: String,
    #[serde(default)]
    pub posted: bool,
    /// defect_key retained so recheck prompts can name the defect.
    #[serde(default)]
    pub defect_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewState {
    pub reviewed_head: String,
    pub reviewed_base: String,
    /// `git patch-id --stable` of base...head; identical content -> skip review.
    #[serde(default)]
    pub patch_id: String,
    #[serde(default)]
    pub findings: Vec<StateFinding>,
}

impl ReviewState {
    pub fn path(repo_root: &Path) -> PathBuf {
        repo_root.join(".revera/state.json")
    }

    pub fn load(repo_root: &Path) -> Result<Option<Self>> {
        let p = Self::path(repo_root);
        if !p.exists() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(&p)?;
        Ok(Some(serde_json::from_str(&s)?))
    }

    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let dir = repo_root.join(".revera");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(Self::path(repo_root), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// True when a rerun would review byte-identical patch content to the
    /// stored state (same patch-id and same base).
    pub fn is_unchanged(&self, base: &str, patch_id: &str) -> bool {
        !self.patch_id.is_empty() && self.patch_id == patch_id && self.reviewed_base == base
    }

    pub fn open_findings(&self) -> Vec<&StateFinding> {
        self.findings
            .iter()
            .filter(|f| matches!(f.status, FindingState::Open | FindingState::Uncertain))
            .collect()
    }

    pub fn has_posted(&self, id: &str) -> bool {
        self.findings.iter().any(|f| f.id == id && f.posted)
    }

    pub fn mark(&mut self, id: &str, status: FindingState) {
        if let Some(f) = self.findings.iter_mut().find(|f| f.id == id) {
            f.status = status;
        }
    }

    /// Upsert a finding into state; an existing `posted` flag is preserved.
    /// The pipeline never sets `posted` — only the publisher does, via
    /// `mark_posted`, after a successful GitHub review post.
    pub fn upsert(&mut self, f: &Finding, status: FindingState) {
        let id = f.id();
        if let Some(e) = self.findings.iter_mut().find(|x| x.id == id) {
            e.status = status;
            e.start_line = f.start_line;
            e.title = f.title.clone();
            return;
        }
        self.findings.push(StateFinding {
            id,
            status,
            file: f.file.clone(),
            start_line: f.start_line,
            title: f.title.clone(),
            posted: false,
            defect_key: f.defect_key.clone(),
        });
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
