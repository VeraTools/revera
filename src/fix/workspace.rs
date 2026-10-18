//! Controller-owned Git state, never a repository created by the agent.
//! Export and hashing use raw blobs, not checkout, attributes, filters or hooks.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MAX_TREE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BLOB_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("fix workspace operation failed: {0}")]
pub struct Error(String);
impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self(crate::redact::diagnostic(&format!("{error:#}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Modified,
    Deleted,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: Status,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub binary: bool,
    pub additions: u64,
    pub deletions: u64,
    pub size: u64,
    pub new_content: Option<String>,
    pub old_content: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Patch {
    pub files: Vec<FileChange>,
    pub unified: String,
}
#[derive(Debug, Clone)]
pub struct Workspace {
    pub tree: PathBuf,
    pub sha: String,
    git: PathBuf,
    objects: PathBuf,
    root: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    mode: u32,
    oid: String,
}

fn git_base() -> Command {
    let mut c = Command::new("git");
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    c.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.attributesFile=/dev/null",
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.safecrlf=false",
        "-c",
        "core.quotePath=true",
    ]);
    c.kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}
async fn output(mut c: Command, input: Option<&[u8]>) -> Result<Vec<u8>> {
    if input.is_some() {
        c.stdin(Stdio::piped());
    }
    let mut child = c.spawn()?;
    let stdin = child.stdin.take();
    let operation = async {
        let write = async {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(input).await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let ((), o) = tokio::try_join!(write, child.wait_with_output())?;
        ensure!(
            o.status.success(),
            "git failed: {}",
            crate::redact::diagnostic(&String::from_utf8_lossy(&o.stderr))
        );
        ensure!(
            o.stdout.len() <= 64 * 1024 * 1024,
            "Git output exceeded 64 MB"
        );
        Ok(o.stdout)
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), operation)
        .await
        .context("fix Git operation timed out")?
}
impl Workspace {
    fn cmd(&self) -> Command {
        let mut c = git_base();
        c.arg(format!("--git-dir={}", self.git.display()))
            .arg(format!("--work-tree={}", self.tree.display()));
        c.arg(format!("--attr-source={EMPTY_TREE}"));
        c.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.objects);
        c.env("GIT_INDEX_FILE", self.root.join("index"));
        c
    }
    async fn git(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut c = self.cmd();
        c.args(args);
        output(c, input).await
    }
    async fn entries(&self) -> Result<BTreeMap<String, Entry>> {
        let raw = self
            .git(&["ls-tree", "-rz", "--full-tree", &self.sha], None)
            .await?;
        let mut out = BTreeMap::new();
        for line in raw.split(|b| *b == 0).filter(|l| !l.is_empty()) {
            let line = std::str::from_utf8(line).context("non UTF-8 Git path")?;
            let (head, path) = line.split_once('\t').context("invalid Git tree record")?;
            ensure!(super::policy::valid_path(path), "unsafe path at pinned SHA");
            let parts: Vec<_> = head.split(' ').collect();
            ensure!(
                parts.len() == 3 && crate::git::is_oid(parts[2]),
                "invalid Git tree entry"
            );
            out.insert(
                path.into(),
                Entry {
                    mode: u32::from_str_radix(parts[0], 8)?,
                    oid: parts[2].into(),
                },
            );
        }
        ensure!(out.len() <= 100_000, "tree exceeds 100000 files");
        Ok(out)
    }
    async fn blob(&self, oid: &str) -> Result<Vec<u8>> {
        let size = self.git(&["cat-file", "-s", oid], None).await?;
        ensure!(
            std::str::from_utf8(&size)?.trim().parse::<u64>()? <= MAX_BLOB_BYTES,
            "blob exceeds 16 MB export limit"
        );
        self.git(&["cat-file", "blob", oid], None).await
    }
    /// Typed controller boundary, preserving the operation's redacted cause.
    pub async fn diff(&self) -> std::result::Result<Patch, Error> {
        self.diff_inner().await.map_err(Into::into)
    }

    async fn diff_inner(&self) -> Result<Patch> {
        let old = self.entries().await?;
        let mut new = BTreeMap::new();
        let mut contents = BTreeMap::new();
        let mut total = 0;
        for (path, mode, bytes) in scan(&self.tree)? {
            total += bytes.len() as u64;
            ensure!(total <= MAX_TREE_BYTES, "tree exceeds 256 MB");
            let hash = self
                .git(
                    &["hash-object", "-w", "--no-filters", "--stdin"],
                    Some(&bytes),
                )
                .await?;
            new.insert(
                path.clone(),
                Entry {
                    mode,
                    oid: std::str::from_utf8(&hash)?.trim().into(),
                },
            );
            contents.insert(path, bytes);
        }
        for (p, e) in &old {
            if e.mode == 0o160000 {
                let full = self.tree.join(p);
                if std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir())
                    && std::fs::read_dir(full)?.next().is_none()
                {
                    new.insert(p.clone(), e.clone());
                }
            }
        }
        self.git(&["read-tree", "--empty"], None).await?;
        let mut index = Vec::new();
        for (p, e) in &new {
            index.extend_from_slice(format!("{:o} {}\t{p}\0", e.mode, e.oid).as_bytes());
        }
        self.git(&["update-index", "-z", "--index-info"], Some(&index))
            .await?;
        let raw = self
            .git(
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "--binary",
                    "--full-index",
                    &self.sha,
                    "--",
                ],
                None,
            )
            .await?;
        let unified = String::from_utf8(raw).context("non UTF-8 diff")?;
        let stats = self
            .git(
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "--numstat",
                    "-z",
                    &self.sha,
                    "--",
                ],
                None,
            )
            .await?;
        let mut counts = BTreeMap::new();
        for s in stats.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let mut p = std::str::from_utf8(s)?.splitn(3, '\t');
            let a = p.next().context("numstat additions")?;
            let d = p.next().context("numstat deletions")?;
            let path = p.next().context("numstat path")?;
            counts.insert(
                path.to_string(),
                (a.parse().unwrap_or(0), d.parse().unwrap_or(0), a == "-"),
            );
        }
        let mut files = Vec::new();
        for path in old
            .keys()
            .chain(new.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let before = old.get(path);
            let after = new.get(path);
            if before == after {
                continue;
            }
            let old_bytes = match before {
                Some(e) if e.mode != 0o160000 => Some(self.blob(&e.oid).await?),
                _ => None,
            };
            let new_bytes = contents.get(path);
            let (additions, deletions, binary_stat) = counts.get(path).copied().unwrap_or_default();
            let binary = binary_stat
                || old_bytes
                    .as_ref()
                    .is_some_and(|b| b.contains(&0) || std::str::from_utf8(b).is_err())
                || new_bytes.is_some_and(|b| b.contains(&0) || std::str::from_utf8(b).is_err());
            files.push(FileChange {
                path: path.clone(),
                status: if before.is_none() {
                    Status::Added
                } else if after.is_none() {
                    Status::Deleted
                } else {
                    Status::Modified
                },
                old_mode: before.map(|e| e.mode),
                new_mode: after.map(|e| e.mode),
                binary,
                additions,
                deletions,
                size: new_bytes.map_or(0, |b| b.len() as u64),
                new_content: new_bytes.and_then(|b| String::from_utf8(b.clone()).ok()),
                old_content: old_bytes.and_then(|b| String::from_utf8(b).ok()),
            });
        }
        Ok(Patch { files, unified })
    }
    pub async fn apply_check(&self, patch: &Patch) -> std::result::Result<(), Error> {
        self.apply_check_inner(patch).await.map_err(Into::into)
    }

    async fn apply_check_inner(&self, patch: &Patch) -> Result<()> {
        ensure!(
            !patch.files.is_empty() && !patch.unified.is_empty(),
            "empty patch"
        );
        let index = self
            .root
            .join(format!("apply-index-{}", uuid::Uuid::new_v4().simple()));
        let result = async {
            let mut c = self.cmd();
            c.env("GIT_INDEX_FILE", &index)
                .args(["read-tree", &self.sha]);
            output(c, None).await?;
            let mut c = self.cmd();
            c.env("GIT_INDEX_FILE", &index).args([
                "apply",
                "--cached",
                "--check",
                "--whitespace=nowarn",
                "-",
            ]);
            output(c, Some(patch.unified.as_bytes())).await?;
            Ok(())
        }
        .await;
        let _ = std::fs::remove_file(index);
        result
    }
}

