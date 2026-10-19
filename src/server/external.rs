//! Public-only GitHub reads and explicitly selected private review targets.
//! This module never obtains a GitHub credential and has no GitHub write client.
use super::{
    Server, auth, dashboard,
    events::{Job, Kind, Repo, Source},
};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path as FsPath,
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub tracking_enabled: bool,
    pub poll_minutes: u64,
    pub max_tracked: usize,
    pub max_repo_size_kb: u64,
    pub reviews_per_hour: usize,
    pub concurrency: usize,
    pub retention_days: u64,
    pub max_checkouts: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            tracking_enabled: false,
            poll_minutes: 60,
            max_tracked: 25,
            max_repo_size_kb: 1024 * 1024,
            reviews_per_hour: 20,
            concurrency: 1,
            retention_days: 7,
            max_checkouts: 50,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if !(15..=1440).contains(&self.poll_minutes)
            || !(1..=25).contains(&self.max_tracked)
            || !(1..=100).contains(&self.reviews_per_hour)
            || !(1..=4).contains(&self.concurrency)
            || !(1..=1024 * 1024).contains(&self.max_repo_size_kb)
            || !(1..=90).contains(&self.retention_days)
            || !(1..=100).contains(&self.max_checkouts)
        {
            bail!(
                "external limits: polling 15–1440 minutes, tracked 1–25, reviews/hour 1–100, concurrency 1–4, repository size 1–1048576 KB, retention 1–90 days, checkouts 1–100"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrUrl {
    pub repo: String,
    pub number: u64,
}
impl PrUrl {
    pub fn parse(input: &str) -> Result<Self> {
        // Inspect the original text, not Url's normalized path (which removes ../).
        let path = input
            .strip_prefix("https://github.com/")
            .context("use https://github.com/OWNER/REPO/pull/NUMBER")?;
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() != 4
            || parts[2] != "pull"
            || parts[3].is_empty()
            || !parts[3].bytes().all(|c| c.is_ascii_digit())
        {
            bail!("use https://github.com/OWNER/REPO/pull/NUMBER");
        }
        let repo = format!("{}/{}", parts[0], parts[1]);
        Repo::parse(&json!({"id": 1, "full_name": repo}), None)?;
        let number = parts[3]
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .context("PR number must be positive")?;
        Ok(Self { repo, number })
    }
    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }
    fn api_path(&self) -> String {
        format!("/repos/{}/pulls/{}", self.repo, self.number)
    }
}

#[derive(Debug)]
pub struct Failure {
    pub status: StatusCode,
    pub message: String,
    pub retry_after: Option<u64>,
}
impl Failure {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
        }
    }
    fn access() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Repository or PR is private, deleted or inaccessible; it requires suitable access/installation.",
        )
    }
    fn upstream(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            crate::redact::diagnostic(&error.to_string()),
        )
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}
impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::redact::diagnostic(&format!("{e:#}")),
        )
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"error": crate::redact::diagnostic(&self.message), "retry_after": self.retry_after}))).into_response();
        if let Some(seconds) = self.retry_after {
            response.headers_mut().insert(
                "Retry-After",
                HeaderValue::from_str(&seconds.to_string()).unwrap(),
            );
        }
        response
    }
}
type Outcome<T> = std::result::Result<T, Failure>;

#[derive(Clone, Serialize, Deserialize)]
pub struct Tracked {
    pub url: String,
    pub repo: String,
    pub repo_id: u64,
    pub number: u64,
    pub actor: String,
    pub title: String,
    pub head_sha: String,
    pub mode: dashboard::Mode,
    pub status: String,
    pub reason: Option<String>,
    pub etag: Option<String>,
    pub next_poll: i64,
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub tracked: BTreeMap<String, Tracked>,
    pub ignored: BTreeMap<String, BTreeSet<String>>,
    pub admissions: Vec<i64>,
    pub retry_at: i64,
}
impl Store {
    pub fn load(data: &FsPath) -> Result<Self> {
        dashboard::read_json(data, "external.json")
    }
    fn save(&self, data: &FsPath) -> Result<()> {
        let mut value = serde_json::to_value(self)?;
        crate::redact::json(&mut value);
        crate::fsutil::write_atomic_in(data, "external.json", &serde_json::to_vec_pretty(&value)?)
    }
    fn admit(&mut self, policy: &Policy, now: i64) -> Outcome<()> {
        self.admissions.retain(|at| *at > now - 3600);
        if self.admissions.len() >= policy.reviews_per_hour {
            return Err(Failure {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: "External review hourly limit reached; try later.".into(),
                retry_after: Some((self.admissions[0] + 3600 - now).max(1) as u64),
            });
        }
        self.admissions.push(now);
        Ok(())
    }
}

