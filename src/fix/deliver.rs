//! Privileged controller path. Never call from a sandbox or before approval.
//! GitHub has no expected-old-SHA parameter: re-read, then force:false so a
//! concurrent forward update cannot lose work. No force-push or blind retry.
use super::workspace::{FileChange, Status};
use crate::github::api::{GitHubApi, GitHubHttpError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("GitHub permission denied")]
    Permission,
    #[error("repository or reference unavailable")]
    NotFound,
    #[error("branch protection rejected the write")]
    ProtectedBranch,
    #[error("target head moved; fresh approval required")]
    MovedHead,
    #[error("branch already exists")]
    RefExists,
    #[error("GitHub conflict")]
    Conflict,
    #[error("GitHub validation rejected the request: {0}")]
    Validation(String),
    #[error("GitHub response was malformed: {0}")]
    Malformed(String),
    #[error("GitHub request failed (may require reconciliation): {0}")]
    Request(String),
    #[error("invalid delivery input: {0}")]
    Invalid(String),
    #[error("branch {branch} created but draft PR failed: {cause}")]
    DraftFailed { branch: String, cause: Box<Error> },
}
fn map_error(error: anyhow::Error) -> Error {
    if let Some(h) = error.downcast_ref::<GitHubHttpError>() {
        let body = h.body.to_lowercase();
        if matches!(h.status, 403 | 409 | 422)
            && [
                "protected branch",
                "protected ref",
                "repository rule",
                "gh006",
                "gh013",
            ]
            .iter()
            .any(|s| body.contains(s))
        {
            return Error::ProtectedBranch;
        }
        return match h.status {
            403 => Error::Permission,
            404 => Error::NotFound,
            409 | 422 if body.contains("fast forward") || body.contains("fast-forward") => {
                Error::MovedHead
            }
            422 if body.contains("reference already exists") => Error::RefExists,
            409 => Error::Conflict,
            422 => Error::Validation(crate::redact::diagnostic(&crate::text::excerpt_bytes(
                &h.body, 500,
            ))),
            _ => Error::Request(crate::redact::diagnostic(&error.to_string())),
        };
    }
    Error::Request(crate::redact::diagnostic(&error.to_string()))
}
fn repo_check(repo: &Repository) -> Result<(), Error> {
    for name in [&repo.owner, &repo.name] {
        if name.is_empty()
            || name == "."
            || name == ".."
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(Error::Invalid("invalid repository name".into()));
        }
    }
    Ok(())
}
fn oid(s: &str) -> Result<(), Error> {
    if crate::git::is_oid(s) {
        Ok(())
    } else {
        Err(Error::Invalid("full SHA required".into()))
    }
}
fn branch_check(s: &str) -> Result<(), Error> {
    if s.is_empty()
        || s.len() > 200
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b) || b == b'.')
        || s.contains("..")
        || s.split('/')
            .any(|c| c.is_empty() || c.starts_with('.') || c.ends_with('.') || c.ends_with(".lock"))
        || s.starts_with('-')
    {
        return Err(Error::Invalid("invalid branch name".into()));
    }
    Ok(())
}
fn response_oid(v: &Value, key: &str) -> Result<String, Error> {
    v[key]
        .as_str()
        .filter(|s| crate::git::is_oid(s))
        .map(str::to_owned)
        .ok_or_else(|| Error::Malformed(format!("missing {key} SHA")))
}
fn ref_response(v: &Value, branch: &str, sha: &str) -> Result<(), Error> {
    if v["ref"] != format!("refs/heads/{branch}")
        || v["object"]["sha"] != sha
        || v["object"]["type"] != "commit"
    {
        return Err(Error::Malformed("ref acknowledgement mismatch".into()));
    }
    Ok(())
}

