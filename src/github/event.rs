use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Parsed `pull_request` / `pull_request_target` webhook payload.
#[derive(Debug, Clone)]
pub struct PrEvent {
    /// `owner/repo` of the base repository.
    pub repo_full_name: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub head_sha: String,
    pub head_ref: String,
    /// `owner/repo` of the head repository (differs on forks).
    pub head_repo_full_name: String,
    pub base_sha: String,
    pub base_ref: String,
}

impl PrEvent {
    pub fn owner_repo(&self) -> (&str, &str) {
        match self.repo_full_name.split_once('/') {
            Some((o, r)) => (o, r),
            None => ("", self.repo_full_name.as_str()),
        }
    }

    pub fn is_fork(&self) -> bool {
        self.head_repo_full_name != self.repo_full_name
    }
}

fn str_at<'a>(v: &'a Value, path: &str) -> Result<&'a str> {
    let mut cur = v;
    for part in path.split('.') {
        cur = &cur[part];
    }
    cur.as_str()
        .with_context(|| format!("event payload missing {path}"))
}

pub fn parse(path: &Path) -> Result<PrEvent> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read event payload {}", path.display()))?;
    let v: Value = serde_json::from_str(&text).context("event payload is not JSON")?;
    if v["pull_request"].is_null() {
        bail!("unsupported event: payload has no pull_request object");
    }
    Ok(PrEvent {
        repo_full_name: str_at(&v, "repository.full_name")?.to_string(),
        number: v["number"]
            .as_u64()
            .context("event payload missing number")?,
        title: str_at(&v, "pull_request.title")
            .unwrap_or_default()
            .to_string(),
        body: v["pull_request"]["body"].as_str().unwrap_or("").to_string(),
        head_sha: str_at(&v, "pull_request.head.sha")?.to_string(),
        head_ref: str_at(&v, "pull_request.head.ref")?.to_string(),
        head_repo_full_name: str_at(&v, "pull_request.head.repo.full_name")?.to_string(),
        base_sha: str_at(&v, "pull_request.base.sha")?.to_string(),
        base_ref: str_at(&v, "pull_request.base.ref")?.to_string(),
    })
}
