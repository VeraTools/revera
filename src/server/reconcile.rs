use super::{
    Server,
    dashboard::{Mode, Trigger},
    events::{Job, Kind, Repo},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

// Connected accounts: account metadata only; unapproved repositories are never fetched.
#[derive(Clone, Default, Serialize)]
pub struct Installations {
    pub installations: Vec<Installation>,
    pub install_url: String,
    pub settings_url: String,
    pub advanced_url: String,
    pub requested_permissions: BTreeMap<String, String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default, rename = "type")]
    pub kind: String,
}

#[derive(Clone, Serialize)]
pub struct Installation {
    pub id: Option<u64>,
    pub account: Account,
    pub repository_selection: Option<String>,
    pub suspended: bool,
    pub permissions: BTreeMap<String, String>,
    pub html_url: String,
    pub status: String,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn installation_event(event: &str, payload: &Value) -> bool {
    match event {
        "installation" => matches!(
            payload["action"].as_str(),
            Some("created" | "deleted" | "suspend" | "unsuspend" | "new_permissions_accepted")
        ),
        "installation_repositories" => {
            matches!(payload["action"].as_str(), Some("added" | "removed"))
        }
        // GitHub documents the rename payload without an action field.
        "installation_target" => payload.get("account").is_some(),
        _ => false,
    }
}

fn permission_level(value: &str) -> u8 {
    match value {
        "read" => 1,
        "write" => 2,
        "admin" => 3,
        _ => 0,
    }
}

pub(super) async fn discover(server: &Server, app: &super::app::App) -> Result<Installations> {
    let previous = server.installations.lock().await.clone();
    let jwt = app.jwt()?;
    let registration = server
        .api
        .request(reqwest::Method::GET, "/app", &jwt, None)
        .await?;
    let requested_permissions: BTreeMap<String, String> =
        serde_json::from_value(registration["permissions"].clone())?;
    let values = server.api.list("/app/installations", &jwt, None).await?;
    let settings_url = if registration["owner"]["type"] == "Organization" {
        format!(
            "https://github.com/organizations/{}/settings/apps/{}",
            registration["owner"]["login"]
                .as_str()
                .unwrap_or(&app.owner),
            app.slug
        )
    } else {
        format!("https://github.com/settings/apps/{}", app.slug)
    };
    let mut result = Installations {
        install_url: format!("https://github.com/apps/{}/installations/new", app.slug),
        advanced_url: format!("{settings_url}/advanced"),
        settings_url: format!("{settings_url}/permissions"),
        requested_permissions,
        ..Default::default()
    };
    let mut dashboard = server.dashboard.lock().await;
    let mut preferences = dashboard.preferences.clone();
    for value in values {
        let account: Account = serde_json::from_value(value["account"].clone())?;
        let id = value["id"].as_u64().context("installation missing id")?;
        let allowed = preferences.owner_allowed(
            &account.login,
            account.id,
            server.implicit_owner(&preferences, app, &account.login, account.id),
        );
        if allowed {
            // Also pin implicit app/env owners. Approval survives renames, but
            // never follows a different account that later takes the old login.
            let explicit = preferences.allowed_owners.iter().any(|login| {
                login.eq_ignore_ascii_case(&account.login)
                    || account.id.is_some_and(|id| {
                        preferences.owner_accounts.get(&login.to_ascii_lowercase()) == Some(&id)
                    })
            });
            if explicit {
                preferences.approve_owner(&account.login, account.id);
            } else if let Some(id) = account.id.filter(|id| *id != 0) {
                preferences
                    .owner_accounts
                    .insert(account.login.to_ascii_lowercase(), id);
            }
        }
        let suspended = !value["suspended_at"].is_null();
        let permissions: BTreeMap<String, String> = serde_json::from_value(
            value
                .get("permissions")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )?;
        let outdated = result.requested_permissions.iter().any(|(key, requested)| {
            permission_level(permissions.get(key).map(String::as_str).unwrap_or(""))
                < permission_level(requested)
        });
        let status = if suspended {
            "suspended"
        } else if !allowed {
            "awaiting_approval"
        } else if outdated {
            "permission_update"
        } else {
            "allowed"
        };
        result.installations.push(Installation {
            id: Some(id),
            repository_selection: value["repository_selection"].as_str().map(str::to_owned),
            suspended,
            permissions,
            html_url: value["html_url"].as_str().unwrap_or("").to_owned(),
            status: status.into(),
            allowed,
            repo_count: (allowed && !suspended).then(|| {
                dashboard
                    .repos
                    .values()
                    .filter(|r| r.repo.installation == Some(id))
                    .count()
            }),
            error: previous
                .installations
                .iter()
                .find(|i| i.id == Some(id))
                .and_then(|i| i.error.clone()),
            account,
        });
    }
    let mut owners = preferences.allowed_owners.clone();
    for fallback in std::iter::once(&app.owner).chain(server.allowed.iter()) {
        let pinned = preferences
            .owner_accounts
            .get(&fallback.to_ascii_lowercase());
        if !pinned.is_some_and(|id| {
            owners.iter().any(|owner| {
                preferences.owner_accounts.get(&owner.to_ascii_lowercase()) == Some(id)
            })
        }) {
            owners.push(fallback.clone());
        }
    }
    owners.sort_by_key(|v| v.to_ascii_lowercase());
    owners.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    for owner in owners {
        let id = preferences
            .owner_accounts
            .get(&owner.to_ascii_lowercase())
            .copied();
        if !preferences.owner_allowed(&owner, id, server.allowed(app, &owner))
            || result.installations.iter().any(|i| {
                i.account.login.eq_ignore_ascii_case(&owner) || id.is_some() && i.account.id == id
            })
        {
            continue;
        }
        result.installations.push(Installation {
            id: None,
            account: Account {
                login: owner,
                id,
                kind: String::new(),
            },
            repository_selection: None,
            suspended: false,
            permissions: BTreeMap::new(),
            html_url: result.install_url.clone(),
            status: "not_installed".into(),
            allowed: true,
            repo_count: None,
            error: None,
        });
    }
    result
        .installations
        .sort_by_key(|i| i.account.login.to_ascii_lowercase());
    if serde_json::to_value(&preferences)? != serde_json::to_value(&dashboard.preferences)? {
        super::dashboard::write_preferences(&server.settings.data_dir, &preferences)?;
        dashboard.preferences = preferences;
    }
    drop(dashboard);
    *server.installations.lock().await = result.clone();
    server.fix_notify.notify_one();
    Ok(result)
}

#[derive(Default, Serialize, Deserialize)]
pub struct Seen(pub BTreeMap<u64, SeenRepo>);

#[derive(Serialize, Deserialize)]
pub struct SeenRepo {
    pub repo: Repo,
    pub pushed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_sha: Option<String>,
    pub heads: BTreeMap<u64, String>,
    /// A webhook can finish before the first inventory of open PRs.
    #[serde(default)]
    pub initialized: bool,
}

impl Seen {
    pub fn index_current(&self, repo_id: u64, sha: &str) -> bool {
        self.0
            .get(&repo_id)
            .and_then(|known| known.indexed_sha.as_deref())
            == Some(sha)
    }

