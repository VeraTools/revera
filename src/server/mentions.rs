use super::{
    Server, app,
    dashboard::{Mode, Preferences, Trigger},
    events::{Job, Kind, Repo},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Review(Option<Mode>),
    Help,
}

#[derive(Debug)]
pub struct Request {
    pub repo: Repo,
    pub number: u64,
    pub comment: u64,
    pub command: Command,
}

/// Only ordinary text can issue commands, not quotations or Markdown code.
pub fn command(body: &str, slug: &str) -> Option<Command> {
    let mention = format!("@{}", slug.to_ascii_lowercase());
    let mut fence = None;
    let mut inline = 0;
    for line in body.lines() {
        let text = line.trim_start();
        if text.starts_with('>') {
            continue;
        }
        let marker = text.chars().next().unwrap_or(' ');
        let count = text.chars().take_while(|c| *c == marker).count();
        if let Some((opening, length)) = fence {
            if marker == opening && count >= length && text[count..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if line.starts_with('\t') || line.len() - text.len() >= 4 {
            continue;
        }
        if matches!(marker, '`' | '~') && count >= 3 {
            fence = Some((marker, count));
            continue;
        }
        let mut plain = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '`' {
                let mut ticks = 1;
                while chars.peek() == Some(&'`') {
                    chars.next();
                    ticks += 1;
                }
                if inline == 0 {
                    inline = ticks;
                } else if inline == ticks {
                    inline = 0;
                }
                plain.push(' ');
            } else if inline == 0 {
                plain.push(c);
            }
        }
        let lower = plain.to_ascii_lowercase();
        for (start, _) in lower.match_indices(&mention) {
            let before = lower[..start].chars().next_back();
            let rest = &lower[start + mention.len()..];
            let identifier = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '@');
            if before.is_some_and(identifier) || rest.chars().next().is_some_and(identifier) {
                continue;
            }
            // Read the command word and an optional mode, so trailing prose or
            // punctuation ("review this, please.") still works.
            let words: Vec<_> = rest
                .split_whitespace()
                .take(2)
                .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
                .collect();
            let mode = |word: Option<&&str>| match word.copied() {
                Some("quick") => Some(Mode::Quick),
                Some("deep") => Some(Mode::Deep),
                Some("standard") => Some(Mode::Standard),
                _ => None,
            };
            return Some(match words.first().copied() {
                Some("review") => Command::Review(mode(words.get(1))),
                Some("quick") => Command::Review(Some(Mode::Quick)),
                Some("deep") => Command::Review(Some(Mode::Deep)),
                _ => Command::Help,
            });
        }
    }
    None
}

pub fn event(value: &Value, slug: &str, preferences: &Preferences) -> Result<Option<Request>> {
    let comment = &value["comment"];
    let login = comment["user"]["login"].as_str().unwrap_or("");
    if value["action"] != "created"
        || !value["issue"]["pull_request"].is_object()
        || login.is_empty()
        || comment["user"]["type"]
            .as_str()
            .is_some_and(|t| t.eq_ignore_ascii_case("bot"))
        || login.to_ascii_lowercase().ends_with("[bot]")
        || !matches!(
            comment["author_association"].as_str(),
            Some("OWNER" | "MEMBER" | "COLLABORATOR")
        )
    {
        return Ok(None);
    }
    let Some(command) = command(comment["body"].as_str().unwrap_or(""), slug) else {
        return Ok(None);
    };
    let installation = value["installation"]["id"]
        .as_u64()
        .context("missing installation")?;
    let repo = Repo::parse(&value["repository"], installation)?;
    if preferences.trigger(&repo.full_name) == Trigger::Off {
        return Ok(None);
    }
    Ok(Some(Request {
        repo,
        number: value["issue"]["number"]
            .as_u64()
            .filter(|n| *n > 0)
            .context("missing PR number")?,
        comment: comment["id"].as_u64().context("missing comment ID")?,
        command,
    }))
}

pub async fn handle(server: &Server, app: &app::App, request: Request) -> Result<()> {
    let mode = {
        let preferences = &server.dashboard.lock().await.preferences;
        if preferences.trigger(&request.repo.full_name) == Trigger::Off {
            return Ok(());
        }
        preferences.mode(&request.repo.full_name)
    };
    if !server
        .allowed_owner(app, request.repo.full_name.split('/').next().unwrap_or(""))
        .await
    {
        return Ok(());
    }
    let token = server
        .api
        .installation_token(
            app,
            request.repo.installation,
            Some(request.repo.id),
            app::Permissions::Comments,
        )
        .await?;
    if request.command == Command::Help {
        return reply(
            server,
            &request,
            &token,
            &format!(
                "Commands: `@{} review` (repository mode), `@{} quick`, `@{} deep`, \
             `@{} review quick|standard|deep`, `@{} help`.",
                app.slug, app.slug, app.slug, app.slug, app.slug
            ),
        )
        .await;
    }
    server
        .api
        .request(
            reqwest::Method::POST,
            &format!(
                "/repos/{}/issues/comments/{}/reactions",
                request.repo.full_name, request.comment
            ),
            &token,
            Some(json!({"content": "eyes"})),
        )
        .await?;
    let pr = server
        .api
        .request(
            reqwest::Method::GET,
            &format!("/repos/{}/pulls/{}", request.repo.full_name, request.number),
            &token,
            None,
        )
        .await?;
    if pr["state"] == "closed" {
        return reply(
            server,
            &request,
            &token,
            "This pull request is closed; no review was queued.",
        )
        .await;
    }
    let Command::Review(override_mode) = request.command else {
        unreachable!()
    };
    let mut job = Job::new(
        request.repo,
        Kind::Review {
            number: request.number,
            pr,
            force: true,
        },
        "mention",
    );
    job.mode = override_mode.unwrap_or(mode);
    server.enqueue(job).await;
    Ok(())
}

async fn reply(server: &Server, request: &Request, token: &str, body: &str) -> Result<()> {
    let mut value = json!({"body": body});
    crate::redact::json(&mut value);
    server
        .api
        .request(
            reqwest::Method::POST,
            &format!(
                "/repos/{}/issues/{}/comments",
                request.repo.full_name, request.number
            ),
            token,
            Some(value),
        )
        .await?;
    Ok(())
}
