use crate::config::{VeraBackend, VeraConfig};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct VeraClient {
    pub exe: PathBuf,
    pub repo_root: PathBuf,
    pub env: Vec<(String, String)>,
    pub backend: String,
    pub exclude: Vec<String>,
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
    pub fn from_config(cfg: &VeraConfig, repo_root: &Path) -> Result<Self> {
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
                let key = std::env::var(&e.api_key_env).with_context(|| {
                    format!("vera.embedding.api_key_env {} is not set", e.api_key_env)
                })?;
                env.push(("EMBEDDING_MODEL_API_KEY".into(), key));
            }
            if let Some(r) = &cfg.reranker {
                env.push(("RERANKER_MODEL_BASE_URL".into(), r.base_url.clone()));
                env.push(("RERANKER_MODEL_ID".into(), r.model.clone()));
                let key = std::env::var(&r.api_key_env).with_context(|| {
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
        })
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .current_dir(&self.repo_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let out = cmd
            .output()
            .await
            .with_context(|| format!("failed to run {} {}", self.exe.display(), args.join(" ")))?;
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

    async fn run_json(&self, args: &[&str]) -> Result<Value> {
        let out = self.run(args).await?;
        serde_json::from_str(&out)
            .with_context(|| format!("vera {}: invalid JSON output", args.join(" ")))
    }

    pub async fn version(&self) -> Result<String> {
        let out = Command::new(&self.exe)
            .arg("--version")
            .output()
            .await
            .context("vera not found")?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // "vera 1.4.1" -> "1.4.1"
        Ok(s.split_whitespace().last().unwrap_or(&s).to_string())
    }

    pub async fn ensure_index(&self) -> Result<Value> {
        let indexed = self.repo_root.join(".vera").exists();
        let summary = if indexed {
            let mut args = vec!["update", ".", "--json"];
            for e in &self.exclude {
                args.push("--exclude");
                args.push(e);
            }
            self.run_json(&args).await?
        } else {
            let mut args = vec!["index", ".", "--json"];
            for e in &self.exclude {
                args.push("--exclude");
                args.push(e);
            }
            self.run_json(&args).await?
        };
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
        let embedding_model = self
            .env
            .iter()
            .find(|(k, _)| k == "EMBEDDING_MODEL_ID")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "local".into());
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
