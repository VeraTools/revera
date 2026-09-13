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

pub async fn is_repo(repo: &Path) -> bool {
    git(repo, &["rev-parse", "--is-inside-work-tree"])
        .await
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// `git diff --no-color --unified=3 base...head` (merge-base form).
pub async fn diff(repo: &Path, base: &str, head: &str) -> Result<String> {
    git(
        repo,
        &[
            "diff",
            "--no-color",
            "--unified=3",
            &format!("{}...{}", base, head),
        ],
    )
    .await
}

/// `git patch-id --stable` of the base...head diff.
pub async fn patch_id(repo: &Path, base: &str, head: &str) -> Result<String> {
    let diff_text = git(
        repo,
        &["diff", "--no-color", &format!("{}...{}", base, head)],
    )
    .await?;
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

pub fn repo_root(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
