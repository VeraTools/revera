use super::{
    Server, app, auth,
    dashboard::{self, Dashboard, JobSummary, Mode, Preferences, Trigger},
    events::{Job, Kind, Repo},
    runner,
};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};

pub struct Error(StatusCode, String);
impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(error: E) -> Self {
        let error = error.into();
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            crate::redact::diagnostic(&format!("{error:#}")),
        )
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error": crate::redact::diagnostic(&self.1)})),
        )
            .into_response()
    }
}
type ApiResult = std::result::Result<Json<Value>, Error>;
fn output(mut value: Value) -> Json<Value> {
    crate::redact::json(&mut value);
    Json(value)
}
fn invalid(error: anyhow::Error) -> Error {
    Error(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
}
fn missing() -> Error {
    Error(StatusCode::NOT_FOUND, "not found".into())
}

pub fn router(server: Arc<Server>) -> Router<Arc<Server>> {
    Router::new()
        .route("/api/session", get(session))
        .route("/api/setup/manifest", get(manifest))
        .route("/api/logout", post(auth::logout))
        .route("/api/overview", get(overview))
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}", get(job))
        .route("/api/jobs/{id}/log", get(log))
        .route("/api/jobs/{id}/rerun", post(rerun))
        .route("/api/jobs/{id}/cancel", post(cancel))
        .route("/api/repos", get(repos).patch(bulk_repo_settings))
        // Connected accounts (separate from repository preferences).
        .route("/api/installations", get(installations))
        .route(
            "/api/installations/reconcile",
            post(reconcile_installations),
        )
        .route(
            "/api/owners/{login}",
            put(approve_owner).delete(revoke_owner),
        )
        .route(
            "/api/repos/{owner}/{repo}",
            axum::routing::patch(repo_settings),
        )
        .route("/api/repos/{owner}/{repo}/index", post(index))
        .route("/api/repos/{owner}/{repo}/pulls", get(pulls))
        .route(
            "/api/repos/{owner}/{repo}/pulls/{number}/review",
            post(review),
        )
        .route("/api/settings", get(settings))
        .route("/api/settings/config", put(config))
        .route(
            "/api/settings/server",
            get(get_server_settings).put(server_settings),
        )
        .route("/api/secrets/{name}", put(secret).delete(delete_secret))
        .route("/api/settings/test", post(test))
        .route("/api/settings/models", post(models))
        .route("/api/events", get(events))
        .route("/api", any(|| async { StatusCode::NOT_FOUND }))
        .route("/api/{*path}", any(|| async { StatusCode::NOT_FOUND }))
        .route_layer(middleware::from_fn_with_state(server, auth::guard))
}

async fn stage(server: &Server) -> &'static str {
    if server.setup.lock().await.app.is_none() {
        return "create_app";
    }
    let mut dashboard = server.dashboard.lock().await;
    if dashboard.preferences.admins.is_empty() {
        return "sign_in";
    }
    if dashboard.installations == 0 {
        return "install";
    }
    let Ok(yaml) = std::fs::read_to_string(&server.config) else {
        return "configure";
    };
    if dashboard
        .sync_health(&server.settings.data_dir, Some(&yaml), &server.session_key)
        .is_err()
    {
        return "configure";
    }
    let Ok(config) = dashboard.validate(&yaml) else {
        return "configure";
    };
    let roles = required_roles(&config);
    if config.vera.enabled {
        let embedding = (config.vera.backend == crate::config::VeraBackend::Api)
            .then_some(config.vera.embedding.as_ref())
            .flatten();
        if embedding
            .into_iter()
            .chain(config.vera.reranker.as_ref())
            .any(|endpoint| {
                dashboard
                    .env(&endpoint.api_key_env)
                    .is_none_or(|key| key.trim().is_empty())
            })
        {
            return "configure";
        }
    }
    if roles.iter().any(|role| {
        dashboard
            .health
            .get(*role)
            .is_none_or(|health| health["ok"].as_bool() != Some(true))
    }) {
        return "configure";
    }
    "done"
}

async fn session(State(server): State<Arc<Server>>, headers: HeaderMap) -> Json<Value> {
    let session = auth::session(&server, &headers).await;
    let mut value = json!({"authenticated": session.is_some(), "setup": {
        "stage": stage(&server).await, "public_url": server.settings.public_url,
        "authorized": auth::bootstrap(&server, &headers)
    }});
    if let Some(session) = session {
        value["user"] = json!(session.user);
        value["csrf"] = json!(session.csrf);
    }
    output(value)
}

#[derive(Default, Deserialize)]
struct ManifestQuery {
    #[serde(default)]
    token: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    org: String,
    #[serde(default)]
    public: bool,
    #[serde(default)]
    publish_fixes: bool,
}
async fn manifest(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(query): Query<ManifestQuery>,
) -> ApiResult {
    let setup = server.setup.lock().await;
    if setup.app.is_some() {
        return Err(missing());
    }
    let token = setup.token.as_deref().ok_or_else(missing)?;
    if !app::token_matches(token, &query.token) && !auth::bootstrap(&server, &headers) {
        return Err(Error(
            StatusCode::UNAUTHORIZED,
            "setup token required".into(),
        ));
    }
    let name = if query.name.is_empty() {
        let url = url::Url::parse(&server.settings.public_url).unwrap();
        let label = url
            .host_str()
            .unwrap()
            .split('.')
            .find(|s| !matches!(*s, "revera" | "www"))
            .unwrap_or("app");
        format!("revera-{label}")
            .chars()
            .take(34)
            .collect::<String>()
    } else {
        query.name
    };
    if name.trim().is_empty() || name.chars().count() > 34 {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app name must be 1–34 characters".into(),
        ));
    }
    let mut url = url::Url::parse("https://github.com").unwrap();
    {
        let mut path = url.path_segments_mut().unwrap();
        if !query.org.is_empty() {
            path.push("organizations").push(&query.org);
        }
        path.push("settings").push("apps").push("new");
    }
    url.query_pairs_mut().append_pair("state", token);
    Ok(output(json!({"post_url": url.as_str(),
        "manifest": app::manifest(&server.settings.public_url, &name, query.public, query.publish_fixes)})))
}