pub async fn create_commit(
    api: &GitHubApi,
    repo: &Repository,
    parent_sha: &str,
    changes: &[FileChange],
    message: &str,
) -> Result<String, Error> {
    repo_check(repo)?;
    oid(parent_sha)?;
    if changes.is_empty() || changes.len() > 100 || message.is_empty() || message.len() > 16_000 {
        return Err(Error::Invalid(
            "empty/oversized changes or commit message".into(),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for f in changes {
        if !seen.insert(&f.path)
            || !super::policy::valid_path(&f.path)
            || super::policy::sensitive(&f.path)
            || f.binary
            || f.size > super::policy::MAX_FILE_BYTES as u64
            || !matches!(f.new_mode.or(f.old_mode), Some(0o100644 | 0o100755))
            || (f.new_mode == Some(0o100755) && f.old_mode != Some(0o100755))
        {
            return Err(Error::Invalid("unsafe file change".into()));
        }
        if f.status != Status::Deleted {
            let content = f
                .new_content
                .as_deref()
                .ok_or_else(|| Error::Invalid("missing new file content".into()))?;
            if content.len() > super::policy::MAX_FILE_BYTES
                || content.contains('\0')
                || crate::redact::text(content) != content
            {
                return Err(Error::Invalid("unsafe file content".into()));
            }
        }
    }
    let parent = api
        .git_get_commit(&repo.owner, &repo.name, parent_sha)
        .await
        .map_err(map_error)?;
    if response_oid(&parent, "sha")? != parent_sha {
        return Err(Error::Malformed("parent commit mismatch".into()));
    }
    let base_tree = response_oid(&parent["tree"], "sha")?;
    let mut tree = Vec::new();
    for f in changes {
        let sha = if f.status == Status::Deleted {
            Value::Null
        } else {
            let b = api
                .git_create_blob(&repo.owner, &repo.name, f.new_content.as_deref().unwrap())
                .await
                .map_err(map_error)?;
            json!(response_oid(&b, "sha")?)
        };
        tree.push(json!({"path":f.path,"mode":format!("{:o}", f.new_mode.or(f.old_mode).unwrap()),"type":"blob","sha":sha}));
    }
    let t = api
        .git_create_tree(&repo.owner, &repo.name, &base_tree, &tree)
        .await
        .map_err(map_error)?;
    let tree = response_oid(&t, "sha")?;
    let commit = api
        .git_create_commit(&repo.owner, &repo.name, parent_sha, &tree, message)
        .await
        .map_err(map_error)?;
    response_oid(&commit, "sha")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPull {
    pub number: u64,
    pub url: String,
    pub branch: String,
    pub commit: String,
}
pub async fn open_draft_pr(
    api: &GitHubApi,
    repo: &Repository,
    commit: &str,
    branch_prefix: &str,
    base_ref: &str,
    title: &str,
    body: &str,
) -> Result<DraftPull, Error> {
    repo_check(repo)?;
    oid(commit)?;
    branch_check(branch_prefix)?;
    branch_check(base_ref)?;
    if title.is_empty() || title.len() > 500 || body.len() > 60_000 {
        return Err(Error::Invalid("PR text size".into()));
    }
    for i in 0..8 {
        let branch = if i == 0 {
            format!("{branch_prefix}-{}", &commit[..12])
        } else {
            format!("{branch_prefix}-{}-{i}", &commit[..12])
        };
        branch_check(&branch)?;
        let created = api
            .git_create_ref(&repo.owner, &repo.name, &branch, commit)
            .await
            .map_err(map_error);
        match created {
            Err(Error::RefExists) => continue,
            Err(e) => return Err(e),
            Ok(v) => ref_response(&v, &branch, commit)?,
        }
        let v = api
            .create_draft_pull(&repo.owner, &repo.name, &branch, base_ref, title, body)
            .await
            .map_err(|e| Error::DraftFailed {
                branch: branch.clone(),
                cause: Box::new(map_error(e)),
            })?;
        let number = v["number"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| Error::Malformed("PR number".into()))?;
        let url = v["html_url"]
            .as_str()
            .filter(|s| s.starts_with("https://"))
            .ok_or_else(|| Error::Malformed("PR URL".into()))?
            .to_owned();
        if v["draft"] != true {
            return Err(Error::Malformed("created PR is not a draft".into()));
        }
        return Ok(DraftPull {
            number,
            url,
            branch,
            commit: commit.into(),
        });
    }
    Err(Error::RefExists)
}

pub async fn fast_forward_branch(
    api: &GitHubApi,
    repo: &Repository,
    branch: &str,
    expected_head: &str,
    commit: &str,
) -> Result<(), Error> {
    repo_check(repo)?;
    branch_check(branch)?;
    oid(expected_head)?;
    oid(commit)?;
    let target = api
        .git_get_commit(&repo.owner, &repo.name, commit)
        .await
        .map_err(map_error)?;
    let parents = target["parents"]
        .as_array()
        .ok_or_else(|| Error::Malformed("commit parents".into()))?;
    if parents.len() != 1 || parents[0]["sha"] != expected_head {
        return Err(Error::Invalid(
            "fix commit must have exactly the approved head as parent".into(),
        ));
    }
    let head = api
        .git_get_ref(&repo.owner, &repo.name, branch)
        .await
        .map_err(map_error)?;
    if head["object"]["sha"] != expected_head {
        return Err(Error::MovedHead);
    }
    ref_response(&head, branch, expected_head)?;
    let v = api
        .git_fast_forward_ref(&repo.owner, &repo.name, branch, commit)
        .await
        .map_err(map_error)?;
    ref_response(&v, branch, commit)
}
