//! Opt-in fix controller. Repository configuration never supplies policy or credentials.
use super::{
    Server, app, auth, dashboard,
    events::{Job, Kind, Repo, Source},
};
use crate::{findings::Finding, fix};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Semaphore, watch};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    #[default]
    Manual,
    AfterReview,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Patch,
    DraftPr,
    SameBranch,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requesters {
    #[default]
    Maintainers,
    Writers,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub attempts: u32,
    pub tool_calls: u32,
    pub seconds: u64,
    pub model_requests: u32,
    pub max_files: usize,
    pub max_patch_kb: usize,
    pub concurrent: usize,
    pub per_user_hour: usize,
    pub per_repo_day: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            attempts: 2,
            tool_calls: 40,
            seconds: 300,
            model_requests: 50,
            max_files: 20,
            max_patch_kb: 200,
            concurrent: 1,
            per_user_hour: 3,
            per_repo_day: 20,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub offer: bool,
    pub execution: bool,
    pub trigger: Trigger,
    pub confirm: bool,
    pub trusted_skip_confirm: bool,
    pub delivery: Delivery,
    pub allow_same_branch: bool,
    pub require_approval: bool,
    pub requesters: Requesters,
    pub author_preview: bool,
    pub allow_workflow_changes: bool,
    pub external: bool,
    pub rereview: bool,
    pub branch_prefix: String,
    pub test_command: Vec<String>,
    pub test_timeout_seconds: u64,
    pub limits: Limits,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            offer: true,
            execution: false,
            trigger: Trigger::Manual,
            confirm: true,
            trusted_skip_confirm: false,
            delivery: Delivery::Patch,
            allow_same_branch: false,
            require_approval: true,
            requesters: Requesters::Maintainers,
            author_preview: false,
            allow_workflow_changes: false,
            external: false,
            rereview: true,
            branch_prefix: "revera/fix-".into(),
            test_command: vec![],
            test_timeout_seconds: 300,
            limits: Limits::default(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Override {
    pub execution: Option<bool>,
    pub trigger: Option<Trigger>,
    pub delivery: Option<Delivery>,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        let l = &self.limits;
        if !(1..=10).contains(&l.attempts)
            || !(1..=1000).contains(&l.tool_calls)
            || !(1..=3600).contains(&l.seconds)
            || !(2..=1000).contains(&l.model_requests)
            || !(1..=100).contains(&l.max_files)
            || !(1..=512).contains(&l.max_patch_kb)
            || !(1..=16).contains(&l.concurrent)
            || !(1..=1000).contains(&l.per_user_hour)
            || !(1..=10000).contains(&l.per_repo_day)
            || !(1..=3600).contains(&self.test_timeout_seconds)
        {
            bail!(
                "fix limits must be positive and within supported bounds (attempts ≤10, files ≤100, patch ≤512 KB, concurrency ≤16, seconds ≤3600, requests 2–1000)"
            );
        }
        if self.branch_prefix.is_empty()
            || self.branch_prefix.len() > 100
            || !self
                .branch_prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-_".contains(&b))
            || self.branch_prefix.starts_with('/')
            || self.branch_prefix.contains("//")
        {
            bail!("branch_prefix must be a safe Git branch prefix");
        }
        if self.test_command.len() > 100
            || self
                .test_command
                .iter()
                .any(|s| s.contains('\0') || s.len() > 4000)
        {
            bail!("invalid approved test command");
        }
        Ok(())
    }
    fn effective(&self, o: Option<&Override>) -> Self {
        let mut p = self.clone();
        if let Some(o) = o {
            p.execution = o.execution.unwrap_or(p.execution);
            p.trigger = o.trigger.unwrap_or(p.trigger);
            p.delivery = o.delivery.unwrap_or(p.delivery);
            if o.delivery.is_some_and(|d| d != Delivery::SameBranch) {
                p.allow_same_branch = false;
            }
        }
        if p.delivery == Delivery::SameBranch && !p.allow_same_branch {
            p.delivery = Delivery::Patch;
        }
        if p.delivery == Delivery::SameBranch {
            p.require_approval = true;
        }
        p
    }
}
async fn policy(server: &Server, repo: &str) -> Policy {
    let d = server.dashboard.lock().await;
    d.preferences
        .fixes
        .effective(d.preferences.repos.get(repo).and_then(|r| r.fixes.as_ref()))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Audit {
    at: String,
    actor: String,
    action: String,
    detail: String,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_at: Option<String>,
}
fn one() -> u64 {
    1
}
fn is_one(n: &u64) -> bool {
    *n == 1
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: String,
    pub reason: Option<String>,
    pub source: String,
    pub requested_by: String,
    pub consent: Option<String>,
    pub repo: String,
    pub repo_source: Source,
    pub pr: u64,
    pub pr_url: String,
    pub pr_title: String,
    pub review_job: String,
    pub base_sha: String,
    pub head_sha: String,
    pub findings: Vec<Value>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub progress: Option<String>,
    pub generation: Option<Value>,
    pub verification: Option<Value>,
    pub tests: Option<Value>,
    pub patch: Option<Value>,
    pub usage: Option<Value>,
    pub delivery: Option<Value>,
    pub verify_review: Option<Value>,
    pub events: Vec<Audit>,
}
impl Record {
    fn audit(&mut self, actor: &str, action: &str, detail: &str) {
        self.updated_at = chrono::Utc::now().to_rfc3339();
        let detail = crate::handoff::text(detail, 4000);
        if let Some(event) = self
            .events
            .iter_mut()
            .rev()
            .find(|e| e.actor == actor && e.action == action && e.detail == detail)
        {
            event.count = event.count.saturating_add(1);
            event.last_at = Some(self.updated_at.clone());
            return;
        }
        self.events.push(Audit {
            at: self.updated_at.clone(),
            actor: actor.into(),
            action: action.into(),
            detail,
            count: 1,
            last_at: None,
        });
        self.bound_events();
    }
    fn bound_events(&mut self) {
        while self.events.len() > 200 {
            let requested = self.events.iter().position(|e| e.action == "requested");
            let approved = self.events.iter().rposition(|e| e.action == "approved");
            let published = self.events.iter().rposition(|e| e.action == "published");
            let oldest = (0..self.events.len())
                .find(|i| ![requested, approved, published].contains(&Some(*i)))
                .unwrap();
            self.events.remove(oldest);
        }
    }
    fn transition(&mut self, state: &str, reason: Option<&str>, actor: &str, action: &str) {
        self.state = state.into();
        self.reason = reason.map(|s| crate::handoff::text(s, 4000));
        self.progress = matches!(
            state,
            "generating" | "validating" | "publishing" | "verifying"
        )
        .then(|| state.into());
        self.audit(actor, action, reason.unwrap_or(state));
    }
    fn active(&self) -> bool {
        matches!(
            self.state.as_str(),
            "awaiting_confirmation"
                | "queued"
                | "generating"
                | "validating"
                | "ready"
                | "publishing"
                | "published"
                | "verifying"
                | "verified"
        )
    }
    fn summary(&self) -> Value {
        let mut v = json!(self);
        for field in ["generation", "verification", "tests", "events"] {
            v.as_object_mut().unwrap().remove(field);
        }
        v["files"] = json!(
            self.patch
                .as_ref()
                .and_then(|p| p["files"].as_array())
                .map_or(0, Vec::len)
        );
        if let Some(p) = v["patch"].as_object_mut() {
            p.remove("files");
        }
        v["resolved"] = json!(
            self.verification
                .as_ref()
                .and_then(|p| p["per_finding"].as_array())
                .map_or(0, |fs| fs
                    .iter()
                    .filter(|f| f["verdict"] == "resolved")
                    .count())
        );
        v["findings_count"] = json!(self.findings.len());
        v
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct ContextData {
    repo: Repo,
    findings: Vec<Finding>,
    policy: Policy,
    key: String,
    preview_only: bool,
    #[serde(default)]
    confirmed_by: Option<String>,
    #[serde(default)]
    note: String,
}
#[derive(Clone)]
struct Entry {
    record: Record,
    context: ContextData,
}
#[derive(Clone, Serialize, Deserialize)]
struct Admission {
    at: i64,
    actor: String,
    repo: String,
}
#[derive(Default)]
pub struct Store {
    entries: BTreeMap<String, Entry>,
    running: BTreeMap<String, watch::Sender<bool>>,
    admissions: Vec<Admission>,
    denials: Vec<Admission>,
    attempts: Vec<Admission>,
}
fn dir(data: &FsPath, id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid fix id");
    }
    crate::fsutil::ensure_dir(data, &FsPath::new("fixes").join(id))
}
fn persist(data: &FsPath, entry: &Entry) -> Result<()> {
    let root = dir(data, &entry.record.id)?;
    let mut context = json!(entry.context);
    crate::redact::json(&mut context);
    crate::fsutil::write_atomic_in(
        &root,
        "controller.json",
        &serde_json::to_vec_pretty(&context)?,
    )?;
    let mut record = json!(entry.record);
    crate::redact::json(&mut record);
    crate::fsutil::write_atomic_in(&root, "fix.json", &serde_json::to_vec_pretty(&record)?)?;
    let log = entry
        .record
        .events
        .iter()
        .map(|e| format!("{} {} {} {}\n", e.at, e.actor, e.action, e.detail))
        .collect::<String>();
    crate::fsutil::write_atomic_in(&root, "log.txt", crate::redact::text(&log).as_bytes())
}
impl Store {
    pub fn load(data: &FsPath) -> Result<Self> {
        let root = crate::fsutil::ensure_dir(data, FsPath::new("fixes"))?;
        let mut store = Self {
            admissions: dashboard::read_json(data, "fix-admissions.json")?,
            denials: dashboard::read_json(data, "fix-denials.json")?,
            attempts: dashboard::read_json(data, "fix-attempts.json")?,
            ..Default::default()
        };
        store
            .admissions
            .retain(|a| a.at > chrono::Utc::now().timestamp() - 86400);
        for item in std::fs::read_dir(root)? {
            let item = item?;
            if !item.file_type()?.is_dir() {
                continue;
            }
            let id = item.file_name().to_string_lossy().into_owned();
            let root = dir(data, &id)?;
            let workspace = root.join("workspace");
            if workspace.exists() {
                crate::fsutil::ensure_dir(&root, FsPath::new("workspace"))?;
                std::fs::remove_dir_all(workspace)?;
            }
            for name in ["fix.json", "controller.json"] {
                crate::fsutil::check_regular_target(&root.join(name))?;
            }
            if !root.join("fix.json").exists() {
                continue;
            }
            let mut entry = Entry {
                record: serde_json::from_slice(&std::fs::read(root.join("fix.json"))?)?,
                context: serde_json::from_slice(&std::fs::read(root.join("controller.json"))?)?,
            };
            if entry.record.id != id {
                bail!("fix record identity mismatch");
            }
            entry.record.bound_events();
            match entry.record.state.as_str() {
                "generating" | "validating" => entry.record.transition(
                    "failed",
                    Some("interrupted by restart"),
                    "revera",
                    "failed",
                ),
                "publishing" => entry.record.transition(
                    "publish_failed",
                    Some("interrupted; check GitHub before retrying"),
                    "revera",
                    "failed",
                ),
                "verifying" => {
                    entry.record.transition(
                        "published",
                        Some("follow-up review incomplete"),
                        "revera",
                        "failed",
                    );
                    if let Some(v) = entry.record.verify_review.as_mut() {
                        v["note"] = json!("follow-up review incomplete");
                    }
                }
                _ => {}
            }
            persist(data, &entry)?;
            store.entries.insert(id, entry);
        }
        store.prune(data)?;
        Ok(store)
    }
    fn prune(&mut self, data: &FsPath) -> Result<()> {
        let mut entries: Vec<_> = self
            .entries
            .values()
            .map(|e| (e.record.created_at.clone(), e.record.id.clone()))
            .collect();
        entries.sort_by(|a, b| b.cmp(a));
        for (_, id) in entries.into_iter().skip(200) {
            let e = &self.entries[&id];
            if matches!(
                e.record.state.as_str(),
                "queued" | "generating" | "validating" | "publishing" | "verifying"
            ) || self.running.contains_key(&id)
            {
                continue;
            }
            std::fs::remove_dir_all(dir(data, &id)?)?;
            self.entries.remove(&id);
        }
        Ok(())
    }
}
async fn save(server: &Server, store: &mut Store, entry: Entry) -> Result<Record> {
    persist(&server.settings.data_dir, &entry)?;
    let record = entry.record.clone();
    store.entries.insert(record.id.clone(), entry);
    let _ = server.events.send(("fix", record.summary()));
    Ok(record)
}
async fn change(server: &Server, id: &str, f: impl FnOnce(&mut Entry)) -> Result<Record> {
    let mut store = server.fixes.lock().await;
    let mut e = store.entries.get(id).cloned().context("fix not found")?;
    // Cancellation/discard is final even if an agent was concurrently finishing.
    if !matches!(e.record.state.as_str(), "cancelled" | "discarded") {
        f(&mut e);
    }
    save(server, &mut store, e).await
}
async fn entry(server: &Server, id: &str) -> Result<Entry> {
    server
        .fixes
        .lock()
        .await
        .entries
        .get(id)
        .cloned()
        .context("fix not found")
}

#[derive(Debug)]
pub(super) struct Failure(StatusCode, String);
impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(e: E) -> Self {
        Self(
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::redact::diagnostic(&format!("{:#}", e.into())),
        )
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
type Outcome<T> = std::result::Result<T, Failure>;
fn conflict(s: &str) -> Failure {
    Failure(StatusCode::CONFLICT, s.into())
}
fn forbidden(s: &str) -> Failure {
    Failure(StatusCode::FORBIDDEN, s.into())
}

async fn review(server: &Server, id: &str) -> Outcome<(dashboard::JobSummary, Vec<Finding>)> {
    let job = server
        .dashboard
        .lock()
        .await
        .jobs
        .get(id)
        .cloned()
        .context("review not found")?;
    if job.kind != "review" || job.status != "complete" {
        return Err(conflict("needs a complete review"));
    }
    let path = dashboard::job_dir(&server.settings.data_dir, id)?.join("report.json");
    crate::fsutil::check_regular_target(&path)?;
    let report: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if report["status"] != "complete" {
        return Err(conflict("needs a complete review"));
    }
    if report["head"].as_str() != job.head_sha.as_deref() {
        return Err(conflict("review head does not match its report"));
    }
    let findings = super::api::report_findings(&report);
    if findings.is_empty() {
        return Err(conflict("review has no accepted findings"));
    }
    Ok((job, findings))
}
fn select(findings: &[Finding], ids: Option<&[String]>) -> Result<Vec<Finding>> {
    let Some(ids) = ids else {
        return Ok(findings.to_vec());
    };
    if ids.is_empty() || ids.len() > 100 {
        bail!("select at least one accepted finding (maximum 100)");
    }
    let mut selected = BTreeMap::new();
    for prefix in ids {
        if !(6..=12).contains(&prefix.len()) || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("finding must be a unique hexadecimal ID prefix of at least 6 characters");
        }
        let matches: Vec<_> = findings
            .iter()
            .filter(|f| f.id().starts_with(&prefix.to_ascii_lowercase()))
            .collect();
        if matches.len() != 1 {
            bail!("finding ID is unknown or ambiguous");
        }
        selected.insert(matches[0].id(), matches[0].clone());
    }
    Ok(selected.into_values().collect())
}
async fn live(server: &Server, job: &dashboard::JobSummary) -> Result<(Repo, Value)> {
    if job.source == Source::External {
        let target = super::external::PrUrl {
            repo: job.repo.clone(),
            number: job.pr.context("missing PR")?,
        };
        let resolved = super::external::resolve(server, &target).await?;
        if job.repo_id != Some(resolved.repo.id) {
            bail!("repository identity changed");
        }
        return Ok((resolved.repo, resolved.pr));
    }
    let repo = server
        .dashboard
        .lock()
        .await
        .repos
        .get(&job.repo)
        .map(|i| i.repo.clone())
        .context("repository is no longer installed or approved")?;
    check_installation(server, &repo).await?;
    let app = server
        .setup
        .lock()
        .await
        .app
        .clone()
        .context("app not configured")?;
    let token = server
        .api
        .installation_token(
            &app,
            repo.installation_id()?,
            Some(repo.id),
            app::Permissions::ReadPulls,
        )
        .await?;
    let pr = server
        .api
        .request(
            reqwest::Method::GET,
            &format!(
                "/repos/{}/pulls/{}",
                repo.full_name,
                job.pr.context("missing PR")?
            ),
            &token,
            None,
        )
        .await?;
    Ok((repo, pr))
}
async fn check_installation(server: &Server, repo: &Repo) -> Result<()> {
    let app = server
        .setup
        .lock()
        .await
        .app
        .clone()
        .context("app not configured")?;
    if !server
        .allowed_owner(&app, repo.full_name.split('/').next().unwrap_or(""))
        .await
    {
        bail!("repository owner approval was revoked");
    }
    let inventory = server.installations.lock().await;
    if !inventory
        .installations
        .iter()
        .any(|i| i.id == repo.installation && i.allowed && !i.suspended && i.error.is_none())
    {
        bail!("installation is no longer allowed or available");
    }
    drop(inventory);
    if !server
        .dashboard
        .lock()
        .await
        .repos
        .values()
        .any(|i| i.repo.id == repo.id && i.repo.installation == repo.installation)
    {
        bail!("repository is no longer installed or approved");
    }
    Ok(())
}
fn fresh(pr: &Value, head: &str) -> Result<()> {
    if pr["state"] != "open" || pr["merged"] == true {
        bail!("PR closed");
    }
    if pr["head"]["sha"].as_str() != Some(head) {
        bail!("PR changed since the review; request a new review");
    }
    Ok(())
}
fn key(job: &dashboard::JobSummary, findings: &[Finding]) -> String {
    use sha2::{Digest, Sha256};
    let ids: BTreeSet<_> = findings.iter().map(Finding::id).collect();
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(job.source, &job.repo, job.pr, &job.id, ids, &job.head_sha)).unwrap(),
    ))
}
fn authorized(permission: &Value, p: &Policy) -> bool {
    permission["permission"] == "admin"
        || matches!(permission["role_name"].as_str(), Some("admin" | "maintain"))
        || (p.requesters == Requesters::Writers && permission["permission"] == "write")
}
async fn authorize(
    server: &Server,
    origin: &str,
    actor: &str,
    repo: &Repo,
    pr: &Value,
    p: &Policy,
) -> Result<bool> {
    if origin == "auto" {
        return Ok(false);
    }
    if origin == "dashboard" {
        if !server.dashboard.lock().await.preferences.admin(actor) {
            bail!("administrator access required");
        }
        return Ok(false);
    }
    if actor.is_empty()
        || actor == "ghost"
        || !actor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        bail!("not authorized to request fixes");
    }
    let app = server
        .setup
        .lock()
        .await
        .app
        .clone()
        .context("app not configured")?;
    let token = server
        .api
        .installation_token(
            &app,
            repo.installation_id()?,
            Some(repo.id),
            app::Permissions::ReadPulls,
        )
        .await?;
    let permission = server
        .api
        .request(
            reqwest::Method::GET,
            &format!("/repos/{}/collaborators/{actor}/permission", repo.full_name),
            &token,
            None,
        )
        .await;
    let permission = match permission {
        Ok(value) => value,
        Err(e)
            if e.downcast_ref::<app::ApiStatus>()
                .is_some_and(|s| s.0 == reqwest::StatusCode::NOT_FOUND) =>
        {
            Value::Null
        }
        Err(e) => return Err(e),
    };
    if authorized(&permission, p) {
        return Ok(false);
    }
    if p.author_preview
        && pr["user"]["login"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(actor))
    {
        return Ok(true);
    }
    bail!("not authorized to request fixes")
}

