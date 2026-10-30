//! GitHub App orchestration; review policy remains in the CLI child.
mod api;
mod app;
mod assets;
mod auth;
mod dashboard;
mod events;
mod external;
mod fixes;
mod guidance;
mod mentions;
mod pulls;
mod queue;
mod reconcile;
mod runner;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use clap::Args;
use serde::Deserialize;
use std::{
    collections::HashSet,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast, watch};

#[derive(Args, Clone, Debug)]
pub struct Settings {
    /// Public HTTPS URL reached by GitHub (TLS terminates at your proxy).
    #[arg(long, env = "REVERA_PUBLIC_URL")]
    pub public_url: String,
    #[arg(long, env = "REVERA_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,
    #[arg(long, env = "REVERA_DATA_DIR", default_value = "/data")]
    pub data_dir: PathBuf,
    /// Operator-trusted default config; defaults to <data-dir>/revera.yaml.
    #[arg(long, env = "REVERA_CONFIG")]
    pub config: Option<PathBuf>,
    #[arg(long, env = "REVERA_CONCURRENCY", default_value = "2")]
    pub concurrency: usize,
    #[arg(long, env = "REVERA_RECONCILE_MINUTES", default_value = "10")]
    pub reconcile_minutes: u64,
}

pub(super) struct Setup {
    app: Option<app::App>,
    token: Option<String>,
}

pub(super) struct Server {
    settings: Settings,
    config: PathBuf,
    api: app::Api,
    setup: Mutex<Setup>,
    allowed: HashSet<String>,
    queue: Mutex<queue::Queue>,
    notify: Notify,
    seen: Mutex<reconcile::Seen>,
    mentions: Mutex<mentions::Recent>,
    runner: runner::Runner,
    dashboard: Mutex<dashboard::Dashboard>,
    session_key: [u8; 32],
    events: broadcast::Sender<(&'static str, serde_json::Value)>,
    stopping: watch::Sender<bool>,
    workers: Mutex<tokio::task::JoinSet<()>>,
    // External public targets never enter installation inventory or credential paths.
    external: Mutex<external::Store>,
    external_poll: Mutex<()>,
    external_heads: Mutex<std::collections::BTreeMap<String, (i64, String)>>,
    // Connected-account inventory and policy mutations are serialized, never
    // holding a dashboard/queue lock across GitHub requests.
    reconcile_lock: Mutex<()>,
    /// The in-memory seen state has a change whose write to disk failed.
    seen_unsaved: std::sync::atomic::AtomicBool,
    reconcile_request: watch::Sender<u64>,
    installations: Mutex<reconcile::Installations>,
    fixes: Mutex<fixes::Store>,
    fix_notify: Notify,
    guidance_cache: Mutex<guidance::Cache>,
    pulls: std::sync::Mutex<pulls::Cache>,
}

impl Server {
    fn config_yaml(&self) -> Result<Option<String>> {
        read_config(&self.settings, &self.config)
    }

    fn allowed(&self, app: &app::App, owner: &str) -> bool {
        app.owner.eq_ignore_ascii_case(owner) || self.allowed.contains(&owner.to_ascii_lowercase())
    }

    async fn enqueue(&self, job: events::Job) {
        self.enqueue_inner(job, None).await;
    }