    #[cfg(test)]
    pub fn observe(&mut self, repo: &Repo, prs: &[Value]) -> Vec<Job> {
        self.observe_with_preferences(
            repo,
            prs,
            &super::dashboard::Preferences {
                default_trigger: Trigger::Auto,
                index_on_push: true,
                ..Default::default()
            },
            &BTreeSet::new(),
            false,
        )
    }

    pub fn observe_with_preferences(
        &mut self,
        repo: &Repo,
        prs: &[Value],
        preferences: &super::dashboard::Preferences,
        retry: &BTreeSet<u64>,
        retry_index: bool,
    ) -> Vec<Job> {
        if !self.0.get(&repo.id).is_some_and(|known| known.initialized) {
            let heads = prs
                .iter()
                .filter(|pr| {
                    !pr["number"]
                        .as_u64()
                        .is_some_and(|number| retry.contains(&number))
                })
                .filter_map(|pr| Some((pr["number"].as_u64()?, pr["head"]["sha"].as_str()?.into())))
                .collect();
            self.0.insert(
                repo.id,
                SeenRepo {
                    repo: repo.clone(),
                    pushed_at: if preferences.index_on_push && retry_index {
                        String::new()
                    } else {
                        repo.pushed_at.clone()
                    },
                    heads,
                    indexed_sha: self
                        .0
                        .get(&repo.id)
                        .and_then(|known| known.indexed_sha.clone()),
                    initialized: true,
                },
            );
            // Setup-skipped PRs are not part of the initial no-backfill baseline.
        }
        let known = self.0.get_mut(&repo.id).expect("initialized repository");
        known.repo = repo.clone();
        let mut jobs = Vec::new();
        if preferences.index_on_push
            && preferences.trigger(&repo.full_name) != Trigger::Off
            && (known.pushed_at != repo.pushed_at || retry_index)
        {
            jobs.push(Job {
                id: uuid::Uuid::new_v4().simple().to_string(),
                trigger: "reconcile".into(),
                mode: Mode::Standard,
                repo: repo.clone(),
                kind: Kind::Index,
            });
        } else if !preferences.index_on_push {
            known.pushed_at.clone_from(&repo.pushed_at);
        }
        for pr in prs.iter().filter(|pr| {
            preferences.trigger(&repo.full_name) == Trigger::Auto && preferences.reviewable(pr)
        }) {
            if let (Some(number), Some(head)) = (pr["number"].as_u64(), pr["head"]["sha"].as_str())
                && (known.heads.get(&number).map(String::as_str) != Some(head)
                    || retry.contains(&number))
            {
                jobs.push(Job {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    trigger: "auto".into(),
                    mode: preferences.mode(&repo.full_name),
                    repo: repo.clone(),
                    kind: Kind::Review {
                        number,
                        pr: pr.clone(),
                        force: false,
                    },
                });
            }
        }
        jobs
    }

