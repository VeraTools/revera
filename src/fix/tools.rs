//! No shell, Git, plugins, or repository configuration in a fix agent.
use super::policy::{MAX_FILE_BYTES, sensitive, valid_path};
use crate::agent::AgentTools;
use crate::provider::ToolSpec;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct FixTools {
    root: PathBuf,
    writable: bool,
    lock: Mutex<()>,
}
impl FixTools {
    pub fn new(root: &Path, writable: bool) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !std::fs::symlink_metadata(root)?.file_type().is_symlink(),
            "workspace is a symlink"
        );
        Ok(Self {
            root: root.canonicalize()?,
            writable,
            lock: Mutex::new(()),
        })
    }
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        if !valid_path(path) || sensitive(path) {
            return Err("path forbidden by content policy".into());
        }
        let mut full = self.root.clone();
        for c in path.split('/') {
            full.push(c);
            match std::fs::symlink_metadata(&full) {
                Ok(m) if m.file_type().is_symlink() => return Err("symlinks are forbidden".into()),
                Ok(m) if !m.is_file() && !m.is_dir() => {
                    return Err("not a regular file/directory".into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
                _ => {}
            }
        }
        Ok(full)
    }
    fn read(&self, path: &str) -> Result<String, String> {
        let full = self.resolve(path)?;
        let f = std::fs::File::open(full).map_err(|e| e.to_string())?;
        use std::io::Read;
        if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES as u64 {
            return Err("file exceeds 1 MB".into());
        }
        let mut b = Vec::new();
        f.take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut b)
            .map_err(|e| e.to_string())?;
        if b.len() > MAX_FILE_BYTES {
            return Err("file exceeds 1 MB".into());
        }
        String::from_utf8(b).map_err(|_| "not UTF-8 text".into())
    }
    fn files(&self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let mut dirs = vec![self.root.clone()];
        let mut seen = 0;
        while let Some(dir) = dirs.pop() {
            for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                seen += 1;
                if seen > 20_000 {
                    return Err("tree exceeds 20000 entries".into());
                }
                let full = e.path();
                let p = full
                    .strip_prefix(&self.root)
                    .unwrap()
                    .to_str()
                    .ok_or("non UTF-8 path")?;
                if self.resolve(p).is_err() {
                    continue;
                }
                let kind = e.file_type().map_err(|e| e.to_string())?;
                if kind.is_dir() {
                    dirs.push(full);
                } else if kind.is_file() {
                    out.push(p.into());
                }
            }
        }
        out.sort();
        Ok(out)
    }
    fn call_inner(&self, name: &str, a: &Value) -> Result<String, String> {
        let _guard = self.lock.lock().unwrap();
        let path = || a["path"].as_str().ok_or_else(|| "missing path".to_string());
        let string = |key: &str| a[key].as_str().ok_or_else(|| format!("missing {key}"));
        match name {
            "read_file" => {
                let s = self.read(path()?)?;
                let start = a["start_line"].as_u64().unwrap_or(1).max(1) as usize;
                Ok(s.lines()
                    .enumerate()
                    .skip(start - 1)
                    .take(400)
                    .map(|(i, l)| format!("{}| {l}\n", i + 1))
                    .collect())
            }
            "list_dir" => {
                let p = a["path"].as_str().unwrap_or("");
                let full = if p.is_empty() {
                    self.root.clone()
                } else {
                    self.resolve(p)?
                };
                let mut names = Vec::new();
                for e in std::fs::read_dir(full)
                    .map_err(|e| e.to_string())?
                    .take(1001)
                {
                    let e = e.map_err(|e| e.to_string())?;
                    let ep = e.path();
                    let rel = ep
                        .strip_prefix(&self.root)
                        .unwrap()
                        .to_str()
                        .ok_or("non UTF-8 path")?;
                    if self.resolve(rel).is_ok() {
                        names.push(rel.to_string());
                    }
                }
                names.sort();
                names.truncate(200);
                Ok(names.join("\n"))
            }
            "find_files" => {
                let glob = globset::Glob::new(string("glob")?)
                    .map_err(|e| e.to_string())?
                    .compile_matcher();
                Ok(self
                    .files()?
                    .into_iter()
                    .filter(|p| glob.is_match(p))
                    .take(200)
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "grep" => {
                let pat = string("pattern")?;
                if pat.len() > 1000 {
                    return Err("pattern too large".into());
                }
                let re = regex::RegexBuilder::new(pat)
                    .size_limit(1024 * 1024)
                    .build()
                    .map_err(|e| e.to_string())?;
                let mut out = String::new();
                let mut count = 0;
                let mut bytes = 0;
                for p in self.files()? {
                    let Ok(s) = self.read(&p) else {
                        continue;
                    };
                    bytes += s.len();
                    if bytes > 16 * 1024 * 1024 {
                        break;
                    }
                    for (i, l) in s.lines().enumerate() {
                        if re.is_match(l) {
                            out.push_str(&format!(
                                "{p}:{}:{}\n",
                                i + 1,
                                crate::text::excerpt_bytes(l, 1000)
                            ));
                            count += 1;
                        }
                        if count >= 200 || out.len() >= 64 * 1024 {
                            return Ok(out);
                        }
                    }
                }
                Ok(out)
            }
            "write_file" | "replace_in_file" | "delete_file" => {
                if !self.writable {
                    return Err("read-only verifier".into());
                }
                let p = path()?;
                let full = self.resolve(p)?;
                if name == "delete_file" {
                    std::fs::remove_file(full).map_err(|e| e.to_string())?;
                    return Ok("deleted".into());
                }
                let content = if name == "write_file" {
                    string("content")?.to_owned()
                } else {
                    let old = string("old")?;
                    let new = string("new")?;
                    let s = self.read(p)?;
                    if old.is_empty()
                        || s.as_bytes()
                            .windows(old.len())
                            .filter(|w| *w == old.as_bytes())
                            .count()
                            != 1
                    {
                        return Err("old must match exactly once".into());
                    }
                    s.replacen(old, new, 1)
                };
                if content.len() > MAX_FILE_BYTES || content.contains('\0') {
                    return Err("content exceeds 1 MB or contains NUL".into());
                }
                if crate::redact::text(&content) != content {
                    return Err("content contains credential material".into());
                }
                let mode = std::fs::metadata(&full).ok().map(|m| m.permissions());
                crate::fsutil::write_repo_file(&self.root, Path::new(p), content.as_bytes())
                    .map_err(|e| e.to_string())?;
                if let Some(mode) = mode {
                    std::fs::set_permissions(full, mode).map_err(|e| e.to_string())?;
                }
                Ok("written".into())
            }
            _ => Err("unknown or unavailable tool".into()),
        }
    }
}
#[async_trait::async_trait]
impl AgentTools for FixTools {
    fn specs(&self) -> Vec<ToolSpec> {
        let mut all = vec![
            (
                "read_file",
                "Read at most 400 numbered lines",
                json!({"path":{"type":"string"},"start_line":{"type":"integer"}}),
                vec!["path"],
            ),
            (
                "list_dir",
                "List a directory (empty path means root)",
                json!({"path":{"type":"string"}}),
                vec![],
            ),
            (
                "find_files",
                "Find files by glob",
                json!({"glob":{"type":"string"}}),
                vec!["glob"],
            ),
            (
                "grep",
                "Bounded Rust regex search",
                json!({"pattern":{"type":"string"}}),
                vec!["pattern"],
            ),
        ];
        if self.writable {
            all.extend([
            ("write_file", "Write a UTF-8 file, at most 1 MB", json!({"path":{"type":"string"},"content":{"type":"string"}}), vec!["path","content"]),
            ("replace_in_file", "Replace one exact unique match", json!({"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}}), vec!["path","old","new"]),
            ("delete_file", "Delete a regular file", json!({"path":{"type":"string"}}), vec!["path"]),
        ]);
        }
        all.into_iter().map(|(name, description, properties, required)| ToolSpec { name: name.into(), description: description.into(), parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}) }).collect()
    }
    async fn call(&self, name: &str, args: Value) -> String {
        match self.call_inner(name, &args) {
            Ok(s) => crate::text::excerpt_bytes(&crate::redact::text(&s), 64 * 1024),
            Err(e) => json!({"error":crate::redact::text(&e)}).to_string(),
        }
    }
}
