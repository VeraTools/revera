//! Installed pull-request inbox and bounded GitHub conversation snapshots.
use super::{
    Server, api, app,
    dashboard::JobSummary,
    events::{Job, Repo, Source},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
pub(super) struct PullSummary {
    repo: String,
    repo_id: u64,
    number: u64,
    title: String,
    author: String,
    author_is_bot: bool,
    draft: bool,
    head_sha: String,
    head_ref: String,
    base_ref: String,
    fork: bool,
    created_at: String,
    updated_at: String,
    url: String,
    labels: Vec<String>,
}

fn text(value: &Value) -> String {
    crate::redact::text(value.as_str().unwrap_or("")).into_owned()
}

impl PullSummary {
    fn parse(repo: &Repo, pr: &Value) -> Option<Self> {
        let head_repo = &pr["head"]["repo"];
        let base_repo = &pr["base"]["repo"];
        let fork = match (head_repo["id"].as_u64(), base_repo["id"].as_u64()) {
            (Some(head), Some(base)) => head != base,
            _ => head_repo["full_name"].as_str().is_some_and(|head| {
                head != base_repo["full_name"].as_str().unwrap_or(&repo.full_name)
            }),
        };
        Some(Self {
            repo: repo.full_name.clone(),
            repo_id: repo.id,
            number: pr["number"].as_u64()?,
            title: text(&pr["title"]),
            author: text(&pr["user"]["login"]),
            author_is_bot: pr["user"]["type"] == "Bot",
            draft: pr["draft"].as_bool().unwrap_or(false),
            head_sha: text(&pr["head"]["sha"]),
            head_ref: text(&pr["head"]["ref"]),
            base_ref: text(&pr["base"]["ref"]),
            fork,
            created_at: text(&pr["created_at"]),
            updated_at: text(&pr["updated_at"]),
            url: text(&pr["html_url"]),
            labels: nodes_array(&pr["labels"])
                .iter()
                .map(|label| text(&label["name"]))
                .collect(),
        })
    }
}

struct RepoPulls {
    pulls: Vec<PullSummary>,
    refreshed_at: String,
}

#[derive(Default)]
pub(super) struct Cache {
    repos: BTreeMap<u64, RepoPulls>,
    // Only GitHub data is cached: jobs, fixes and policy are joined live on each GET.
    details: BTreeMap<(u64, u64), (Instant, Value)>,
}

impl Cache {
    fn detail(&mut self, key: (u64, u64)) -> Option<Value> {
        self.details
            .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(20));
        self.details.get(&key).map(|(_, value)| value.clone())
    }

    fn refreshed_at(&self, ids: &BTreeSet<u64>) -> Option<&str> {
        ids.iter()
            .map(|id| self.repos.get(id).map(|r| r.refreshed_at.as_str()))
            .collect::<Option<Vec<_>>>()
            .and_then(|times| times.into_iter().min())
    }

    pub(super) fn listing(&mut self, repo: &Repo, prs: &[Value]) {
        self.repos.insert(
            repo.id,
            RepoPulls {
                pulls: prs
                    .iter()
                    .filter(|pr| pr["state"] != "closed")
                    .filter_map(|pr| PullSummary::parse(repo, pr))
                    .collect(),
                refreshed_at: chrono::Utc::now().to_rfc3339(),
            },
        );
    }

    pub(super) fn webhook(&mut self, payload: &Value) {
        let action = payload["action"].as_str().unwrap_or("");
        if !matches!(
            action,
            "opened"
                | "reopened"
                | "edited"
                | "synchronize"
                | "ready_for_review"
                | "converted_to_draft"
                | "closed"
        ) {
            return;
        }
        let Ok(repo) = Repo::parse(
            &payload["repository"],
            payload["installation"]["id"].as_u64(),
        ) else {
            return;
        };
        let Some(pull) = PullSummary::parse(&repo, &payload["pull_request"]) else {
            return;
        };
        self.details.remove(&(repo.id, pull.number));
        // an unlisted repository waits for its first full listing; one
        // upserted PR would otherwise pass for the whole open set
        let Some(cached) = self.repos.get_mut(&repo.id) else {
            return;
        };
        cached.pulls.retain(|pr| pr.number != pull.number);
        if action != "closed" && payload["pull_request"]["state"] != "closed" {
            cached.pulls.push(pull);
        }
        cached.refreshed_at = chrono::Utc::now().to_rfc3339();
    }

    pub(super) fn remove_repo(&mut self, repo_id: u64) {
        self.repos.remove(&repo_id);
        self.details.retain(|(id, _), _| *id != repo_id);
    }

    pub(super) fn retain_repos(&mut self, ids: &BTreeSet<u64>) {
        self.repos.retain(|id, _| ids.contains(id));
        self.details.retain(|(id, _), _| ids.contains(id));
    }
}

