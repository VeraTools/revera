//! Operator-owned playbook files; all edits validate a complete in-memory view.
use super::{Server, api::Error};
use crate::playbook::{self, Overrides, Playbook};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path as FilePath, PathBuf},
    sync::Arc,
};

type Response = std::result::Result<Json<Value>, Error>;

pub fn router() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/playbook", get(read))
        .route(
            "/api/playbook/reviewer",
            put(put_reviewer).delete(delete_reviewer),
        )
        .route(
            "/api/playbook/skills/{name}",
            put(put_skill).delete(delete_skill),
        )
}

fn invalid(error: impl std::fmt::Display) -> Error {
    Error(StatusCode::BAD_REQUEST, error.to_string())
}

/// Unlike a CLI operator path, this server-owned directory must not be a link.
fn files(server: &Server, omit: Option<&FilePath>) -> Result<Overrides> {
    let dir = server.settings.data_dir.join("playbook");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Overrides::load(&dir, omit),
        Ok(_) => bail!("{} must be a real directory, not a symlink", dir.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Overrides::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", dir.display())),
    }
}

fn view(files: &Overrides, book: &Playbook, error: Option<String>) -> Json<Value> {
    let skills: Vec<_> = playbook::BUILTIN_SKILLS.iter().map(|(name, builtin)| (*name, Some(*builtin)))
        .chain(files.skills.keys().filter(|name| !playbook::BUILTIN_SKILLS.iter().any(|(n, _)| n == name))
            .map(|name| (name.as_str(), None)))
        .map(|(name, builtin)| {
            let override_text = files.skills.get(name);
            let effective = override_text.map(String::as_str).or(builtin).unwrap_or_default();
            let metadata = playbook::validate_skill(name, effective).ok();
            json!({"name":name,"description":metadata.as_ref().map(|s| s.description.as_str()).unwrap_or_default(),
                "builtin":builtin,"override":override_text,"effective":effective,
                "enabled":metadata.as_ref().is_some_and(|s| s.enabled),"custom":builtin.is_none()})
        }).collect();
    let mut value = json!({
        "limits":{"reviewer_max_bytes":playbook::REVIEWER_MAX_BYTES,"skill_max_bytes":playbook::SKILL_MAX_BYTES,
            "total_max_bytes":playbook::TOTAL_MAX_BYTES,"max_skills":playbook::MAX_SKILLS},
        "reviewer":{"builtin":playbook::BUILTIN_REVIEWER,"override":files.reviewer,
            "effective":files.reviewer.as_deref().unwrap_or(playbook::BUILTIN_REVIEWER)},
        "skills":skills,"total_bytes":book.total_bytes(),"digest":book.digest,
    });
    if let Some(error) = error {
        value["error"] = json!(error);
    }
    crate::redact::json(&mut value);
    Json(value)
}

async fn read(State(server): State<Arc<Server>>) -> Json<Value> {
    let _guard = server.playbook.lock().await;
    match files(&server, None) {
        Ok(files) => match Playbook::from_overrides(&files) {
            Ok(book) => view(&files, &book, None),
            Err(error) => view(&files, &Playbook::builtin(), Some(format!("{error:#}"))),
        },
        Err(error) => view(
            &Overrides::default(),
            &Playbook::builtin(),
            Some(format!("{error:#}")),
        ),
    }
}

#[derive(Deserialize)]
struct Content {
    content: String,
}

async fn put_reviewer(
    State(server): State<Arc<Server>>,
    input: std::result::Result<Json<Content>, JsonRejection>,
) -> Response {
    change(
        &server,
        None,
        Some(input.map_err(|e| invalid(e.body_text()))?.0.content),
    )
    .await
}
async fn delete_reviewer(State(server): State<Arc<Server>>) -> Response {
    change(&server, None, None).await
}
async fn put_skill(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    input: std::result::Result<Json<Content>, JsonRejection>,
) -> Response {
    change(
        &server,
        Some(&name),
        Some(input.map_err(|e| invalid(e.body_text()))?.0.content),
    )
    .await
}
async fn delete_skill(State(server): State<Arc<Server>>, Path(name): Path<String>) -> Response {
    change(&server, Some(&name), None).await
}

async fn change(server: &Server, name: Option<&str>, content: Option<String>) -> Response {
    if name.is_some_and(|name| !playbook::valid_name(name)) {
        return Err(invalid("invalid skill name"));
    }
    if let Some(text) = &content {
        if crate::redact::text(text) != text.as_str() {
            return Err(invalid("content contains a registered secret"));
        }
        match name {
            Some(name) => {
                playbook::validate_skill(name, text).map_err(|e| invalid(format!("{e:#}")))?;
            }
            None => playbook::validate_reviewer(text).map_err(invalid)?,
        }
    }
    let _guard = server.playbook.lock().await;
    let relative = name
        .map(|name| PathBuf::from("skills").join(format!("{name}.md")))
        .unwrap_or_else(|| "reviewer.md".into());
    let target = server.settings.data_dir.join("playbook").join(&relative);
    let builtin = name.map_or(Some(playbook::BUILTIN_REVIEWER), |name| {
        playbook::BUILTIN_SKILLS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, text)| *text)
    });
    if content.is_none() && builtin.is_none() && !target.try_exists()? {
        return Err(Error(StatusCode::NOT_FOUND, "skill not found".into()));
    }
    let mut files = files(server, Some(&relative)).map_err(|e| invalid(format!("{e:#}")))?;
    let content = content.filter(|text| Some(text.as_str()) != builtin);
    match name {
        Some(name) => {
            if let Some(text) = &content {
                files.skills.insert(name.into(), text.clone());
            }
        }
        None => files.reviewer.clone_from(&content),
    }
    let book = Playbook::from_overrides(&files).map_err(|e| invalid(format!("{e:#}")))?;
    let relative = FilePath::new("playbook").join(relative);
    if let Some(content) = content {
        crate::fsutil::write_repo_file(&server.settings.data_dir, &relative, content.as_bytes())?;
    } else if std::fs::symlink_metadata(&target).is_ok() {
        crate::fsutil::ensure_dir(&server.settings.data_dir, relative.parent().unwrap())?;
        crate::fsutil::check_regular_target(&target)?;
        std::fs::remove_file(&target)?;
    }
    Ok(view(&files, &book, None))
}