pub async fn prepare(
    checkout_git_dir: &Path,
    sha: &str,
    dir: &Path,
) -> std::result::Result<Workspace, Error> {
    prepare_inner(checkout_git_dir, sha, dir)
        .await
        .map_err(Into::into)
}

async fn prepare_inner(checkout_git_dir: &Path, sha: &str, dir: &Path) -> Result<Workspace> {
    ensure!(
        crate::git::is_oid(sha),
        "workspace SHA must be a full object ID"
    );
    ensure!(
        dir.is_absolute() && !dir.exists(),
        "workspace must be a new absolute directory"
    );
    let parent = dir.parent().context("workspace parent")?;
    crate::fsutil::ensure_dir(
        parent,
        Path::new(dir.file_name().context("workspace name")?),
    )?;
    let tree = crate::fsutil::ensure_dir(dir, Path::new("tree"))?;
    let root = crate::fsutil::ensure_dir(dir, Path::new("control"))?;
    let git = root.join("git");
    let source = checkout_git_dir.canonicalize()?;
    let mut c = git_base();
    c.arg(format!("--git-dir={}", source.display())).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ]);
    let objects =
        PathBuf::from(std::str::from_utf8(&output(c, None).await?)?.trim()).canonicalize()?;
    let mut c = git_base();
    c.args(["init", "--bare", "--template="]).arg(&git);
    output(c, None).await?;
    let ws = Workspace {
        tree,
        sha: sha.into(),
        git,
        objects,
        root,
    };
    ws.git(&["cat-file", "-e", &format!("{sha}^{{commit}}")], None)
        .await?;
    // Ensure the empty tree exists even in repositories that never had one.
    ws.git(&["hash-object", "-w", "-t", "tree", "--stdin"], Some(&[]))
        .await?;
    let mut total = 0;
    for (path, e) in ws.entries().await? {
        if e.mode == 0o160000 {
            crate::fsutil::ensure_dir(&ws.tree, Path::new(&path))?;
            continue;
        }
        let bytes = ws.blob(&e.oid).await?;
        total += bytes.len() as u64;
        ensure!(total <= MAX_TREE_BYTES, "tree exceeds 256 MB export limit");
        match e.mode {
            0o100644 | 0o100755 => {
                crate::fsutil::write_repo_file(&ws.tree, Path::new(&path), &bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        ws.tree.join(path),
                        std::fs::Permissions::from_mode(e.mode & 0o777),
                    )?;
                }
            }
            #[cfg(unix)]
            0o120000 => {
                use std::os::unix::{ffi::OsStrExt, fs::symlink};
                let parent = Path::new(&path).parent().unwrap();
                crate::fsutil::ensure_dir(&ws.tree, parent)?;
                symlink(std::ffi::OsStr::from_bytes(&bytes), ws.tree.join(path))?;
            }
            _ => bail!("unsupported tree mode"),
        }
    }
    Ok(ws)
}

