use super::{
    Server,
    events::{Job, Kind, Repo},
};
use anyhow::{Context, Result, bail};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub admins: Vec<String>,
    pub allowed_owners: Vec<String>,
    pub default_enabled: bool,
    pub repos: BTreeMap<String, RepoPreference>,
    pub review_drafts: bool,
    pub skip_authors: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoPreference {
    pub enabled: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            admins: vec![],
            allowed_owners: vec![],
            default_enabled: true,
            repos: BTreeMap::new(),
            review_drafts: false,
            skip_authors: vec!["dependabot[bot]".into(), "renovate[bot]".into()],
        }
    }
}

impl Preferences {
    pub fn enabled(&self, repo: &str) -> bool {
        self.repos
            .get(repo)
            .map_or(self.default_enabled, |r| r.enabled)
    }
    pub fn reviewable(&self, pr: &Value) -> bool {
        (self.review_drafts || pr["draft"].as_bool() != Some(true))
            && !self.skip_authors.iter().any(|a| {
                pr["user"]["login"]
                    .as_str()
                    .is_some_and(|login| login.eq_ignore_ascii_case(a))
            })
            && pr["state"].as_str() != Some("closed")
    }
    pub fn admin(&self, login: &str) -> bool {
        self.admins.iter().any(|a| a.eq_ignore_ascii_case(login))
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct FindingCounts {
    pub total: u64,
    pub high: u64,
    pub medium: u64,
    pub low: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CheckRecovery {
    pub id: u64,
    pub repo: Repo,
    pub pending: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct JobSummary {
    /// Persisted by `persist_job`, but never included in dashboard responses.
    #[serde(default, skip_serializing)]
    pub check: Option<CheckRecovery>,
    pub id: String,
    pub kind: String,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub trigger: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub findings: FindingCounts,
    pub queued_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_run_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
}

impl JobSummary {
    pub fn queued(job: &Job) -> Self {
        let (pr, title, author, head_sha, pr_url) = match &job.kind {
            Kind::Review { number, pr, .. } => (
                Some(*number),
                string(&pr["title"]),
                string(&pr["user"]["login"]),
                string(&pr["head"]["sha"]),
                string(&pr["html_url"]),
            ),
            _ => (None, None, None, None, None),
        };
        Self {
            check: None,
            id: job.id.clone(),
            kind: if pr.is_some() { "review" } else { "index" }.into(),
            repo: job.repo.full_name.clone(),
            pr,
            title,
            author,
            head_sha,
            trigger: job.trigger.clone(),
            status: "queued".into(),
            reason: None,
            findings: FindingCounts::default(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            tokens: None,
            check_run_url: None,
            pr_url,
        }
    }
    pub fn terminal(&self) -> bool {
        !matches!(self.status.as_str(), "queued" | "running")
    }
    pub fn set_state(&mut self, status: &str, reason: Option<String>) {
        self.status = status.into();
        self.reason = reason;
        let now = chrono::Utc::now();
        if status == "running" {
            self.started_at = Some(now.to_rfc3339());
        }
        if self.terminal() {
            self.finished_at = Some(now.to_rfc3339());
            self.duration_ms = self
                .started_at
                .as_ref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|start| {
                    (now - start.with_timezone(&chrono::Utc))
                        .num_milliseconds()
                        .max(0) as u64
                });
        }
    }
}

fn string(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub repo: Repo,
    pub private: bool,
    pub html_url: String,
    pub has_override: bool,
}

#[derive(Default, Deserialize)]
struct Inventory {
    installations: usize,
    repos: BTreeMap<String, RepoInfo>,
}

#[derive(Default, Deserialize)]
struct ProbeHealth {
    key: String,
    models: BTreeMap<String, Value>,
}

pub struct Dashboard {
    pub bootstrap_used: bool,
    pub preferences: Preferences,
    pub secrets: BTreeMap<String, String>,
    pub jobs: BTreeMap<String, JobSummary>,
    pub repos: BTreeMap<String, RepoInfo>,
    pub health: BTreeMap<String, Value>,
    health_key: String,
    pub health_generation: u64,
    /// Latest probe started per role; only that probe may record a result.
    pub probe_seq: BTreeMap<String, u64>,
    pub github: Value,
    pub installations: usize,
}

pub fn read_json<T: serde::de::DeserializeOwned + Default>(data: &Path, name: &str) -> Result<T> {
    let path = data.join(name);
    crate::fsutil::check_regular_target(&path)?;
    if path.exists() {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    } else {
        Ok(T::default())
    }
}

impl Dashboard {
    pub fn load(data: &Path, seen: &super::reconcile::Seen) -> Result<Self> {
        let preferences: Preferences = read_json(data, "settings.json")?;
        crate::fsutil::check_regular_target(&data.join("bootstrap-admin"))?;
        let bootstrap_used =
            data.join("bootstrap-admin").exists() || !preferences.admins.is_empty();
        let secrets: BTreeMap<String, String> = read_json(data, "secrets.json")?;
        for value in secrets.values() {
            crate::redact::register(value);
        }
        let inventory: Option<Inventory> = read_json(data, "inventory.json")?;
        let inventory = inventory.unwrap_or_else(|| Inventory {
            installations: seen
                .0
                .values()
                .map(|known| known.repo.installation)
                .collect::<BTreeSet<_>>()
                .len(),
            repos: BTreeMap::new(),
        });
        let health: ProbeHealth = read_json(data, "health.json")?;
        let mut jobs = BTreeMap::new();
        for entry in std::fs::read_dir(data.join("jobs"))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let meta = entry.path().join("meta.json");
            crate::fsutil::check_regular_target(&meta)?;
            if meta.exists() {
                let mut job: JobSummary = serde_json::from_slice(&std::fs::read(meta)?)?;
                if !job.terminal() {
                    job.set_state("cancelled", Some("server restarted".into()));
                    persist_job(data, &job)?;
                }
                jobs.insert(job.id.clone(), job);
            }
        }
        Ok(Self {
            bootstrap_used,
            preferences,
            secrets,
            jobs,
            repos: inventory.repos,
            health: health.models,
            health_key: health.key,
            health_generation: 0,
            probe_seq: BTreeMap::new(),
            github: json!({"ok": false}),
            installations: inventory.installations,
        })
    }
    pub fn env(&self, name: &str) -> Option<String> {
        let value = self
            .secrets
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok());
        if let Some(value) = &value {
            crate::redact::register(value);
        }
        value
    }
    pub fn parse(&self, yaml: &str) -> Result<crate::config::Config> {
        crate::config::Config::parse_with_env(yaml, "server config", &|name| self.env(name))
    }
    pub fn validate(&self, yaml: &str) -> Result<crate::config::Config> {
        let config = self.parse(yaml)?;
        config.check_trust(true)?;
        config.check_publication(crate::config::PublishMode::Comment)?;
        config.validate_for_with_env(config.review.strategy, &|name| {
            self.env(name).is_some_and(|v| !v.trim().is_empty())
        })?;
        Ok(config)
    }
    pub fn sync_health(&mut self, data: &Path, yaml: Option<&str>, key: &[u8]) -> Result<()> {
        let text = yaml.unwrap_or_default();
        let mut names: BTreeSet<_> = Self::referenced(text).into_iter().collect();
        if let Ok(expanded) = crate::config::expand_env_with(text, &|name| self.env(name))
            && let Ok(value) = serde_saphyr::from_str::<Value>(&expanded)
        {
            // YAML aliases, quoted keys and interpolated key names must cover the
            // same credentials as ordinary `api_key_env: NAME` fields.
            credential_references(&value, &mut names);
        }
        let referenced: BTreeMap<_, _> = names
            .into_iter()
            .map(|name| {
                let value = self.env(&name);
                (name, value)
            })
            .collect();
        // Keyed hashing lets us detect credential changes without storing credentials
        // or a reusable unkeyed digest of them. The cookie key never leaves this host.
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key");
        mac.update(b"revera-probe-health-v1\0");
        mac.update(&serde_json::to_vec(&(yaml, referenced))?);
        let fingerprint = hex::encode(mac.finalize().into_bytes());
        if self.health_key != fingerprint {
            self.health.clear();
            self.health_generation = self.health_generation.wrapping_add(1);
            self.health_key = fingerprint;
            self.persist_health(data)?;
        }
        Ok(())
    }
    pub fn persist_health(&self, data: &Path) -> Result<()> {
        let mut value = json!({"key": self.health_key, "models": self.health});
        crate::redact::json(&mut value);
        crate::fsutil::write_atomic_in(data, "health.json", &serde_json::to_vec_pretty(&value)?)
    }
    pub fn retry_index(&self, repo: &str) -> bool {
        self.jobs
            .values()
            .filter(|job| job.repo == repo && job.kind == "index")
            .max_by(|a, b| a.queued_at.cmp(&b.queued_at).then(a.id.cmp(&b.id)))
            .is_some_and(|job| matches!(job.status.as_str(), "failed" | "partial"))
    }
    pub fn unconfigured_reviews(&self, repo: &str) -> BTreeSet<u64> {
        let mut latest = BTreeMap::<u64, &JobSummary>::new();
        for job in self.jobs.values().filter(|job| job.repo == repo) {
            if let Some(number) = job.pr {
                let previous = latest.entry(number).or_insert(job);
                if (&job.queued_at, &job.id) > (&previous.queued_at, &previous.id) {
                    *previous = job;
                }
            }
        }
        latest
            .into_iter()
            .filter_map(|(number, job)| {
                (job.status == "skipped"
                    && job.reason.as_deref() == Some(super::runner::UNCONFIGURED))
                .then_some(number)
            })
            .collect()
    }
    pub fn referenced(yaml: &str) -> Vec<String> {
        let regex = regex::Regex::new(
            r#"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|api_key_env\s*:\s*["']?([A-Za-z_][A-Za-z0-9_]*)"#,
        )
        .unwrap();
        regex
            .captures_iter(yaml)
            .filter_map(|c| c.get(1).or_else(|| c.get(2)))
            .map(|m| m.as_str().to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn secret_status(&self, yaml: &str) -> Vec<Value> {
        let names = Self::referenced(yaml)
            .into_iter()
            .chain(self.secrets.keys().cloned())
            .collect::<BTreeSet<_>>();
        names
            .into_iter()
            .map(|name| {
                let source = if self.secrets.contains_key(&name) {
                    "dashboard"
                } else if crate::config::env_is_set(&name) {
                    "environment"
                } else {
                    "missing"
                };
                json!({"name": name, "source": source})
            })
            .collect()
    }
    pub fn authorize(&mut self, login: &str, bootstrap: bool, data: &Path) -> Result<()> {
        if self.preferences.admin(login) {
            return Ok(());
        }
        if self.bootstrap_used || !self.preferences.admins.is_empty() || !bootstrap {
            bail!("administrator access required");
        }
        let mut preferences = self.preferences.clone();
        preferences.admins.push(login.into());
        crate::fsutil::write_secret_atomic(data, "bootstrap-admin", login.as_bytes())?;
        self.bootstrap_used = true;
        write_preferences(data, &preferences)?;
        self.preferences = preferences;
        Ok(())
    }
}

fn credential_references(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "api_key_env"
                    && let Some(name) = value.as_str()
                {
                    names.insert(name.into());
                }
                credential_references(value, names);
            }
        }
        Value::Array(values) => {
            for value in values {
                credential_references(value, names);
            }
        }
        _ => {}
    }
}

pub fn write_inventory(
    data: &Path,
    installations: usize,
    repos: &BTreeMap<String, RepoInfo>,
) -> Result<()> {
    let mut value = json!({"installations": installations, "repos": repos});
    crate::redact::json(&mut value);
    crate::fsutil::write_atomic_in(data, "inventory.json", &serde_json::to_vec_pretty(&value)?)
}

pub fn write_preferences(data: &Path, preferences: &Preferences) -> Result<()> {
    crate::fsutil::write_atomic_in(
        data,
        "settings.json",
        &serde_json::to_vec_pretty(preferences)?,
    )
}

pub fn secret_name(name: &str, token_env: &str) -> Result<()> {
    crate::config::check_credential_env("secret", name, token_env)
}

pub fn job_dir(data: &Path, id: &str) -> Result<std::path::PathBuf> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        bail!("invalid job id");
    }
    crate::fsutil::ensure_dir(data, &Path::new("jobs").join(id))
}