    async fn enqueue_inner(
        &self,
        mut job: events::Job,
        admission: Option<&queue::ReviewAdmission>,
    ) -> bool {
        let preferences = self.dashboard.lock().await.preferences.clone();
        if job.repo.source() == events::Source::Installed
            && !matches!(job.kind, events::Kind::Remove)
            && preferences.trigger(&job.repo.full_name) == dashboard::Trigger::Off
        {
            return false;
        }
        if matches!(job.trigger.as_str(), "webhook" | "reconcile" | "auto")
            && let events::Kind::Review { pr, .. } = &job.kind
        {
            if preferences.trigger(&job.repo.full_name) != dashboard::Trigger::Auto
                || !preferences.reviewable(pr)
            {
                return false;
            }
            job.trigger = "auto".into();
            job.mode = preferences.mode(&job.repo.full_name);
        }
        let app = self.setup.lock().await.app.clone();
        let mut queue = self.queue.lock().await;
        if job.repo.source() == events::Source::Installed
            && !matches!(job.kind, events::Kind::Remove)
        {
            let installations = self.installations.lock().await;
            if !installations.install_url.is_empty()
                && !installations.installations.iter().any(|i| {
                    i.id == job.repo.installation && i.allowed && !i.suspended && i.error.is_none()
                })
            {
                return false;
            }
            drop(installations);
            let owner = job.repo.full_name.split('/').next().unwrap_or("");
            if let Some(app) = app.as_ref()
                && !self.allowed_owner(app, owner).await
            {
                return false;
            }
        }
        if *self.stopping.borrow() || admission.is_some_and(|token| !queue.admits(job.key(), token))
        {
            return false;
        }
        let previous = queue.pending.get(&job.key()).cloned();
        queue.enqueue(job.clone());
        let mut accepted = queue
            .pending
            .get(&job.key())
            .is_some_and(|j| j.id == job.id);
        let mut skipped = false;
        if accepted && !matches!(job.kind, events::Kind::Remove) {
            if let Some(previous) = previous
                && let Err(e) = self
                    .transition(&previous, "cancelled", Some("superseded".into()))
                    .await
            {
                tracing::warn!(
                    "job metadata: {}",
                    crate::redact::diagnostic(&e.to_string())
                );
            }
            let mut summary = dashboard::JobSummary::queued(&job);
            if self.config_yaml().is_ok_and(|yaml| yaml.is_none()) {
                summary.set_state("skipped", Some(runner::UNCONFIGURED.into()));
                skipped = true;
                queue.pending.remove(&job.key());
            }
            if let Err(e) = self.update_job(summary).await {
                queue.pending.remove(&job.key());
                accepted = false;
                tracing::warn!(
                    "job metadata: {}",
                    crate::redact::diagnostic(&e.to_string())
                );
            }
        }
        drop(queue);
        if skipped && let Err(error) = self.prune_jobs().await {
            tracing::warn!(
                "job pruning: {}",
                crate::redact::diagnostic(&error.to_string())
            );
        }
        self.notify.notify_one();
        accepted && !skipped
    }

    async fn allowed_owner(&self, app: &app::App, owner: &str) -> bool {
        self.allowed_account(app, owner, None).await
    }

    async fn allowed_account(&self, app: &app::App, owner: &str, id: Option<u64>) -> bool {
        let dashboard = self.dashboard.lock().await;
        let preferences = &dashboard.preferences;
        preferences.owner_allowed(owner, id, self.implicit_owner(preferences, app, owner, id))
    }

    fn implicit_owner(
        &self,
        preferences: &dashboard::Preferences,
        app: &app::App,
        owner: &str,
        id: Option<u64>,
    ) -> bool {
        self.allowed(app, owner)
            || id
                .or_else(|| {
                    preferences
                        .owner_accounts
                        .get(&owner.to_ascii_lowercase())
                        .copied()
                })
                .is_some_and(|id| {
                    std::iter::once(&app.owner)
                        .chain(self.allowed.iter())
                        .any(|login| {
                            preferences.owner_accounts.get(&login.to_ascii_lowercase()) == Some(&id)
                        })
                })
    }