// Connected-account endpoints. All routes use the existing admin + CSRF guard.
async fn installations(State(server): State<Arc<Server>>) -> ApiResult {
    let _guard = server.reconcile_lock.lock().await;
    let app = server.setup.lock().await.app.clone().ok_or_else(missing)?;
    Ok(output(json!(
        super::reconcile::discover(&server, &app).await?
    )))
}

async fn reconcile_installations(State(server): State<Arc<Server>>) -> ApiResult {
    super::reconcile::once(&server).await?;
    Ok(output(json!(*server.installations.lock().await)))
}

async fn change_owner(server: &Server, login: String, approve: bool) -> ApiResult {
    if login.is_empty()
        || !login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(invalid(anyhow::anyhow!("invalid GitHub account login")));
    }
    let _guard = server.reconcile_lock.lock().await;
    let app = server.setup.lock().await.app.clone().ok_or_else(missing)?;
    // Approval must refer to a real installation, not caller-supplied identity.
    // Revocation remains available during a GitHub outage using pinned identity.
    let discovered = super::reconcile::discover(server, &app).await;
    let accounts = match discovered {
        Ok(accounts) => accounts,
        Err(error) if approve => return Err(error.into()),
        Err(_) => server.installations.lock().await.clone(),
    };
    let account = accounts
        .installations
        .iter()
        .find(|i| i.account.login.eq_ignore_ascii_case(&login));
    if approve && account.is_none_or(|i| i.id.is_none()) {
        return Err(missing());
    }
    let id = account.and_then(|i| i.account.id);
    {
        let mut dashboard = server.dashboard.lock().await;
        let mut preferences = dashboard.preferences.clone();
        let id = id.or_else(|| {
            preferences
                .owner_accounts
                .get(&login.to_ascii_lowercase())
                .copied()
        });
        if approve {
            preferences.approve_owner(&login, id);
        } else {
            preferences.revoke_owner(&login, id);
        }
        dashboard::write_preferences(&server.settings.data_dir, &preferences)?;
        dashboard.preferences = preferences;
    }
    if !approve {
        super::reconcile::evict_unapproved(server, &app).await?;
    }
    // Policy is already durable and revocation already applied even if GitHub
    // fails. Report that separately so a retry never silently re-authorizes it.
    let warning = super::reconcile::once_locked(server)
        .await
        .err()
        .map(|error| {
            crate::redact::diagnostic(&format!("Owner policy saved; refresh failed: {error:#}"))
        });
    Ok(output(json!({"ok": true, "warning": warning})))
}

async fn approve_owner(State(server): State<Arc<Server>>, Path(login): Path<String>) -> ApiResult {
    change_owner(&server, login, true).await
}
async fn revoke_owner(State(server): State<Arc<Server>>, Path(login): Path<String>) -> ApiResult {
    change_owner(&server, login, false).await
}

fn ordered(dashboard: &Dashboard) -> Vec<JobSummary> {
    let mut jobs: Vec<_> = dashboard.jobs.values().cloned().collect();
    jobs.sort_by(|a, b| b.queued_at.cmp(&a.queued_at).then(b.id.cmp(&a.id)));
    jobs
}

fn history_window(jobs: &[JobSummary], cutoff: chrono::DateTime<chrono::Utc>) -> Value {
    let window: Vec<_> = jobs
        .iter()
        .filter(|job| {
            chrono::DateTime::parse_from_rfc3339(&job.queued_at)
                .is_ok_and(|queued| queued >= cutoff)
        })
        .collect();
    let reviews: Vec<_> = window.iter().filter(|job| job.kind == "review").collect();
    let mut durations: Vec<_> = reviews
        .iter()
        .filter(|job| matches!(job.status.as_str(), "complete" | "partial" | "failed"))
        .filter_map(|job| job.duration_ms)
        .collect();
    durations.sort_unstable();
    let median = if durations.is_empty() {
        None
    } else {
        let middle = durations.len() / 2;
        Some(if durations.len() % 2 == 0 {
            (durations[middle - 1] as f64 + durations[middle] as f64) / 2.0
        } else {
            durations[middle] as f64
        })
    };
    let tokens = |key: &str| {
        window
            .iter()
            .filter_map(|job| job.tokens.as_ref())
            .map(|tokens| tokens[key].as_u64().unwrap_or(0))
            .sum::<u64>()
    };
    let prompt = tokens("prompt");
    json!({
        "reviews": reviews.len(),
        "failed": reviews.iter().filter(|job| job.status == "failed").count(),
        "findings": reviews.iter().map(|job| job.findings.total).sum::<u64>(),
        "severity": {
            "high": reviews.iter().map(|job| job.findings.high).sum::<u64>(),
            "medium": reviews.iter().map(|job| job.findings.medium).sum::<u64>(),
            "low": reviews.iter().map(|job| job.findings.low).sum::<u64>()
        },
        "tokens": prompt + tokens("completion"),
        "prompt_tokens": prompt,
        "cached_tokens": tokens("cached"),
        "median_duration_ms": median
    })
}

