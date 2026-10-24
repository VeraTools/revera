//! Admin-only, read-only preview of guidance at the default branch.
use super::{Server, app, events::Repo};
use crate::guidance::{self, Skill};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone)]
struct Candidate {
    sha: String,
    bytes: u64,
}

#[derive(Clone)]
struct Snapshot {
    response: Value,
    candidates: BTreeMap<String, Candidate>,
}

#[derive(Default)]
pub(super) struct Cache(VecDeque<(u64, String, Snapshot)>);

struct Error(StatusCode, String);
impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(error: E) -> Self {
        Self(
            StatusCode::BAD_GATEWAY,
            crate::redact::diagnostic(&format!("{:#}", error.into())),
        )
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": crate::redact::text(&self.1)}))).into_response()
    }
}
fn missing() -> Error {
    Error(StatusCode::NOT_FOUND, "not found".into())
}

pub(super) fn router() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/repos/{owner}/{repo}/guidance", get(preview))
        .route("/api/repos/{owner}/{repo}/guidance/file", get(file))
}

async fn repository(server: &Server, owner: &str, name: &str) -> Result<Repo, Error> {
    server
        .dashboard
        .lock()
        .await
        .repos
        .get(&format!("{owner}/{name}"))
        .filter(|r| r.repo.installation.is_some())
        .map(|r| r.repo.clone())
        .ok_or_else(missing)
}

async fn token(server: &Server, repo: &Repo) -> Result<String> {
    let app = server
        .setup
        .lock()
        .await
        .app
        .clone()
        .context("app not configured")?;
    server
        .api
        .installation_token(
            &app,
            repo.installation_id()?,
            Some(repo.id),
            app::Permissions::ReadPulls,
        )
        .await
}