    pub fn index_failed(&mut self, job: &Job) {
        if let Some(known) = self.0.get_mut(&job.repo.id) {
            known.indexed_sha = None;
        }
    }

    pub fn finished(&mut self, job: &Job, indexed_sha: Option<&str>) {
        if job.repo.source() == super::events::Source::External {
            return;
        }
        let known = self.0.entry(job.repo.id).or_insert_with(|| SeenRepo {
            repo: job.repo.clone(),
            pushed_at: String::new(),
            indexed_sha: None,
            heads: BTreeMap::new(),
            initialized: false,
        });
        match &job.kind {
            Kind::Review { number, .. } => {
                known.heads.insert(*number, job.head().into());
            }
            Kind::Index => {
                known.pushed_at.clone_from(&job.repo.pushed_at);
                if let Some(sha) = indexed_sha {
                    known.indexed_sha = Some(sha.into());
                }
            }
            Kind::Remove => {
                self.0.remove(&job.repo.id);
            }
        }
    }
}

pub async fn save(server: &Server, seen: &Seen) -> Result<()> {
    let mut value = serde_json::to_value(seen)?;
    crate::redact::json(&mut value);
    crate::fsutil::write_atomic_in(
        &server.settings.data_dir,
        "seen.json",
        &serde_json::to_vec_pretty(&value)?,
    )
}

pub(super) async fn once(server: &Server) -> Result<()> {
    let _guard = server.reconcile_lock.lock().await;
    server.recover_checks().await?;
    once_locked(server).await
}

pub(super) async fn evict_unapproved(server: &Server, app: &super::app::App) -> Result<()> {
    let mut repos: BTreeMap<_, _> = server
        .dashboard
        .lock()
        .await
        .repos
        .values()
        .map(|info| (info.repo.id, info.repo.clone()))
        .collect();
    repos.extend(
        server
            .seen
            .lock()
            .await
            .0
            .values()
            .map(|known| (known.repo.id, known.repo.clone())),
    );
    {
        let queue = server.queue.lock().await;
        repos.extend(
            queue
                .pending
                .values()
                .chain(queue.running.values().map(|r| &r.job))
                .filter(|job| job.repo.source() == super::events::Source::Installed)
                .map(|job| (job.repo.id, job.repo.clone())),
        );
    }
    for repo in repos.into_values() {
        let owner = repo.full_name.split('/').next().unwrap_or("");
        if !server.allowed_owner(app, owner).await {
            server.remove_repo(repo).await?;
        }
    }
    let _ = server.events.send(("repos", serde_json::json!({})));
    Ok(())
}

pub(super) async fn once_locked(server: &Server) -> Result<()> {
    let Some(app) = server.setup.lock().await.app.clone() else {
        return Ok(());
    };
    let mut known: BTreeMap<_, _> = server
        .seen
        .lock()
        .await
        .0
        .values()
        .map(|known| (known.repo.id, known.repo.clone()))
        .collect();
    known.extend(
        server
            .dashboard
            .lock()
            .await
            .repos
            .values()
            .map(|info| (info.repo.id, info.repo.clone())),
    );
    {
        let queue = server.queue.lock().await;
        known.extend(
            queue
                .pending
                .values()
                .chain(queue.running.values().map(|r| &r.job))
                .filter(|job| job.repo.source() == super::events::Source::Installed)
                .map(|job| (job.repo.id, job.repo.clone())),
        );
    }
    let mut accounts = discover(server, &app).await?;
    let mut inventory = BTreeMap::new();
    let mut installed = 0;
    for installation in &mut accounts.installations {
        if !installation.allowed || installation.suspended {
            continue;
        }
        let Some(id) = installation.id else {
            continue;
        };
        installed += 1;
        let access: Result<_> = async {
            let token = server
                .api
                .installation_token(&app, id, None, super::app::Permissions::ReadPulls)
                .await?;
            let repos = server
                .api
                .list("/installation/repositories", &token, Some("repositories"))
                .await?;
            Ok((token, repos))
        }
        .await;
        let (token, repos) = match access {
            Ok(access) => access,
            Err(error) => {
                installation.error = Some(crate::redact::diagnostic(&format!(
                    "GitHub access unavailable: {error:#}"
                )));
                installation.repo_count = None;
                if !error
                    .downcast_ref::<super::app::ApiStatus>()
                    .is_some_and(|status| status.denied())
                {
                    // A temporary outage is not an uninstall. Preserve the last
                    // inventory for this approved installation, without new jobs.
                    inventory.extend(
                        server
                            .dashboard
                            .lock()
                            .await
                            .repos
                            .iter()
                            .filter(|(_, info)| info.repo.installation == Some(id))
                            .map(|(name, info)| (name.clone(), info.clone())),
                    );
                }
                continue;
            }
        };
        installation.repo_count = Some(repos.len());
        installation.error = None;
        if let Some(cached) = server
            .installations
            .lock()
            .await
            .installations
            .iter_mut()
            .find(|i| i.id == Some(id))
        {
            cached.error = None;
        }
        for value in repos {
            let repo = Repo::parse(&value, id)?;
            if let Some(previous) = known.get(&repo.id)
                && previous.full_name != repo.full_name
            {
                server.remove_repo(previous.clone()).await?;
            }
            if !repo
                .full_name
                .split('/')
                .next()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(&installation.account.login))
            {
                anyhow::bail!("repository owner differs from installation account");
            }
            let prs = server
                .api
                .list(
                    &format!("/repos/{}/pulls?state=open", repo.full_name),
                    &token,
                    None,
                )
                .await?;
            let mut seen = server.seen.lock().await;
            let (preferences, retry, retry_at) = {
                let dashboard = server.dashboard.lock().await;
                (
                    dashboard.preferences.clone(),
                    dashboard.unconfigured_reviews(&repo.full_name),
                    dashboard
                        .preferences
                        .index_on_push
                        .then(|| dashboard.index_retry_at(&repo.full_name))
                        .flatten(),
                )
            };
            let jobs = seen.observe_with_preferences(
                &repo,
                &prs,
                &preferences,
                &retry,
                retry_at.is_some(),
            );
            save(server, &seen).await?;
            drop(seen);
            let workspace = server.settings.data_dir.join("repos").join(&repo.full_name);
            let reference = format!("refs/remotes/origin/{}", repo.default_branch);
            let mut has_override = false;
            let env = super::runner::child_env(
                "GITHUB_TOKEN",
                &token,
                &server.settings.data_dir,
                &repo.full_name,
            );
            if workspace.join(".git").is_dir()
                && let Ok(sha) = crate::git::rev_parse_with_env(&workspace, &reference, &env).await
            {
                has_override = crate::git::read_blob_with_env(
                    &workspace,
                    &sha,
                    "revera.yaml",
                    1024 * 1024,
                    &env,
                )
                .await?
                .is_some();
            }
            let info = super::dashboard::RepoInfo {
                repo: repo.clone(),
                private: value["private"].as_bool().unwrap_or(false),
                html_url: value["html_url"].as_str().unwrap_or("").into(),
                has_override,
                open_prs: Some(prs.len() as u64),
            };
            inventory.insert(repo.full_name.clone(), info.clone());
            for job in jobs {
                // Both an unadvanced push checkpoint and a failed legacy job can
                // request an index. Neither may bypass reconcile retry backoff.
                if matches!(job.kind, Kind::Index)
                    && retry_at.is_some_and(|at| at > chrono::Utc::now())
                {
                    continue;
                }
                if !server.queue.lock().await.contains(job.key()) {
                    if matches!(job.kind, Kind::Index)
                        && index_head_current(server, &repo, &token).await
                    {
                        let mut seen = server.seen.lock().await;
                        if let Some(known) = seen.0.get_mut(&repo.id) {
                            known.pushed_at.clone_from(&repo.pushed_at);
                        }
                        save(server, &seen).await?;
                        continue;
                    }
                    server.enqueue(job).await;
                }
            }
            server.notify.notify_one();
        }
    }
    let removed: Vec<_> = known
        .into_values()
        .filter(|repo| !inventory.values().any(|info| info.repo.id == repo.id))
        .collect();
    for repo in removed {
        server.remove_repo(repo).await?;
    }
    let mut dashboard = server.dashboard.lock().await;
    super::dashboard::write_inventory(&server.settings.data_dir, installed, &inventory)?;
    dashboard.installations = installed;
    dashboard.repos = inventory;
    let errors: Vec<_> = accounts
        .installations
        .iter()
        .filter_map(|i| i.error.as_deref())
        .collect();
    dashboard.github = if errors.is_empty() {
        serde_json::json!({"ok": true})
    } else {
        serde_json::json!({"ok": false, "error": errors.join("; ")})
    };
    drop(dashboard);
    *server.installations.lock().await = accounts;
    let _ = server.events.send(("repos", serde_json::json!({})));
    Ok(())
}