async fn overview(State(server): State<Arc<Server>>) -> ApiResult {
    let app = server.setup.lock().await.app.clone().ok_or_else(missing)?;
    let registration = server
        .api
        .request(reqwest::Method::GET, "/app", &app.jwt()?, None)
        .await
        .ok();
    let issue_comment = registration
        .as_ref()
        .and_then(|value| value["events"].as_array())
        .map(|events| events.iter().any(|event| event == "issue_comment"));
    let settings_url = if registration
        .as_ref()
        .is_some_and(|v| v["owner"]["type"] == "Organization")
    {
        format!(
            "https://github.com/organizations/{}/settings/apps/{}/permissions",
            app.owner, app.slug
        )
    } else {
        format!("https://github.com/settings/apps/{}/permissions", app.slug)
    };
    let mut dashboard = server.dashboard.lock().await;
    let jobs = ordered(&dashboard);
    let yaml = server.config_yaml()?;
    dashboard.sync_health(
        &server.settings.data_dir,
        yaml.as_deref(),
        &server.session_key,
    )?;
    let config = yaml.and_then(|yaml| dashboard.parse(&yaml).ok());
    let models: Vec<_> = config
        .as_ref()
        .into_iter()
        .flat_map(|cfg| {
            [
                ("investigator", &cfg.models.investigator),
                ("validator", cfg.effective_validator()),
            ]
        })
        .map(|(role, route)| {
            dashboard
                .health
                .get(role)
                .cloned()
                .unwrap_or_else(|| json!({"role": role, "model": route.model}))
        })
        .collect();
    let github = dashboard.github.clone();
    let now = chrono::Utc::now();
    let recent: Vec<_> = jobs.iter().take(10).cloned().collect();
    let last_24h = history_window(&jobs, now - chrono::Duration::hours(24));
    let last_7d = history_window(&jobs, now - chrono::Duration::days(7));
    let history_since = jobs.last().map(|job| &job.queued_at);
    let running: Vec<_> = jobs
        .iter()
        .filter(|j| j.status == "running")
        .cloned()
        .collect();
    let queued: Vec<_> = jobs
        .iter()
        .filter(|j| j.status == "queued")
        .cloned()
        .collect();
    drop(dashboard);
    let mut version = None;
    let enabled = config.as_ref().is_some_and(|c| c.vera.enabled);
    if let Some(cfg) = &config
        && enabled
    {
        let mut vera = crate::vera::VeraClient::disabled(&server.settings.data_dir);
        vera.exe = cfg.vera.executable.clone().into();
        version = tokio::time::timeout(Duration::from_secs(5), vera.version())
            .await
            .ok()
            .and_then(Result::ok);
    }
    Ok(output(json!({
        "app": {"name": if app.name.is_empty() { &app.slug } else { &app.name },
            "slug": app.slug, "html_url": app.html_url, "owner": app.owner,
            "install_url": format!("https://github.com/apps/{}/installations/new", app.slug),
            "issue_comment_subscribed": issue_comment, "settings_url": settings_url},
        "health": {"github": github, "models": models,
            "vera": {"enabled": enabled, "version": version}},
        "queue": {"running": running, "queued": queued}, "last_24h": last_24h,
        "last_7d": last_7d, "history_since": history_since, "recent": recent
    })))
}

#[derive(Default, Deserialize)]
struct JobsQuery {
    repo: Option<String>,
    status: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    before: Option<String>,
    q: Option<String>,
}
async fn jobs(State(server): State<Arc<Server>>, Query(query): Query<JobsQuery>) -> Json<Value> {
    let dashboard = server.dashboard.lock().await;
    let all = ordered(&dashboard);
    let search = query.q.as_deref().unwrap_or("").to_lowercase();
    let number = search.strip_prefix('#').unwrap_or(&search);
    let pr = (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| number.parse::<u64>().ok())
        .flatten();
    let offset = query
        .before
        .as_ref()
        .and_then(|id| all.iter().position(|j| &j.id == id))
        .map_or(0, |n| n + 1);
    // Browsers send unset filters as empty values; they mean "any".
    let exact = |filter: &Option<String>, value: &str| {
        filter.as_deref().is_none_or(|v| v.is_empty() || v == value)
    };
    let mut filtered = all.into_iter().skip(offset).filter(|job| {
        exact(&query.repo, &job.repo)
            && exact(&query.status, &job.status)
            && exact(&query.kind, &job.kind)
            && (search.is_empty()
                || job.repo.to_lowercase().contains(&search)
                || job
                    .title
                    .as_ref()
                    .is_some_and(|v| v.to_lowercase().contains(&search))
                || job
                    .author
                    .as_ref()
                    .is_some_and(|v| v.to_lowercase().contains(&search))
                || pr.is_some_and(|number| job.pr == Some(number)))
    });
    let result: Vec<_> = filtered
        .by_ref()
        .take(query.limit.unwrap_or(50).clamp(1, 500))
        .collect();
    let next = filtered
        .next()
        .and_then(|_| result.last().map(|j| j.id.clone()));
    let mut value = json!({"jobs": result});
    if let Some(next) = next {
        value["next"] = json!(next);
    }
    output(value)
}