/// All entry points converge here. Consent is attached to this durable record only.
fn admit_attempt(
    s: &Server,
    store: &mut Store,
    actor: &str,
    repo: &str,
    p: &Policy,
) -> Outcome<()> {
    let now = chrono::Utc::now().timestamp();
    let mut attempts = store.attempts.clone();
    attempts.retain(|a| a.at > now - 3600);
    if attempts
        .iter()
        .filter(|a| a.actor.eq_ignore_ascii_case(actor))
        .count()
        >= p.limits.per_user_hour
    {
        return Err(Failure(
            StatusCode::TOO_MANY_REQUESTS,
            "fix command attempt rate limit reached; try later".into(),
        ));
    }
    attempts.push(Admission {
        at: now,
        actor: actor.into(),
        repo: repo.into(),
    });
    crate::fsutil::write_atomic_in(
        &s.settings.data_dir,
        "fix-attempts.json",
        &serde_json::to_vec(&attempts)?,
    )?;
    store.attempts = attempts;
    Ok(())
}
pub async fn request(
    server: &Server,
    origin: &str,
    actor: &str,
    review_job: &str,
    finding_ids: Option<&[String]>,
    once: bool,
) -> Outcome<Record> {
    let (job, all) = review(server, review_job).await?;
    let selected = select(&all, finding_ids)?;
    let p = policy(server, &job.repo).await;
    if job.source == Source::External && !p.external {
        return Err(forbidden(
            "Enable external fix previews in Settings → Fixes first",
        ));
    }
    let (repo, pr) = live(server, &job).await?;
    fresh(&pr, job.head_sha.as_deref().unwrap_or(""))?;
    let permission = authorize(server, origin, actor, &repo, &pr, &p).await;
    let denied = permission
        .as_ref()
        .err()
        .map(|e| crate::redact::diagnostic(&e.to_string()));
    let preview_only = permission.unwrap_or(false);
    let key = key(&job, &selected);
    let mut store = server.fixes.lock().await;
    if denied.is_some() {
        let now = chrono::Utc::now().timestamp();
        let mut denials = store.denials.clone();
        denials.retain(|a| a.at > now - 3600);
        if denials.iter().any(|a| a.actor.eq_ignore_ascii_case(actor)) {
            return Err(Failure(
                StatusCode::TOO_MANY_REQUESTS,
                "denied command reply limit reached".into(),
            ));
        }
        denials.push(Admission {
            at: now,
            actor: actor.into(),
            repo: job.repo.clone(),
        });
        crate::fsutil::write_atomic_in(
            &server.settings.data_dir,
            "fix-denials.json",
            &serde_json::to_vec(&denials)?,
        )?;
        store.denials = denials;
    } else {
        admit_attempt(server, &mut store, actor, &job.repo, &p)?;
    }
    if let Some(mut e) = store
        .entries
        .values()
        .find(|e| e.context.key == key && e.record.active())
        .cloned()
    {
        e.record.audit(actor, "requested", origin);
        e.record.audit(
            actor,
            if denied.is_some() {
                "permission_denied"
            } else {
                "confirmed"
            },
            denied
                .as_deref()
                .unwrap_or("permission granted; existing request"),
        );
        let record = save(server, &mut store, e).await?;
        return if let Some(reason) = denied {
            Err(forbidden(&reason))
        } else {
            Ok(record)
        };
    }
    let now = chrono::Utc::now();
    let count = |window: i64, user: bool| {
        store
            .admissions
            .iter()
            .filter(|a| {
                a.at > now.timestamp() - window
                    && if user {
                        a.actor.eq_ignore_ascii_case(actor)
                    } else {
                        a.repo.eq_ignore_ascii_case(&job.repo)
                    }
            })
            .count()
    };
    if denied.is_none()
        && (count(3600, true) >= p.limits.per_user_hour
            || count(86400, false) >= p.limits.per_repo_day)
    {
        return Err(conflict("fix request rate limit reached; try later"));
    }
    let consent = if denied.is_some() {
        None
    } else if once {
        Some("once".into())
    } else {
        p.execution.then(|| "persistent".into())
    };
    let trusted_skip = p.execution
        && p.trusted_skip_confirm
        && !preview_only
        && matches!(origin, "mention" | "check");
    let queue = consent.is_some() && (once || origin == "auto" || !p.confirm || trusted_skip);
    let mut record = Record { id: uuid::Uuid::new_v4().simple().to_string(), created_at: now.to_rfc3339(), updated_at: now.to_rfc3339(), state: if queue {"queued"} else {"awaiting_confirmation"}.into(), reason: None, source: origin.into(), requested_by: actor.into(), consent, repo: job.repo.clone(), repo_source:job.source, pr:job.pr.unwrap(), pr_url:format!("https://github.com/{}/pull/{}",job.repo,job.pr.unwrap()), pr_title:crate::handoff::text(pr["title"].as_str().unwrap_or(""),500), review_job:job.id, base_sha:job.base_sha.clone().filter(|s|crate::git::is_oid(s)).context("review missing base SHA")?, head_sha:job.head_sha.clone().filter(|s|crate::git::is_oid(s)).context("review missing head SHA")?, findings:selected.iter().map(|f|json!({"id":f.id(),"severity":f.severity,"title":crate::handoff::text(&f.title,500),"file":crate::handoff::text(&f.file,1000),"start_line":f.start_line,"end_line":f.end_line.unwrap_or(f.start_line)})).collect(), attempt:0, max_attempts:p.limits.attempts, progress:None, generation:None, verification:None, tests:None, patch:None, usage:None, delivery:None, verify_review:None, events:vec![] };
    record.audit(actor, "requested", origin);
    if let Some(reason) = denied.as_deref() {
        record.transition("failed", Some(reason), actor, "permission_denied");
    } else {
        record.audit(actor, "confirmed", "permission granted");
    }
    if once && denied.is_none() {
        record.audit(actor, "consent_once", "this request only");
    }
    if denied.is_none() {
        let mut admissions = store.admissions.clone();
        admissions.retain(|a| a.at > now.timestamp() - 86400);
        admissions.push(Admission {
            at: now.timestamp(),
            actor: actor.into(),
            repo: record.repo.clone(),
        });
        crate::fsutil::write_atomic_in(
            &server.settings.data_dir,
            "fix-admissions.json",
            &serde_json::to_vec(&admissions)?,
        )?;
        store.admissions = admissions;
    }
    let record = save(
        server,
        &mut store,
        Entry {
            record,
            context: ContextData {
                repo,
                findings: selected,
                policy: p,
                key,
                preview_only,
                confirmed_by: (origin == "dashboard").then(|| actor.into()),
                note: String::new(),
            },
        },
    )
    .await?;
    store.prune(&server.settings.data_dir)?;
    drop(store);
    if let Some(reason) = denied {
        return Err(forbidden(&reason));
    }
    server.fix_notify.notify_one();
    status(server, &record).await;
    Ok(record)
}

