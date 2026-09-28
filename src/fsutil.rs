//! Filesystem writes for Revera-owned files under the repository
//! (`.revera/`). The checked-out tree is PR content, so every directory on
//! the way and the target itself must not be a symlink; replacement is
//! atomic (temp file in the same directory, fsync, rename).

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Error unless `p` is absent or a real (non-symlink) directory.
fn check_dir(p: &Path) -> Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => {
            bail!("{} is a symlink; refusing to write through it", p.display())
        }
        Ok(m) if !m.is_dir() => bail!("{} exists and is not a directory", p.display()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("stat {}", p.display())),
    }
}

/// Ensure `root/rel_dir` exists as a real directory tree under `root`,
/// creating missing components without following symlinks.
pub fn ensure_dir(root: &Path, rel_dir: &Path) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    for comp in rel_dir.components() {
        match comp {
            std::path::Component::Normal(c) => cur.push(c),
            _ => bail!("{} is not a plain relative path", rel_dir.display()),
        }
        check_dir(&cur)?;
        if !cur.exists() {
            std::fs::create_dir(&cur).with_context(|| format!("create {}", cur.display()))?;
        }
    }
    Ok(cur)
}

/// Error when `p` exists as a symlink (or a non-file).
pub fn check_regular_target(p: &Path) -> Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => {
            bail!("{} is a symlink; refusing to replace it", p.display())
        }
        Ok(m) if !m.is_file() => bail!("{} exists and is not a regular file", p.display()),
        _ => Ok(()),
    }
}

/// Atomically replace `dir/name` with `bytes`. `dir` must already be a
/// checked real directory (see [`ensure_dir`]).
pub fn write_atomic_in(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    check_dir(dir)?;
    let target = dir.join(name);
    check_regular_target(&target)?;
    let tmp = dir.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    let res = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target).with_context(|| format!("replace {}", target.display()))?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Atomically write `root/rel` (e.g. `.revera/state.json`), refusing any
/// symlinked component.
pub fn write_repo_file(root: &Path, rel: &Path, bytes: &[u8]) -> Result<()> {
    let name = rel
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no file name", rel.display()))?;
    let dir = ensure_dir(root, rel.parent().unwrap_or(Path::new("")))?;
    write_atomic_in(&dir, name, bytes)
}

/// Write a caller-chosen output path (`--out`). Relative paths, and
/// absolute paths inside `root`, get the same symlink checks as state
/// files; other absolute paths are the operator's explicit choice, but the
/// final component must still not be a symlink.
pub fn write_output(root: &Path, out: &Path, bytes: &[u8]) -> Result<()> {
    if out.is_absolute() {
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        for r in [root, canon_root.as_path()] {
            if let Ok(rel) = out.strip_prefix(r) {
                return write_repo_file(r, rel, bytes);
            }
        }
        let dir = out.parent().context("output path has no parent")?;
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let name = out
            .file_name()
            .and_then(|n| n.to_str())
            .context("output path has no file name")?;
        return write_atomic_in(dir, name, bytes);
    }
    write_repo_file(root, out, bytes)
}