struct Read {
    value: Option<Value>,
    etag: Option<String>,
}
async fn read(server: &Server, path: &str, etag: Option<&str>) -> Outcome<Read> {
    let now = chrono::Utc::now().timestamp();
    let retry_at = server.external.lock().await.retry_at;
    if retry_at > now {
        return Err(Failure {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("GitHub rate limit; retry in {} seconds.", retry_at - now),
            retry_after: Some((retry_at - now) as u64),
        });
    }
    let response = server
        .api
        .public_get(path, etag)
        .await
        .map_err(Failure::upstream)?;
    let status = response.status();
    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
        let headers = response.headers();
        let seconds = headers
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<u64>().ok())
            .or_else(|| {
                (headers
                    .get("x-ratelimit-remaining")
                    .and_then(|h| h.to_str().ok())
                    == Some("0"))
                .then(|| {
                    headers
                        .get("x-ratelimit-reset")?
                        .to_str()
                        .ok()?
                        .parse::<i64>()
                        .ok()
                })
                .flatten()
                .map(|reset| (reset - now).max(1) as u64)
            })
            .unwrap_or(60)
            .clamp(1, 86400);
        let mut store = server.external.lock().await;
        store.retry_at = now + seconds as i64;
        store
            .save(&server.settings.data_dir)
            .map_err(Failure::upstream)?;
        return Err(Failure {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("GitHub rate limit/access restriction; retry in {seconds} seconds."),
            retry_after: Some(seconds),
        });
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return Err(Failure::access());
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Read { value: None, etag });
    }
    if !status.is_success() {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            format!("GitHub read returned HTTP {}", status.as_u16()),
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Failure::upstream)? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "GitHub metadata exceeds 2 MB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Read {
        value: Some(serde_json::from_slice(&bytes).map_err(Failure::upstream)?),
        etag,
    })
}

pub struct Resolved {
    pub repo: Repo,
    pub pr: Value,
}
fn public_repo(value: &Value, policy: &Policy) -> Outcome<Repo> {
    if value["private"].as_bool() != Some(false) {
        return Err(Failure::access());
    }
    let size = value["size"]
        .as_u64()
        .context("GitHub repository size is missing")?;
    if size > policy.max_repo_size_kb {
        return Err(Failure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Repository exceeds the external review size limit (default 1 GB).",
        ));
    }
    Ok(Repo::parse(value, None)?)
}
fn validate_pr(pr: &Value, target: &PrUrl, repo: &Repo) -> Outcome<()> {
    if pr["number"].as_u64() != Some(target.number)
        || pr["base"]["repo"]["id"].as_u64() != Some(repo.id)
        || pr["base"]["repo"]["private"].as_bool() != Some(false)
    {
        return Err(Failure::access());
    }
    if pr["state"].as_str() != Some("open") || pr["merged"].as_bool() == Some(true) {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            "PR is closed or merged; external reviews require an open PR.",
        ));
    }
    for side in ["base", "head"] {
        if !pr[side]["sha"].as_str().is_some_and(crate::git::is_oid) {
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "PR has invalid commit ids",
            ));
        }
    }
    Ok(())
}
pub async fn resolve(server: &Server, target: &PrUrl) -> Outcome<Resolved> {
    let policy = server.dashboard.lock().await.preferences.external.clone();
    let metadata = read(server, &format!("/repos/{}", target.repo), None)
        .await?
        .value
        .context("missing repository metadata")?;
    let repo = public_repo(&metadata, &policy)?;
    let result = read(server, &target.api_path(), None).await?;
    let mut pr = result.value.context("missing PR metadata")?;
    validate_pr(&pr, target, &repo)?;
    // The API is authority for identity, never URLs supplied by the caller or PR text.
    pr["html_url"] = json!(
        PrUrl {
            repo: repo.full_name.clone(),
            number: target.number
        }
        .url()
    );
    Ok(Resolved { repo, pr })
}