fn output(mut value: Value) -> Json<Value> {
    crate::redact::json(&mut value);
    Json(value)
}
fn missing() -> api::Error {
    api::Error(StatusCode::NOT_FOUND, "not found".into())
}
fn github_error() -> api::Error {
    // Never expose GraphQL error bodies, token responses or upstream diagnostics.
    api::Error(
        StatusCode::BAD_GATEWAY,
        "GitHub pull request unavailable".into(),
    )
}

async fn allowed(server: &Server, app: &app::App, repo: &Repo) -> bool {
    if repo.source() != Source::Installed
        || !server
            .allowed_owner(app, repo.full_name.split('/').next().unwrap_or(""))
            .await
    {
        return false;
    }
    let installations = server.installations.lock().await;
    // Before initial discovery, the persisted inventory is the known installation set.
    installations.install_url.is_empty()
        || installations
            .installations
            .iter()
            .any(|i| i.id == repo.installation && i.allowed && !i.suspended)
}

fn jobs_for<'a>(
    jobs: impl Iterator<Item = &'a JobSummary>,
    repo_id: u64,
    number: u64,
) -> Vec<JobSummary> {
    let mut jobs: Vec<_> = jobs
        .filter(|j| {
            j.source == Source::Installed && j.repo_id == Some(repo_id) && j.pr == Some(number)
        })
        .cloned()
        .collect();
    jobs.sort_by(|a, b| b.queued_at.cmp(&a.queued_at).then_with(|| b.id.cmp(&a.id)));
    jobs
}

fn review(jobs: &[JobSummary], head: &str) -> Value {
    let reviews: Vec<_> = jobs.iter().filter(|job| job.kind == "review").collect();
    let latest = reviews.first().map(|j| {
        json!({
            "id": j.id, "status": j.status, "mode": j.mode, "head_sha": j.head_sha,
            "findings": j.findings, "finished_at": j.finished_at, "queued_at": j.queued_at,
        })
    });
    let active = reviews.iter().find(|job| !job.terminal());
    let state = if active.is_some() {
        "reviewing"
    } else if let Some(job) = reviews.first() {
        if job.head_sha.as_deref() != Some(head) {
            "outdated"
        } else {
            match job.status.as_str() {
                "failed" | "cancelled" => "failed",
                "partial" => "partial",
                "complete" if job.findings.total > 0 => "findings",
                "complete" => "clean",
                "skipped" => "skipped",
                _ => "failed",
            }
        }
    } else {
        "not_reviewed"
    };
    json!({"latest": latest, "state": state, "active_job": active.map(|j| &j.id)})
}

pub(super) async fn inbox(State(server): State<Arc<Server>>) -> Json<Value> {
    let Some(app) = server.setup.lock().await.app.clone() else {
        return output(json!({"refreshed_at": null, "pulls": []}));
    };
    let (repos, preferences, jobs) = {
        let dashboard = server.dashboard.lock().await;
        (
            dashboard
                .repos
                .values()
                .map(|info| info.repo.clone())
                .collect::<Vec<_>>(),
            dashboard.preferences.clone(),
            dashboard.jobs.values().cloned().collect::<Vec<_>>(),
        )
    };
    let mut ids = BTreeSet::new();
    for repo in repos {
        if allowed(&server, &app, &repo).await {
            ids.insert(repo.id);
        }
    }
    let cache = server.pulls.lock().unwrap();
    let refreshed_at = cache.refreshed_at(&ids);
    let mut pulls: Vec<_> = cache
        .repos
        .iter()
        .filter(|(id, _)| ids.contains(id))
        .flat_map(|(_, repo)| repo.pulls.iter())
        .collect();
    pulls.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.number.cmp(&b.number))
    });
    let pulls: Vec<_> = pulls
        .into_iter()
        .map(|pull| {
            let mut value = json!(pull);
            value["trigger"] = json!(preferences.trigger(&pull.repo));
            value["mode"] = json!(preferences.mode(&pull.repo));
            value["review"] = review(
                &jobs_for(jobs.iter(), pull.repo_id, pull.number),
                &pull.head_sha,
            );
            value
        })
        .collect();
    output(json!({"refreshed_at": refreshed_at, "pulls": pulls}))
}

const DETAIL_QUERY: &str = r#"
query PullDetail($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      number title body url state isDraft merged isCrossRepository createdAt updatedAt headRefName
      baseRefName headRefOid additions deletions changedFiles reviewDecision
      author { login __typename }
      labels(first: 20) { totalCount nodes { name color } }
      comments(first: 100) { totalCount nodes { id author { login __typename } body createdAt url } }
      reviews(first: 50) { totalCount nodes { id author { login __typename } state body submittedAt url } }
      reviewThreads(first: 100) { totalCount nodes {
        id isResolved isOutdated path line originalLine
        comments(first: 50) { totalCount nodes {
          id author { login __typename } body createdAt url diffHunk pullRequestReview { id }
        } }
      } }
    }
  }
}
"#;

