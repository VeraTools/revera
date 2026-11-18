use crate::config::{
    DEFAULT_RERANK_CANDIDATES, DEFAULT_RERANK_MAX_DOC_CHARS, VeraBackend, VeraConfig,
};
use anyhow::{Context, Result, bail};
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
    pub candidates: u32,
    pub max_doc_chars: u32,
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
        if let Some(r) = this {
            v.push(("retrieval.rerank_candidates", r.candidates.to_string()));
            v.push((
                "retrieval.reranker_max_doc_chars",
                r.max_doc_chars.to_string(),
            ));
        }
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
    /// Embedding throughput pairs for `vera config set` (api backend only).
    pub embedding_pairs: Vec<(&'static str, String)>,
    /// Hash of the index-shaping config; stored with the index.
    pub index_key: String,
    /// Searches where Vera fell back to unreranked results.
    pub rerank_fallbacks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Searches where Vera fell back to keyword-only (BM25) results.
    pub semantic_fallbacks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
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

/// Short diagnostic label for a Vera invocation without `--exclude` flag
/// lists or query strings that would crowd out the error tail.
fn command_label(args: &[&str]) -> String {
    match args {
        ["config", "set", key, ..] => format!("vera config set {key}"),
        ["config", ..] => "vera config".into(),
        [sub, ..] => format!("vera {sub}"),
        [] => "vera".into(),
    }
}

fn requires_reindex(err: &str) -> bool {
    err.contains("Index format version mismatch") || err.contains("Please re-index")
}

fn index_args<'a>(verb: &'a str, exclude: &'a [String]) -> Vec<&'a str> {
    let mut args = vec![verb, ".", "--json"];
    for e in exclude {
        args.push("--exclude");
        args.push(e);
    }
    args
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

fn remove_any(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("stat {}", path.display())),
        Ok(m) if m.file_type().is_dir() => {
            std::fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
        }
        Ok(_) => std::fs::remove_file(path).with_context(|| format!("remove {}", path.display())),
    }
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
            embedding_pairs: vec![],
            index_key: String::new(),
            rerank_fallbacks: Default::default(),
            semantic_fallbacks: Default::default(),
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
        let mut embedding_pairs = vec![];
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
            embedding_pairs = e.embedding_config_pairs();
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
                    candidates: r.candidates.unwrap_or(DEFAULT_RERANK_CANDIDATES),
                    max_doc_chars: r.max_doc_chars.unwrap_or(DEFAULT_RERANK_MAX_DOC_CHARS),
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
            embedding_pairs,
            index_key: cfg.index_key(),
            rerank_fallbacks: Default::default(),
            semantic_fallbacks: Default::default(),
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

    /// Apply embedding throughput (api backend) and reranker settings to
    /// the Revera-owned Vera home through the public `vera config set`
    /// interface. With no reranker configured, reranking is explicitly
    /// disabled (Vera would otherwise fall back to a local reranker model).
    pub async fn configure(&self) -> Result<RerankState> {
        if self.backend == "disabled" {
            return Ok(RerankState::Off);
        }
        std::fs::create_dir_all(&self.home)
            .with_context(|| format!("create vera home {}", self.home.display()))?;
        for (k, v) in &self.embedding_pairs {
            self.run_bounded(&["config", "set", k, v], Some(QUERY_TIMEOUT))
                .await
                .context("vera config set (embedding)")?;
        }
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
        let label = command_label(args);
        let Some(limit) = self.time_left(cap) else {
            bail!("{label}: run time budget exhausted");
        };
        let mut cmd = self.command();
        cmd.args(args);
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to run {} ({label})", self.exe.display()))?;
        let out = match tokio::time::timeout(limit, child.wait_with_output()).await {
            Ok(r) => r.with_context(|| format!("{label} failed"))?,
            Err(_) => {
                // the child is killed and reaped by `kill_on_drop` when the
                // dropped future releases it
                bail!("{label} timed out after {}s", limit.as_secs());
            }
        };
        if !out.status.success() {
            let stderr = crate::redact::diagnostic(&String::from_utf8_lossy(&out.stderr));
            let tail: String = stderr
                .chars()
                .rev()
                .take(500)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            bail!("{label} failed: {}", tail.trim());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("reranker unavailable") {
            self.rerank_fallbacks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if stderr.contains("BM25-only") {
            self.semantic_fallbacks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if out.stdout.len() > MAX_OUTPUT {
            bail!("{label} output exceeds {MAX_OUTPUT} bytes");
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
            .with_context(|| format!("{}: invalid JSON output", command_label(args)))
    }

    async fn run_json_index(&self, args: &[&str]) -> Result<Value> {
        let out = self.run_bounded(args, None).await?;
        serde_json::from_str(&out)
            .with_context(|| format!("{}: invalid JSON output", command_label(args)))
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
    /// configuration, if it cannot. An index without `vera-cache.json`
    /// metadata, non-directory/symlink paths, or a mismatching `index_key`
    /// cannot be trusted to match the current backend, model, and exclusions.
    pub fn cache_incompatibility(&self) -> Option<String> {
        let vera_meta = std::fs::symlink_metadata(self.repo_root.join(".vera"));
        if let Ok(m) = &vera_meta
            && !m.file_type().is_dir()
        {
            return Some("existing .vera is not a real directory".into());
        }
        if std::fs::symlink_metadata(self.repo_root.join(".revera"))
            .is_ok_and(|m| !m.file_type().is_dir())
        {
            return Some(".revera is not a real directory".into());
        }
        let p = Self::cache_info_path(&self.repo_root);
        match std::fs::symlink_metadata(&p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if vera_meta.is_ok() {
                    return Some("index has no vera-cache.json metadata".into());
                }
                return None;
            }
            Err(e) => return Some(format!("unreadable vera-cache.json: {e}")),
            Ok(m) if !m.file_type().is_file() => {
                return Some("vera-cache.json is not a regular file".into());
            }
            Ok(_) => {}
        }
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => return Some(format!("unreadable vera-cache.json: {e}")),
        };
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

    fn discard_index(&self, index_dir: &Path) -> Result<()> {
        remove_any(index_dir)?;
        let revera_dir = self.repo_root.join(".revera");
        if std::fs::symlink_metadata(&revera_dir).is_ok_and(|m| !m.file_type().is_dir()) {
            remove_any(&revera_dir)?;
        } else {
            let _ = remove_any(&Self::cache_info_path(&self.repo_root));
        }
        // saved embeddings are keyed by model name only, not endpoint
        let _ = remove_any(&self.repo_root.join(".vera.resume"));
        Ok(())
    }

    fn annotate_index_error(&self, err: anyhow::Error) -> anyhow::Error {
        let msg = format!("{err:#}");
        if msg.contains("timed out after ")
            && std::fs::symlink_metadata(self.repo_root.join(".vera.resume/embeddings.db"))
                .is_ok_and(|m| m.file_type().is_file())
        {
            anyhow::anyhow!("{msg}; partial embeddings saved to resume on next run")
        } else {
            err
        }
    }

    /// Index (or incrementally update) the repository. An existing index
    /// built with a different backend/embedding model is discarded first;
    /// cache info is written only after a successful index.
    pub async fn ensure_index(&self) -> Result<Value> {
        self.ensure_index_within(None).await
    }

    /// [`Self::ensure_index`], including its health check, stopped after
    /// `cap`. Vera keeps finished embeddings in `.vera.resume`, so a later
    /// run continues the build.
    pub async fn ensure_index_within(&self, cap: Option<Duration>) -> Result<Value> {
        let Some(cap) = cap else {
            return self.index_or_update().await;
        };
        let end = Instant::now() + cap;
        let deadline = self.deadline.map_or(end, |d| d.min(end));
        self.clone().with_deadline(deadline).index_or_update().await
    }

    async fn index_or_update(&self) -> Result<Value> {
        let index_dir = self.repo_root.join(".vera");
        let mut indexed = std::fs::symlink_metadata(&index_dir).is_ok();
        if indexed && let Some(why) = self.cache_incompatibility() {
            tracing::warn!("discarding incompatible vera index: {why}");
            self.discard_index(&index_dir)?;
            indexed = false;
        }
        self.claim_saved_embeddings()?;
        let mut verb = if indexed { "update" } else { "index" };
        let summary = match self.run_json_index(&index_args(verb, &self.exclude)).await {
            Ok(summary) => summary,
            Err(e) if indexed && requires_reindex(&format!("{e:#}")) => {
                tracing::warn!("discarding incompatible vera index after update failure: {e:#}");
                self.discard_index(&index_dir)?;
                self.claim_saved_embeddings()?;
                verb = "index";
                self.run_json_index(&index_args(verb, &self.exclude))
                    .await
                    .map_err(|err| self.annotate_index_error(err))?
            }
            Err(e) => return Err(self.annotate_index_error(e)),
        };
        if !is_real_dir(&index_dir) {
            bail!(
                "vera {verb} reported success but {} is missing",
                index_dir.display()
            );
        }
        if let Err(e) = self.check_health().await {
            let _ = remove_any(&index_dir);
            let _ = remove_any(&Self::cache_info_path(&self.repo_root));
            return Err(e.context(format!("vera {verb} left an unhealthy index")));
        }
        self.write_cache_info().await.ok();
        tracing::info!("vera {verb}: {}", index_summary_line(&summary));
        Ok(summary)
    }

    /// Keep `.vera.resume` only if it was saved under the current index
    /// identity, then record that identity in it, so an interrupted build
    /// is never resumed with another embedding endpoint or exclusion list.
    fn claim_saved_embeddings(&self) -> Result<()> {
        let rel = Path::new(".vera.resume/revera-index-key");
        let resume = self.repo_root.join(".vera.resume");
        match std::fs::symlink_metadata(&resume) {
            Ok(m) if !m.file_type().is_dir() => {
                tracing::info!("removing non-directory .vera.resume");
                remove_any(&resume)?;
            }
            Ok(_) => {
                let key_path = self.repo_root.join(rel);
                let valid_key = std::fs::symlink_metadata(&key_path)
                    .is_ok_and(|m| m.file_type().is_file())
                    && std::fs::read_to_string(&key_path).ok().as_deref()
                        == Some(self.index_key.as_str());
                if !valid_key {
                    tracing::info!("discarding saved embeddings from another index identity");
                    remove_any(&resume)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("stat {}", resume.display())),
        }
        crate::fsutil::write_repo_file(&self.repo_root, rel, self.index_key.as_bytes())
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
            updated_at: crate::timeutil::now_rfc3339(),
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

/// Counters from a `vera index|update --json` summary, in a fixed order;
/// fields the installed Vera does not report are left out.
fn index_summary_line(summary: &Value) -> String {
    const KEYS: &[&str] = &[
        "files_parsed",
        "chunks_created",
        "embeddings_generated",
        "embeddings_reused",
        "embedding_requests",
        "embedding_retries",
        "embedding_timeouts",
        "embedding_failed_batches",
    ];
    let mut parts: Vec<String> = KEYS
        .iter()
        .filter_map(|k| summary[*k].as_u64().map(|n| format!("{k}={n}")))
        .collect();
    if let Some(s) = summary["elapsed_secs"].as_f64() {
        parts.push(format!("elapsed={s:.1}s"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn incompatible_index_also_drops_saved_embeddings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        for path in [".vera", ".vera.resume", ".revera"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        std::fs::write(root.join(".vera.resume/embeddings.db"), "old").unwrap();
        std::fs::write(
            root.join(".revera/vera-cache.json"),
            r#"{"vera_version":"2.4.1","backend":"api","embedding_model":null,"dim":null,"updated_at":"","index_key":"old"}"#,
        )
        .unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  index) [ -e .vera.resume/embeddings.db ] && exit 1; mkdir .vera; echo '{}';;\n  stats) echo '{\"chunk_count\":1}';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe;
        client.backend = "api".into();
        client.index_key = "new".into();
        client.ensure_index().await.unwrap();
        assert!(!root.join(".vera.resume/embeddings.db").exists());
    }

    #[tokio::test]
    async fn interrupted_build_from_another_identity_is_not_resumed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join(".vera.resume")).unwrap();
        std::fs::write(root.join(".vera.resume/embeddings.db"), "old").unwrap();
        std::fs::write(root.join(".vera.resume/revera-index-key"), "old").unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  index) [ -e .vera.resume/embeddings.db ] && exit 1; [ \"$(cat .vera.resume/revera-index-key)\" = new ] || exit 1; mkdir .vera; echo '{}';;\n  stats) echo '{\"chunk_count\":1}';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe;
        client.backend = "api".into();
        client.index_key = "new".into();
        client.ensure_index().await.unwrap();
    }

    #[tokio::test]
    async fn interrupted_build_with_the_same_identity_is_resumed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join(".vera.resume")).unwrap();
        std::fs::write(root.join(".vera.resume/embeddings.db"), "saved").unwrap();
        std::fs::write(root.join(".vera.resume/revera-index-key"), "same").unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  index) [ -e .vera.resume/embeddings.db ] || exit 1; mkdir .vera; echo '{}';;\n  stats) echo '{\"chunk_count\":1}';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe;
        client.backend = "api".into();
        client.index_key = "same".into();
        client.ensure_index().await.unwrap();
    }

    #[tokio::test]
    async fn index_cap_also_bounds_the_health_check() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  index) mkdir .vera; echo '{}';;\n  stats) sleep 5;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut client = super::VeraClient::disabled(dir.path());
        client.exe = exe;
        client.backend = "api".into();
        let start = std::time::Instant::now();
        let result = client
            .ensure_index_within(Some(std::time::Duration::from_secs(1)))
            .await;
        assert!(result.is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
    }

    #[test]
    fn index_summary_line_keeps_known_counters_only() {
        let v = serde_json::json!({
            "files_parsed": 168, "chunks_created": 1773, "embedding_retries": 0,
            "elapsed_secs": 26.26, "parse_errors": [], "unknown": 5
        });
        assert_eq!(
            super::index_summary_line(&v),
            "files_parsed=168 chunks_created=1773 embedding_retries=0 elapsed=26.3s"
        );
    }

    #[test]
    fn url_queries_are_removed_from_vera_errors() {
        let err = "error sending request for url (https://h.example/v1/embeddings?key=abc%2Fdef&x=1): timeout";
        let out = crate::redact::diagnostic(err);
        assert!(!out.contains("abc"), "{out}");
        assert!(
            out.contains("https://h.example/v1/embeddings?[query removed]"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn index_errors_omit_exclude_flags_and_note_saved_resume_embeddings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        for path in [".vera", ".vera.resume", ".revera"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        std::fs::write(root.join(".vera.resume/embeddings.db"), "saved").unwrap();
        std::fs::write(root.join(".vera.resume/revera-index-key"), "same").unwrap();
        std::fs::write(
            root.join(".revera/vera-cache.json"),
            r#"{"vera_version":"2.4.1","backend":"api","embedding_model":"m","dim":8,"updated_at":"","index_key":"same"}"#,
        )
        .unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  update) echo 'HTTP 429 rate limit' >&2; exit 1;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe.clone();
        client.backend = "api".into();
        client.index_key = "same".into();
        client.exclude = vec![".env".into(), "**/*.pem".into()];

        let err = format!("{:#}", client.ensure_index().await.unwrap_err());
        assert!(err.starts_with("vera update failed:"), "{err}");
        assert!(err.contains("HTTP 429 rate limit"), "{err}");
        assert!(!err.contains("--exclude"), "{err}");

        std::fs::write(&exe, "#!/bin/sh\ncase $1 in\n  update) sleep 5;;\nesac\n").unwrap();
        let err = format!(
            "{:#}",
            client
                .ensure_index_within(Some(std::time::Duration::from_secs(1)))
                .await
                .unwrap_err()
        );
        assert!(err.starts_with("vera update timed out after"), "{err}");
        assert!(
            err.contains("partial embeddings saved to resume on next run"),
            "{err}"
        );
        assert!(!err.contains("--exclude"), "{err}");
    }

    #[tokio::test]
    async fn existing_index_without_cache_metadata_is_rebuilt() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  update) exit 1;;\n  index) mkdir -p .vera; touch .vera/fresh; echo '{}';;\n  stats) echo '{\"chunk_count\":1}';;\n  --version) echo 'vera 2.4.1';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe;
        client.backend = "api".into();
        client.index_key = "k1".into();
        assert!(client.cache_incompatibility().is_none());

        std::fs::create_dir_all(root.join(".vera")).unwrap();
        std::fs::write(root.join(".vera/stale"), "old").unwrap();
        assert!(client.cache_incompatibility().is_some());

        client.ensure_index().await.unwrap();
        assert!(!root.join(".vera/stale").exists());
        assert!(root.join(".vera/fresh").exists());
        assert!(super::VeraClient::cache_info_path(&root).exists());
    }

    #[tokio::test]
    async fn update_reindex_error_rebuilds_index_while_transient_error_keeps_warm_index() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        for path in [".vera", ".revera"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        std::fs::write(root.join(".vera/old"), "v1").unwrap();
        std::fs::write(
            root.join(".revera/vera-cache.json"),
            r#"{"vera_version":"1.4.1","backend":"api","embedding_model":"m","dim":8,"updated_at":"","index_key":"same"}"#,
        )
        .unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  update) echo 'Error: update failed: Index format version mismatch (expected 2, found Some(\"1\")).' >&2; exit 1;;\n  index) mkdir -p .vera; touch .vera/rebuilt; echo '{}';;\n  stats) echo '{\"chunk_count\":1}';;\n  --version) echo 'vera 2.4.1';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe.clone();
        client.backend = "api".into();
        client.index_key = "same".into();
        client.ensure_index().await.unwrap();
        assert!(!root.join(".vera/old").exists());
        assert!(root.join(".vera/rebuilt").exists());

        std::fs::write(root.join(".vera/warm"), "keep").unwrap();
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  update) echo 'HTTP 429' >&2; exit 1;;\nesac\n",
        )
        .unwrap();
        assert!(client.ensure_index().await.is_err());
        assert!(root.join(".vera/warm").exists());
        assert!(super::VeraClient::cache_info_path(&root).exists());
    }

    #[tokio::test]
    async fn unhealthy_index_is_removed_with_cache_metadata() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  index) mkdir -p .vera; touch .vera/bad; echo '{}';;\n  stats) echo '{\"chunk_count\":0}';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut client = super::VeraClient::disabled(&root);
        client.exe = exe;
        client.backend = "api".into();
        let err = format!("{:#}", client.ensure_index().await.unwrap_err());
        assert!(err.contains("empty index"), "{err}");
        assert!(!root.join(".vera").exists());
        assert!(!super::VeraClient::cache_info_path(&root).exists());
    }

    #[tokio::test]
    async fn non_directory_or_symlink_vera_paths_are_replaced() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("vera");
        std::fs::write(
            &exe,
            "#!/bin/sh\ncase $1 in\n  update) exit 1;;\n  index) mkdir -p .vera; touch .vera/built; echo '{}';;\n  stats) echo '{\"chunk_count\":1}';;\n  --version) echo 'vera 2.4.1';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        // 1. Regular file at .vera and .vera.resume
        let root1 = dir.path().join("repo-file");
        std::fs::create_dir_all(&root1).unwrap();
        std::fs::write(root1.join(".vera"), "not a dir").unwrap();
        std::fs::write(root1.join(".vera.resume"), "not a dir").unwrap();
        let mut client1 = super::VeraClient::disabled(&root1);
        client1.exe = exe.clone();
        client1.backend = "api".into();
        client1.index_key = "k1".into();
        client1.ensure_index().await.unwrap();
        assert!(super::is_real_dir(&root1.join(".vera")));
        assert!(root1.join(".vera/built").exists());

        // 2. Broken symlink at .vera
        let root2 = dir.path().join("repo-broken-link");
        std::fs::create_dir_all(&root2).unwrap();
        symlink(dir.path().join("missing-target"), root2.join(".vera")).unwrap();
        let mut client2 = super::VeraClient::disabled(&root2);
        client2.exe = exe.clone();
        client2.backend = "api".into();
        client2.index_key = "k1".into();
        client2.ensure_index().await.unwrap();
        assert!(super::is_real_dir(&root2.join(".vera")));
        assert!(root2.join(".vera/built").exists());

        // 3. Directory symlink at .vera even with matching vera-cache.json
        let root3 = dir.path().join("repo-dir-link");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root3.join(".revera")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "untouched").unwrap();
        symlink(&outside, root3.join(".vera")).unwrap();
        std::fs::write(
            root3.join(".revera/vera-cache.json"),
            r#"{"vera_version":"2.4.1","backend":"api","embedding_model":"m","dim":8,"updated_at":"","index_key":"k1"}"#,
        )
        .unwrap();
        let mut client3 = super::VeraClient::disabled(&root3);
        client3.exe = exe;
        client3.backend = "api".into();
        client3.index_key = "k1".into();
        client3.ensure_index().await.unwrap();
        assert!(super::is_real_dir(&root3.join(".vera")));
        assert!(root3.join(".vera/built").exists());
        assert!(outside.join("keep").exists());
    }
}