    fn request_reconcile(&self) {
        self.reconcile_request
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    async fn close(&self, key: events::Key) -> Result<()> {
        fixes::invalidate(
            self,
            key.0,
            key.2,
            Some(key.1),
            "PR closed or review cancelled",
        )
        .await?;
        let mut queue = self.queue.lock().await;
        let jobs: Vec<_> = queue
            .pending
            .get(&key)
            .into_iter()
            .chain(queue.running.get(&key).map(|running| &running.job))
            .cloned()
            .collect();
        queue.close(key);
        for job in jobs {
            self.transition(&job, "cancelled", Some("cancelled".into()))
                .await?;
        }
        Ok(())
    }

    async fn remove_repo(&self, repo: events::Repo) -> Result<()> {
        fixes::invalidate(
            self,
            repo.id,
            events::Source::Installed,
            None,
            "repository removed or approval revoked",
        )
        .await?;
        {
            let mut dashboard = self.dashboard.lock().await;
            dashboard.repos.retain(|_, info| info.repo.id != repo.id);
            dashboard::write_inventory(
                &self.settings.data_dir,
                dashboard.installations,
                &dashboard.repos,
            )?;
        }
        self.pulls.lock().unwrap().remove_repo(repo.id);
        let mut queue = self.queue.lock().await;
        for job in queue.remove_repo(repo) {
            self.transition(&job, "cancelled", Some("repository removed".into()))
                .await?;
        }
        self.notify.notify_one();
        Ok(())
    }

    async fn cancel_jobs(&self) -> Result<()> {
        let mut queue = self.queue.lock().await;
        let mut jobs: Vec<_> = std::mem::take(&mut queue.pending).into_values().collect();
        for running in queue.running.values() {
            let _ = running.cancel.send(true);
            jobs.push(running.job.clone());
        }
        for job in jobs {
            self.transition(&job, "cancelled", Some("server shutting down".into()))
                .await?;
        }
        Ok(())
    }

    async fn handle(&self, app: &app::App, event: events::Event) -> Result<()> {
        match event {
            events::Event::Job(job) => self.enqueue(job).await,
            events::Event::Close(repo, number) => {
                self.close((repo, number, events::Source::Installed))
                    .await?
            }
            events::Event::Rerequest(repo, number) => {
                if self
                    .dashboard
                    .lock()
                    .await
                    .preferences
                    .trigger(&repo.full_name)
                    == dashboard::Trigger::Off
                {
                    return Ok(());
                }
                let token = self
                    .api
                    .installation_token(
                        app,
                        repo.installation_id()?,
                        Some(repo.id),
                        app::Permissions::ReadPulls,
                    )
                    .await?;
                let pr = self
                    .api
                    .request(
                        reqwest::Method::GET,
                        &format!("/repos/{}/pulls/{number}", repo.full_name),
                        &token,
                        None,
                    )
                    .await?;
                if pr["state"].as_str() != Some("closed") {
                    let mode = self
                        .dashboard
                        .lock()
                        .await
                        .preferences
                        .mode(&repo.full_name);
                    self.enqueue(events::Job {
                        id: uuid::Uuid::new_v4().simple().to_string(),
                        trigger: "rerun".into(),
                        mode,
                        repo,
                        kind: events::Kind::Review {
                            number,
                            pr,
                            force: true,
                        },
                    })
                    .await;
                }
            }
            events::Event::Mention(request) => mentions::handle(self, app, request).await?,
            events::Event::RemoveRepos(repos) => {
                for repo in repos {
                    self.remove_repo(repo).await?;
                }
            }
            events::Event::RemoveInstallation(id) => {
                let mut repos: Vec<_> = self
                    .seen
                    .lock()
                    .await
                    .0
                    .values()
                    .filter(|r| r.repo.installation == Some(id))
                    .map(|r| r.repo.clone())
                    .collect();
                {
                    let queue = self.queue.lock().await;
                    repos.extend(
                        queue
                            .pending
                            .values()
                            .chain(queue.running.values().map(|r| &r.job))
                            .filter(|j| j.repo.installation == Some(id))
                            .map(|j| j.repo.clone()),
                    );
                }
                repos.extend(
                    self.dashboard
                        .lock()
                        .await
                        .repos
                        .values()
                        .filter(|info| info.repo.installation == Some(id))
                        .map(|info| info.repo.clone()),
                );
                repos.sort_by_key(|repo| repo.id);
                repos.dedup_by_key(|repo| repo.id);
                for repo in repos {
                    self.remove_repo(repo).await?;
                }
            }
            events::Event::Ignore => {}
        }
        Ok(())
    }
}

fn read_config(settings: &Settings, path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(yaml) => Ok(Some(yaml)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && settings.config.is_none() => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn initialize(
    mut settings: Settings,
    runner: runner::Runner,
    api: app::Api,
) -> Result<Arc<Server>> {
    let url = url::Url::parse(&settings.public_url).context("invalid --public-url")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("--public-url must be an HTTPS URL without credentials, query or fragment");
    }
    if settings.concurrency == 0 || settings.reconcile_minutes == 0 {
        bail!("concurrency and reconcile-minutes must be positive");
    }
    settings.public_url = settings.public_url.trim_end_matches('/').into();
    let cwd = std::env::current_dir()?;
    let mut data_dir = PathBuf::new();
    for component in cwd.join(&settings.data_dir).components() {
        match component {
            std::path::Component::ParentDir => {
                data_dir.pop();
            }
            std::path::Component::CurDir => {}
            other => data_dir.push(other),
        }
    }
    settings.data_dir = data_dir;
    // Check every component, not only the final data directory.
    let relative = settings
        .data_dir
        .strip_prefix("/")
        .context("data directory must be absolute")?;
    crate::fsutil::ensure_dir(std::path::Path::new("/"), relative)?;
    settings.data_dir = settings.data_dir.canonicalize()?;
    for dir in ["repos", "jobs", "cache", "external", "fixes"] {
        crate::fsutil::ensure_dir(&settings.data_dir, std::path::Path::new(dir))?;
    }
    let config = settings
        .config
        .as_ref()
        .map(|p| cwd.join(p))
        .unwrap_or_else(|| settings.data_dir.join("revera.yaml"));
    let seen = dashboard::read_json::<reconcile::Seen>(&settings.data_dir, "seen.json")?;
    let mentions = mentions::Recent::load(&settings.data_dir)?;
    let mut dashboard = dashboard::Dashboard::load(&settings.data_dir, &seen)?;
    let session_key = auth::key(&settings.data_dir)?;
    let yaml = read_config(&settings, &config).with_context(|| {
        format!(
            "server default config {} must exist and be valid",
            config.display()
        )
    })?;
    let config = if let Some(yaml) = yaml.as_deref() {
        dashboard.parse(yaml)?.check_trust(true)?;
        config.canonicalize()?
    } else {
        config
    };
    if config.starts_with(settings.data_dir.join("repos"))
        || config.starts_with(settings.data_dir.join("external"))
    {
        bail!("server default config must be outside the repository workspaces");
    }
    dashboard.sync_health(&settings.data_dir, yaml.as_deref(), &session_key)?;
    let app_path = settings.data_dir.join("app.json");
    crate::fsutil::check_regular_target(&app_path)?;
    let app = if app_path.exists() {
        let app: app::App = serde_json::from_slice(&std::fs::read(app_path)?)?;
        app.register_secrets();
        app.jwt()?;
        Some(app)
    } else {
        None
    };
    let token = if app.is_none() {
        let path = settings.data_dir.join("setup-token");
        crate::fsutil::check_regular_target(&path)?;
        let token = if path.exists() {
            std::fs::read_to_string(path)?
        } else {
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            crate::fsutil::write_secret_atomic(
                &settings.data_dir,
                "setup-token",
                token.as_bytes(),
            )?;
            token
        };
        // The contract's one bootstrap link is the sole intentional token disclosure.
        eprintln!(
            "open {}/setup?token={} to create the GitHub App",
            settings.public_url, token
        );
        Some(token)
    } else {
        None
    };
    let allowed = std::env::var("REVERA_ALLOWED_OWNERS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let external = external::Store::load(&settings.data_dir)?;
    dashboard.preferences.external.validate()?;
    dashboard.preferences.fixes.validate()?;
    let fixes = fixes::Store::load(&settings.data_dir)?;
    let server = Arc::new(Server {
        settings,
        config,
        api,
        setup: Mutex::new(Setup { app, token }),
        allowed,
        queue: Mutex::new(queue::Queue::default()),
        notify: Notify::new(),
        seen: Mutex::new(seen),
        mentions: Mutex::new(mentions),
        runner,
        dashboard: Mutex::new(dashboard),
        session_key,
        events: broadcast::channel(1024).0,
        stopping: watch::channel(false).0,
        workers: Mutex::new(tokio::task::JoinSet::new()),
        external: Mutex::new(external),
        external_poll: Mutex::new(()),
        external_heads: Mutex::new(std::collections::BTreeMap::new()),
        reconcile_lock: Mutex::new(()),
        seen_unsaved: std::sync::atomic::AtomicBool::new(false),
        reconcile_request: watch::channel(0).0,
        installations: Mutex::new(reconcile::Installations::default()),
        fixes: Mutex::new(fixes),
        fix_notify: Notify::new(),
        guidance_cache: Mutex::new(guidance::Cache::default()),
        pulls: std::sync::Mutex::new(pulls::Cache::default()),
    });
    if let Err(error) = server.recover_checks().await {
        tracing::warn!(
            "orphan check recovery: {}",
            crate::redact::diagnostic(&error.to_string())
        );
    }
    Ok(server)
}

pub async fn serve(settings: Settings) -> Result<()> {
    let api = app::Api::new(
        std::env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".into()),
    )?;
    let server = initialize(settings, runner::Runner::current()?, api).await?;
    let listener = tokio::net::TcpListener::bind(server.settings.listen).await?;
    serve_until(
        server,
        listener,
        shutdown(),
        std::time::Duration::from_secs(10),
    )
    .await
}

async fn serve_until(
    server: Arc<Server>,
    listener: tokio::net::TcpListener,
    signal: impl std::future::Future<Output = ()>,
    deadline: std::time::Duration,
) -> Result<()> {
    let scheduler = tokio::spawn(dispatch(server.clone()));
    let fix_scheduler = tokio::spawn(fixes::dispatch(server.clone()));
    let reconcile = tokio::spawn(reconcile::loop_forever(server.clone()));
    let (drain, mut draining) = watch::channel(false);
    let http = axum::serve(listener, router(server.clone())).with_graceful_shutdown(async move {
        let _ = draining.wait_for(|draining| *draining).await;
    });
    let http = std::future::IntoFuture::into_future(http);
    tokio::pin!(http);
    let result = tokio::select! {
        result = &mut http => Some(result),
        _ = signal => None,
    };
    server.stopping.send_replace(true);
    scheduler.abort();
    fix_scheduler.abort();
    reconcile.abort();
    let _ = tokio::join!(scheduler, fix_scheduler, reconcile);
    let fixes_stopped = fixes::shutdown(&server).await;
    let cancelled = server.cancel_jobs().await;
    drain.send_replace(true);
    let finished = tokio::time::timeout(deadline, async {
        let result = match result {
            Some(result) => result,
            None => http.await,
        };
        while server.workers.lock().await.join_next().await.is_some() {}
        result
    })
    .await;
    match finished {
        Ok(result) => result?,
        Err(_) => {
            tracing::warn!("shutdown drain deadline reached");
            let mut workers = server.workers.lock().await;
            workers.abort_all();
            while workers.join_next().await.is_some() {}
        }
    }
    cancelled?;
    fixes_stopped?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn dispatch(server: Arc<Server>) {
    let semaphore = Arc::new(Semaphore::new(server.settings.concurrency));
    loop {
        server.notify.notified().await;
        loop {
            let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                break;
            };
            let external_limit = server
                .dashboard
                .lock()
                .await
                .preferences
                .external
                .concurrency;
            let mut queue = server.queue.lock().await;
            if *server.stopping.borrow() {
                return;
            }
            let Some((job, cancel)) = queue.take_with_external_limit(external_limit) else {
                break;
            };
            drop(queue);
            let worker = server.clone();
            let mut workers = server.workers.lock().await;
            while workers.try_join_next().is_some() {}
            workers.spawn(async move {
                runner::run(worker.clone(), &job, cancel).await;
                let mut queue = worker.queue.lock().await;
                if queue
                    .running
                    .get(&job.key())
                    .is_some_and(|r| r.job.id == job.id)
                {
                    queue.running.remove(&job.key());
                }
                drop(queue);
                fixes::refresh_review_status(&worker, &job).await;
                drop(permit);
                worker.notify.notify_one();
            });
        }
    }
}

fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(status))
        .route("/setup", get(auth::setup))
        .route("/setup/callback", get(callback))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/webhook", post(webhook))
        .merge(api::router(server.clone()))
        .fallback(assets::serve)
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .with_state(server)
}

async fn status() -> Response {
    assets::page("/")
}

#[derive(Deserialize, Default)]
struct SetupQuery {
    #[serde(default)]
    token: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
}

async fn callback(State(server): State<Arc<Server>>, Query(query): Query<SetupQuery>) -> Response {
    let mut setup = server.setup.lock().await;
    if setup.app.is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !setup
        .token
        .as_ref()
        .is_some_and(|t| app::token_matches(t, &query.state))
        || query.code.is_empty()
        || !query
            .code
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result: Result<app::App> = async {
        let value = server
            .api
            .request(
                reqwest::Method::POST,
                &format!("/app-manifests/{}/conversions", query.code),
                "",
                None,
            )
            .await?;
        let app = app::App::from_conversion(&value)?;
        crate::fsutil::write_secret_atomic(
            &server.settings.data_dir,
            "app.json",
            &serde_json::to_vec_pretty(&app)?,
        )?;
        Ok(app)
    }
    .await;
    match result {
        Ok(app) => {
            let redirect = format!("{}/auth/login", server.settings.public_url);
            setup.app = Some(app);
            setup.token = None;
            if let Err(e) = std::fs::remove_file(server.settings.data_dir.join("setup-token")) {
                tracing::warn!(
                    "cannot delete setup token: {}",
                    crate::redact::diagnostic(&e.to_string())
                );
            }
            server.notify.notify_one();
            Redirect::to(&redirect).into_response()
        }
        Err(e) => {
            tracing::warn!(
                "app setup failed: {}",
                crate::redact::diagnostic(&format!("{e:#}"))
            );
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn webhook(State(server): State<Arc<Server>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Some(app) = server.setup.lock().await.app.clone() else {
        return StatusCode::UNAUTHORIZED;
    };
    let signature = headers
        .get("X-Hub-Signature-256")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !app::signature_valid(&app.webhook_secret, &body, signature) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if event == "ping" {
        return StatusCode::OK;
    }
    // Lifecycle events from unapproved installers can refresh account status,
    // but must never reach the job mapper (or expose their repositories).
    if reconcile::installation_event(event, &payload) {
        if event == "installation"
            && matches!(payload["action"].as_str(), Some("deleted" | "suspend"))
        {
            let mut inventory = server.installations.lock().await;
            for installation in inventory
                .installations
                .iter_mut()
                .filter(|i| i.id == payload["installation"]["id"].as_u64())
            {
                installation.suspended = true;
                installation.status = "suspended".into();
                installation.repo_count = None;
            }
        }
        let account = &payload["installation"]["account"];
        if matches!(
            payload["action"].as_str(),
            Some("deleted" | "removed" | "suspend")
        ) && server
            .allowed_account(
                &app,
                account["login"].as_str().unwrap_or(""),
                account["id"].as_u64(),
            )
            .await
        {
            let mapped = if payload["action"] == "suspend" {
                payload["installation"]["id"]
                    .as_u64()
                    .map(events::Event::RemoveInstallation)
                    .ok_or_else(|| anyhow::anyhow!("missing installation"))
            } else {
                events::map_with_preferences(
                    event,
                    &payload,
                    app.id,
                    &server.dashboard.lock().await.preferences,
                )
            };
            match mapped {
                Ok(removal) => {
                    if let Err(error) = server.handle(&app, removal).await {
                        tracing::warn!(
                            "installation removal: {}",
                            crate::redact::diagnostic(&format!("{error:#}"))
                        );
                    }
                }
                Err(_) => return StatusCode::BAD_REQUEST,
            }
        }
        server.request_reconcile();
        return StatusCode::ACCEPTED;
    }
    let owner = &payload["repository"]["owner"];
    if !server
        .allowed_account(
            &app,
            owner["login"].as_str().unwrap_or(""),
            owner["id"].as_u64(),
        )
        .await
    {
        tracing::warn!("ignored webhook from an owner outside the allowed owners");
        return StatusCode::ACCEPTED;
    }
    let preferences = server.dashboard.lock().await.preferences.clone();
    if event == "pull_request" {
        server.pulls.lock().unwrap().webhook(&payload);
    }
    if event == "check_run" && payload["action"] == "requested_action" {
        tokio::spawn(async move {
            if let Err(error) = fixes::check_action(&server, &app, &payload).await {
                tracing::warn!(
                    "fix check action: {}",
                    crate::redact::diagnostic(&error.to_string())
                );
            }
        });
        return StatusCode::ACCEPTED;
    }
    let mapped = if event == "issue_comment" {
        mentions::event(&payload, &app.slug, &preferences)
            .map(|request| request.map_or(events::Event::Ignore, events::Event::Mention))
    } else {
        events::map_with_preferences(event, &payload, app.id, &preferences)
    };
    let Ok(event) = mapped else {
        return StatusCode::BAD_REQUEST;
    };
    if let events::Event::Job(job) = &event
        && matches!(job.kind, events::Kind::Index)
    {
        let sha = if job.trigger == "pull_request" {
            payload["pull_request"]["base"]["sha"].as_str()
        } else {
            payload["after"].as_str()
        };
        if let Some(sha) = sha
            && server.seen.lock().await.index_current(job.repo.id, sha)
        {
            return StatusCode::ACCEPTED;
        }
    }
    // Apply ordinary queue mutations in delivery order; only API work is detached.
    if matches!(
        event,
        events::Event::Rerequest(_, _) | events::Event::Mention(_)
    ) {
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = server.handle(&app, event).await {
                tracing::warn!(
                    "webhook work failed: {}",
                    crate::redact::diagnostic(&format!("{e:#}"))
                );
            }
        });
    } else if let Err(e) = server.handle(&app, event).await {
        tracing::warn!(
            "webhook work failed: {}",
            crate::redact::diagnostic(&format!("{e:#}"))
        );
    }
    StatusCode::ACCEPTED
}
