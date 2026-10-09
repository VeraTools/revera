use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Wall-clock bound for any single git subprocess.
const GIT_TIMEOUT: Duration = Duration::from_secs(300);
/// Output bound for any single git subprocess (stdout).
const GIT_MAX_OUTPUT: usize = 256 * 1024 * 1024;

/// A git command that never prompts, never runs repository-configured
/// external diff/textconv drivers or fsmonitor hooks, and ignores the
/// user's global/system config for those knobs.
fn git_cmd(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.external=",
            "-c",
            "core.pager=cat",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}

/// Run a git command with bounded time and output; returns (status, stdout, stderr).
async fn run_bounded(mut cmd: Command, what: &str) -> Result<(Option<i32>, Vec<u8>, String)> {
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to run {what}"))?;
    let mut stdout = child.stdout.take().context("git stdout")?;
    let mut stderr = child.stderr.take().context("git stderr")?;
    let work = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read_out = async {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 {
                    return Ok::<bool, std::io::Error>(false);
                }
                if out.len() + n > GIT_MAX_OUTPUT {
                    return Ok(true);
                }
                out.extend_from_slice(&buf[..n]);
            }
        };
        let read_err = async {
            let mut buf = Vec::new();
            (&mut stderr).take(64 * 1024).read_to_end(&mut buf).await?;
            err = buf;
            Ok::<(), std::io::Error>(())
        };
        let (over, _) = tokio::try_join!(read_out, read_err)?;
        Ok::<(bool, Vec<u8>, Vec<u8>), std::io::Error>((over, out, err))
    };
    let (over, out, err) = match tokio::time::timeout(GIT_TIMEOUT, work).await {
        Ok(r) => r.with_context(|| format!("{what}: reading output"))?,
        Err(_) => {
            let _ = child.kill().await;
            bail!("{what} timed out after {}s", GIT_TIMEOUT.as_secs());
        }
    };
    if over {
        let _ = child.kill().await;
        bail!("{what} output exceeds {} bytes", GIT_MAX_OUTPUT);
    }
    let status = child.wait().await?;
    Ok((
        status.code(),
        out,
        String::from_utf8_lossy(&err).trim().to_string(),
    ))
}

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = git_cmd(repo);
    cmd.args(args);
    let what = format!("git {}", args.first().copied().unwrap_or(""));
    let (code, out, err) = run_bounded(cmd, &what).await?;
    if code != Some(0) {
        bail!("git {} failed: {}", args.join(" "), err);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Full hex object id (SHA-1 or SHA-256).
pub fn is_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub async fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    let out = git(repo, &["rev-parse", "--verify", "--end-of-options", rev])
        .await?
        .trim()
        .to_string();
    if !is_oid(&out) {
        bail!("git rev-parse {rev}: unexpected output {out:?}");
    }
    Ok(out)
}

/// Resolve `rev` to a validated commit id (option-terminated, so a
/// revision like `--output=x` is never parsed as a flag).
pub async fn resolve_commit(repo: &Path, rev: &str) -> Result<String> {
    rev_parse(repo, &format!("{rev}^{{commit}}")).await
}