fn nodes_array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn nodes(connection: &Value) -> &[Value] {
    nodes_array(&connection["nodes"])
}
fn truncated(connection: &Value) -> bool {
    connection["totalCount"].as_u64().unwrap_or(0) > nodes(connection).len() as u64
}
fn author(value: &Value, bot: &str) -> Value {
    json!({"login": value["login"], "bot": value["__typename"] == "Bot",
        // GraphQL names a Bot actor by its slug; REST appends `[bot]`
        "revera": value["login"].as_str().is_some_and(|l| l == bot || l.strip_suffix("[bot]") == Some(bot))})
}
fn comment(value: &Value, bot: &str) -> Value {
    json!({"id": value["id"], "author": author(&value["author"], bot),
        "body": super::runner::strip_state(value["body"].as_str().unwrap_or("")),
        "created_at": value["createdAt"], "url": value["url"]})
}
fn map_detail(pr: &Value, repo: &Repo, bot: &str) -> Value {
    let threads: Vec<_> = nodes(&pr["reviewThreads"]).iter().map(|thread| {
        let first = nodes(&thread["comments"]).first();
        let hunk = first.and_then(|c| c["diffHunk"].as_str()).unwrap_or("");
        let lines: Vec<_> = hunk.lines().collect();
        let hunk = lines[lines.len().saturating_sub(12)..].join("\n");
        json!({"id": thread["id"], "is_resolved": thread["isResolved"], "is_outdated": thread["isOutdated"],
            "path": thread["path"], "line": thread["line"], "original_line": thread["originalLine"],
            "diff_hunk": crate::redact::text(&hunk),
            "review_id": first.map(|c| c["pullRequestReview"]["id"].clone()),
            "comments": nodes(&thread["comments"]).iter().map(|c| comment(c, bot)).collect::<Vec<_>>(),
            "truncated": truncated(&thread["comments"])})
    }).collect();
    json!({"repo": repo.full_name, "repo_id": repo.id,
        "number": pr["number"], "title": pr["title"],
        "body": super::runner::strip_state(pr["body"].as_str().unwrap_or("")),
        "url": pr["url"], "state": pr["state"].as_str().unwrap_or("").to_ascii_lowercase(),
        "draft": pr["isDraft"], "merged": pr["merged"], "fork": pr["isCrossRepository"].as_bool().unwrap_or(false),
        "created_at": pr["createdAt"], "updated_at": pr["updatedAt"],
        "head_ref": pr["headRefName"], "base_ref": pr["baseRefName"], "head_sha": pr["headRefOid"],
        "additions": pr["additions"], "deletions": pr["deletions"], "changed_files": pr["changedFiles"],
        "review_decision": pr["reviewDecision"], "author": author(&pr["author"], bot),
        "labels": nodes(&pr["labels"]).iter().map(|l| json!({"name": l["name"], "color": l["color"]})).collect::<Vec<_>>(),
        "comments": nodes(&pr["comments"]).iter().map(|c| comment(c, bot)).collect::<Vec<_>>(),
        "reviews": nodes(&pr["reviews"]).iter().map(|r| json!({"id": r["id"], "author": author(&r["author"], bot),
            "state": r["state"], "body": super::runner::strip_state(r["body"].as_str().unwrap_or("")),
            "submitted_at": r["submittedAt"], "url": r["url"]})).collect::<Vec<_>>(),
        "threads": threads,
        "truncated": {"labels": truncated(&pr["labels"]), "comments": truncated(&pr["comments"]),
            "reviews": truncated(&pr["reviews"]), "threads": truncated(&pr["reviewThreads"])}})
}

