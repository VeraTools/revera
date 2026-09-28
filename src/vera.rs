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
/// Cap on captured Vera stdout (JSON results).
const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// Ambient variables Vera reads that Revera always controls itself; they
/// are removed from the child environment before Revera's own are set.
/// Platform, proxy and certificate settings pass through untouched.
fn is_vera_controlled(key: &str) -> bool {
    key.starts_with("VERA_")
        || key.starts_with("EMBEDDING_MODEL_")
        || key.starts_with("RERANKER_MODEL_")
}

/// Reranker state as observed by Revera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RerankState {
    /// No reranker configured; reranking explicitly disabled in Vera.
    #[default]
    Off,
    /// Configured and activated in the isolated Vera home.
    Enabled,
    /// Configured but activation failed; retrieval runs unreranked.
    Degraded,
}

/// `vera config set` values applied to the Revera-owned Vera home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankSettings {
    pub protocol: Option<&'static str>,
    pub endpoint_path: Option<String>,
    pub return_documents: Option<crate::config::ReturnDocuments>,
}

impl RerankSettings {
    /// `(key, value)` pairs for `vera config set`; `null` clears a key.
    pub fn config_pairs(this: Option<&Self>) -> Vec<(&'static str, String)> {
        let mut v = vec![("retrieval.reranking_enabled", this.is_some().to_string())];
        let (protocol, path, docs) = match this {
            Some(r) => (
                r.protocol.map(str::to_string),
                r.endpoint_path.clone(),
                r.return_documents,
            ),
            None => (None, None, None),
        };
        v.push((
            "retrieval.reranker_protocol",
            protocol.unwrap_or_else(|| "null".into()),
        ));
        v.push((
            "retrieval.reranker_endpoint_path",
            path.unwrap_or_else(|| "null".into()),
        ));
        v.push((
            "retrieval.reranker_return_documents",
            crate::config::ReturnDocuments::config_value(docs).to_string(),
        ));
        v
    }
}