pub async fn submit(
    server: &Server,
    url: &str,
    mode: dashboard::Mode,
    trigger: &str,
) -> Outcome<Value> {
    let target = PrUrl::parse(url)?;
    let resolved = resolve(server, &target).await?;
    enqueue(server, resolved, target.number, mode, trigger, None).await
}
async fn enqueue(
    server: &Server,
    resolved: Resolved,
    number: u64,
    mode: dashboard::Mode,
    trigger: &str,
    admission: Option<&super::queue::ReviewAdmission>,
) -> Outcome<Value> {
    let policy = server.dashboard.lock().await.preferences.external.clone();
    let admitted_at = chrono::Utc::now().timestamp();
    {
        let mut store = server.external.lock().await;
        let mut next = store.clone();
        next.admit(&policy, admitted_at)?;
        next.save(&server.settings.data_dir)?;
        *store = next;
    }
    let mut job = Job::new(
        resolved.repo,
        Kind::Review {
            number,
            pr: resolved.pr,
            force: true,
        },
        trigger,
    );
    job.mode = mode;
    let id = job.id.clone();
    if !server.enqueue_inner(job, admission).await {
        // Admissions are reserved before enqueue to serialize the hourly cap.
        // A rejected/skipped job must not consume that reservation or its head.
        let mut store = server.external.lock().await;
        let mut next = store.clone();
        if let Some(index) = next.admissions.iter().position(|at| *at == admitted_at) {
            next.admissions.remove(index);
        }
        next.save(&server.settings.data_dir)?;
        *store = next;
        return Err(Failure::new(
            StatusCode::CONFLICT,
            "External review was not queued; check server configuration and retry.",
        ));
    }
    Ok(json!({"id": id}))
}

/// Lane C's staleness hook: Some(true/false), or None when metadata cannot be read.
/// Public visibility is rechecked on every resolution, with a short cache per target.
pub async fn stale(server: &Server, summary: &dashboard::JobSummary) -> Option<bool> {
    if summary.source != Source::External {
        return None;
    }
    let head = summary.head_sha.as_deref()?;
    let target = PrUrl::parse(summary.pr_url.as_deref()?).ok()?;
    let key = target.url();
    let now = chrono::Utc::now().timestamp();
    if let Some((at, current)) = server.external_heads.lock().await.get(&key)
        && now - at < 60
    {
        return Some(head != current);
    }
    let resolved = resolve(server, &target).await.ok()?;
    let current = resolved.pr["head"]["sha"].as_str()?.to_owned();
    let mut cache = server.external_heads.lock().await;
    cache.retain(|_, (at, _)| now - *at < 60);
    if cache.len() >= 500 {
        cache.clear();
    }
    cache.insert(key, (now, current.clone()));
    Some(head != current)
}

