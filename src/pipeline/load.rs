use super::common::ReviewRequest;
use crate::git;
use anyhow::{bail, Result};

/// The reviewed change: resolved refs and the raw unified diff.
pub struct LoadedDiff {
    pub base_sha: String,
    pub head_sha: String,
    pub raw_diff: String,
    pub patch_id: String,
    pub head_tree: String,
}

/// Resolve refs and produce the diff under review, refusing a head that is
/// not the checked-out tree (reviewer tools read the working tree).
pub async fn load_diff(repo: &std::path::Path, req: &ReviewRequest) -> Result<LoadedDiff> {
    let loaded = if req.uncommitted {
        let current_head = git::current_head(repo).await?;
        let raw_diff = git::diff_uncommitted(repo).await?;
        let patch_id = git::patch_id_from_diff(repo, &raw_diff)
            .await
            .unwrap_or_else(|_| "uncommitted-empty".to_string());
        LoadedDiff {
            base_sha: current_head.clone(),
            head_sha: format!("{current_head}+dirty"),
            raw_diff,
            patch_id,
            head_tree: "working_tree".to_string(),
        }
    } else {
        let base_sha = git::rev_parse(repo, &req.base).await?;
        let head_rev = req.head.as_deref().unwrap_or("HEAD");
        let head_sha = git::rev_parse(repo, head_rev).await?;
        let current_head = git::current_head(repo).await?;
        if head_sha != current_head {
            bail!(
                "head {head_sha} is not the checked-out tree (HEAD is {current_head}); reviewer tools read the working tree, so check out the PR head first (GitHub Actions: actions/checkout with ref: ${{{{ github.event.pull_request.head.sha }}}})"
            );
        }
        if git::tracked_dirty(repo).await? {
            bail!(
                "working tree has uncommitted changes to tracked files; commit or stash them so the reviewed tree matches {head_sha} (HEAD is {current_head})"
            );
        }
        let raw_diff = git::diff(repo, &req.base, head_rev).await?;
        let patch_id = git::patch_id(repo, &req.base, head_rev)
            .await
            .unwrap_or_default();
        let head_tree = git::tree_id(repo, head_rev).await?;
        LoadedDiff {
            base_sha,
            head_sha,
            raw_diff,
            patch_id,
            head_tree,
        }
    };
    Ok(loaded)
}