async fn blob(server: &Server, repo: &Repo, token: &str, sha: &str, cap: usize) -> Result<String> {
    anyhow::ensure!(crate::git::is_oid(sha), "invalid blob identity");
    let bytes = server
        .api
        .blob_prefix(
            &format!("/repos/{}/git/blobs/{sha}", repo.full_name),
            token,
            cap,
        )
        .await?;
    // A byte cap can split the final UTF-8 scalar.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Skill files read for one preview; duplicates by name still leave room
/// for a full index.
const MAX_SKILL_FETCHES: usize = 2 * guidance::MAX_SKILLS;

async fn snapshot(server: &Server, repo: &Repo, token: &str) -> Result<Snapshot> {
    let branch: String =
        url::form_urlencoded::byte_serialize(repo.default_branch.as_bytes()).collect();
    let tree = server
        .api
        .request(
            reqwest::Method::GET,
            &format!("/repos/{}/git/trees/{branch}?recursive=1", repo.full_name),
            token,
            None,
        )
        .await?;
    let sha = tree["sha"].as_str().context("tree identity missing")?;
    if let Some((_, _, cached)) = server
        .guidance_cache
        .lock()
        .await
        .0
        .iter()
        .find(|(id, key, _)| *id == repo.id && key == sha)
    {
        return Ok(cached.clone());
    }
    let mut guidance_files = BTreeMap::new();
    let mut skill_files = BTreeMap::new();
    for entry in tree["tree"].as_array().context("tree entries missing")? {
        if entry["type"] != "blob" || !matches!(entry["mode"].as_str(), Some("100644" | "100755")) {
            continue;
        }
        let Some(path) = entry["path"].as_str() else {
            continue;
        };
        let Some(sha) = entry["sha"].as_str().filter(|s| crate::git::is_oid(s)) else {
            continue;
        };
        let candidate = Candidate {
            sha: sha.into(),
            bytes: entry["size"].as_u64().unwrap_or(0),
        };
        if guidance::guidance_kind(path).is_some() {
            guidance_files.insert(path.to_owned(), candidate.clone());
        }
        if guidance::skill_path(path).is_some() {
            skill_files.insert(path.to_owned(), candidate);
        }
    }
    let mut candidates: BTreeMap<_, _> = guidance_files.into_iter().take(64).collect();
    let files: Vec<_> = candidates
        .iter()
        .map(|(path, file)| {
            json!({
                "path": path, "dir": path.rsplit_once('/').map_or("", |(dir, _)| dir),
                "kind": guidance::guidance_kind(path), "bytes": file.bytes
            })
        })
        .collect();
    // one blob request per skill: fetch only the candidates the index could
    // use, in the loader's order, and count the rest as omitted
    let mut ordered: Vec<_> = skill_files.into_iter().collect();
    ordered.sort_by_key(|(path, _)| (guidance::skill_path(path).map(|(r, _)| r), path.clone()));
    let unfetched = ordered.len().saturating_sub(MAX_SKILL_FETCHES);
    ordered.truncate(MAX_SKILL_FETCHES);
    let skill_files: BTreeMap<_, _> = ordered.into_iter().collect();
    let parsed: Vec<Skill> = stream::iter(skill_files.clone())
        .map(|(path, file)| async move {
            let text = blob(server, repo, token, &file.sha, guidance::FRONT_MATTER_BYTES).await?;
            Ok::<_, anyhow::Error>(guidance::parse_skill(&path, &text))
        })
        .buffer_unordered(6)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_>>()?;
    let (skills, omitted) = guidance::select_skills(parsed);
    let omitted = omitted + unfetched;
    let skills: Vec<_> = skills.iter().map(|skill| {
        let file = skill_files[&skill.path].clone();
        let value = json!({"name": skill.name, "path": skill.path, "description": skill.description, "bytes": file.bytes});
        candidates.insert(skill.path.clone(), file);
        value
    }).collect();
    let mut response = json!({
        "full_name": repo.full_name, "default_branch": repo.default_branch, "tree": sha,
        "truncated_tree": tree["truncated"].as_bool().unwrap_or(false),
        "files": files, "skills": skills, "skills_omitted": omitted
    });
    crate::redact::json(&mut response);
    let snapshot = Snapshot {
        response,
        candidates,
    };
    let mut cache = server.guidance_cache.lock().await;
    cache.0.retain(|(id, key, _)| *id != repo.id || key != sha);
    cache
        .0
        .push_back((repo.id, sha.to_owned(), snapshot.clone()));
    while cache.0.len() > 32 {
        cache.0.pop_front();
    }
    Ok(snapshot)
}

async fn preview(
    State(server): State<Arc<Server>>,
    Path((owner, name)): Path<(String, String)>,
) -> Result<Json<Value>, Error> {
    let repo = repository(&server, &owner, &name).await?;
    let token = token(&server, &repo).await?;
    let mut value = snapshot(&server, &repo, &token).await?.response;
    value["full_name"] = json!(repo.full_name);
    value["default_branch"] = json!(repo.default_branch);
    let global = match server.config_yaml()? {
        Some(yaml) => crate::config::Config::parse(&yaml, "server config")?.review,
        None => crate::config::ReviewConfig::default(),
    };
    let pref = server
        .dashboard
        .lock()
        .await
        .preferences
        .repos
        .get(&repo.full_name)
        .cloned()
        .unwrap_or_default();
    value["global"] =
        json!({"guidance": global.guidance, "notes_chars": global.notes.chars().count()});
    value["repo"] = json!({"guidance": pref.guidance, "notes": pref.notes});
    value["effective_guidance"] = json!(pref.guidance.unwrap_or(global.guidance));
    value["max_bytes"] = json!(global.guidance_max_bytes);
    // Preferences/config are overlaid on cached tree content so PATCH is visible immediately.
    crate::redact::json(&mut value);
    Ok(Json(value))
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

async fn file(
    State(server): State<Arc<Server>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<FileQuery>,
) -> Result<Response, Error> {
    // Reject plainly non-candidate paths before acquiring credentials or querying GitHub.
    if guidance::guidance_kind(&query.path).is_none() && guidance::skill_path(&query.path).is_none()
    {
        return Err(missing());
    }
    let repo = repository(&server, &owner, &name).await?;
    let token = token(&server, &repo).await?;
    let snapshot = snapshot(&server, &repo, &token).await?;
    let candidate = snapshot.candidates.get(&query.path).ok_or_else(missing)?;
    let text = blob(&server, &repo, &token, &candidate.sha, 256 * 1024).await?;
    let text = crate::text::excerpt_bytes(&crate::redact::text(&text), 256 * 1024);
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        text,
    )
        .into_response())
}