#[derive(Debug, Clone)]
pub struct VeraClient {
    pub exe: PathBuf,
    pub repo_root: PathBuf,
    pub env: Vec<(String, String)>,
    pub backend: String,
    pub exclude: Vec<String>,
    /// Revera-owned `VERA_HOME` (never the user's global Vera home).
    pub home: PathBuf,
    /// `Some` when a reranker is configured.
    pub rerank: Option<RerankSettings>,
    /// Hash of the index-shaping config; stored with the index.
    pub index_key: String,
    /// Searches where Vera fell back to unreranked results.
    pub rerank_fallbacks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
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
    /// [`VeraConfig::index_key`] the index was built with (absent in
    /// indexes written by older Revera).
    #[serde(default)]
    pub index_key: Option<String>,
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
            home: PathBuf::new(),
            rerank: None,
            index_key: String::new(),
            rerank_fallbacks: Default::default(),
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
            c.exclude = cfg.effective_excludes();
            return Ok(c);
        }
        let home = cfg.vera_home();
        let backend = match cfg.backend {
            VeraBackend::Api => "api",
            VeraBackend::Local => "local",
        }
        .to_string();
        let mut env: Vec<(String, String)> = vec![
            ("VERA_NO_UPDATE_CHECK".into(), "1".into()),
            ("VERA_HOME".into(), home.display().to_string()),
            (
                "VERA_BACKEND".into(),
                match cfg.backend {
                    VeraBackend::Api => "api".into(),
                    VeraBackend::Local => crate::config::LOCAL_VERA_BACKEND.into(),
                },
            ),
        ];
        let key = |what: &str, name: &str| {
            crate::redact::secret_env(name)
                .with_context(|| format!("vera.{what}.api_key_env {name} is not set"))
        };
        if cfg.backend == VeraBackend::Api {
            let e = cfg
                .embedding
                .as_ref()
                .context("vera.backend = api needs a vera.embedding endpoint")?;
            env.push(("EMBEDDING_MODEL_BASE_URL".into(), e.base_url.clone()));
            env.push(("EMBEDDING_MODEL_ID".into(), e.model.clone()));
            env.push((
                "EMBEDDING_MODEL_API_KEY".into(),
                key("embedding", &e.api_key_env)?,
            ));
        }
        // the reranker is independent of the embedding backend: local
        // embeddings with a remote reranker are supported by Vera
        let rerank = match &cfg.reranker {
            Some(r) => {
                env.push(("RERANKER_MODEL_BASE_URL".into(), r.base_url.clone()));
                env.push(("RERANKER_MODEL_ID".into(), r.model.clone()));
                env.push((
                    "RERANKER_MODEL_API_KEY".into(),
                    key("reranker", &r.api_key_env)?,
                ));
                Some(RerankSettings {
                    protocol: r.protocol.map(|p| p.as_str()),
                    endpoint_path: r.endpoint_path.clone(),
                    return_documents: r.return_documents,
                })
            }
            None => None,
        };
        Ok(Self {
            exe: PathBuf::from(&cfg.executable),
            repo_root: repo_root.to_path_buf(),
            env,
            backend,
            exclude: cfg.effective_excludes(),
            home,
            rerank,
            index_key: cfg.index_key(),
            rerank_fallbacks: Default::default(),
            deadline: None,
        })
    }

    /// A Vera command with a sanitized environment: ambient Vera/embedding/
    /// reranker overrides removed, Revera's own values set.
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        for (k, _) in std::env::vars_os() {
            if k.to_str().is_some_and(is_vera_controlled) {
                cmd.env_remove(&k);
            }
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.current_dir(&self.repo_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// Apply reranker settings to the Revera-owned Vera home through the
    /// public `vera config set` interface. With no reranker configured,
    /// reranking is explicitly disabled (Vera would otherwise fall back to
    /// a local reranker model).
    pub async fn configure(&self) -> Result<RerankState> {
        if self.backend == "disabled" {
            return Ok(RerankState::Off);
        }
        std::fs::create_dir_all(&self.home)
            .with_context(|| format!("create vera home {}", self.home.display()))?;
        let pairs = RerankSettings::config_pairs(self.rerank.as_ref());
        for (k, v) in &pairs {
            let r = self
                .run_bounded(&["config", "set", k, v], Some(QUERY_TIMEOUT))
                .await;
            if let Err(e) = r {
                if self.rerank.is_some() {
                    tracing::warn!("vera reranker activation failed: {e:#}");
                    // never leave a half-applied reranker config behind; if
                    // it cannot be switched off the home's state is unknown
                    self.run_bounded(
                        &["config", "set", "retrieval.reranking_enabled", "false"],
                        Some(QUERY_TIMEOUT),
                    )
                    .await
                    .context("disable vera reranker after failed activation")?;
                    return Ok(RerankState::Degraded);
                }
                return Err(e.context("vera config set"));
            }
        }
        Ok(if self.rerank.is_some() {
            RerankState::Enabled
        } else {
            RerankState::Off
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
        let mut cmd = self.command();
        cmd.args(args);
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
        if String::from_utf8_lossy(&out.stderr).contains("reranker unavailable") {
            self.rerank_fallbacks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if out.stdout.len() > MAX_OUTPUT {
            bail!("vera {} output exceeds {MAX_OUTPUT} bytes", args.join(" "));
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
            self.command().arg("--version").output(),
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
        match &info.index_key {
            Some(k) if k == &self.index_key => None,
            Some(k) => Some(format!("index identity changed {k} -> {}", self.index_key)),
            // legacy metadata predates the recorded exclusion policy, so the
            // index may hold content that is excluded now
            None => Some("index has no recorded identity (legacy cache)".into()),
        }
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
        if let Err(e) = self.check_health().await {
            let _ = std::fs::remove_file(Self::cache_info_path(&self.repo_root));
            return Err(e.context(format!("vera {verb} left an unhealthy index")));
        }
        self.write_cache_info().await.ok();
        Ok(summary)
    }

    /// Health probe: the index must open via `vera stats --json` and hold
    /// at least one chunk. Runs after every index/update so the cache info
    /// (and the Action cache save keyed on it) only ever describe an index
    /// that answered a query.
    pub async fn check_health(&self) -> Result<Value> {
        let stats = self.run_json(&["stats", "--json"]).await?;
        let chunks = stats["chunk_count"].as_u64().unwrap_or(0);
        if chunks == 0 {
            bail!("vera stats reports an empty index (0 chunks)");
        }
        Ok(stats)
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
            index_key: Some(self.index_key.clone()),
        };
        crate::fsutil::write_repo_file(
            &self.repo_root,
            Path::new(".revera/vera-cache.json"),
            serde_json::to_string_pretty(&info)?.as_bytes(),
        )
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
        let mut args: Vec<String> = vec!["search".into()];
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
        // positional last, after `--`: a model-chosen query is never a flag
        args.extend(["--".into(), query.into()]);
        self.run_json(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .await
    }

    pub async fn references(&self, symbol: &str, callees: bool, limit: u32) -> Result<Value> {
        let mut args = vec!["references".to_string()];
        if callees {
            args.push("--callees".into());
        }
        args.extend(["-n".into(), limit.to_string(), "--json".into()]);
        args.extend(["--".into(), symbol.to_string()]);
        self.run_json(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .await
    }

    pub async fn grep(&self, pattern: &str, path_glob: Option<&str>, limit: u32) -> Result<Value> {
        let mut args = vec!["grep".to_string()];
        if let Some(p) = path_glob {
            args.extend(["--path".into(), p.into()]);
        }
        args.extend(["-n".into(), limit.to_string(), "--json".into()]);
        args.extend(["--".into(), pattern.to_string()]);
        self.run_json(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .await
    }

    pub async fn overview(&self) -> Result<Value> {
        self.run_json(&["overview", "--json"]).await
    }
}