pub fn persist_job(data: &Path, summary: &JobSummary) -> Result<()> {
    let dir = job_dir(data, &summary.id)?;
    let mut value = serde_json::to_value(summary)?;
    if let Some(check) = &summary.check {
        value["check"] = serde_json::to_value(check)?;
    }
    crate::redact::json(&mut value);
    crate::fsutil::write_atomic_in(&dir, "meta.json", &serde_json::to_vec_pretty(&value)?)
}

impl Server {
    pub async fn recover_checks(&self) -> Result<()> {
        let Some(app) = self.setup.lock().await.app.clone() else {
            return Ok(());
        };
        let queue = self.queue.lock().await;
        let active: BTreeSet<_> = queue
            .pending
            .values()
            .chain(queue.running.values().map(|running| &running.job))
            .map(|job| job.id.clone())
            .collect();
        drop(queue);
        let jobs: Vec<_> = self
            .dashboard
            .lock()
            .await
            .jobs
            .values()
            .filter(|job| {
                !active.contains(&job.id) && job.check.as_ref().is_some_and(|check| check.pending)
            })
            .cloned()
            .collect();
        for mut job in jobs {
            let check = job.check.as_mut().unwrap();
            let token = self
                .api
                .installation_token(
                    &app,
                    check.repo.installation,
                    Some(check.repo.id),
                    super::app::Permissions::Checks,
                )
                .await?;
            let path = format!("/repos/{}/check-runs/{}", check.repo.full_name, check.id);
            let current = self
                .api
                .request(reqwest::Method::GET, &path, &token, None)
                .await?;
            if current["status"].as_str() != Some("completed") {
                self.api
                    .request(
                        reqwest::Method::PATCH,
                        &path,
                        &token,
                        Some(json!({
                            "status": "completed", "conclusion": "cancelled",
                            "completed_at": chrono::Utc::now().to_rfc3339(),
                            "output": {"title": "Review cancelled", "summary": "server restarted"}
                        })),
                    )
                    .await?;
            }
            check.pending = false;
            self.update_job(job).await?;
        }
        Ok(())
    }