async fn summary(server: &Server, id: &str) -> std::result::Result<JobSummary, Error> {
    server
        .dashboard
        .lock()
        .await
        .jobs
        .get(id)
        .cloned()
        .ok_or_else(missing)
}
async fn job(State(server): State<Arc<Server>>, Path(id): Path<String>) -> ApiResult {
    let summary = summary(&server, &id).await?;
    let dir = dashboard::job_dir(&server.settings.data_dir, &id)?;
    let path = dir.join("report.json");
    crate::fsutil::check_regular_target(&path)?;
    let report: Option<Value> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let mut value = json!({"job": summary, "findings": []});
    if let Some(report) = report {
        if let Some(markdown) = report["plan"]["summary_markdown"].as_str() {
            value["summary_markdown"] = json!(runner::strip_state(markdown));
        }
        value["findings"] = json!(
            report["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| {
                    let mut finding = json!({"severity": f["severity"], "file": f["file"],
                "title": f["title"], "status": f["validation_status"]});
                    if !f["start_line"].is_null() {
                        finding["line"] = f["start_line"].clone();
                    }
                    if !f["claim"].is_null() {
                        finding["body"] = f["claim"].clone();
                    }
                    finding
                })
                .collect::<Vec<_>>()
        );
        for field in ["timing", "ledger", "stats", "strategy"] {
            if !report[field].is_null() {
                value[field] = report[field].clone();
            }
        }
    }
    Ok(output(value))
}
async fn log(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
) -> std::result::Result<Response, Error> {
    summary(&server, &id).await?;
    let path = dashboard::job_dir(&server.settings.data_dir, &id)?.join("log.txt");
    crate::fsutil::check_regular_target(&path)?;
    let text = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    Ok(crate::redact::text(&text).into_owned().into_response())
}

async fn repo(server: &Server, name: &str) -> std::result::Result<Repo, Error> {
    server
        .dashboard
        .lock()
        .await
        .repos
        .get(name)
        .map(|r| r.repo.clone())
        .ok_or_else(missing)
}
async fn fetch_pr(server: &Server, repo: &Repo, number: u64) -> Result<Value> {
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
            repo.installation,
            Some(repo.id),
            app::Permissions::ReadPulls,
        )
        .await?;
    server
        .api
        .request(
            reqwest::Method::GET,
            &format!("/repos/{}/pulls/{number}", repo.full_name),
            &token,
            None,
        )
        .await
}
async fn active(server: &Server, repo: &Repo) -> Result<(), Error> {
    if server
        .dashboard
        .lock()
        .await
        .preferences
        .trigger(&repo.full_name)
        == Trigger::Off
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "repository trigger is off".into(),
        ));
    }
    Ok(())
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRequest {
    mode: Option<Mode>,
}
async fn submit_review(
    server: &Server,
    repo: Repo,
    number: u64,
    trigger: &str,
    mode: Option<Mode>,
) -> ApiResult {
    active(server, &repo).await?;
    let mode = mode.unwrap_or(
        server
            .dashboard
            .lock()
            .await
            .preferences
            .mode(&repo.full_name),
    );
    let pr = fetch_pr(server, &repo, number).await?;
    if pr["state"].as_str() == Some("closed") {
        return Err(Error(StatusCode::CONFLICT, "PR is closed".into()));
    }
    let mut job = Job::new(
        repo,
        Kind::Review {
            number,
            pr,
            force: true,
        },
        trigger,
    );
    job.mode = mode;
    let id = job.id.clone();
    server.enqueue(job).await;
    Ok(output(json!({"id": id})))
}
async fn rerun(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    input: Option<Json<ReviewRequest>>,
) -> ApiResult {
    let job = summary(&server, &id).await?;
    let number = job
        .pr
        .ok_or_else(|| Error(StatusCode::CONFLICT, "not a review job".into()))?;
    let repo = repo(&server, &job.repo).await?;
    submit_review(&server, repo, number, "rerun", input.and_then(|v| v.0.mode)).await
}
async fn cancel(State(server): State<Arc<Server>>, Path(id): Path<String>) -> ApiResult {
    let job = summary(&server, &id).await?;
    if job.terminal() {
        return Err(Error(StatusCode::CONFLICT, "job already finished".into()));
    }
    let key = {
        let queue = server.queue.lock().await;
        queue
            .pending
            .iter()
            .find(|(_, j)| j.id == id)
            .map(|(k, _)| *k)
            .or_else(|| {
                queue
                    .running
                    .iter()
                    .find(|(_, r)| r.job.id == id)
                    .map(|(k, _)| *k)
            })
    };
    if let Some(key) = key {
        server.close(key).await?;
    }
    Ok(output(json!({"ok": true})))
}