pub(super) async fn detail(
    State(server): State<Arc<Server>>,
    Path((owner, name, number)): Path<(String, String, u64)>,
) -> Result<Json<Value>, api::Error> {
    let repo = api::repo(&server, &format!("{owner}/{name}")).await?;
    let app = server.setup.lock().await.app.clone().ok_or_else(missing)?;
    if !allowed(&server, &app, &repo).await {
        return Err(missing());
    }
    let cached = server.pulls.lock().unwrap().detail((repo.id, number));
    let mut value = if let Some(value) = cached {
        value
    } else {
        let token = server
            .api
            .installation_token(
                &app,
                repo.installation_id()?,
                Some(repo.id),
                app::Permissions::ReadPulls,
            )
            .await
            .map_err(|_| github_error())?;
        let response = server.api.request(reqwest::Method::POST, "/graphql", &token,
            Some(json!({"query": DETAIL_QUERY, "variables": {"owner": owner, "repo": name, "number": number}})))
            .await.map_err(|_| github_error())?;
        if response
            .get("errors")
            .is_some_and(|errors| !nodes_array(errors).is_empty() || !errors.is_array())
        {
            return Err(github_error());
        }
        let pr = &response["data"]["repository"]["pullRequest"];
        if !pr.is_object() {
            return Err(github_error());
        }
        let mut value = map_detail(pr, &repo, &app.slug);
        crate::redact::json(&mut value);
        server
            .pulls
            .lock()
            .unwrap()
            .details
            .insert((repo.id, number), (Instant::now(), value.clone()));
        value
    };
    {
        let dashboard = server.dashboard.lock().await;
        let jobs = jobs_for(dashboard.jobs.values(), repo.id, number);
        value["review"] = review(&jobs, value["head_sha"].as_str().unwrap_or(""));
        value["jobs"] = json!(jobs);
        value["trigger"] = json!(dashboard.preferences.trigger(&repo.full_name));
        value["mode"] = json!(dashboard.preferences.mode(&repo.full_name));
    }
    value["fixes"] = json!(server.fixes.lock().await.for_pull(repo.id, number));
    Ok(output(value))
}

pub(super) async fn enqueue_review(server: &Server, job: Job) -> Result<String, api::Error> {
    let id = job.id.clone();
    // Forced reviews always insert this ID, even behind a same-head running review;
    // they replace (and cancel) a pending review rather than coalescing into its ID.
    server.enqueue_inner(job, None).await;
    // Setup-skipped requests still have a real stored job. Rejected admission or a
    // metadata write failure must not return an ID that the dashboard cannot load.
    if !server.dashboard.lock().await.jobs.contains_key(&id) {
        return Err(api::Error(
            StatusCode::CONFLICT,
            "review could not be queued".into(),
        ));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_cache_expires_after_twenty_seconds_and_repo_eviction_clears_both_caches() {
        let mut cache = Cache::default();
        cache
            .details
            .insert((1, 7), (Instant::now(), json!({"number": 7})));
        cache.details.insert(
            (1, 8),
            (
                Instant::now() - Duration::from_secs(21),
                json!({"number": 8}),
            ),
        );
        cache.repos.insert(
            1,
            RepoPulls {
                pulls: vec![],
                refreshed_at: "2026-10-01T00:00:00Z".into(),
            },
        );
        cache.repos.insert(
            2,
            RepoPulls {
                pulls: vec![],
                refreshed_at: "2026-10-02T00:00:00Z".into(),
            },
        );
        assert_eq!(cache.detail((1, 7)), Some(json!({"number": 7})));
        assert!(!cache.details.contains_key(&(1, 8)));
        assert_eq!(cache.detail((2, 7)), None);
        assert_eq!(
            cache.refreshed_at(&BTreeSet::from([1, 2])),
            Some("2026-10-01T00:00:00Z")
        );
        assert_eq!(
            cache.refreshed_at(&BTreeSet::from([2])),
            Some("2026-10-02T00:00:00Z")
        );
        assert_eq!(cache.refreshed_at(&BTreeSet::from([1, 3])), None);
        assert_eq!(cache.refreshed_at(&BTreeSet::new()), None);
        cache.retain_repos(&BTreeSet::from([2]));
        assert!(cache.details.is_empty());
        assert!(!cache.repos.contains_key(&1));
        cache.remove_repo(2);
        assert!(cache.repos.is_empty());
    }

    #[test]
    fn bodies_strip_state_and_redact_credentials_in_every_conversation_location() {
        let credential = "pull-detail-redaction-test-only-credential";
        crate::redact::register(credential);
        let body = format!("visible {credential}<!-- revera-state:hidden -->");
        let pr = json!({"body": body, "comments": {"totalCount": 1, "nodes": [{"body": body}]},
            "reviews": {"totalCount": 1, "nodes": [{"body": body}]},
            "reviewThreads": {"totalCount": 1, "nodes": [{"comments": {"totalCount": 1, "nodes": [{"body": body, "diffHunk": credential}]}}]}});
        let repo = Repo::parse(&json!({"id": 1, "full_name": "owner/repo"}), 9).unwrap();
        let value = map_detail(&pr, &repo, "revera[bot]");
        assert!(!value.to_string().contains(credential));
        assert!(!value.to_string().contains("revera-state"));
        assert_eq!(value["comments"][0]["body"], value["body"]);
        assert_eq!(value["reviews"][0]["body"], value["body"]);
        assert_eq!(value["threads"][0]["comments"][0]["body"], value["body"]);
        assert_eq!(
            value["truncated"],
            json!({"labels": false, "comments": false, "reviews": false, "threads": false})
        );
    }
}