/// A failed head lookup keeps the existing fallback: let the index job fetch it.
async fn index_head_current(server: &Server, repo: &Repo, token: &str) -> bool {
    let mut url = url::Url::parse("https://api.github.com").expect("static URL");
    url.path_segments_mut()
        .expect("hierarchical URL")
        .extend(["repos"])
        .extend(repo.full_name.split('/'))
        .extend(["branches", &repo.default_branch]);
    let Ok(value) = server
        .api
        .request(reqwest::Method::GET, url.path(), token, None)
        .await
    else {
        return false;
    };
    let Some(sha) = value["commit"]["sha"]
        .as_str()
        .filter(|sha| crate::git::is_oid(sha))
    else {
        return false;
    };
    server.seen.lock().await.index_current(repo.id, sha)
}

pub(super) async fn tick(server: &Server) -> Result<()> {
    super::api::test_untested(server).await;
    if let Err(error) = super::external::poll(server).await {
        tracing::warn!(
            "external poll failed: {}",
            crate::redact::diagnostic(&format!("{error:#}"))
        );
    }
    once(server).await
}

pub async fn loop_forever(server: Arc<Server>) {
    let mut requests = server.reconcile_request.subscribe();
    let mut interval =
        tokio::time::interval(Duration::from_secs(server.settings.reconcile_minutes * 60));
    loop {
        let webhook = tokio::select! {
            _ = interval.tick() => false,
            result = requests.changed() => {
                if result.is_err() { return; }
                true
            },
        };
        if webhook {
            // Coalesce a burst: refresh once no request has arrived for 250 ms, within 5 s.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                requests.borrow_and_update();
                let quiet =
                    (tokio::time::Instant::now() + Duration::from_millis(250)).min(deadline);
                match tokio::time::timeout_at(quiet, requests.changed()).await {
                    Ok(Err(_)) => return,
                    Ok(Ok(())) if tokio::time::Instant::now() < deadline => {}
                    _ => break,
                }
            }
            requests.borrow_and_update();
        }
        if let Err(e) = tick(&server).await {
            server.dashboard.lock().await.github = serde_json::json!({
                "ok": false, "error": crate::redact::diagnostic(&format!("{e:#}"))
            });
            tracing::warn!(
                "reconcile failed: {}",
                crate::redact::diagnostic(&format!("{e:#}"))
            );
        }
    }
}