#[derive(Serialize)]
struct Capability {
    mode: Delivery,
    available: bool,
    reason: String,
}
async fn capabilities(
    server: &Server,
    e: &Entry,
    pr: Option<&Value>,
    permissions: Option<&BTreeMap<String, String>>,
) -> Vec<Capability> {
    let p = policy(server, &e.record.repo).await;
    let settings_url = server.installations.lock().await.settings_url.clone();
    let settings_url = if settings_url.is_empty() {
        let slug = server
            .setup
            .lock()
            .await
            .app
            .as_ref()
            .map(|a| a.slug.clone())
            .unwrap_or_default();
        format!("https://github.com/settings/apps/{slug}/permissions")
    } else {
        settings_url
    };
    let owned;
    let permissions = if let Some(v) = permissions {
        Some(v)
    } else {
        owned = server
            .installations
            .lock()
            .await
            .installations
            .iter()
            .find(|i| i.id == e.context.repo.installation && i.allowed && !i.suspended)
            .map(|i| i.permissions.clone());
        owned.as_ref()
    };
    let fork = pr.is_none_or(|pr| {
        pr["head"]["repo"]["full_name"]
            .as_str()
            .is_none_or(|s| !s.eq_ignore_ascii_case(&e.record.repo))
    });
    let workflows = e
        .record
        .patch
        .as_ref()
        .and_then(|p| p["files"].as_array())
        .is_some_and(|fs| {
            fs.iter().any(|f| {
                f["path"]
                    .as_str()
                    .is_some_and(|s| s.starts_with(".github/workflows/"))
            })
        });
    let reason = |mode| {
        if e.record.repo_source == Source::External {
            return "Revera is not installed on this repository".into();
        }
        if e.context.preview_only {
            return "PR author requests are private previews only".into();
        }
        if !permissions.is_some_and(|p| p.get("contents").is_some_and(|v| v == "write")) {
            return format!("Needs Contents: write — update the app's permissions: {settings_url}");
        }
        if fork {
            return if mode == Delivery::SameBranch {
                "Revera cannot push to contributor forks"
            } else {
                "the PR branch lives in a fork"
            }
            .into();
        }
        if mode == Delivery::SameBranch && !p.allow_same_branch {
            return "Same-branch delivery is disabled by server or repository policy".into();
        }
        if workflows
            && (!p.allow_workflow_changes
                || !permissions.is_some_and(|p| p.get("workflows").is_some_and(|v| v == "write")))
        {
            return "Workflow changes need policy approval and Workflows: write".into();
        }
        String::new()
    };
    let mut out = vec![Capability {
        mode: Delivery::Patch,
        available: true,
        reason: String::new(),
    }];
    for mode in [Delivery::DraftPr, Delivery::SameBranch] {
        let reason = reason(mode);
        out.push(Capability {
            mode,
            available: reason.is_empty(),
            reason,
        });
    }
    out
}

