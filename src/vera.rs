use crate::config::{VeraBackend, VeraConfig};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Per-invocation cap for query subcommands (search/grep/references).
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct VeraClient {
    pub exe: PathBuf,
    pub repo_root: PathBuf,
    pub env: Vec<(String, String)>,
    pub backend: String,
    pub exclude: Vec<String>,
    /// Every subprocess is bounded by the remaining time to this deadline;
    /// a timed-out child is killed and reaped.
    pub deadline: Option<Instant>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VeraCacheInfo {
    pub vera_version: String,
    pub backend: String,
    pub embedding_model: String,
    pub dim: Option<u32>,
    pub updated_at: String,
}

impl VeraClient {
    /// A client for `vera.enabled = false`: no credentials are resolved and
    /// no executable is required; every call fails fast.
    pub fn disabled(repo_root: &Path) -> Self {
        Self {
            exe: PathBuf::from("vera"),
            repo_root: repo_root.to_path_buf(),
            env: vec![],
            backend: "disabled".into(),
            exclude: vec![],
            deadline: None,
        }
    }

    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    pub fn from_config(cfg: &VeraConfig, repo_root: &Path) -> Result<Self> {
        if !cfg.enabled {
            let mut c = Self::disabled(repo_root);
            c.exclude = cfg.exclude.clone();
            return Ok(c);
        }
        let mut env: Vec<(String, String)> = vec![
            ("VERA_NO_UPDATE_CHECK".into(), "1".into()),
            (
                "VERA_HOME".into(),
                std::env::var("VERA_HOME").unwrap_or_else(|_| {
                    format!("{}/.vera", std::env::var("HOME").unwrap_or_default())
                }),
            ),
        ];
        let backend = match cfg.backend {
            VeraBackend::Api => "api",
            VeraBackend::Local => "local",
        }
        .to_string();
        if cfg.backend == VeraBackend::Api {
            env.push(("VERA_BACKEND".into(), "api".into()));
            if let Some(e) = &cfg.embedding {
                env.push(("EMBEDDING_MODEL_BASE_URL".into(), e.base_url.clone()));
                env.push(("EMBEDDING_MODEL_ID".into(), e.model.clone()));
                let key = std::env::var(&e.api_key_env)
                    .ok()
                    .filter(|k| !k.trim().is_empty())
                    .with_context(|| {
                        format!("vera.embedding.api_key_env {} is not set", e.api_key_env)
                    })?;
                env.push(("EMBEDDING_MODEL_API_KEY".into(), key));
            }
            if let Some(r) = &cfg.reranker {
                env.push(("RERANKER_MODEL_BASE_URL".into(), r.base_url.clone()));
                env.push(("RERANKER_MODEL_ID".into(), r.model.clone()));
                let key = std::env::var(&r.api_key_env)
                    .ok()
                    .filter(|k| !k.trim().is_empty())
                    .with_context(|| {
                        format!("vera.reranker.api_key_env {} is not set", r.api_key_env)
                    })?;
                env.push(("RERANKER_MODEL_API_KEY".into(), key));
            }
        }
        Ok(Self {
            exe: PathBuf::from(&cfg.executable),
            repo_root: repo_root.to_path_buf(),
            env,
            backend,
            exclude: cfg.exclude.clone(),
            deadline: None,
        })
    }

    /// Time left before the run deadline, capped by `cap`. `None` when the
    /// deadline has already passed.
    fn time_left(&self, cap: Option<Duration>) -> Option<Duration> {
        let left = match self.deadline {
            Some(d) => d.checked_duration_since(Instant::now())?,
            None => Duration::from_secs(24 * 3600),
        };
        Some(match cap {
            Some(c) => left.min(c),
            None => left,
        })
    }

    async fn run_bounded(&self, args: &[&str], cap: Option<Duration>) -> Result<String> {
        if self.backend == "disabled" {
            bail!("vera is disabled by config");
        }
        let Some(limit) = self.time_left(cap) else {
            bail!("vera {}: run time budget exhausted", args.join(" "));
        };
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .current_dir(&self.repo_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to run {} {}", self.exe.display(), args.join(" ")))?;
        let out = match tokio::time::timeout(limit, child.wait_with_output()).await {
            Ok(r) => r.with_context(|| format!("vera {} failed", args.join(" ")))?,
            Err(_) => {
                // the child is killed and reaped by `kill_on_drop` when the
                // dropped future releases it
                bail!(
                    "vera {} timed out after {}s",
                    args.join(" "),
                    limit.as_secs()
                );
            }
        };
        if !out.status.success() {
            let tail = String::from_utf8_lossy(&out.stderr);
            let tail: String = tail
                .chars()
                .rev()
                .take(500)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            bail!("vera {} failed: {}", args.join(" "), tail.trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Query subcommand: bounded by the deadline and `QUERY_TIMEOUT`.
    async fn run(&self, args: &[&str]) -> Result<String> {
        self.run_bounded(args, Some(QUERY_TIMEOUT)).await
    }

    async fn run_json(&self, args: &[&str]) -> Result<Value> {
        let out = self.run(args).await?;
        serde_json::from_str(&out)
            .with_context(|| format!("vera {}: invalid JSON output", args.join(" ")))
    }

    async fn run_json_index(&self, args: &[&str]) -> Result<Value> {
        let out = self.run_bounded(args, None).await?;
        serde_json::from_str(&out)
            .with_context(|| format!("vera {}: invalid JSON output", args.join(" ")))
    }

    pub async fn version(&self) -> Result<String> {
        if self.backend == "disabled" {
            bail!("vera is disabled by config");
        }
        let out = tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(&self.exe)
                .arg("--version")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("vera --version timed out"))?
        .context("vera not found")?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // "vera 1.4.1" -> "1.4.1"
        Ok(s.split_whitespace().last().unwrap_or(&s).to_string())
    }

    fn embedding_model(&self) -> String {
        self.env
            .iter()
            .find(|(k, _)| k == "EMBEDDING_MODEL_ID")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "local".into())
    }

    /// Why an existing `.vera` index cannot be reused with this
    /// configuration, if it cannot. Missing cache info (legacy index) is
    /// tolerated; a mismatching backend or embedding model is not.
    pub fn cache_incompatibility(&self) -> Option<String> {
        let p = Self::cache_info_path(&self.repo_root);
        let text = std::fs::read_to_string(&p).ok()?;
        let info: VeraCacheInfo = match serde_json::from_str(&text) {
            Ok(i) => i,
            Err(e) => return Some(format!("unreadable vera-cache.json: {e}")),
        };
        if info.backend != self.backend {
            return Some(format!(
                "backend changed {} -> {}",
                info.backend, self.backend
            ));
        }
        let want = self.embedding_model();
        if info.embedding_model != want {
            return Some(format!(
                "embedding model changed {} -> {}",
                info.embedding_model, want
            ));
        }
        None
    }

    /// Index (or incrementally update) the repository. An existing index
    /// built with a different backend/embedding model is discarded first;
    /// cache info is written only after a successful index.
    pub async fn ensure_index(&self) -> Result<Value> {
        let index_dir = self.repo_root.join(".vera");
        let mut indexed = index_dir.exists();
        if indexed {
            if let Some(why) = self.cache_incompatibility() {
                tracing::warn!("discarding incompatible vera index: {why}");
                std::fs::remove_dir_all(&index_dir)
                    .with_context(|| format!("remove {}", index_dir.display()))?;
                let _ = std::fs::remove_file(Self::cache_info_path(&self.repo_root));
                indexed = false;
            }
        }
        let verb = if indexed { "update" } else { "index" };
        let mut args = vec![verb, ".", "--json"];
        for e in &self.exclude {
            args.push("--exclude");
            args.push(e);
        }
        let summary = self.run_json_index(&args).await?;
        if !index_dir.exists() {
            bail!(
                "vera {verb} reported success but {} is missing",
                index_dir.display()
            );
        }
        self.write_cache_info().await.ok();
        Ok(summary)
    }

    pub async fn write_cache_info(&self) -> Result<()> {
        let version = self.version().await.unwrap_or_default();
        let manifest = self.repo_root.join(".vera/vectors.manifest");
        let dim = std::fs::read_to_string(&manifest).ok().and_then(|t| {
            serde_json::from_str::<Value>(&t)
                .ok()
                .and_then(|v| v["dim"].as_u64().map(|d| d as u32))
                .or_else(|| {
                    // manifest may be text "dim=768" or similar
                    t.split(|c: char| !c.is_ascii_digit() && c != '=')
                        .find_map(|tok| tok.strip_prefix("dim=").and_then(|n| n.parse().ok()))
                })
        });
        let embedding_model = self.embedding_model();
        let info = VeraCacheInfo {
            vera_version: version,
            backend: self.backend.clone(),
            embedding_model,
            dim,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        let dir = self.repo_root.join(".revera");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("vera-cache.json"),
            serde_json::to_string_pretty(&info)?,
        )?;
        Ok(())
    }

    pub fn cache_info_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".revera/vera-cache.json")
    }

    pub async fn search(
        &self,
        query: &str,
        intent: Option<&str>,
        path_glob: Option<&str>,
        lang: Option<&str>,
        limit: u32,
    ) -> Result<Value> {
        let mut args: Vec<String> = vec!["search".into(), query.into()];
        if let Some(i) = intent {
            args.extend(["--intent".into(), i.into()]);
        }
        if let Some(p) = path_glob {
            args.extend(["--path".into(), p.into()]);
        }
        if let Some(l) = lang {
            args.extend(["--lang".into(), l.into()]);
        }
        args.extend(["-n".into(), limit.to_string(), "--json".into()]);
        self.run_json(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .await
    }

    pub async fn references(&self, symbol: &str, callees: bool, limit: u32) -> Result<Value> {
        let mut args = vec!["references".to_string(), symbol.to_string()];
        if callees {
            args.push("--callees".into());
        }
        args.extend(["-n".into(), limit.to_string(), "--json".into()]);
        self.run_json(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .await
    }

    pub async fn grep(&self, pattern: &str, path_glob: Option<&str>, limit: u32) -> Result<Value> {
        let mut args = vec!["grep".to_string(), pattern.to_string()];
        if let Some(p) = path_glob {
            args.extend(["--path".into(), p.into()]);
        }
        args.extend(["-n".into(), limit.to_string(), "--json".into()]);
        self.run_json(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .await
    }

    pub async fn overview(&self) -> Result<Value> {
        self.run_json(&["overview", "--json"]).await
    }
}