async fn repos(State(server): State<Arc<Server>>) -> Json<Value> {
    let dashboard = server.dashboard.lock().await;
    let jobs = ordered(&dashboard);
    output(json!(
        dashboard
            .repos
            .values()
            .map(|info| {
                let running = jobs.iter().any(|j| {
                    j.repo == info.repo.full_name && j.kind == "index" && j.status == "running"
                });
                let ready = server
                    .settings
                    .data_dir
                    .join("repos")
                    .join(&info.repo.full_name)
                    .join(".vera/vectors.manifest")
                    .exists();
                let last = jobs.iter().find(|j| j.repo == info.repo.full_name);
                let reviews: Vec<_> = jobs
                    .iter()
                    .filter(|job| job.repo == info.repo.full_name && job.kind == "review")
                    .collect();
                let updated = jobs
                    .iter()
                    .find(|j| {
                        j.repo == info.repo.full_name && j.kind == "index" && j.status == "complete"
                    })
                    .and_then(|j| j.finished_at.as_ref());
                let state = if running {
                    "building"
                } else if ready {
                    "ready"
                } else {
                    "none"
                };
                let mut index = json!({"state": state});
                if let Some(updated) = updated {
                    index["updated_at"] = json!(updated);
                }
                if let Some(retry_at) = dashboard
                    .index_retry_at(&info.repo.full_name)
                    .filter(|at| *at > chrono::Utc::now())
                {
                    index["retry_at"] = json!(retry_at.to_rfc3339());
                }
                let mut value = json!({"full_name": info.repo.full_name, "private": info.private,
            "default_branch": info.repo.default_branch,
            "trigger": dashboard.preferences.trigger(&info.repo.full_name),
            "mode": dashboard.preferences.mode(&info.repo.full_name),
            "trigger_override": dashboard.preferences.repos.get(&info.repo.full_name)
                .and_then(|r| r.trigger),
            "mode_override": dashboard.preferences.repos.get(&info.repo.full_name)
                .and_then(|r| r.mode),
            "has_override": info.has_override, "index": index, "html_url": info.html_url,
            "review_count": reviews.len(),
            "finding_count": reviews.iter().map(|job| job.findings.total).sum::<u64>()});
                if let Some(pushed_at) = info
                    .repo
                    .pushed_at
                    .parse::<i64>()
                    .ok()
                    .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                {
                    value["pushed_at"] =
                        json!(pushed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                }
                if let Some(open_prs) = info.open_prs {
                    value["open_prs"] = json!(open_prs);
                }
                if let Some(last) = last {
                    value["last_job"] = json!(last);
                }
                if let Some(review) = reviews.first() {
                    value["last_review"] = json!(review);
                }
                value
            })
            .collect::<Vec<_>>()
    ))
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RepoSettings {
    #[serde(deserialize_with = "nullable")]
    trigger: Option<Option<Trigger>>,
    #[serde(deserialize_with = "nullable")]
    mode: Option<Option<Mode>>,
    enabled: Option<bool>,
}
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BulkRepoSettings {
    repos: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    trigger: Option<Option<Trigger>>,
    #[serde(deserialize_with = "nullable")]
    mode: Option<Option<Mode>>,
}

async fn bulk_repo_settings(
    State(server): State<Arc<Server>>,
    Json(input): Json<BulkRepoSettings>,
) -> ApiResult {
    if input.repos.is_empty() || input.repos.len() > 1000 {
        return Err(invalid(anyhow::anyhow!("repos must contain 1–1000 names")));
    }
    if input.trigger.is_none() && input.mode.is_none() {
        return Err(invalid(anyhow::anyhow!("trigger or mode is required")));
    }
    let mut dashboard = server.dashboard.lock().await;
    if input
        .repos
        .iter()
        .any(|name| !dashboard.repos.contains_key(name))
    {
        return Err(missing());
    }
    let names: std::collections::BTreeSet<_> = input.repos.into_iter().collect();
    let mut preferences = dashboard.preferences.clone();
    for name in &names {
        let settings = preferences.repos.entry(name.clone()).or_default();
        if let Some(trigger) = input.trigger {
            settings.trigger = trigger;
        }
        if let Some(mode) = input.mode {
            settings.mode = mode;
        }
    }
    dashboard::write_preferences(&server.settings.data_dir, &preferences)?;
    dashboard.preferences = preferences;
    let _ = server.events.send(("repos", json!({})));
    Ok(output(json!({"ok": true, "updated": names.len()})))
}

async fn repo_settings(
    State(server): State<Arc<Server>>,
    Path((owner, name)): Path<(String, String)>,
    Json(input): Json<RepoSettings>,
) -> ApiResult {
    let full_name = format!("{owner}/{name}");
    repo(&server, &full_name).await?;
    let mut dashboard = server.dashboard.lock().await;
    let mut preferences = dashboard.preferences.clone();
    let settings = preferences.repos.entry(full_name).or_default();
    if let Some(trigger) = input.trigger {
        settings.trigger = trigger;
    } else if let Some(enabled) = input.enabled {
        settings.trigger = (!enabled).then_some(Trigger::Off);
    }
    if let Some(mode) = input.mode {
        settings.mode = mode;
    }
    dashboard::write_preferences(&server.settings.data_dir, &preferences)?;
    dashboard.preferences = preferences;
    let _ = server.events.send(("repos", json!({})));
    Ok(output(json!({"ok": true})))
}
async fn index(
    State(server): State<Arc<Server>>,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult {
    let repo = repo(&server, &format!("{owner}/{name}")).await?;
    active(&server, &repo).await?;
    let job = Job::new(repo, Kind::Index, "manual");
    let id = job.id.clone();
    server.enqueue(job).await;
    Ok(output(json!({"id": id})))
}
async fn pulls(
    State(server): State<Arc<Server>>,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult {
    let repo = repo(&server, &format!("{owner}/{name}")).await?;
    let app = server.setup.lock().await.app.clone().ok_or_else(missing)?;
    let token = server
        .api
        .installation_token(
            &app,
            repo.installation,
            Some(repo.id),
            app::Permissions::ReadPulls,
        )
        .await?;
    let prs = server
        .api
        .list(
            &format!("/repos/{}/pulls?state=open", repo.full_name),
            &token,
            None,
        )
        .await?;
    let dashboard = server.dashboard.lock().await;
    let jobs = ordered(&dashboard);
    Ok(output(json!(
        prs.iter()
            .map(|pr| {
                let mut value = json!({"number": pr["number"], "title": pr["title"],
            "author": pr["user"]["login"], "draft": pr["draft"],
            "head_sha": pr["head"]["sha"], "updated_at": pr["updated_at"]});
                if let Some(last) = jobs
                    .iter()
                    .find(|j| j.repo == repo.full_name && j.pr == pr["number"].as_u64())
                {
                    value["last_job"] = json!(last);
                }
                value
            })
            .collect::<Vec<_>>()
    )))
}
async fn review(
    State(server): State<Arc<Server>>,
    Path((owner, name, number)): Path<(String, String, u64)>,
    input: Option<Json<ReviewRequest>>,
) -> ApiResult {
    let repo = repo(&server, &format!("{owner}/{name}")).await?;
    submit_review(
        &server,
        repo,
        number,
        "manual",
        input.and_then(|v| v.0.mode),
    )
    .await
}

async fn settings(State(server): State<Arc<Server>>) -> ApiResult {
    let yaml = server.config_yaml()?.unwrap_or_default();
    let dashboard = server.dashboard.lock().await;
    let mut preferences = dashboard.preferences.clone();
    for owner in &server.allowed {
        if !preferences
            .allowed_owners
            .iter()
            .any(|v| v.eq_ignore_ascii_case(owner))
        {
            preferences.allowed_owners.push(owner.clone());
        }
    }
    Ok(output(json!({"yaml": yaml, "server": preferences,
        "secrets": dashboard.secret_status(&yaml),
        "referenced_env": Dashboard::referenced(&yaml)})))
}
#[derive(Deserialize)]
struct Yaml {
    yaml: String,
}
async fn config(State(server): State<Arc<Server>>, Json(input): Json<Yaml>) -> ApiResult {
    let mut dashboard = server.dashboard.lock().await;
    dashboard.validate(&input.yaml).map_err(invalid)?;
    let first = server.config_yaml()?.is_none();
    crate::fsutil::write_output(
        &server.settings.data_dir,
        &server.config,
        input.yaml.as_bytes(),
    )?;
    dashboard.sync_health(
        &server.settings.data_dir,
        Some(&input.yaml),
        &server.session_key,
    )?;
    drop(dashboard);
    if first {
        tokio::spawn(async move {
            if let Err(error) = super::reconcile::once(&server).await {
                tracing::warn!(
                    "reconcile after configuration: {}",
                    crate::redact::diagnostic(&format!("{error:#}"))
                );
            }
        });
    }
    Ok(output(json!({"ok": true})))
}
async fn server_settings(
    State(server): State<Arc<Server>>,
    Json(mut input): Json<Preferences>,
) -> ApiResult {
    if input.admins.iter().any(|name| name.trim().is_empty()) {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "administrator logins must not be blank".into(),
        ));
    }
    for name in input.repos.keys() {
        let value = json!({"id": 1, "full_name": name});
        Repo::parse(&value, 1).map_err(invalid)?;
    }
    let _guard = server.reconcile_lock.lock().await;
    let mut dashboard = server.dashboard.lock().await;
    // Older dashboard clients do not know identity/revocation fields. A normal
    // settings save must not erase them and implicitly re-authorize an owner.
    let previous = dashboard.preferences.clone();
    input.owner_accounts = previous.owner_accounts.clone();
    input.revoked_owners = previous.revoked_owners.clone();
    input.revoked_owner_ids = previous.revoked_owner_ids.clone();
    let removed: Vec<_> = previous
        .allowed_owners
        .iter()
        .filter(|owner| {
            !input
                .allowed_owners
                .iter()
                .any(|v| v.eq_ignore_ascii_case(owner))
        })
        .cloned()
        .collect();
    let added: Vec<_> = input
        .allowed_owners
        .iter()
        .filter(|owner| {
            !previous
                .allowed_owners
                .iter()
                .any(|v| v.eq_ignore_ascii_case(owner))
        })
        .cloned()
        .collect();
    for owner in &removed {
        input.revoke_owner(
            owner,
            previous
                .owner_accounts
                .get(&owner.to_ascii_lowercase())
                .copied(),
        );
    }
    for owner in &added {
        input.approve_owner(
            owner,
            previous
                .owner_accounts
                .get(&owner.to_ascii_lowercase())
                .copied(),
        );
    }
    dashboard::write_preferences(&server.settings.data_dir, &input)?;
    dashboard.preferences = input;
    drop(dashboard);
    if !removed.is_empty() || !added.is_empty() {
        let app = server.setup.lock().await.app.clone();
        if let Some(app) = app {
            super::reconcile::evict_unapproved(&server, &app).await?;
        }
        server.request_reconcile();
    }
    let _ = server.events.send(("repos", json!({})));
    Ok(output(json!({"ok": true})))
}
async fn get_server_settings(State(server): State<Arc<Server>>) -> Json<Value> {
    output(json!(server.dashboard.lock().await.preferences))
}
#[derive(Deserialize)]
struct Secret {
    value: String,
}
async fn change_secret(server: &Server, name: String, value: Option<String>) -> ApiResult {
    let mut dashboard = server.dashboard.lock().await;
    let yaml = server.config_yaml()?;
    let text = yaml.as_deref().unwrap_or_default();
    let token_env = dashboard
        .parse(text)
        .or_else(|_| {
            crate::config::Config::parse_with_env(text, "server config", &|name| {
                dashboard.env(name).or_else(|| Some("UNSET".into()))
            })
        })
        .map(|c| c.github.token_env)
        .unwrap_or_else(|_| "GITHUB_TOKEN".into());
    dashboard::secret_name(&name, &token_env).map_err(invalid)?;
    let mut secrets = dashboard.secrets.clone();
    if let Some(value) = value {
        if value.trim().is_empty() {
            return Err(Error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "secret value must not be empty; use DELETE to remove it".into(),
            ));
        }
        crate::redact::register(&value);
        secrets.insert(name.clone(), value);
    } else {
        secrets.remove(&name);
    }
    crate::fsutil::write_secret_atomic(
        &server.settings.data_dir,
        "secrets.json",
        &serde_json::to_vec_pretty(&secrets)?,
    )?;
    dashboard.secrets = secrets;
    dashboard.sync_health(
        &server.settings.data_dir,
        yaml.as_deref(),
        &server.session_key,
    )?;
    let environment = crate::config::env_is_set(&name);
    let source = if dashboard.secrets.contains_key(&name) {
        "dashboard"
    } else if environment {
        "environment"
    } else {
        "missing"
    };
    Ok(output(
        json!({"name": name, "source": source, "environment": environment}),
    ))
}
async fn secret(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    Json(input): Json<Secret>,
) -> ApiResult {
    change_secret(&server, name, Some(input.value)).await
}
async fn delete_secret(State(server): State<Arc<Server>>, Path(name): Path<String>) -> ApiResult {
    change_secret(&server, name, None).await
}

#[derive(Deserialize)]
struct Role {
    role: String,
    fallback: Option<usize>,
}
enum Probe {
    Model(crate::config::ModelRoute, String),
    Embedding(crate::config::VeraEndpoint, String),
}

fn probe(config: &crate::config::Config, dashboard: &Dashboard, role: &str) -> Result<Probe> {
    config.check_trust(true)?;
    if role == "embedding" {
        let endpoint = config
            .vera
            .embedding
            .clone()
            .context("embedding endpoint is not configured")?;
        let key = dashboard
            .env(&endpoint.api_key_env)
            .filter(|key| !key.trim().is_empty())
            .context("embedding key is missing")?;
        return Ok(Probe::Embedding(endpoint, key));
    }
    let route = match role {
        "investigator" => config.models.investigator.clone(),
        "validator" => config.effective_validator().clone(),
        _ => bail!("role must be investigator, validator or embedding"),
    };
    let key = if route.protocol == crate::config::Protocol::Scripted {
        String::new()
    } else {
        route
            .api_key_env
            .as_deref()
            .and_then(|name| dashboard.env(name))
            .filter(|key| !key.trim().is_empty())
            .context("model API key is missing")?
    };
    Ok(Probe::Model(route, key))
}

impl Probe {
    async fn test(self) -> Result<()> {
        match self {
            Self::Model(route, key) => crate::provider::probe::model(route, key).await?,
            Self::Embedding(endpoint, key) => {
                crate::provider::probe::embedding(endpoint, key).await?
            }
        }
        Ok(())
    }
    async fn models(self) -> Result<Vec<String>> {
        let (route, key) = match self {
            Self::Model(route, key) => (route, key),
            Self::Embedding(endpoint, key) => {
                let route = serde_json::from_value(json!({
                    "protocol": "openai-chat", "base_url": endpoint.base_url,
                    "model": endpoint.model, "api_key_env": endpoint.api_key_env
                }))?;
                (route, key)
            }
        };
        Ok(crate::provider::probe::models(&route, &key).await?)
    }
}
async fn test(State(server): State<Arc<Server>>, Json(input): Json<Role>) -> ApiResult {
    Ok(output(run_test(&server, input).await?))
}

/// Tests each role the saved config needs that has no current result, so a
/// configured server reports its health without anyone opening Settings.
pub(super) async fn test_untested(server: &Server) {
    let roles = {
        let mut dashboard = server.dashboard.lock().await;
        let Ok(Some(yaml)) = server.config_yaml() else {
            return;
        };
        if dashboard
            .sync_health(&server.settings.data_dir, Some(&yaml), &server.session_key)
            .is_err()
        {
            return;
        }
        let Ok(config) = dashboard.validate(&yaml) else {
            return;
        };
        required_roles(&config)
            .into_iter()
            .filter(|role| !dashboard.health.contains_key(*role))
            .collect::<Vec<_>>()
    };
    for role in roles {
        // An investigator test also covers a validator that inherits its route.
        if server.dashboard.lock().await.health.contains_key(role) {
            continue;
        }
        let role = Role {
            role: role.into(),
            fallback: None,
        };
        if let Err(Error(_, error)) = run_test(server, role).await {
            tracing::warn!("automatic model test: {error}");
        }
    }
}

fn required_roles(config: &crate::config::Config) -> Vec<&'static str> {
    let mut roles = vec!["investigator"];
    if config.review.validate {
        roles.push("validator");
    }
    if config.vera.enabled && config.vera.backend == crate::config::VeraBackend::Api {
        roles.push("embedding");
    }
    roles
}

