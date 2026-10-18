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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    #[default]
    Mention,
    Auto,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Quick,
    #[default]
    Standard,
    Deep,
}

impl Mode {
    pub fn profile(self) -> Option<&'static str> {
        match self {
            Self::Quick => Some("quick"),
            Self::Standard => None,
            Self::Deep => Some("deep"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(from = "PreferencesFile")]
pub struct Preferences {
    pub admins: Vec<String>,
    pub allowed_owners: Vec<String>,
    /// Login aliases pinned to GitHub account IDs. Keep old aliases to prevent
    /// a newly registered account taking over a previously approved login.
    pub owner_accounts: BTreeMap<String, u64>,
    pub revoked_owners: Vec<String>,
    pub revoked_owner_ids: BTreeSet<u64>,
    pub default_trigger: Trigger,
    pub default_mode: Mode,
    pub repos: BTreeMap<String, RepoPreference>,
    pub review_drafts: bool,
    pub skip_authors: Vec<String>,
    pub external: super::external::Policy,
    pub fixes: super::fixes::Policy,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(from = "RepoPreferenceFile")]
pub struct RepoPreference {
    pub trigger: Option<Trigger>,
    pub mode: Option<Mode>,
    pub fixes: Option<super::fixes::Override>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RepoPreferenceFile {
    trigger: Option<Trigger>,
    mode: Option<Mode>,
    enabled: Option<bool>,
    fixes: Option<super::fixes::Override>,
}

impl From<RepoPreferenceFile> for RepoPreference {
    fn from(file: RepoPreferenceFile) -> Self {
        Self {
            trigger: file
                .trigger
                .or_else(|| (file.enabled == Some(false)).then_some(Trigger::Off)),
            mode: file.mode,
            fixes: file.fixes,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PreferencesFile {
    admins: Vec<String>,
    allowed_owners: Vec<String>,
    owner_accounts: BTreeMap<String, u64>,
    revoked_owners: Vec<String>,
    revoked_owner_ids: BTreeSet<u64>,
    default_trigger: Option<Trigger>,
    default_mode: Mode,
    default_enabled: Option<bool>,
    repos: BTreeMap<String, RepoPreference>,
    review_drafts: bool,
    skip_authors: Option<Vec<String>>,
    external: super::external::Policy,
    fixes: super::fixes::Policy,
}

impl From<PreferencesFile> for Preferences {
    fn from(file: PreferencesFile) -> Self {
        Self {
            admins: file.admins,
            allowed_owners: file.allowed_owners,
            owner_accounts: file.owner_accounts,
            revoked_owners: file.revoked_owners,
            revoked_owner_ids: file.revoked_owner_ids,
            default_trigger: file.default_trigger.unwrap_or_else(|| {
                if file.default_enabled == Some(false) {
                    Trigger::Off
                } else {
                    Trigger::Mention
                }
            }),
            default_mode: file.default_mode,
            repos: file.repos,
            review_drafts: file.review_drafts,
            external: file.external,
            fixes: file.fixes,
            skip_authors: file
                .skip_authors
                .unwrap_or_else(|| Self::default().skip_authors),
        }
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            admins: vec![],
            allowed_owners: vec![],
            owner_accounts: BTreeMap::new(),
            revoked_owners: vec![],
            revoked_owner_ids: BTreeSet::new(),
            default_trigger: Trigger::Mention,
            default_mode: Mode::Standard,
            repos: BTreeMap::new(),
            review_drafts: false,
            external: super::external::Policy::default(),
            fixes: super::fixes::Policy::default(),
            skip_authors: vec!["dependabot[bot]".into(), "renovate[bot]".into()],
        }
    }
}

impl Preferences {
    pub fn owner_allowed(&self, login: &str, id: Option<u64>, implicit: bool) -> bool {
        let pinned = self
            .owner_accounts
            .iter()
            .find(|(v, _)| v.eq_ignore_ascii_case(login));
        if self
            .revoked_owners
            .iter()
            .any(|v| v.eq_ignore_ascii_case(login))
            || id
                .or_else(|| pinned.map(|(_, id)| *id))
                .is_some_and(|id| self.revoked_owner_ids.contains(&id))
        {
            return false;
        }
        if let (Some(id), Some((_, pinned))) = (id, pinned)
            && id != *pinned
        {
            return false;
        }
        implicit
            || self.allowed_owners.iter().any(|allowed| {
                allowed.eq_ignore_ascii_case(login)
                    || id.is_some_and(|id| {
                        self.owner_accounts.get(&allowed.to_ascii_lowercase()) == Some(&id)
                    })
            })
    }

    pub fn approve_owner(&mut self, login: &str, id: Option<u64>) {
        let login = login.to_ascii_lowercase();
        self.revoked_owners
            .retain(|v| !v.eq_ignore_ascii_case(&login));
        if let Some(id) = id.filter(|id| *id != 0) {
            self.revoked_owner_ids.remove(&id);
            self.allowed_owners.retain(|owner| {
                self.owner_accounts.get(&owner.to_ascii_lowercase()) != Some(&id)
                    || owner.eq_ignore_ascii_case(&login)
            });
            self.owner_accounts.insert(login.clone(), id);
        }
        if !self
            .allowed_owners
            .iter()
            .any(|v| v.eq_ignore_ascii_case(&login))
        {
            self.allowed_owners.push(login);
        }
    }

    pub fn revoke_owner(&mut self, login: &str, id: Option<u64>) {
        self.allowed_owners.retain(|v| {
            !v.eq_ignore_ascii_case(login)
                && !id
                    .is_some_and(|id| self.owner_accounts.get(&v.to_ascii_lowercase()) == Some(&id))
        });
        if !self
            .revoked_owners
            .iter()
            .any(|v| v.eq_ignore_ascii_case(login))
        {
            self.revoked_owners.push(login.to_ascii_lowercase());
        }
        if let Some(id) = id {
            self.revoked_owner_ids.insert(id);
        }
    }

    fn load(data: &Path) -> Result<Self> {
        let path = data.join("settings.json");
        crate::fsutil::check_regular_target(&path)?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let legacy = value.get("default_enabled").is_some()
            || value["repos"]
                .as_object()
                .is_some_and(|repos| repos.values().any(|repo| repo.get("enabled").is_some()));
        let preferences = serde_json::from_value(value)?;
        if legacy {
            write_preferences(data, &preferences)?;
        }
        Ok(preferences)
    }
    pub fn trigger(&self, repo: &str) -> Trigger {
        self.repos
            .get(repo)
            .and_then(|r| r.trigger)
            .unwrap_or(self.default_trigger)
    }
    pub fn mode(&self, repo: &str) -> Mode {
        self.repos
            .get(repo)
            .and_then(|r| r.mode)
            .unwrap_or(self.default_mode)
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
    #[serde(default)]
    pub source: super::events::Source,
    #[serde(default)]
    pub publication: super::events::Publication,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
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
    #[serde(default)]
    pub mode: Mode,
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
            source: job.repo.source(),
            publication: job.publication(),
            repo_id: Some(job.repo.id),
            base_sha: match &job.kind {
                Kind::Review { pr, .. } => string(&pr["base"]["sha"]),
                _ => None,
            },
            id: job.id.clone(),
            kind: if pr.is_some() { "review" } else { "index" }.into(),
            repo: job.repo.full_name.clone(),
            pr,
            title,
            author,
            head_sha,
            trigger: job.trigger.clone(),
            mode: job.mode,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_prs: Option<u64>,
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
        let preferences = Preferences::load(data)?;
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
                .filter_map(|known| known.repo.installation)
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
    pub fn index_retry_at(&self, repo: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        let mut history: Vec<_> = self
            .jobs
            .values()
            .filter(|job| job.repo == repo && job.kind == "index")
            .collect();
        history.sort_by(|a, b| b.queued_at.cmp(&a.queued_at).then(b.id.cmp(&a.id)));
        let mut failures = 0;
        let mut latest = None;
        for job in history {
            match job.status.as_str() {
                "complete" => break,
                "failed" | "partial" => {
                    failures += 1;
                    latest.get_or_insert(job);
                }
                _ => {}
            }
        }
        let latest = latest?;
        let finished = chrono::DateTime::parse_from_rfc3339(latest.finished_at.as_deref()?)
            .ok()?
            .with_timezone(&chrono::Utc);
        let minutes = (1..failures).fold(10, |delay, _| (delay * 2).min(24 * 60));
        Some(finished + chrono::Duration::minutes(minutes))
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
        let mut names = Self::referenced(yaml)
            .into_iter()
            .chain(self.secrets.keys().cloned())
            .collect::<BTreeSet<_>>();
        // The web-search key has a default name, so it may not appear in the YAML.
        let expanded = crate::config::expand_env_with(yaml, &|name| self.env(name))
            .unwrap_or_else(|_| yaml.to_owned());
        if let Ok(value) = serde_saphyr::from_str::<Value>(&expanded)
            && value["web_search"]["enabled"] == true
        {
            names.insert(
                value["web_search"]["api_key_env"]
                    .as_str()
                    .unwrap_or("EXA_API_KEY")
                    .to_owned(),
            );
        }
        names
            .into_iter()
            .map(|name| {
                let environment = crate::config::env_is_set(&name);
                let source = if self.secrets.contains_key(&name) {
                    "dashboard"
                } else if environment {
                    "environment"
                } else {
                    "missing"
                };
                json!({"name": name, "source": source, "environment": environment})
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
                job.source == super::events::Source::Installed
                    && !active.contains(&job.id)
                    && job
                        .check
                        .as_ref()
                        .is_some_and(|check| check.pending && check.repo.installation.is_some())
            })
            .cloned()
            .collect();
        for mut job in jobs {
            let check = job.check.as_mut().unwrap();
            let owner = check.repo.full_name.split('/').next().unwrap_or("");
            let inventory = self.installations.lock().await;
            let inaccessible = !inventory.install_url.is_empty()
                && !inventory
                    .installations
                    .iter()
                    .any(|i| i.id == check.repo.installation && i.allowed && !i.suspended);
            drop(inventory);
            if inaccessible || !self.allowed_owner(&app, owner).await {
                check.pending = false;
                self.update_job(job).await?;
                continue;
            }
            let token = self
                .api
                .installation_token(
                    &app,
                    check.repo.installation_id()?,
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