pub async fn is_shallow(repo: &Path) -> bool {
    git(repo, &["rev-parse", "--is-shallow-repository"])
        .await
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// Content of a regular tracked file at `rev` (never a symlink, submodule
/// or tree). `Ok(None)` when absent or not a regular file; errors when
/// larger than `max_bytes`.
pub async fn read_blob(
    repo: &Path,
    rev: &str,
    path: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    if !is_oid(rev) {
        bail!("read_blob: {rev:?} is not a resolved object id");
    }
    let listing = git(repo, &["ls-tree", "-z", "--full-tree", rev, "--", path]).await?;
    let entry = listing.split('\0').find(|e| !e.is_empty());
    let Some(entry) = entry else {
        return Ok(None);
    };
    let (meta, name) = entry.split_once('\t').context("malformed ls-tree output")?;
    if name != path {
        return Ok(None);
    }
    let mut parts = meta.split_whitespace();
    let (mode, kind, oid) = (parts.next(), parts.next(), parts.next());
    if kind != Some("blob") || !matches!(mode, Some("100644") | Some("100755")) {
        return Ok(None);
    }
    let oid = oid.context("malformed ls-tree output")?;
    let size: usize = git(repo, &["cat-file", "-s", oid]).await?.trim().parse()?;
    if size > max_bytes {
        bail!("{path} at {rev} is {size} bytes (limit {max_bytes})");
    }
    let mut cmd = git_cmd(repo);
    cmd.args(["cat-file", "blob", oid]);
    let (code, out, err) = run_bounded(cmd, "git cat-file").await?;
    if code != Some(0) {
        bail!("git cat-file {oid} failed: {err}");
    }
    Ok(Some(out))
}

/// Regular tracked files in the tree at `rev` (matching `read_blob` modes).
pub async fn tracked_files(repo: &Path, rev: &str) -> Result<HashSet<String>> {
    if !is_oid(rev) {
        bail!("tracked_files: {rev:?} is not a resolved object id");
    }
    let listing = git(repo, &["ls-tree", "-r", "-z", "--full-tree", rev]).await?;
    let mut files = HashSet::new();
    for entry in listing.split('\0').filter(|entry| !entry.is_empty()) {
        let (meta, path) = entry.split_once('\t').context("malformed ls-tree output")?;
        let mut parts = meta.split_whitespace();
        let (mode, kind) = (parts.next(), parts.next());
        if kind == Some("blob") && matches!(mode, Some("100644") | Some("100755")) {
            files.insert(path.to_string());
        }
    }
    Ok(files)
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
    if !is_oid(sha) {
        bail!("refusing to check out {sha:?}: not a full object id");
    }
    git(repo, &["checkout", "--detach", "--quiet", sha, "--"])
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
    is_oid(sha)
        && git(repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .await
            .is_ok()
}

/// Fetch a single sha from origin (shallow repos may lack the base).
pub async fn fetch_sha(repo: &Path, sha: &str) -> Result<()> {
    if !is_oid(sha) {
        bail!("refusing to fetch {sha:?}: not a full object id");
    }
    git(
        repo,
        &["fetch", "--no-tags", "--depth=1", "origin", "--", sha],
    )
    .await
    .map(|_| ())
}

/// Merge base of `base` and `head`. In a shallow clone a missing merge
/// base is fetched (deepening, then unshallowing) instead of silently
/// changing the diff semantics.
pub async fn merge_base(repo: &Path, base: &str, head: &str) -> Result<String> {
    let find = || async {
        git(repo, &["merge-base", "--end-of-options", base, head])
            .await
            .map(|s| s.trim().to_string())
    };
    if let Ok(mb) = find().await
        && is_oid(&mb)
    {
        return Ok(mb);
    }
    if is_shallow(repo).await {
        for deepen in ["--deepen=200", "--unshallow"] {
            tracing::info!("merge base of {base}...{head} missing; fetching history ({deepen})");
            if git(repo, &["fetch", "--no-tags", deepen, "origin"])
                .await
                .is_err()
            {
                continue;
            }
            if let Ok(mb) = find().await
                && is_oid(&mb)
            {
                return Ok(mb);
            }
        }
    }
    bail!(
        "no merge base between {base} and {head}; cannot compute the pull request diff (fetch full history, e.g. actions/checkout fetch-depth: 0)"
    )
}

/// `git diff base...head` (merge-base form) with external diff and
/// textconv drivers disabled. Never degrades to a two-dot diff.
pub async fn diff(repo: &Path, base: &str, head: &str) -> Result<String> {
    let mb = merge_base(repo, base, head).await?;
    git(
        repo,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--unified=3",
            "--end-of-options",
            &mb,
            head,
        ],
    )
    .await
}

/// `git patch-id --stable` of the base...head diff.
pub async fn patch_id(repo: &Path, base: &str, head: &str) -> Result<String> {
    let diff_text = diff(repo, base, head).await?;
    let mut child = git_cmd(repo)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
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
    let mut cmd = git_cmd(repo);
    cmd.args(&args);
    let (code, out, err) = run_bounded(cmd, "git grep").await?;
    match code {
        Some(0) | Some(1) => Ok(String::from_utf8_lossy(&out).into_owned()),
        _ => bail!("git grep failed: {err}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tracked_files_lists_only_regular_files_at_the_requested_revision() {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("tracked file.rs"), "tracked\n").unwrap();
        symlink("tracked file.rs", repo.path().join("link.rs")).unwrap();
        run_git(repo.path(), &["add", "--", "."]);
        run_git(
            repo.path(),
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "base",
            ],
        );
        let head = resolve_commit(repo.path(), "HEAD").await.unwrap();
        std::fs::write(repo.path().join("untracked.rs"), "not in head\n").unwrap();

        let files = tracked_files(repo.path(), &head).await.unwrap();

        assert_eq!(files, HashSet::from(["tracked file.rs".to_string()]));
    }
}