async fn run_test(server: &Server, input: Role) -> Result<Value, Error> {
    if !matches!(
        input.role.as_str(),
        "investigator" | "validator" | "embedding"
    ) {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid role".into(),
        ));
    }
    if input.fallback.is_some() && input.role == "embedding" {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "embedding does not support fallback model tests".into(),
        ));
    }
    let start = Instant::now();
    let (generation, model, started, prepared) = {
        let mut dashboard = server.dashboard.lock().await;
        let yaml = server.config_yaml()?;
        dashboard.sync_health(
            &server.settings.data_dir,
            yaml.as_deref(),
            &server.session_key,
        )?;
        let config = yaml
            .context("models are not configured yet")
            .and_then(|yaml| dashboard.parse(&yaml));
        let config = if let Some(index) = input.fallback {
            let mut config = config.map_err(invalid)?;
            config.check_trust(true).map_err(invalid)?;
            let route = if input.role == "validator" {
                config.effective_validator()
            } else {
                &config.models.investigator
            };
            config.models.investigator = route.fallback.get(index).cloned().ok_or_else(|| {
                Error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "fallback model index is out of range".into(),
                )
            })?;
            Ok(config)
        } else {
            config
        };
        let model = config
            .as_ref()
            .ok()
            .and_then(|config| match input.role.as_str() {
                "investigator" => Some(config.models.investigator.model.clone()),
                "validator" => Some(config.effective_validator().model.clone()),
                _ => config
                    .vera
                    .embedding
                    .as_ref()
                    .map(|endpoint| endpoint.model.clone()),
            });
        let inherited = config
            .as_ref()
            .is_ok_and(|config| config.validator_inherited());
        let probe_role = if input.fallback.is_some() {
            "investigator"
        } else {
            &input.role
        };
        let prepared = config.and_then(|config| probe(&config, &dashboard, probe_role));
        let mut roles = Vec::new();
        if input.fallback.is_none() {
            roles.push(input.role.clone());
            if input.role == "investigator" && inherited {
                roles.push("validator".into());
            }
        }
        let started: Vec<(String, u64)> = roles
            .into_iter()
            .map(|role| {
                let seq = dashboard.probe_seq.entry(role.clone()).or_default();
                *seq += 1;
                (role, *seq)
            })
            .collect();
        (dashboard.health_generation, model, started, prepared)
    };
    let result = match prepared {
        Ok(probe) => tokio::time::timeout(Duration::from_secs(60), probe.test()).await,
        Err(error) => Ok(Err(error)),
    };
    let latency = start.elapsed().as_millis() as u64;
    let mut value = json!({"ok": false, "latency_ms": latency});
    match result {
        Ok(Ok(())) => {
            value["ok"] = json!(true);
        }
        failure => {
            let error = match failure {
                Ok(Err(e)) => format!("{e:#}"),
                Err(_) => "test timed out".into(),
                _ => unreachable!(),
            };
            value["error"] = json!(crate::redact::diagnostic(&error));
        }
    };
    // Optional fallback probes never replace primary health or complete setup.
    if input.fallback.is_some() {
        return Ok(value);
    }
    let mut health = value.clone();
    health["role"] = json!(input.role);
    health["model"] = json!(model.unwrap_or_default());
    health["checked_at"] = json!(chrono::Utc::now().to_rfc3339());
    let mut dashboard = server.dashboard.lock().await;
    let yaml = server.config_yaml()?;
    dashboard.sync_health(
        &server.settings.data_dir,
        yaml.as_deref(),
        &server.session_key,
    )?;
    if generation == dashboard.health_generation {
        let mut changed = false;
        for (role, seq) in started {
            if dashboard.probe_seq.get(&role) != Some(&seq) {
                continue;
            }
            let mut result = health.clone();
            result["role"] = json!(role);
            dashboard.health.insert(role, result);
            changed = true;
        }
        if changed {
            dashboard.persist_health(&server.settings.data_dir)?;
        }
    }
    Ok(value)
}
async fn models(State(server): State<Arc<Server>>, Json(input): Json<Role>) -> ApiResult {
    let prepared = {
        let dashboard = server.dashboard.lock().await;
        let config = dashboard.parse(&std::fs::read_to_string(&server.config)?)?;
        probe(&config, &dashboard, &input.role).map_err(invalid)?
    };
    let models = prepared.models().await.map_err(invalid)?;
    Ok(output(json!({"models": models})))
}

async fn events(
    State(server): State<Arc<Server>>,
    axum::Extension(session): axum::Extension<auth::Session>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let state = (
        server.events.subscribe(),
        server.stopping.subscribe(),
        server,
        session,
    );
    let stream = futures::stream::unfold(
        state,
        |(mut rx, mut stopping, server, session)| async move {
            loop {
                if *stopping.borrow() {
                    return None;
                }
                let received = tokio::select! {
                    biased;
                    _ = stopping.changed() => return None,
                    received = rx.recv() => received,
                };
                match received {
                    Ok((name, mut value)) => {
                        let dashboard = server.dashboard.lock().await;
                        if session.exp <= chrono::Utc::now().timestamp()
                            || !dashboard.preferences.admin(&session.user.login)
                        {
                            return None;
                        }
                        drop(dashboard);
                        crate::redact::json(&mut value);
                        let event = Event::default().event(name).json_data(value).unwrap();
                        return Some((Ok(event), (rx, stopping, server, session)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return None,
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}
