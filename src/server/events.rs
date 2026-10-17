use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    pub id: u64,
    pub installation: u64,
    pub full_name: String,
    pub default_branch: String,
    pub pushed_at: String,
}

impl Repo {
    pub fn parse(v: &Value, installation: u64) -> Result<Self> {
        let full_name = v["full_name"]
            .as_str()
            .context("repository missing full_name")?;
        let parts: Vec<_> = full_name.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || *part == "."
                    || *part == ".."
                    || !part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            })
        {
            bail!("invalid repository full_name");
        }
        Ok(Self {
            id: v["id"].as_u64().context("repository missing id")?,
            installation,
            full_name: full_name.into(),
            default_branch: v["default_branch"].as_str().unwrap_or("main").into(),
            pushed_at: pushed_at(&v["pushed_at"]),
        })
    }
}

/// REST responses give `pushed_at` as an RFC 3339 string, push webhooks as
/// Unix seconds; normalize so the two compare equal.
fn pushed_at(value: &Value) -> String {
    match value {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|t| t.timestamp().to_string())
            .unwrap_or_else(|_| s.clone()),
        Value::Number(n) => n.as_i64().map_or_else(|| n.to_string(), |t| t.to_string()),
        _ => String::new(),
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Review { number: u64, pr: Value, force: bool },
    Index,
    Remove,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub trigger: String,
    pub repo: Repo,
    pub kind: Kind,
}

impl Job {
    pub fn new(repo: Repo, kind: Kind, trigger: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            trigger: trigger.into(),
            repo,
            kind,
        }
    }
    pub fn key(&self) -> (u64, u64) {
        (
            self.repo.id,
            match &self.kind {
                Kind::Review { number, .. } => *number,
                _ => 0,
            },
        )
    }
    pub fn head(&self) -> &str {
        match &self.kind {
            Kind::Review { pr, .. } => pr["head"]["sha"].as_str().unwrap_or(""),
            _ => "",
        }
    }
    pub fn event(&self) -> Value {
        match &self.kind {
            Kind::Review { number, pr, .. } => {
                json!({
                    "repository": {"full_name": self.repo.full_name},
                    "number": number,
                    "pull_request": pr
                })
            }
            _ => Value::Null,
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Job(Job),
    Close(u64, u64),
    Rerequest(Repo, u64),
    RemoveInstallation(u64),
    RemoveRepos(Vec<Repo>),
    Ignore,
}

#[cfg(test)]
pub fn map(event: &str, v: &Value, app_id: u64) -> Result<Event> {
    map_with_preferences(event, v, app_id, &super::dashboard::Preferences::default())
}

pub fn map_with_preferences(
    event: &str,
    v: &Value,
    app_id: u64,
    preferences: &super::dashboard::Preferences,
) -> Result<Event> {
    let installation = v["installation"]["id"]
        .as_u64()
        .context("event missing installation")?;
    let action = v["action"].as_str().unwrap_or("");
    if event == "installation" && action == "deleted" {
        return Ok(Event::RemoveInstallation(installation));
    }
    if event == "installation_repositories" && action == "removed" {
        let repos = v["repositories_removed"]
            .as_array()
            .context("event missing removed repositories")?
            .iter()
            .map(|r| Repo::parse(r, installation))
            .collect::<Result<_>>()?;
        return Ok(Event::RemoveRepos(repos));
    }
    if !matches!(event, "pull_request" | "push" | "check_run") {
        return Ok(Event::Ignore);
    }
    let repo = Repo::parse(&v["repository"], installation)?;
    match event {
        "pull_request" => {
            let number = v["number"].as_u64().context("event missing PR number")?;
            if action == "closed" {
                return Ok(Event::Close(repo.id, number));
            }
            if matches!(
                action,
                "opened" | "reopened" | "synchronize" | "ready_for_review"
            ) && preferences.reviewable(&v["pull_request"])
            {
                return Ok(Event::Job(Job {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    trigger: "webhook".into(),
                    repo,
                    kind: Kind::Review {
                        number,
                        pr: v["pull_request"].clone(),
                        force: false,
                    },
                }));
            }
        }
        "push"
            if v["ref"].as_str() == Some(&format!("refs/heads/{}", repo.default_branch))
                && v["deleted"].as_bool() != Some(true) =>
        {
            return Ok(Event::Job(Job {
                id: uuid::Uuid::new_v4().simple().to_string(),
                trigger: "webhook".into(),
                repo,
                kind: Kind::Index,
            }));
        }
        "check_run"
            if action == "rerequested"
                && v["check_run"]["name"].as_str() == Some("Revera")
                && v["check_run"]["app"]["id"].as_u64() == Some(app_id) =>
        {
            let number = v["check_run"]["external_id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .context("check missing PR external_id")?;
            return Ok(Event::Rerequest(repo, number));
        }
        _ => {}
    }
    Ok(Event::Ignore)
}