pub fn router() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/external/reviews", post(create))
        .route("/api/external/reviews/{id}/stale", get(staleness))
        .route("/api/external/discover", get(discover))
        .route("/api/external/tracking", get(list).post(track))
        .route(
            "/api/external/tracking/{repo_id}/{number}",
            axum::routing::delete(untrack),
        )
        .route("/api/external/ignore", post(ignore))
        .route("/api/settings/external", get(policy).put(save_policy))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    url: String,
    #[serde(default)]
    mode: dashboard::Mode,
}
async fn create(
    State(server): State<Arc<Server>>,
    Json(input): Json<Review>,
) -> Outcome<Json<Value>> {
    Ok(Json(
        submit(&server, &input.url, input.mode, "manual").await?,
    ))
}
async fn staleness(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
) -> Outcome<Json<Value>> {
    let summary = server
        .dashboard
        .lock()
        .await
        .jobs
        .get(&id)
        .cloned()
        .context("review not found")?;
    Ok(Json(json!({"stale": stale(&server, &summary).await})))
}
async fn policy(State(server): State<Arc<Server>>) -> Json<Value> {
    Json(json!(server.dashboard.lock().await.preferences.external))
}
async fn save_policy(
    State(server): State<Arc<Server>>,
    Json(input): Json<Policy>,
) -> Outcome<Json<Value>> {
    input.validate()?;
    let mut dashboard = server.dashboard.lock().await;
    let mut preferences = dashboard.preferences.clone();
    preferences.external = input;
    dashboard::write_preferences(&server.settings.data_dir, &preferences)?;
    dashboard.preferences = preferences;
    Ok(Json(json!({"ok": true})))
}
async fn list(State(server): State<Arc<Server>>) -> Json<Value> {
    Json(json!({"tracked": server.external.lock().await.tracked.values().collect::<Vec<_>>()}))
}
async fn discover(
    State(server): State<Arc<Server>>,
    axum::Extension(session): axum::Extension<auth::Session>,
) -> Outcome<Json<Value>> {
    // Login is from the authenticated session; no arbitrary search syntax is accepted.
    let login = session.user.login;
    if login.is_empty()
        || !login
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(Failure::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid GitHub login",
        ));
    }
    let query = format!("is:pr is:open is:public author:{login}");
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", &query)
        .append_pair("per_page", "100")
        .finish();
    let value = read(&server, &format!("/search/issues?{params}"), None)
        .await?
        .value
        .context("missing search result")?;
    let installed: BTreeSet<_> = server
        .dashboard
        .lock()
        .await
        .repos
        .keys()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let ignored = server
        .external
        .lock()
        .await
        .ignored
        .get(&login.to_ascii_lowercase())
        .cloned()
        .unwrap_or_default();
    let prs: Vec<_> = value["items"].as_array().into_iter().flatten().filter_map(|item| {
        let target = PrUrl::parse(item["html_url"].as_str()?).ok()?;
        let url = target.url();
        if installed.contains(&target.repo.to_ascii_lowercase()) || ignored.contains(&url) { return None; }
        Some(json!({"url": url, "repo": target.repo, "number": target.number,
            "title": crate::text::excerpt_bytes(&crate::redact::text(item["title"].as_str().unwrap_or("PR")), 500)}))
    }).collect();
    Ok(Json(
        json!({"prs": prs, "incomplete": value["incomplete_results"].as_bool().unwrap_or(false) || value["total_count"].as_u64().unwrap_or(0) > 100}),
    ))
}
async fn track(
    State(server): State<Arc<Server>>,
    axum::Extension(session): axum::Extension<auth::Session>,
    Json(input): Json<Review>,
) -> Outcome<Json<Value>> {
    let target = PrUrl::parse(&input.url)?;
    let policy = server.dashboard.lock().await.preferences.external.clone();
    if !policy.tracking_enabled {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            "Enable Track my external PRs first.",
        ));
    }
    let resolved = resolve(&server, &target).await?;
    if !resolved.pr["user"]["login"]
        .as_str()
        .is_some_and(|login| login.eq_ignore_ascii_case(&session.user.login))
    {
        return Err(Failure::new(
            StatusCode::FORBIDDEN,
            "Track only public PRs authored by your signed-in GitHub account.",
        ));
    }
    if server
        .dashboard
        .lock()
        .await
        .repos
        .values()
        .any(|info| info.repo.id == resolved.repo.id)
    {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            "Repository is already installed; use its review settings.",
        ));
    }
    let tracked = Tracked {
        url: PrUrl {
            repo: resolved.repo.full_name.clone(),
            number: target.number,
        }
        .url(),
        repo: resolved.repo.full_name.clone(),
        repo_id: resolved.repo.id,
        number: target.number,
        actor: session.user.login,
        title: crate::text::excerpt_bytes(
            &crate::redact::text(resolved.pr["title"].as_str().unwrap_or("PR")),
            500,
        ),
        // Empty until its initial review is admitted, so a cap never silently loses it.
        head_sha: String::new(),
        etag: None,
        mode: input.mode,
        status: "tracking".into(),
        reason: None,
        next_poll: 0,
    };
    let mut store = server.external.lock().await;
    if store
        .tracked
        .get(&tracked.url)
        .is_some_and(|entry| entry.status == "tracking")
    {
        return Ok(Json(json!({"ok": true})));
    }
    if store
        .tracked
        .values()
        .filter(|entry| entry.status == "tracking")
        .count()
        >= policy.max_tracked
    {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            format!(
                "Tracked PR limit reached (maximum {}); untrack a PR first.",
                policy.max_tracked
            ),
        ));
    }
    let mut next = store.clone();
    if next.tracked.len() >= 100 {
        let oldest = next
            .tracked
            .iter()
            .filter(|(_, item)| item.status != "tracking")
            .min_by_key(|(_, item)| item.next_poll)
            .map(|(key, _)| key.clone());
        if let Some(key) = oldest {
            next.tracked.remove(&key);
        }
    }
    next.tracked.insert(tracked.url.clone(), tracked);
    next.save(&server.settings.data_dir)?;
    *store = next;
    Ok(Json(json!({"ok": true})))
}
async fn untrack(
    State(server): State<Arc<Server>>,
    Path((repo_id, number)): Path<(u64, u64)>,
) -> Outcome<Json<Value>> {
    let mut store = server.external.lock().await;
    let mut next = store.clone();
    for entry in next
        .tracked
        .values_mut()
        .filter(|item| item.repo_id == repo_id && item.number == number)
    {
        entry.status = "stopped".into();
        entry.reason = Some("untracked manually".into());
    }
    next.save(&server.settings.data_dir)?;
    *store = next;
    drop(store);
    server.close((repo_id, number, Source::External)).await?;
    Ok(Json(json!({"ok": true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ignore {
    url: String,
}
async fn ignore(
    State(server): State<Arc<Server>>,
    axum::Extension(session): axum::Extension<auth::Session>,
    Json(input): Json<Ignore>,
) -> Outcome<Json<Value>> {
    let url = PrUrl::parse(&input.url)?.url();
    let mut store = server.external.lock().await;
    let mut next = store.clone();
    let ignored = next
        .ignored
        .entry(session.user.login.to_ascii_lowercase())
        .or_default();
    if ignored.len() >= 1000 && !ignored.contains(&url) {
        return Err(Failure::new(
            StatusCode::CONFLICT,
            "Ignore list limit reached",
        ));
    }
    ignored.insert(url);
    next.save(&server.settings.data_dir)?;
    *store = next;
    Ok(Json(json!({"ok": true})))
}

/// Called by the existing reconcile cadence, even when installation reconciliation fails.
pub async fn poll(server: &Server) -> Result<()> {
    let Ok(_poll) = server.external_poll.try_lock() else {
        return Ok(());
    };
    let policy = server.dashboard.lock().await.preferences.external.clone();
    if policy.tracking_enabled {
        let now = chrono::Utc::now().timestamp();
        let due: Vec<_> = server
            .external
            .lock()
            .await
            .tracked
            .values()
            .filter(|item| item.status == "tracking" && item.next_poll <= now)
            .cloned()
            .collect();
        for item in due {
            if let Err(error) = poll_one(server, &policy, &item, now).await {
                let mut store = server.external.lock().await;
                if let Some(current) = store.tracked.get_mut(&item.url)
                    && current.status == "tracking"
                {
                    current.reason = Some(crate::redact::diagnostic(&error.message));
                    current.next_poll =
                        now + error.retry_after.unwrap_or(policy.poll_minutes * 60) as i64;
                }
                store.save(&server.settings.data_dir)?;
                if error.retry_after.is_some() {
                    break;
                }
            }
        }
    }
    prune(server, &policy).await
}
async fn poll_one(server: &Server, policy: &Policy, item: &Tracked, now: i64) -> Outcome<()> {
    let admission =
        server
            .queue
            .lock()
            .await
            .review_admission((item.repo_id, item.number, Source::External));
    let target = PrUrl::parse(&item.url)?;
    // Visibility can change since tracking was selected. Check it before using PR data.
    let metadata = match read(server, &format!("/repos/{}", target.repo), None).await {
        Err(error) if error.status == StatusCode::UNPROCESSABLE_ENTITY => {
            return stop(server, item, "deleted or requires access/installation").await;
        }
        result => result?,
    }
    .value
    .context("missing repository metadata")?;
    if metadata["private"].as_bool() != Some(false) {
        return stop(server, item, "repository is no longer public").await;
    }
    let repo = public_repo(&metadata, policy)?;
    if repo.id != item.repo_id {
        return stop(server, item, "repository identity changed").await;
    }
    let result = match read(server, &target.api_path(), item.etag.as_deref()).await {
        Err(error) if error.status == StatusCode::UNPROCESSABLE_ENTITY => {
            return stop(server, item, "deleted or requires access/installation").await;
        }
        result => result?,
    };
    let mut next_head = item.head_sha.clone();
    if let Some(pr) = result.value {
        if pr["state"].as_str() == Some("closed") || pr["merged"].as_bool() == Some(true) {
            return stop(server, item, "PR closed or merged").await;
        }
        validate_pr(&pr, &target, &repo)?;
        next_head = pr["head"]["sha"].as_str().unwrap().into();
        if next_head != item.head_sha {
            // Serialize selection/untrack against admission, without holding locks over network reads.
            let current = server.external.lock().await.tracked.get(&item.url).cloned();
            if current.as_ref().is_none_or(|current| {
                current.status != "tracking" || current.head_sha != item.head_sha
            }) {
                return Ok(());
            }
            enqueue(
                server,
                Resolved { repo, pr },
                item.number,
                item.mode,
                "tracking",
                Some(&admission),
            )
            .await?;
        }
    }
    let mut store = server.external.lock().await;
    if let Some(current) = store.tracked.get_mut(&item.url)
        && current.status == "tracking"
    {
        current.head_sha = next_head;
        if result.etag.is_some() {
            current.etag = result.etag;
        }
        current.next_poll = now + (policy.poll_minutes * 60) as i64;
        current.reason = None;
    }
    store.save(&server.settings.data_dir)?;
    Ok(())
}
async fn stop(server: &Server, item: &Tracked, reason: &str) -> Outcome<()> {
    let mut store = server.external.lock().await;
    if let Some(current) = store.tracked.get_mut(&item.url) {
        current.status = "stopped".into();
        current.reason = Some(reason.into());
    }
    store.save(&server.settings.data_dir)?;
    drop(store);
    server
        .close((item.repo_id, item.number, Source::External))
        .await?;
    Ok(())
}

async fn prune(server: &Server, policy: &Policy) -> Result<()> {
    let root = crate::fsutil::ensure_dir(&server.settings.data_dir, FsPath::new("external"))?;
    // Queue lock prevents a checkout becoming active between selection and removal.
    let queue = server.queue.lock().await;
    let mut keep: BTreeSet<_> = queue
        .pending
        .values()
        .chain(queue.running.values().map(|r| &r.job))
        .filter(|job| job.repo.source() == Source::External)
        .map(|job| job.repo.id)
        .collect();
    keep.extend(
        server
            .external
            .lock()
            .await
            .tracked
            .values()
            .filter(|item| item.status == "tracking")
            .map(|item| item.repo_id),
    );
    let cutoff = chrono::Utc::now() - chrono::Duration::days(policy.retention_days as i64);
    let dashboard = server.dashboard.lock().await;
    let recent: BTreeMap<_, _> = dashboard
        .jobs
        .values()
        .filter(|job| job.source == Source::External)
        .filter_map(|job| {
            Some((
                job.repo_id?,
                chrono::DateTime::parse_from_rfc3339(&job.queued_at).ok()?,
            ))
        })
        .fold(BTreeMap::new(), |mut acc, (id, at)| {
            acc.entry(id)
                .and_modify(|old| *old = at.max(*old))
                .or_insert(at);
            acc
        });
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u64>().ok())
        {
            dirs.push((id, recent.get(&id).copied()));
        }
    }
    dirs.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (index, (id, at)) in dirs.into_iter().enumerate() {
        if !keep.contains(&id) && (index >= policy.max_checkouts || at.is_none_or(|at| at < cutoff))
        {
            let dir = crate::fsutil::ensure_dir(&root, FsPath::new(&id.to_string()))?;
            std::fs::remove_dir_all(dir)?;
        }
    }
    Ok(())
}