pub fn router() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/fixes", get(list).post(create))
        .route("/api/fixes/plan", get(plan))
        .route("/api/fixes/{id}", get(detail))
        .route("/api/fixes/{id}/patch.diff", get(download))
        .route("/api/fixes/{id}/instructions.md", get(instructions))
        .route("/api/fixes/{id}/confirm", post(confirm))
        .route("/api/fixes/{id}/approve", post(approve))
        .route("/api/fixes/{id}/regenerate", post(regenerate))
        .route("/api/fixes/{id}/cancel", post(cancel))
        .route("/api/fixes/{id}/discard", post(discard))
        .route("/api/settings/fixes", get(settings).put(save_settings))
        .route(
            "/api/repos/{owner}/{repo}/fixes",
            axum::routing::put(save_override),
        )
}
#[derive(Default, Deserialize)]
struct Filter {
    repo: Option<String>,
    state: Option<String>,
    job: Option<String>,
}
async fn list(State(s): State<Arc<Server>>, Query(q): Query<Filter>) -> Json<Value> {
    let store = s.fixes.lock().await;
    let mut entries: Vec<_> = store
        .entries
        .values()
        .filter(|e| {
            q.repo
                .as_ref()
                .is_none_or(|v| v.is_empty() || *v == e.record.repo)
                && q.state
                    .as_ref()
                    .is_none_or(|v| v.is_empty() || *v == e.record.state)
                && q.job
                    .as_ref()
                    .is_none_or(|v| v.is_empty() || *v == e.record.review_job)
        })
        .collect();
    entries.sort_by(|a, b| b.record.updated_at.cmp(&a.record.updated_at));
    Json(json!({"fixes":entries.iter().map(|e|e.record.summary()).collect::<Vec<_>>()}))
}
#[derive(Deserialize)]
struct Plan {
    job: String,
    findings: Option<String>,
}
async fn plan(State(s): State<Arc<Server>>, Query(q): Query<Plan>) -> Outcome<Json<Value>> {
    let summary = s
        .dashboard
        .lock()
        .await
        .jobs
        .get(&q.job)
        .cloned()
        .context("review not found")?;
    let p = policy(&s, &summary.repo).await;
    let mut blocked = None;
    let findings = match review(&s, &q.job).await {
        Ok((_, fs)) => fs,
        Err(e) => {
            blocked = Some(e.1);
            vec![]
        }
    };
    let ids = q
        .findings
        .map(|v| v.split(',').map(str::to_owned).collect::<Vec<_>>());
    let selected = select(&findings, ids.as_deref())?;
    let existing = s
        .fixes
        .lock()
        .await
        .entries
        .values()
        .find(|e| e.context.key == key(&summary, &selected) && e.record.active())
        .map(|e| e.record.summary());
    let mut delivery = vec![Capability {
        mode: Delivery::Patch,
        available: true,
        reason: String::new(),
    }];
    if summary.source == Source::External && !p.external {
        blocked = Some("Enable external fix previews in Settings → Fixes first".into());
    }
    if blocked.is_none() {
        match live(&s, &summary).await {
            Ok((repo, pr)) => {
                if let Err(e) = fresh(&pr, summary.head_sha.as_deref().unwrap_or("")) {
                    blocked = Some(e.to_string());
                }
                let dummy = preview_entry(&summary, repo, selected.clone(), p.clone());
                delivery = capabilities(&s, &dummy, Some(&pr), None).await;
            }
            Err(e) => blocked = Some(e.to_string()),
        }
    }
    let review_url = format!("{}/reviews/{}", s.settings.public_url, summary.id);
    let pr_url = format!(
        "https://github.com/{}/pull/{}",
        summary.repo,
        summary.pr.unwrap_or(0)
    );
    let context = crate::handoff::Context {
        repo: &summary.repo,
        pr_url: &pr_url,
        base: summary.base_sha.as_deref().unwrap_or(""),
        head: summary.head_sha.as_deref().unwrap_or(""),
        review_url: Some(&review_url),
        stale: None,
    };
    Ok(Json(
        json!({"job":summary,"findings":selected.iter().map(|f|super::api::finding_detail(f,&context)).collect::<Vec<_>>(),"execution_enabled":p.execution,"consent_required":!p.execution,"confirm_required":p.confirm,"sandbox":sandbox_json().await,"limits":p.limits,"estimate":{"max_model_requests":p.limits.model_requests,"max_seconds":p.limits.seconds,"attempts":p.limits.attempts},"delivery":delivery,"existing":existing,"blocked":blocked}),
    ))
}
fn preview_entry(
    job: &dashboard::JobSummary,
    repo: Repo,
    findings: Vec<Finding>,
    policy: Policy,
) -> Entry {
    let record:Record=serde_json::from_value(json!({"id":"0","created_at":"","updated_at":"","state":"","reason":null,"source":"dashboard","requested_by":"","consent":null,"repo":job.repo,"repo_source":job.source,"pr":job.pr.unwrap_or(0),"pr_url":"","pr_title":"","review_job":job.id,"base_sha":job.base_sha.clone().unwrap_or_default(),"head_sha":job.head_sha.clone().unwrap_or_default(),"findings":[],"attempt":0,"max_attempts":policy.limits.attempts,"progress":null,"generation":null,"verification":null,"tests":null,"patch":null,"usage":null,"delivery":null,"verify_review":null,"events":[]})).unwrap();
    Entry {
        record,
        context: ContextData {
            repo,
            findings,
            policy,
            key: String::new(),
            preview_only: false,
            confirmed_by: None,
            note: String::new(),
        },
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    job: String,
    findings: Option<Vec<String>>,
    #[serde(default)]
    once: bool,
}
async fn create(
    State(s): State<Arc<Server>>,
    axum::Extension(session): axum::Extension<auth::Session>,
    Json(input): Json<Create>,
) -> Outcome<Json<Value>> {
    Ok(Json(
        json!({"fix":request(&s,"dashboard",&session.user.login,&input.job,input.findings.as_deref(),input.once).await?}),
    ))
}
async fn detail(State(s): State<Arc<Server>>, Path(id): Path<String>) -> Outcome<Json<Value>> {
    let e = entry(&s, &id).await?;
    let job = s
        .dashboard
        .lock()
        .await
        .jobs
        .get(&e.record.review_job)
        .cloned();
    let live = if let Some(job) = job {
        live(&s, &job).await.ok()
    } else {
        None
    };
    let delivery = capabilities(&s, &e, live.as_ref().map(|(_, pr)| pr), None).await;
    let patch = read_patch(&s, &id)?;
    let approve = e.record.state == "ready"
        && delivery
            .iter()
            .any(|c| c.mode != Delivery::Patch && c.available)
        && live
            .as_ref()
            .is_some_and(|(_, pr)| fresh(pr, &e.record.head_sha).is_ok());
    let state = e.record.state.as_str();
    let regen = regenerable(&e.record);
    Ok(Json(
        json!({"fix":e.record,"patch":patch,"delivery":delivery,"actions":{"confirm":state=="awaiting_confirmation","approve":approve,"regenerate":regen,"cancel":matches!(state,"awaiting_confirmation"|"queued"|"generating"|"validating"),"discard":matches!(state,"ready"|"failed"|"validation_failed"|"unsupported"|"stale"|"publish_failed"|"cancelled")},"reasons":{"approve":if approve{""}else{"Needs a ready patch, fresh open PR, and an available write destination"},"regenerate":if regen{""}else{"Attempt limit reached or this state cannot regenerate"}}}),
    ))
}
fn read_patch(s: &Server, id: &str) -> Result<String> {
    let path = dir(&s.settings.data_dir, id)?.join("patch.diff");
    crate::fsutil::check_regular_target(&path)?;
    if !path.exists() {
        return Ok(String::new());
    }
    Ok(crate::text::excerpt_bytes(
        &crate::redact::text(&std::fs::read_to_string(path)?),
        512 * 1024,
    ))
}
async fn download(State(s): State<Arc<Server>>, Path(id): Path<String>) -> Outcome<Response> {
    entry(&s, &id).await?;
    Ok((
        [
            ("content-type", "text/plain; charset=utf-8"),
            (
                "content-disposition",
                "attachment; filename=\"revera-fix.diff\"",
            ),
        ],
        read_patch(&s, &id)?,
    )
        .into_response())
}
async fn instructions(State(s): State<Arc<Server>>, Path(id): Path<String>) -> Outcome<Response> {
    let e = entry(&s, &id).await?;
    let job = s
        .dashboard
        .lock()
        .await
        .jobs
        .get(&e.record.review_job)
        .cloned();
    let stale = if let Some(job) = job {
        live(&s, &job)
            .await
            .ok()
            .map(|(_, pr)| fresh(&pr, &e.record.head_sha).is_err())
    } else {
        None
    };
    let url = format!("{}/reviews/{}", s.settings.public_url, e.record.review_job);
    let c = crate::handoff::Context {
        repo: &e.record.repo,
        pr_url: &e.record.pr_url,
        base: &e.record.base_sha,
        head: &e.record.head_sha,
        review_url: Some(&url),
        stale,
    };
    Ok((
        [
            ("content-type", "text/markdown; charset=utf-8"),
            (
                "content-disposition",
                "attachment; filename=\"revera-fix-instructions.md\"",
            ),
        ],
        crate::handoff::build(&c, &e.context.findings, None),
    )
        .into_response())
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Consent {
    once: bool,
}
async fn confirm(
    State(s): State<Arc<Server>>,
    Path(id): Path<String>,
    axum::Extension(session): axum::Extension<auth::Session>,
    Json(input): Json<Consent>,
) -> Outcome<Json<Value>> {
    let mut store = s.fixes.lock().await;
    let mut e = store.entries.get(&id).cloned().context("fix not found")?;
    let p = policy(&s, &e.record.repo).await;
    if e.record.state != "awaiting_confirmation" {
        return Err(conflict("fix is not awaiting confirmation"));
    }
    if !input.once && !p.execution {
        return Err(conflict(
            "one-time consent required while execution is disabled",
        ));
    }
    e.record.consent = Some(if input.once { "once" } else { "persistent" }.into());
    e.context.confirmed_by = Some(session.user.login.clone());
    e.record.audit(
        &session.user.login,
        if input.once {
            "consent_once"
        } else {
            "confirmed"
        },
        "generation approved",
    );
    e.record
        .transition("queued", None, &session.user.login, "confirmed");
    let r = save(&s, &mut store, e).await?;
    drop(store);
    s.fix_notify.notify_one();
    Ok(Json(json!({"fix":r})))
}
fn regenerable(r: &Record) -> bool {
    r.attempt < r.max_attempts
        && matches!(
            r.state.as_str(),
            "ready" | "failed" | "validation_failed" | "unsupported" | "cancelled"
        )
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Guidance {
    note: String,
}
async fn regenerate(
    State(s): State<Arc<Server>>,
    Path(id): Path<String>,
    axum::Extension(session): axum::Extension<auth::Session>,
    Json(input): Json<Guidance>,
) -> Outcome<Json<Value>> {
    if input.note.chars().count() > 2000 {
        return Err(Failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "guidance exceeds 2000 characters".into(),
        ));
    }
    let mut store = s.fixes.lock().await;
    let mut e = store.entries.get(&id).cloned().context("fix not found")?;
    if !regenerable(&e.record) || store.running.contains_key(&id) {
        return Err(conflict("attempt limit reached or state cannot regenerate"));
    }
    e.context.note = crate::handoff::text(&input.note, 8000);
    e.context.confirmed_by = Some(session.user.login.clone());
    e.record
        .transition("queued", None, &session.user.login, "confirmed");
    let r = save(&s, &mut store, e).await?;
    drop(store);
    s.fix_notify.notify_one();
    Ok(Json(json!({"fix":r})))
}
async fn stop(s: &Server, id: &str, actor: &str, discard: bool) -> Outcome<Record> {
    let mut store = s.fixes.lock().await;
    let mut e = store.entries.get(id).cloned().context("fix not found")?;
    if matches!(
        e.record.state.as_str(),
        "publishing" | "published" | "verifying" | "verified"
    ) {
        return Err(conflict(
            "published or publishing fixes cannot be cancelled or discarded",
        ));
    }
    if let Some(cancel) = store.running.get(id) {
        cancel.send_replace(true);
    }
    e.record.transition(
        if discard { "discarded" } else { "cancelled" },
        None,
        actor,
        if discard { "discarded" } else { "cancelled" },
    );
    Ok(save(s, &mut store, e).await?)
}
async fn cancel(
    State(s): State<Arc<Server>>,
    Path(id): Path<String>,
    axum::Extension(session): axum::Extension<auth::Session>,
) -> Outcome<Json<Value>> {
    Ok(Json(
        json!({"fix":stop(&s,&id,&session.user.login,false).await?}),
    ))
}
async fn discard(
    State(s): State<Arc<Server>>,
    Path(id): Path<String>,
    axum::Extension(session): axum::Extension<auth::Session>,
) -> Outcome<Json<Value>> {
    Ok(Json(
        json!({"fix":stop(&s,&id,&session.user.login,true).await?}),
    ))
}
async fn sandbox_json() -> Value {
    let p = fix::sandbox::probe().await;
    json!({"available":p.available,"proc":p.proc,"reason":p.reason.unwrap_or_default()})
}
async fn settings(State(s): State<Arc<Server>>) -> Outcome<Json<Value>> {
    let (p, repos, models) = {
        let d = s.dashboard.lock().await;
        let cfg = s.config_yaml()?.and_then(|y| d.parse(&y).ok());
        (
            d.preferences.fixes.clone(),
            d.preferences
                .repos
                .iter()
                .filter_map(|(k, v)| v.fixes.as_ref().map(|o| (k.clone(), o.clone())))
                .collect::<BTreeMap<_, _>>(),
            json!({"fixer":cfg.as_ref().map(|c|format!("{:?}/{}",c.effective_fixer().protocol,c.effective_fixer().model)).unwrap_or_default(),"fix_verifier":cfg.as_ref().map(|c|format!("{:?}/{}",c.effective_fix_verifier().protocol,c.effective_fix_verifier().model)).unwrap_or_default()}),
        )
    };
    Ok(Json(
        json!({"fixes":p,"repos":repos,"sandbox":sandbox_json().await,"models":models}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyInput {
    fixes: Policy,
}
async fn save_settings(
    State(s): State<Arc<Server>>,
    Json(input): Json<PolicyInput>,
) -> Outcome<Json<Value>> {
    input.fixes.validate()?;
    let mut d = s.dashboard.lock().await;
    let mut p = d.preferences.clone();
    p.fixes = input.fixes;
    dashboard::write_preferences(&s.settings.data_dir, &p)?;
    d.preferences = p;
    drop(d);
    s.fix_notify.notify_one();
    settings(State(s)).await
}
async fn save_override(
    State(s): State<Arc<Server>>,
    Path((owner, repo)): Path<(String, String)>,
    Json(input): Json<Option<Override>>,
) -> Outcome<Json<Value>> {
    let name = format!("{owner}/{repo}");
    let mut d = s.dashboard.lock().await;
    if !d.repos.contains_key(&name) {
        return Err(conflict("repository is not installed"));
    }
    let mut p = d.preferences.clone();
    p.repos.entry(name).or_default().fixes = input;
    dashboard::write_preferences(&s.settings.data_dir, &p)?;
    d.preferences = p;
    Ok(Json(json!({"ok":true})))
}

pub async fn dispatch(server: Arc<Server>) {
    let permits = Arc::new(Semaphore::new(16));
    server.fix_notify.notify_one();
    loop {
        server.fix_notify.notified().await;
        loop {
            if *server.stopping.borrow() {
                return;
            }
            let max = server
                .dashboard
                .lock()
                .await
                .preferences
                .fixes
                .limits
                .concurrent;
            let inventory_ready = !server.installations.lock().await.install_url.is_empty();
            let mut store = server.fixes.lock().await;
            if store.running.len() >= max {
                break;
            }
            let Some(id) = store
                .entries
                .values()
                .filter(|e| {
                    e.record.state == "queued"
                        && !store.running.contains_key(&e.record.id)
                        && (e.record.repo_source == Source::External || inventory_ready)
                })
                .min_by_key(|e| &e.record.created_at)
                .map(|e| e.record.id.clone())
            else {
                break;
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                break;
            };
            let (cancel, rx) = watch::channel(false);
            store.running.insert(id.clone(), cancel);
            drop(store);
            let s = server.clone();
            server.workers.lock().await.spawn(async move {
                run(s.clone(), &id, rx).await;
                s.fixes.lock().await.running.remove(&id);
                drop(permit);
                s.fix_notify.notify_one();
            });
        }
    }
}
pub async fn shutdown(s: &Server) -> Result<()> {
    let mut store = s.fixes.lock().await;
    for tx in store.running.values() {
        tx.send_replace(true);
    }
    let ids: Vec<_> = store
        .entries
        .values()
        .filter(|e| {
            matches!(
                e.record.state.as_str(),
                "queued" | "generating" | "validating"
            )
        })
        .map(|e| e.record.id.clone())
        .collect();
    for id in ids {
        let mut e = store.entries[&id].clone();
        e.record
            .transition("failed", Some("interrupted by restart"), "revera", "failed");
        save(s, &mut store, e).await?;
    }
    Ok(())
}
async fn cancelled(mut rx: watch::Receiver<bool>) {
    if !*rx.borrow() {
        let _ = rx.changed().await;
    }
}
async fn run(s: Arc<Server>, id: &str, rx: watch::Receiver<bool>) {
    let result = tokio::select! {biased;_=cancelled(rx)=>Ok(()),result=generate(&s,id)=>result};
    if let Err(e) = result {
        let reason = crate::redact::diagnostic(&format!("{e:#}"));
        let _ = change(&s, id, |e| {
            e.record
                .transition("failed", Some(&reason), "revera", "failed")
        })
        .await;
    }
    if let Ok(root) = dir(&s.settings.data_dir, id) {
        let ws = root.join("workspace");
        if ws.exists() {
            let _ = std::fs::remove_dir_all(ws);
        }
    }
    if let Ok(e) = entry(&s, id).await {
        status(&s, &e.record).await;
    }
    let _ = s.fixes.lock().await.prune(&s.settings.data_dir);
}

/// Revocations/closures interrupt paid work promptly, not just at publication.
pub async fn invalidate(
    s: &Server,
    repo: u64,
    source: Source,
    number: Option<u64>,
    reason: &str,
) -> Result<()> {
    let mut store = s.fixes.lock().await;
    let ids: Vec<_> = store
        .entries
        .values()
        .filter(|e| {
            e.context.repo.id == repo
                && e.record.repo_source == source
                && number.is_none_or(|n| e.record.pr == n)
                && matches!(
                    e.record.state.as_str(),
                    "awaiting_confirmation" | "queued" | "generating" | "validating" | "ready"
                )
        })
        .map(|e| e.record.id.clone())
        .collect();
    for id in ids {
        if let Some(tx) = store.running.get(&id) {
            tx.send_replace(true);
        }
        let mut e = store.entries[&id].clone();
        e.record
            .transition("cancelled", Some(reason), "revera", "cancelled");
        save(s, &mut store, e).await?;
    }
    Ok(())
}
fn patch_policy(p: &Policy) -> fix::policy::Policy {
    fix::policy::Policy {
        allow_workflow_changes: p.allow_workflow_changes,
        max_patch_bytes: p.limits.max_patch_kb * 1024,
        max_files: p.limits.max_files,
        ..Default::default()
    }
}
fn messages(issues: &[fix::policy::Issue]) -> Vec<String> {
    issues
        .iter()
        .map(|i| {
            format!(
                "{}{}",
                i.path
                    .as_ref()
                    .map(|p| format!("{p}: "))
                    .unwrap_or_default(),
                i.rule
            )
        })
        .collect()
}
fn add_usage(current: &mut Option<Value>, ledger: &crate::report::LedgerReport) {
    let mut v = current.take().unwrap_or_else(
        || json!({"requests":0,"prompt_tokens":0,"completion_tokens":0,"routes":[]}),
    );
    for (key, n) in [
        ("requests", ledger.requests),
        ("prompt_tokens", ledger.prompt_tokens),
        ("completion_tokens", ledger.completion_tokens),
    ] {
        v[key] = json!(v[key].as_u64().unwrap_or(0) + n);
    }
    let mut routes: BTreeSet<String> = v["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    routes.extend(ledger.by_route.iter().map(|r| r.route.clone()));
    v["routes"] = json!(routes);
    *current = Some(v);
}
async fn checkout(s: &Server, e: &Entry) -> Result<PathBuf> {
    let path = if e.record.repo_source == Source::External {
        FsPath::new("external")
            .join(e.context.repo.id.to_string())
            .join(format!("pull-{}", e.record.pr))
    } else {
        FsPath::new("repos").join(&e.record.repo)
    };
    let ws = crate::fsutil::ensure_dir(&s.settings.data_dir, &path)?;
    let mut git = tokio::process::Command::new("git");
    git.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(&ws)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "rev-parse",
            "--absolute-git-dir",
        ])
        .kill_on_drop(true);
    let output = git.output().await?;
    if !output.status.success() {
        bail!("review checkout is unavailable; request a new review");
    }
    let git_dir = PathBuf::from(std::str::from_utf8(&output.stdout)?.trim());
    let present = tokio::process::Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args([
            "cat-file",
            "-e",
            &format!("{}^{{commit}}", e.record.head_sha),
        ])
        .kill_on_drop(true)
        .output()
        .await?;
    if !present.status.success() {
        // Fetch only the pinned commit, using controller-only credentials. No checkout
        // hooks, attributes, credential helpers, or repository commands are run.
        let mut command = tokio::process::Command::new("git");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "http.followRedirects=false",
            ]);
        if e.record.repo_source == Source::Installed {
            let app = s
                .setup
                .lock()
                .await
                .app
                .clone()
                .context("app not configured")?;
            let token = s
                .api
                .installation_token(
                    &app,
                    e.context.repo.installation_id()?,
                    Some(e.context.repo.id),
                    app::Permissions::Index,
                )
                .await?;
            let env = super::runner::child_env(
                "GITHUB_TOKEN",
                &token,
                &s.settings.data_dir,
                &e.record.repo,
            );
            command.envs(env);
        }
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            command
                .args([
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    &format!("https://github.com/{}.git", e.record.repo),
                    &e.record.head_sha,
                ])
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        if !result.status.success() {
            bail!("could not fetch reviewed head");
        }
    }
    Ok(git_dir)
}
async fn authorize_execution(s: &Server, id: &str, pr: &Value) -> Result<()> {
    let e = entry(s, id).await?;
    let p = policy(s, &e.record.repo).await;
    // Legacy dashboard requests retain their original admin; old confirmations
    // are recovered from their audit record rather than trusting the requester.
    let admin = e.context.confirmed_by.as_deref().or_else(|| {
        e.record
            .events
            .iter()
            .rev()
            .find(|event| {
                matches!(event.action.as_str(), "confirmed" | "consent_once")
                    && event.detail == "generation approved"
            })
            .map(|event| event.actor.as_str())
    });
    let (origin, actor) = admin.map_or(
        (e.record.source.as_str(), e.record.requested_by.as_str()),
        |actor| ("dashboard", actor),
    );
    match authorize(s, origin, actor, &e.context.repo, pr, &p).await {
        Ok(preview) => {
            change(s, id, |e| {
                // A confirming administrator never promotes an author preview.
                e.context.preview_only |= preview;
                e.record
                    .audit(actor, "confirmed", "execution permission rechecked");
            })
            .await?;
            Ok(())
        }
        Err(error) => {
            change(s, id, |e| {
                e.record
                    .audit(actor, "permission_denied", "execution permission revoked")
            })
            .await?;
            Err(error)
        }
    }
}
async fn generate(s: &Server, id: &str) -> Result<()> {
    let e = entry(s, id).await?;
    if e.record.state != "queued" {
        return Ok(());
    }
    let (job, _) = review(s, &e.record.review_job)
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?;
    let (_, pr) = match live(s, &job).await {
        Ok(live) => live,
        Err(error) if error.to_string().contains("closed or merged") => {
            change(s, id, |e| {
                e.record
                    .transition("stale", Some("PR closed"), "revera", "stale")
            })
            .await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if let Err(error) = fresh(&pr, &e.record.head_sha) {
        change(s, id, |e| {
            e.record
                .transition("stale", Some(&error.to_string()), "revera", "stale")
        })
        .await?;
        return Ok(());
    }
    let current = policy(s, &e.record.repo).await;
    authorize_execution(s, id, &pr).await?;
    if e.record.consent.is_none()
        || (e.record.consent.as_deref() == Some("persistent") && !current.execution)
    {
        change(s, id, |e| {
            e.record.transition(
                "awaiting_confirmation",
                Some("execution disabled; confirm this run"),
                "revera",
                "confirmed",
            )
        })
        .await?;
        return Ok(());
    }
    if e.record.repo_source == Source::External && !current.external {
        bail!("external fix previews are disabled");
    }
    let probe = fix::sandbox::probe().await;
    if !probe.available {
        change(s, id, |e| {
            e.record
                .transition("unsupported", probe.reason.as_deref(), "revera", "failed")
        })
        .await?;
        return Ok(());
    }
    // Current policy may tighten an admitted request, never expand its snapshot caps.
    let mut p = e.context.policy.clone();
    p.allow_workflow_changes &= current.allow_workflow_changes;
    p.limits.attempts = p.limits.attempts.min(current.limits.attempts);
    p.limits.model_requests = p.limits.model_requests.min(current.limits.model_requests);
    p.limits.seconds = p.limits.seconds.min(current.limits.seconds);
    p.limits.tool_calls = p.limits.tool_calls.min(current.limits.tool_calls);
    p.limits.max_files = p.limits.max_files.min(current.limits.max_files);
    p.limits.max_patch_kb = p.limits.max_patch_kb.min(current.limits.max_patch_kb);
    let (cfg, secrets) = {
        let d = s.dashboard.lock().await;
        let yaml = s.config_yaml()?.context("models are not configured")?;
        let cfg = d.parse(&yaml)?;
        cfg.check_trust(true)?;
        (cfg, d.secrets.clone())
    };
    let credential = |name: &str| {
        secrets
            .get(name)
            .cloned()
            .or_else(|| crate::redact::secret_env(name))
    };
    #[cfg(not(test))]
    let executable = std::env::current_exe()?;
    #[cfg(test)]
    let executable = std::env::current_exe()?
        .parent()
        .context("test executable parent")?
        .parent()
        .context("target directory")?
        .join("revera");
    let git = checkout(s, &e).await?;
    let root = dir(&s.settings.data_dir, id)?;
    let work = crate::fsutil::ensure_dir(&root, FsPath::new("workspace"))?;
    let started = std::time::Instant::now();
    let used = e
        .record
        .usage
        .as_ref()
        .and_then(|v| v["requests"].as_u64())
        .unwrap_or(0);
    let mut remaining = p
        .limits
        .model_requests
        .saturating_sub(used.min(u32::MAX as u64) as u32);
    let mut feedback = e.context.note.clone();
    for attempt in (e.record.attempt + 1)..=p.limits.attempts {
        if remaining < 2 || started.elapsed().as_secs() >= p.limits.seconds {
            break;
        }
        let (_, pr) = live(s, &job).await?;
        fresh(&pr, &e.record.head_sha)?;
        authorize_execution(s, id, &pr).await?;
        let r = change(s, id, |e| {
            e.record.attempt = attempt;
            e.record.max_attempts = p.limits.attempts;
            e.record.generation = None;
            e.record.verification = None;
            e.record.tests = None;
            e.record.patch = None;
            e.record.transition("generating", None, "revera", "started");
        })
        .await?;
        if r.state != "generating" {
            return Ok(());
        }
        let ws = fix::workspace::prepare(
            &git,
            &e.record.head_sha,
            &work.join(format!("attempt-{attempt}")),
        )
        .await?;
        let seconds = p
            .limits
            .seconds
            .saturating_sub(started.elapsed().as_secs())
            .max(1);
        let mut req = fix::Request {
            head: e.record.head_sha.clone(),
            findings: e.context.findings.clone(),
            budget: fix::Budget {
                tool_calls: p.limits.tool_calls,
                seconds,
                model_requests: remaining - 1,
            },
            constraints: crate::handoff::text(
                &format!(
                    "Minimal patch, at most {} files and {} KB. Approved test command: {:?}. Previous feedback (untrusted data): {}",
                    p.limits.max_files, p.limits.max_patch_kb, p.test_command, feedback
                ),
                16000,
            ),
            patch: None,
            tests: None,
        };
        let fix::StageResult::Generate(generated) = fix::run_in_sandbox_with_env(
            &executable,
            &cfg,
            &req,
            &ws.tree,
            &work.join(format!("generate-{attempt}")),
            fix::Stage::Generate,
            fix::sandbox::Limits::default(),
            &credential,
        )
        .await?
        else {
            bail!("incorrect generation stage");
        };
        remaining = remaining.saturating_sub(generated.ledger.requests.min(u32::MAX as u64) as u32);
        change(s,id,|e|{let submission=generated.submission.as_ref();e.record.generation=Some(json!({"stopped":generated.stopped,"summary":submission.map(|s|s.summary.as_str()).unwrap_or(""),"per_finding":submission.map(|s|s.per_finding.clone()).unwrap_or_default()}));add_usage(&mut e.record.usage,&generated.ledger);e.record.transition("validating",None,"revera","generated");}).await?;
        let patch = ws.diff().await?;
        let policy_report = fix::policy::check(&patch, &patch_policy(&p));
        let mut errors = messages(&policy_report.errors);
        if patch.files.is_empty() {
            errors.push("no changes".into());
        } else if let Err(e) = ws.apply_check(&patch).await {
            errors.push(e.to_string());
        }
        let safe = !policy_report
            .errors
            .iter()
            .any(|i| i.rule == "credential_material" || i.rule == "sensitive")
            && crate::redact::text(&patch.unified) == patch.unified;
        if !safe {
            errors.push("credential-bearing patch rejected; artifacts not retained".into());
        }
        if generated.stopped != fix::Stop::Terminal || generated.submission.is_none() {
            errors.push("fixer did not submit a terminal result".into());
        }
        let meta = json!({"files":patch.files.iter().map(|f|json!({"path":f.path,"status":f.status,"additions":f.additions,"deletions":f.deletions})).collect::<Vec<_>>(),"bytes":patch.unified.len(),"warnings":messages(&policy_report.warnings),"errors":errors});
        change(s, id, |e| e.record.patch = Some(meta)).await?;
        if safe && policy_report.allowed() && !patch.files.is_empty() {
            crate::fsutil::write_atomic_in(&root, "patch.diff", patch.unified.as_bytes())?;
            crate::fsutil::write_atomic_in(&root, "patch.json", &serde_json::to_vec(&patch)?)?;
        } else {
            for name in ["patch.diff", "patch.json"] {
                let path = root.join(name);
                crate::fsutil::check_regular_target(&path)?;
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
        }
        if errors.is_empty() {
            let mut profile = fix::sandbox::Profile::default();
            profile.limits.wall = Duration::from_secs(
                p.test_timeout_seconds
                    .min(p.limits.seconds.saturating_sub(started.elapsed().as_secs()))
                    .max(1),
            );
            let tests = fix::tests::run_copy(
                &profile,
                &p.test_command,
                &ws.tree,
                &work.join(format!("tests-{attempt}")),
            )
            .await?;
            let text = tests
                .output
                .as_ref()
                .map(|o| format!("{}{}", o.stdout, o.stderr))
                .or_else(|| tests.reason.clone())
                .unwrap_or_default();
            change(s, id, |e| {
                e.record.tests = Some(
                    json!({"outcome":tests.outcome,"output":crate::handoff::text(&text,65536)}),
                );
                e.record
                    .audit("revera", "tests", "approved sandbox tests completed");
            })
            .await?;
            req.patch = Some(patch);
            req.tests = Some(tests.clone());
            req.budget.model_requests = remaining;
            req.budget.seconds = p
                .limits
                .seconds
                .saturating_sub(started.elapsed().as_secs())
                .max(1);
            if remaining == 0 {
                errors.push("model request limit reached before independent verification".into());
            } else {
                let fix::StageResult::Verify(verified) = fix::run_in_sandbox_with_env(
                    &executable,
                    &cfg,
                    &req,
                    &ws.tree,
                    &work.join(format!("verify-{attempt}")),
                    fix::Stage::Verify,
                    fix::sandbox::Limits::default(),
                    &credential,
                )
                .await?
                else {
                    bail!("incorrect verification stage");
                };
                remaining =
                    remaining.saturating_sub(verified.ledger.requests.min(u32::MAX as u64) as u32);
                let ready = verified.stopped == fix::Stop::Terminal
                    && verified.submission.as_ref().is_some_and(|v| {
                        v.new_problems.is_empty()
                            && v.per_finding
                                .iter()
                                .any(|f| matches!(f.verdict, fix::Verdict::Resolved))
                    })
                    && matches!(
                        tests.outcome,
                        fix::tests::Outcome::Passed | fix::tests::Outcome::NotConfigured
                    );
                change(s, id, |e| {
                    e.record.verification = verified.submission.as_ref().map(|v| json!(v));
                    add_usage(&mut e.record.usage, &verified.ledger);
                    e.record.audit(
                        "revera",
                        "validated",
                        "fresh independent verifier completed",
                    );
                    if ready {
                        e.record.transition("ready", None, "revera", "ready");
                    }
                })
                .await?;
                if ready {
                    return Ok(());
                }
                feedback = format!(
                    "Verifier stopped {:?}; verdicts {}; tests {:?}",
                    verified.stopped,
                    json!(verified.submission),
                    tests.outcome
                );
            }
        } else {
            feedback = errors.join("; ");
        }
        change(s, id, |e| {
            if let Some(p) = e.record.patch.as_mut() {
                p["errors"] = json!(errors);
            }
            e.record.reason = Some(crate::handoff::text(&feedback, 4000));
        })
        .await?;
    }
    let last = entry(s, id)
        .await?
        .record
        .reason
        .unwrap_or_else(|| "validation or resource limits prevented a verified fix".into());
    change(s, id, |e| {
        e.record
            .transition("validation_failed", Some(&last), "revera", "failed")
    })
    .await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    delivery: Delivery,
}
async fn approve(
    State(s): State<Arc<Server>>,
    Path(id): Path<String>,
    axum::Extension(session): axum::Extension<auth::Session>,
    Json(input): Json<Approval>,
) -> Outcome<Json<Value>> {
    Ok(Json(
        json!({"fix":publish(&s,&id,&session.user.login,input.delivery).await?}),
    ))
}
async fn publish(s: &Server, id: &str, actor: &str, mode: Delivery) -> Outcome<Record> {
    if !s.dashboard.lock().await.preferences.admin(actor) {
        return Err(forbidden("administrator access required"));
    }
    if mode == Delivery::Patch {
        return Err(conflict("patch previews are downloaded, not published"));
    }
    // Claim the ready state before any GitHub wait: simultaneous approvals cannot write twice.
    let mut store = s.fixes.lock().await;
    let mut e = store.entries.get(id).cloned().context("fix not found")?;
    if e.record.repo_source == Source::External {
        return Err(forbidden(
            "Revera cannot publish to a repository where it is not installed",
        ));
    }
    if e.record.state != "ready" {
        return Err(conflict("only a ready patch can be approved"));
    }
    e.record.audit(
        actor,
        "approved",
        if mode == Delivery::SameBranch {
            "same_branch"
        } else {
            "draft_pr"
        },
    );
    e.record.transition("publishing", None, actor, "approved");
    save(s, &mut store, e.clone()).await?;
    drop(store);
    let result = deliver(s, &e, mode).await;
    let record = match result {
        Ok(delivery) => {
            let r = change(s, id, |e| {
                e.record.delivery = Some(delivery);
                e.record
                    .transition("published", None, "revera", "published");
            })
            .await?;
            if policy(s, &r.repo).await.rereview
                && let Err(error) = followup(s, id).await
            {
                change(s,id,|e|{e.record.transition("published",Some("follow-up review incomplete"),"revera","failed");e.record.verify_review=Some(json!({"job":"","state":"","resolved":[],"still_open":[],"new_findings":0,"note":crate::handoff::text(&format!("follow-up review could not be queued: {error}"),1000)}));}).await?;
            }
            entry(s, id).await?.record
        }
        Err(error) => {
            let reason = crate::redact::diagnostic(&format!("{error:#}"));
            change(s, id, |e| {
                e.record.delivery = Some(
                    json!({"mode":mode,"branch":"","commit":"","pr":0,"url":"","error":reason}),
                );
                e.record
                    .transition("publish_failed", Some(&reason), "revera", "failed");
            })
            .await?
        }
    };
    status(s, &record).await;
    Ok(record)
}
async fn deliver(s: &Server, e: &Entry, mode: Delivery) -> Result<Value> {
    let job = s
        .dashboard
        .lock()
        .await
        .jobs
        .get(&e.record.review_job)
        .cloned()
        .context("review no longer available")?;
    let (repo, pr) = live(s, &job).await?;
    fresh(&pr, &e.record.head_sha)?;
    if repo.id != e.context.repo.id || repo.installation != e.context.repo.installation {
        bail!("installation or repository changed since approval");
    }
    let app = s
        .setup
        .lock()
        .await
        .app
        .clone()
        .context("app not configured")?;
    // Live grant lookup; cached installation inventory is not publication authority.
    let installation = s
        .api
        .request(
            reqwest::Method::GET,
            &format!("/app/installations/{}", repo.installation_id()?),
            &app.jwt()?,
            None,
        )
        .await?;
    if installation["suspended_at"].as_str().is_some() {
        bail!("installation is suspended");
    }
    let permissions: BTreeMap<String, String> =
        serde_json::from_value(installation["permissions"].clone())?;
    let caps = capabilities(s, e, Some(&pr), Some(&permissions)).await;
    let cap = caps
        .iter()
        .find(|c| c.mode == mode)
        .context("unsupported delivery")?;
    if !cap.available {
        bail!("{}", cap.reason);
    }
    let p = policy(s, &e.record.repo).await;
    let root = dir(&s.settings.data_dir, &e.record.id)?;
    let path = root.join("patch.json");
    crate::fsutil::check_regular_target(&path)?;
    let patch: fix::workspace::Patch = serde_json::from_slice(&std::fs::read(path)?)?;
    if !fix::policy::check(&patch, &patch_policy(&p)).allowed() {
        bail!("patch no longer satisfies current policy");
    }
    if !matches!(
        e.record.tests.as_ref().and_then(|v| v["outcome"].as_str()),
        Some("passed" | "not_configured")
    ) {
        bail!("tests did not pass");
    }
    if !e
        .record
        .verification
        .as_ref()
        .and_then(|v| v["per_finding"].as_array())
        .is_some_and(|fs| fs.iter().any(|f| f["verdict"] == "resolved"))
    {
        bail!("no independently resolved findings");
    }
    let workflows = patch
        .files
        .iter()
        .any(|f| f.path.starts_with(".github/workflows/"));
    let token = s
        .api
        .installation_token(
            &app,
            repo.installation_id()?,
            Some(repo.id),
            if workflows {
                app::Permissions::FixWorkflows
            } else {
                app::Permissions::Fix
            },
        )
        .await?;
    let api = crate::github::api::GitHubApi::with_base(s.api.base(), &token);
    let (owner, name) = e
        .record
        .repo
        .split_once('/')
        .context("invalid repository")?;
    let target = fix::deliver::Repository {
        owner: owner.into(),
        name: name.into(),
    };
    let ids = e
        .context
        .findings
        .iter()
        .map(Finding::id)
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!(
        "Fix Revera findings {ids}\n\n{}",
        e.context
            .findings
            .iter()
            .map(|f| format!(
                "- {}: {}",
                f.id(),
                crate::handoff::text(&f.title, 500).replace(['\n', '\r'], " ")
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let commit =
        fix::deliver::create_commit(&api, &target, &e.record.head_sha, &patch.files, &message)
            .await?;
    // Creating Git objects is not a ref mutation. Recheck policy, installation,
    // PR head and live grants again immediately before the single ref write.
    check_installation(s, &repo).await?;
    let (_, latest) = live(s, &job).await?;
    fresh(&latest, &e.record.head_sha)?;
    let installation = s
        .api
        .request(
            reqwest::Method::GET,
            &format!("/app/installations/{}", repo.installation_id()?),
            &app.jwt()?,
            None,
        )
        .await?;
    let permissions: BTreeMap<String, String> =
        serde_json::from_value(installation["permissions"].clone())?;
    if installation["suspended_at"].as_str().is_some()
        || !capabilities(s, e, Some(&latest), Some(&permissions))
            .await
            .iter()
            .any(|c| c.mode == mode && c.available)
    {
        bail!("installation permissions changed before delivery");
    }
    let branch = pr["head"]["ref"].as_str().context("PR branch is missing")?;
    if latest["head"]["ref"].as_str() != Some(branch) {
        bail!("PR target branch changed");
    }
    match mode {
        Delivery::DraftPr => {
            let draft = fix::deliver::open_draft_pr(
                &api,
                &target,
                &commit,
                p.branch_prefix.trim_end_matches('-'),
                branch,
                &format!("Fix Revera findings {ids}"),
                &format!(
                    "Approved Revera fix preview: {}/fixes/{}\n\nFindings: {ids}",
                    s.settings.public_url, e.record.id
                ),
            )
            .await?;
            Ok(
                json!({"mode":mode,"branch":draft.branch,"commit":draft.commit,"pr":draft.number,"url":draft.url,"error":""}),
            )
        }
        Delivery::SameBranch => {
            fix::deliver::fast_forward_branch(&api, &target, branch, &e.record.head_sha, &commit)
                .await?;
            Ok(
                json!({"mode":mode,"branch":branch,"commit":commit,"pr":e.record.pr,"url":e.record.pr_url,"error":""}),
            )
        }
        Delivery::Patch => bail!("patch delivery cannot write"),
    }
}

#[cfg(test)]
#[path = "fixes_tests.rs"]
mod tests;
async fn followup(s: &Server, id: &str) -> Result<()> {
    let e = entry(s, id).await?;
    let delivery = e.record.delivery.as_ref().context("missing delivery")?;
    let number = delivery["pr"].as_u64().context("missing published PR")?;
    let mut summary = s
        .dashboard
        .lock()
        .await
        .jobs
        .get(&e.record.review_job)
        .cloned()
        .context("review no longer available")?;
    summary.pr = Some(number);
    let (repo, pr) = live(s, &summary).await?;
    if pr["head"]["sha"] != delivery["commit"] {
        bail!("published head changed before follow-up review");
    }
    let mut job = Job::new(
        repo,
        Kind::Review {
            number,
            pr,
            force: true,
        },
        "fix",
    );
    job.mode = summary.mode;
    let review_id = job.id.clone();
    change(s,id,|e|{e.record.verify_review=Some(json!({"job":review_id,"state":"queued","resolved":[],"still_open":[],"new_findings":0,"note":""}));e.record.transition("verifying",None,"revera","published");}).await?;
    s.enqueue(job).await;
    if !s.dashboard.lock().await.jobs.contains_key(&review_id) {
        bail!("follow-up review was not admitted");
    }
    Ok(())
}

pub async fn review_terminated(s: &Server, summary: &dashboard::JobSummary) -> Result<()> {
    let mut store = s.fixes.lock().await;
    let ids: Vec<_> = store
        .entries
        .values()
        .filter(|e| {
            e.record.state == "verifying"
                && e.record
                    .verify_review
                    .as_ref()
                    .is_some_and(|v| v["job"] == summary.id)
        })
        .map(|e| e.record.id.clone())
        .collect();
    for id in ids {
        let mut e = store.entries[&id].clone();
        e.record.transition(
            "published",
            Some("follow-up review did not complete"),
            "revera",
            "failed",
        );
        if let Some(v) = e.record.verify_review.as_mut() {
            v["state"] = json!(summary.status);
            v["note"] = json!("follow-up review did not complete");
            v["resolved"] = json!([]);
        }
        save(s, &mut store, e).await?;
    }
    Ok(())
}
pub async fn review_finished(s: &Server, job: &Job) -> Result<()> {
    let summary = s
        .dashboard
        .lock()
        .await
        .jobs
        .get(&job.id)
        .cloned()
        .context("review missing")?;
    let linked: Vec<_> = s
        .fixes
        .lock()
        .await
        .entries
        .values()
        .filter(|e| {
            e.record
                .verify_review
                .as_ref()
                .is_some_and(|v| v["job"] == job.id)
        })
        .cloned()
        .collect();
    for e in linked {
        let incomplete = if summary.status == "complete" {
            "follow-up review incomplete"
        } else {
            "follow-up review did not complete"
        };
        let report = (|| -> Result<Value> {
            let root = dashboard::job_dir(&s.settings.data_dir, &job.id)?;
            let path = root.join("report.json");
            crate::fsutil::check_regular_target(&path)?;
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        })()
        .ok()
        .filter(|report| {
            summary.status == "complete"
                && report["status"] == "complete"
                && e.record
                    .delivery
                    .as_ref()
                    .is_some_and(|d| report["head"] == d["commit"])
        });
        let record = if let Some(report) = report {
            completed_followup(s, &e, job, &report).await?
        } else {
            change(s, &e.record.id, |e| {
                e.record
                    .transition("published", Some(incomplete), "revera", "failed");
                if let Some(v) = e.record.verify_review.as_mut() {
                    v["state"] = json!(summary.status);
                    v["note"] = json!(incomplete);
                }
            })
            .await?
        };
        status(s, &record).await;
    }
    // Follow-up reviews must never create an automatic repair loop.
    if job.trigger != "fix" && summary.status == "complete" && summary.findings.total > 0 {
        let p = policy(s, &summary.repo).await;
        if p.execution && p.trigger == Trigger::AfterReview {
            let _ = request(s, "auto", "revera", &job.id, None, false).await;
        }
    }
    Ok(())
}
async fn completed_followup(s: &Server, e: &Entry, job: &Job, report: &Value) -> Result<Record> {
    let accepted: BTreeSet<_> = super::api::report_findings(report)
        .iter()
        .map(Finding::id)
        .collect();
    let selected: BTreeSet<_> = e.context.findings.iter().map(Finding::id).collect();
    let same = e
        .record
        .delivery
        .as_ref()
        .is_some_and(|d| d["mode"] == "same_branch");
    // Same-branch absence alone is not evidence: use review state, which
    // retains open findings not covered by the follow-up diff.
    let open: BTreeSet<String> = if same {
        report["plan"]["state"]["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| matches!(f["status"].as_str(), Some("open" | "uncertain")))
            .filter_map(|f| f["id"].as_str().map(str::to_owned))
            .collect()
    } else {
        accepted.clone()
    };
    let pre: BTreeSet<String> = e
        .record
        .verification
        .as_ref()
        .and_then(|v| v["per_finding"].as_array())
        .into_iter()
        .flatten()
        .filter(|f| f["verdict"] == "resolved")
        .filter_map(|f| f["id"].as_str().map(str::to_owned))
        .collect();
    let state_resolved: BTreeSet<_> = report["plan"]["state"]["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["status"] == "resolved")
        .filter_map(|f| f["id"].as_str())
        .collect();
    let resolved: Vec<_> = selected
        .iter()
        .filter(|id| {
            !open.contains(*id)
                && if same {
                    state_resolved.contains(id.as_str())
                } else {
                    pre.contains(*id)
                }
        })
        .cloned()
        .collect();
    let still_open: Vec<_> = selected
        .iter()
        .filter(|id| !resolved.contains(id))
        .cloned()
        .collect();
    let note = if same {
        "Only explicit resolved follow-up state is resolution evidence; absent IDs remain still_open/uncertain, with no resolution inferred"
    } else {
        "Draft PR resolution uses the pre-publish independent verifier when the finding is absent from the follow-up review"
    };
    change(s,&e.record.id,|e|{e.record.verify_review=Some(json!({"job":job.id,"state":"complete","resolved":resolved,"still_open":still_open,"new_findings":accepted.difference(&selected).count(),"note":note}));e.record.transition("verified",None,"revera","verified");}).await
}
async fn status(s: &Server, r: &Record) {
    if r.repo_source != Source::Installed
        || !matches!(
            r.state.as_str(),
            "awaiting_confirmation"
                | "ready"
                | "published"
                | "verifying"
                | "verified"
                | "failed"
                | "validation_failed"
                | "publish_failed"
        )
    {
        return;
    }
    if let Err(error) = update_status(s, r).await {
        tracing::warn!(
            "fix status: {}",
            crate::redact::diagnostic(&error.to_string())
        );
    }
}
async fn update_status(s: &Server, r: &Record) -> Result<()> {
    let e = entry(s, &r.id).await?;
    check_installation(s, &e.context.repo).await?;
    let app = s.setup.lock().await.app.clone().context("app missing")?;
    let token = s
        .api
        .installation_token(
            &app,
            e.context.repo.installation_id()?,
            Some(e.context.repo.id),
            app::Permissions::Comments,
        )
        .await?;
    let comments = s
        .api
        .list(
            &format!("/repos/{}/issues/{}/comments", r.repo, r.pr),
            &token,
            None,
        )
        .await?;
    let yaml = s.config_yaml()?.context("config missing")?;
    let marker = s.dashboard.lock().await.parse(&yaml)?.github.summary_marker;
    let login = format!("{}[bot]", app.slug);
    let Some(comment) = comments.iter().find(|c| {
        c["user"]["login"]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case(&login))
            && c["body"].as_str().is_some_and(|v| v.contains(&marker))
    }) else {
        return Ok(());
    };
    let body = comment["body"].as_str().context("comment body missing")?;
    let block = format!(
        "<!-- revera:fix-status -->\nFix: **{}** · [Dashboard]({}/fixes/{})\n<!-- /revera:fix-status -->",
        r.state, s.settings.public_url, r.id
    );
    let body = if let Some(old) = crate::github::publish::fix_status_block(body) {
        body.replacen(old, &block, 1)
    } else {
        format!("{body}\n{block}")
    };
    if body == comment["body"].as_str().unwrap_or("") {
        return Ok(());
    }
    s.api
        .request(
            reqwest::Method::PATCH,
            &format!(
                "/repos/{}/issues/comments/{}",
                r.repo,
                comment["id"].as_u64().context("comment id missing")?
            ),
            &token,
            Some(json!({"body":body})),
        )
        .await?;
    Ok(())
}

async fn latest(s: &Server, repo: &str, number: u64) -> Option<dashboard::JobSummary> {
    s.dashboard
        .lock()
        .await
        .jobs
        .values()
        .filter(|j| {
            j.source == Source::Installed
                && j.repo.eq_ignore_ascii_case(repo)
                && j.pr == Some(number)
                && j.status == "complete"
        })
        .max_by(|a, b| a.queued_at.cmp(&b.queued_at).then(a.id.cmp(&b.id)))
        .cloned()
}
pub async fn mention(s: &Server, app: &app::App, command: super::mentions::Request) -> Result<()> {
    if !s
        .allowed_owner(app, command.repo.full_name.split('/').next().unwrap_or(""))
        .await
    {
        return Ok(());
    }
    if !s
        .mentions
        .lock()
        .await
        .claim(&s.settings.data_dir, command.repo.id, command.comment)?
    {
        return Ok(());
    }
    check_installation(s, &command.repo).await?;
    let token = s
        .api
        .installation_token(
            app,
            command.repo.installation_id()?,
            Some(command.repo.id),
            app::Permissions::Comments,
        )
        .await?;
    let Some(job) = latest(s, &command.repo.full_name, command.number).await else {
        return super::mentions::reply(
            s,
            &command,
            &token,
            &format!("Ask for a review first: `@{} review`.", app.slug),
        )
        .await;
    };
    let super::mentions::Command::Fix(ref action) = command.command else {
        return Ok(());
    };
    let outcome = match action {
        super::mentions::FixCommand::Request(ids) => {
            request(s, "mention", &command.actor, &job.id, ids.as_deref(), false).await
        }
        super::mentions::FixCommand::Status | super::mentions::FixCommand::Cancel => {
            let p = policy(s, &job.repo).await;
            let (_, pr) = live(s, &job).await?;
            let auth = authorize(s, "mention", &command.actor, &command.repo, &pr, &p).await;
            match auth {
                Err(e) => Err(forbidden(&e.to_string())),
                Ok(preview) => {
                    let mut store = s.fixes.lock().await;
                    if let Err(error) = admit_attempt(s, &mut store, &command.actor, &job.repo, &p)
                    {
                        if error.0 == StatusCode::TOO_MANY_REQUESTS {
                            return Ok(());
                        }
                        bail!("{}", error.1);
                    }
                    drop(store);
                    let e = s
                        .fixes
                        .lock()
                        .await
                        .entries
                        .values()
                        .filter(|e| {
                            e.record.repo == job.repo
                                && e.record.pr == command.number
                                && (!preview
                                    || e.record.requested_by.eq_ignore_ascii_case(&command.actor))
                        })
                        .max_by_key(|e| &e.record.created_at)
                        .cloned();
                    if let Some(e) = e {
                        if matches!(action, super::mentions::FixCommand::Cancel) {
                            stop(s, &e.record.id, &command.actor, false).await
                        } else {
                            Ok(e.record)
                        }
                    } else {
                        Err(conflict(
                            "No fix request exists; open the dashboard to request one",
                        ))
                    }
                }
            }
        }
    };
    match outcome {
        Ok(r)
            if r.state == "awaiting_confirmation"
                || matches!(action, super::mentions::FixCommand::Status) =>
        {
            super::mentions::reply(
                s,
                &command,
                &token,
                &format!(
                    "Fix {}: [open the dashboard]({}/fixes/{}) to confirm or view this request.",
                    r.state, s.settings.public_url, r.id
                ),
            )
            .await
        }
        Ok(_) => {
            s.api
                .request(
                    reqwest::Method::POST,
                    &format!(
                        "/repos/{}/issues/comments/{}/reactions",
                        command.repo.full_name, command.comment
                    ),
                    &token,
                    Some(json!({"content":"eyes"})),
                )
                .await?;
            Ok(())
        }
        Err(e) if e.0 == StatusCode::TOO_MANY_REQUESTS => Ok(()),
        Err(e) => {
            // Only controller-generated explanations are public; never echo PR,
            // model, or arbitrary error text as active Markdown.
            let explanation = crate::report::public_text(&e.1);
            super::mentions::reply(
                s,
                &command,
                &token,
                &format!(
                    "{explanation}. [Open the dashboard]({}/fixes/new?job={}).",
                    s.settings.public_url, job.id
                ),
            )
            .await
        }
    }
}
pub async fn check_action(s: &Server, app: &app::App, payload: &Value) -> Result<()> {
    let check = &payload["check_run"];
    if payload["action"] != "requested_action"
        || payload["requested_action"]["identifier"] != "revera_fix"
        || check["app"]["id"].as_u64() != Some(app.id)
        || check["name"] != "Revera"
    {
        return Ok(());
    }
    let actor = payload["sender"]["login"].as_str().unwrap_or("");
    if payload["sender"]["type"] == "Bot" || actor.ends_with("[bot]") || actor == "ghost" {
        return Ok(());
    }
    let number = check["external_id"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .context("invalid check PR association")?;
    let id = check["id"].as_u64().context("missing check id")?;
    let installation = payload["installation"]["id"]
        .as_u64()
        .context("missing installation")?;
    let repo = Repo::parse(&payload["repository"], installation)?;
    let job = s
        .dashboard
        .lock()
        .await
        .jobs
        .values()
        .find(|j| {
            j.source == Source::Installed
                && j.pr == Some(number)
                && j.repo == repo.full_name
                && j.check.as_ref().is_some_and(|c| {
                    c.id == id && c.repo.id == repo.id && c.repo.installation == Some(installation)
                })
        })
        .cloned()
        .context("check does not match a stored review")?;
    if check["head_sha"].as_str() != job.head_sha.as_deref() {
        bail!("check head does not match the review");
    }
    let (_, pr) = live(s, &job).await?;
    fresh(&pr, job.head_sha.as_deref().unwrap_or(""))?;
    // Check deliveries have their own durable claim namespace, separate from
    // issue-comment IDs. Claim before any request/status write.
    {
        let _store = s.fixes.lock().await;
        let mut claims: Vec<(u64, u64, String)> =
            dashboard::read_json(&s.settings.data_dir, "fix-checks.json")?;
        if claims.contains(&(repo.id, id, actor.into())) {
            return Ok(());
        }
        claims.push((repo.id, id, actor.into()));
        if claims.len() > 4096 {
            claims.remove(0);
        }
        crate::fsutil::write_atomic_in(
            &s.settings.data_dir,
            "fix-checks.json",
            &serde_json::to_vec(&claims)?,
        )?;
    }
    request(s, "check", actor, &job.id, None, false)
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?;
    Ok(())
}
