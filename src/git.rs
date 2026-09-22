use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run git")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("git {} failed: {}", args.join(" "), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    Ok(git(repo, &["rev-parse", rev]).await?.trim().to_string())
}

pub async fn current_head(repo: &Path) -> Result<String> {
    rev_parse(repo, "HEAD").await
}

pub async fn tracked_dirty(repo: &Path) -> Result<bool> {
    Ok(
        !git(repo, &["status", "--porcelain", "--untracked-files=no"])
            .await?
            .trim()
            .is_empty(),
    )
}

pub async fn checkout_detached(repo: &Path, sha: &str) -> Result<()> {
    git(repo, &["checkout", "--detach", "--quiet", sha])
        .await
        .map(|_| ())
}

/// Ensure the working tree materializes the requested commit.
pub async fn materialize_head(repo: &Path, sha: &str) -> Result<()> {
    let current = current_head(repo).await?;
    if current != sha && !has_commit(repo, sha).await {
        fetch_sha(repo, sha).await?;
    }
    if tracked_dirty(repo).await? {
        bail!(
            "working tree has uncommitted changes to tracked files; commit or stash them so the reviewed tree matches {sha} (HEAD is {current})"
        );
    }
    if current != sha {
        checkout_detached(repo, sha).await?;
    }
    Ok(())
}

pub async fn is_repo(repo: &Path) -> bool {
    git(repo, &["rev-parse", "--is-inside-work-tree"])
        .await
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// `git cat-file -e <sha>` — is the object present locally?
pub async fn has_commit(repo: &Path, sha: &str) -> bool {
    git(repo, &["cat-file", "-e", sha]).await.is_ok()
}

/// Fetch a single sha from origin (shallow repos may lack the base).
pub async fn fetch_sha(repo: &Path, sha: &str) -> Result<()> {
    git(repo, &["fetch", "--no-tags", "--depth=1", "origin", sha])
        .await
        .map(|_| ())
}

/// `git diff --no-color --unified=3 base...head` (merge-base form);
/// falls back to two-dot `base head` when no merge-base exists (shallow clone).
pub async fn diff(repo: &Path, base: &str, head: &str) -> Result<String> {
    match git(
        repo,
        &[
            "diff",
            "--no-color",
            "--unified=3",
            "--end-of-options",
            &format!("{}...{}", base, head),
        ],
    )
    .await
    {
        Ok(d) => Ok(d),
        Err(e) => {
            tracing::warn!("three-dot diff failed ({e}); falling back to two-dot diff");
            git(
                repo,
                &[
                    "diff",
                    "--no-color",
                    "--unified=3",
                    "--end-of-options",
                    base,
                    head,
                ],
            )
            .await
        }
    }
}

/// Determine default base branch (tries main, then master).
pub async fn default_branch(repo: &Path) -> Result<String> {
    if git(repo, &["rev-parse", "--verify", "main"]).await.is_ok() {
        Ok("main".into())
    } else if git(repo, &["rev-parse", "--verify", "master"]).await.is_ok() {
        Ok("master".into())
    } else {
        bail!("could not determine default branch (neither main nor master found)")
    }
}

/// `git diff --no-color --unified=3 HEAD` (uncommitted working tree diff against HEAD).
pub async fn diff_uncommitted(repo: &Path) -> Result<String> {
    git(
        repo,
        &[
            "diff",
            "--no-color",
            "--unified=3",
            "--end-of-options",
            "HEAD",
        ],
    )
    .await
}

/// Compute patch-id from a raw diff string.
pub async fn patch_id_from_diff(repo: &Path, diff_text: &str) -> Result<String> {
    let mut child = Command::new("git")
        .args(["patch-id", "--stable"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("git patch-id")?;
    use tokio::io::AsyncWriteExt;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(diff_text.as_bytes())
        .await
        .context("feed diff to patch-id")?;
    drop(child.stdin.take());
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!(
            "git patch-id failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

/// `git patch-id --stable` of the base...head diff.
pub async fn patch_id(repo: &Path, base: &str, head: &str) -> Result<String> {
    let diff_text = diff(repo, base, head).await?;
    patch_id_from_diff(repo, &diff_text).await
}

/// Files changed between base...head, one `path status` per line.
pub async fn changed_files(repo: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let out = git(
        repo,
        &[
            "diff",
            "--no-color",
            "--name-status",
            &format!("{}...{}", base, head),
        ],
    )
    .await?;
    Ok(out.lines().map(|l| l.to_string()).collect())
}

/// Tree object id of `rev` — identifies exact content regardless of
/// commit metadata.
pub async fn tree_id(repo: &Path, rev: &str) -> Result<String> {
    rev_parse(repo, &format!("{rev}^{{tree}}")).await
}

fn exclude_pathspecs(exclude: &[String]) -> Vec<String> {
    exclude
        .iter()
        .filter(|g| !g.trim().is_empty())
        .map(|g| format!(":(exclude,glob){g}"))
        .collect()
}

/// `git grep -n -I -E` over tracked files at the checked-out head, honouring
/// exclusion globs. Output lines are `path:line:text`. A non-matching pattern
/// yields an empty string; an invalid pattern is an error.
pub async fn grep(
    repo: &Path,
    pattern: &str,
    path_glob: Option<&str>,
    exclude: &[String],
) -> Result<String> {
    let mut args: Vec<String> = vec![
        "grep".into(),
        "-n".into(),
        "-I".into(),
        "-E".into(),
        "--no-color".into(),
        "-e".into(),
        pattern.to_string(),
        "--".into(),
    ];
    match path_glob {
        Some(g) if !g.trim().is_empty() => args.push(format!(":(glob){g}")),
        _ => args.push(".".into()),
    }
    args.extend(exclude_pathspecs(exclude));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = Command::new("git")
        .args(&argv)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run git grep")?;
    match out.status.code() {
        Some(0) | Some(1) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        _ => bail!(
            "git grep failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// Tracked files at the checked-out head matching an optional glob, honouring
/// exclusion globs.
pub async fn ls_files(repo: &Path, glob: Option<&str>, exclude: &[String]) -> Result<Vec<String>> {
    let mut args: Vec<String> = vec!["ls-files".into(), "--".into()];
    match glob {
        Some(g) if !g.trim().is_empty() => args.push(format!(":(glob){g}")),
        _ => args.push(".".into()),
    }
    args.extend(exclude_pathspecs(exclude));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git(repo, &argv).await?;
    Ok(out.lines().map(|l| l.to_string()).collect())
}

pub fn repo_root(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