    pub async fn update_job(&self, mut summary: JobSummary) -> Result<()> {
        let mut dashboard = self.dashboard.lock().await;
        if summary.check.is_none() {
            summary.check = dashboard
                .jobs
                .get(&summary.id)
                .and_then(|job| job.check.clone());
        }
        // Cancellation is final even if the runner was concurrently starting or finishing.
        if let Some(previous) = dashboard.jobs.get(&summary.id)
            && previous.status == "cancelled"
        {
            summary.status.clone_from(&previous.status);
            summary.reason.clone_from(&previous.reason);
            summary.started_at.clone_from(&previous.started_at);
            summary.finished_at.clone_from(&previous.finished_at);
            summary.duration_ms = previous.duration_ms;
        }
        persist_job(&self.settings.data_dir, &summary)?;
        dashboard.jobs.insert(summary.id.clone(), summary.clone());
        let _ = self.events.send(("job", serde_json::to_value(summary)?));
        Ok(())
    }
    pub async fn transition(&self, job: &Job, status: &str, reason: Option<String>) -> Result<()> {
        if matches!(job.kind, Kind::Remove) {
            return Ok(());
        }
        let mut summary = self
            .dashboard
            .lock()
            .await
            .jobs
            .get(&job.id)
            .cloned()
            .unwrap_or_else(|| JobSummary::queued(job));
        summary.set_state(status, reason);
        self.update_job(summary).await
    }
    pub async fn prune_jobs(&self) -> Result<()> {
        let queue = self.queue.lock().await;
        let active: BTreeSet<_> = queue
            .pending
            .values()
            .chain(queue.running.values().map(|running| &running.job))
            .map(|job| job.id.as_str())
            .collect();
        let mut dashboard = self.dashboard.lock().await;
        let mut jobs: Vec<_> = dashboard.jobs.values().cloned().collect();
        jobs.sort_by(|a, b| b.queued_at.cmp(&a.queued_at).then(b.id.cmp(&a.id)));
        for summary in jobs.into_iter().skip(500).filter(JobSummary::terminal) {
            if active.contains(summary.id.as_str())
                || summary.check.as_ref().is_some_and(|check| check.pending)
            {
                continue;
            }
            let dir = job_dir(&self.settings.data_dir, &summary.id)?;
            std::fs::remove_dir_all(dir).context("remove old job")?;
            dashboard.jobs.remove(&summary.id);
        }
        Ok(())
    }
}
