use super::{
    Server,
    dashboard::{Mode, Trigger},
    events::{Job, Kind, Repo},
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

#[derive(Default, Serialize, Deserialize)]
pub struct Seen(pub BTreeMap<u64, SeenRepo>);

#[derive(Serialize, Deserialize)]
pub struct SeenRepo {
    pub repo: Repo,
    pub pushed_at: String,
    pub heads: BTreeMap<u64, String>,
    /// A webhook can finish before the first inventory of open PRs.
    #[serde(default)]
    pub initialized: bool,
}

impl Seen {
    #[cfg(test)]
    pub fn observe(&mut self, repo: &Repo, prs: &[Value]) -> Vec<Job> {
        self.observe_with_preferences(
            repo,
            prs,
            &super::dashboard::Preferences {
                default_trigger: Trigger::Auto,
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
                    pushed_at: if retry_index {
                        String::new()
                    } else {
                        repo.pushed_at.clone()
                    },
                    heads,
                    initialized: true,
                },
            );
            // Setup-skipped PRs are not part of the initial no-backfill baseline.
        }
        let known = self.0.get_mut(&repo.id).expect("initialized repository");
        known.repo = repo.clone();
        let mut jobs = Vec::new();
        if preferences.trigger(&repo.full_name) != Trigger::Off
            && (known.pushed_at != repo.pushed_at || retry_index)
        {
            jobs.push(Job {
                id: uuid::Uuid::new_v4().simple().to_string(),
                trigger: "reconcile".into(),
                mode: Mode::Standard,
                repo: repo.clone(),
                kind: Kind::Index,
            });
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

    pub fn finished(&mut self, job: &Job) {
        let known = self.0.entry(job.repo.id).or_insert_with(|| SeenRepo {
            repo: job.repo.clone(),
            pushed_at: String::new(),
            heads: BTreeMap::new(),
            initialized: false,
        });
        match &job.kind {
            Kind::Review { number, .. } => {
                known.heads.insert(*number, job.head().into());
            }
            Kind::Index => known.pushed_at.clone_from(&job.repo.pushed_at),
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
    server.recover_checks().await?;
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
    let installations = server
        .api
        .list("/app/installations", &app.jwt()?, None)
        .await?;
    let mut inventory = BTreeMap::new();
    let mut installed = 0;
    for installation in installations {
        let owner = installation["account"]["login"].as_str().unwrap_or("");
        if !server.allowed_owner(&app, owner).await {
            tracing::warn!("ignored installation outside the allowed owners");
            continue;
        }
        let Some(id) = installation["id"].as_u64() else {
            continue;
        };
        installed += 1;
        let token = server
            .api
            .installation_token(&app, id, None, super::app::Permissions::ReadPulls)
            .await?;
        let repos = server
            .api
            .list("/installation/repositories", &token, Some("repositories"))
            .await?;
        for value in repos {
            let repo = Repo::parse(&value, id)?;
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
                    dashboard.index_retry_at(&repo.full_name),
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
            };
            inventory.insert(repo.full_name.clone(), info.clone());
            server
                .dashboard
                .lock()
                .await
                .repos
                .insert(repo.full_name.clone(), info);
            for job in jobs {
                // Both an unadvanced push checkpoint and a failed legacy job can
                // request an index. Neither may bypass reconcile retry backoff.
                if matches!(job.kind, Kind::Index)
                    && retry_at.is_some_and(|at| at > chrono::Utc::now())
                {
                    continue;
                }
                if !server.queue.lock().await.contains(job.key()) {
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
    dashboard.github = serde_json::json!({"ok": true});
    let _ = server.events.send(("repos", serde_json::json!({})));
    Ok(())
}

pub(super) async fn tick(server: &Server) -> Result<()> {
    super::api::test_untested(server).await;
    once(server).await
}

pub async fn loop_forever(server: Arc<Server>) {
    let mut interval =
        tokio::time::interval(Duration::from_secs(server.settings.reconcile_minutes * 60));
    loop {
        interval.tick().await;
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