/// Snapshot without following symlinks or interpreting nested Git repositories.
fn scan(root: &Path) -> Result<Vec<(String, u32, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    let mut count = 0;
    let mut total = 0;
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            count += 1;
            ensure!(count <= 100_000, "tree exceeds 100000 entries");
            if e.file_name().eq_ignore_ascii_case(".git") {
                continue;
            }
            let full = e.path();
            let path = full
                .strip_prefix(root)?
                .to_str()
                .context("non UTF-8 path")?
                .to_owned();
            ensure!(super::policy::valid_path(&path), "unsafe workspace path");
            let m = std::fs::symlink_metadata(&full)?;
            if m.is_dir() {
                dirs.push(full);
                continue;
            }
            let (mode, bytes) = if m.is_file() {
                ensure!(
                    m.len() <= MAX_BLOB_BYTES,
                    "file exceeds 16 MB snapshot limit"
                );
                let mut mode = 0o100644;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if m.permissions().mode() & 0o111 != 0 {
                        mode = 0o100755;
                    }
                }
                (mode, std::fs::read(full)?)
            } else if m.file_type().is_symlink() {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    (
                        0o120000,
                        std::fs::read_link(full)?.as_os_str().as_bytes().to_vec(),
                    )
                }
                #[cfg(not(unix))]
                {
                    bail!("symlinks unsupported");
                }
            } else {
                bail!("special files forbidden in workspace");
            };
            total += bytes.len() as u64;
            ensure!(total <= MAX_TREE_BYTES, "tree exceeds 256 MB");
            out.push((path, mode, bytes));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// A disposable test tree: no Git metadata; symlinks are preserved, never followed.
pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    ensure!(!destination.exists(), "test copy already exists");
    crate::fsutil::ensure_dir(
        destination.parent().context("copy parent")?,
        Path::new(destination.file_name().context("copy name")?),
    )?;
    for (path, mode, bytes) in scan(source)? {
        if mode == 0o120000 {
            #[cfg(unix)]
            {
                use std::os::unix::{ffi::OsStrExt, fs::symlink};
                crate::fsutil::ensure_dir(destination, Path::new(&path).parent().unwrap())?;
                symlink(std::ffi::OsStr::from_bytes(&bytes), destination.join(path))?;
            }
        } else {
            crate::fsutil::write_repo_file(destination, Path::new(&path), &bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    destination.join(path),
                    std::fs::Permissions::from_mode(mode & 0o777),
                )?;
            }
        }
    }
    Ok(())
}
