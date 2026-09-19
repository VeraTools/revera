use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fmt;

#[derive(Debug)]
pub struct GitHubHttpError {
    pub status: u16,
    pub body: String,
}

impl fmt::Display for GitHubHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GitHub HTTP {}: {}", self.status, excerpt(&self.body))
    }
}

impl std::error::Error for GitHubHttpError {}

#[derive(Debug, Clone, Default)]
pub struct GhComment {
    pub id: u64,
    pub body: String,
    /// Author login when the API reported one.
    pub author: Option<String>,
    pub author_is_bot: bool,
}

/// Comment payload for `create_review`: a RIGHT-side inline comment.
#[derive(Debug, Clone)]
pub struct ReviewComment {
    pub path: String,
    pub line: u32,
    /// Set when the comment spans a range (end_line > line).
    pub end_line: Option<u32>,
    pub body: String,
}

pub struct GitHubApi {
    pub base: String,
    token: String,
    http: reqwest::Client,
}

fn excerpt(s: &str) -> String {
    s.chars().take(300).collect()
}

impl GitHubApi {
    /// `base` defaults to https://api.github.com; `GITHUB_API_URL` overrides.
    pub fn new(token: &str) -> Self {
        let base = std::env::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".into())
            .trim_end_matches('/')
            .to_string();
        let http = reqwest::Client::builder()
            .user_agent("revera")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            base,
            token: token.to_string(),
            http,
        }
    }

    /// Constructor for tests/custom endpoints.
    pub fn with_base(base: &str, token: &str) -> Self {
        let mut a = Self::new(token);
        a.base = base.trim_end_matches('/').to_string();
        a
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("github request failed")?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status >= 400 {
            return Err(GitHubHttpError { status, body: text }.into());
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Current head sha of a pull request.
    pub async fn get_pull(&self, owner: &str, repo: &str, n: u64) -> Result<String> {
        let v = self
            .send(self.http.get(format!(
                "{}/repos/{}/{}/pulls/{}",
                self.base, owner, repo, n
            )))
            .await?;
        v["head"]["sha"]
            .as_str()
            .map(|s| s.to_string())
            .context("pull payload missing head.sha")
    }

    /// All issue comments on the PR (paginated, per_page=100).
    pub async fn list_issue_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<GhComment>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let v = self
                .send(self.http.get(format!(
                    "{}/repos/{}/{}/issues/{}/comments?per_page=100&page={}",
                    self.base, owner, repo, n, page
                )))
                .await?;
            let arr = v.as_array().cloned().unwrap_or_default();
            let count = arr.len();
            for c in arr {
                out.push(GhComment {
                    id: c["id"].as_u64().unwrap_or(0),
                    body: c["body"].as_str().unwrap_or("").to_string(),
                    author: c["user"]["login"].as_str().map(str::to_string),
                    author_is_bot: c["user"]["type"].as_str() == Some("Bot"),
                });
            }
            if count < 100 {
                return Ok(out);
            }
            page += 1;
        }
    }

    /// All inline review comments on the PR (paginated), with authors.
    pub async fn list_review_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<GhComment>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let v = self
                .send(self.http.get(format!(
                    "{}/repos/{}/{}/pulls/{}/comments?per_page=100&page={}",
                    self.base, owner, repo, n, page
                )))
                .await?;
            let arr = v.as_array().cloned().unwrap_or_default();
            let count = arr.len();
            for c in &arr {
                out.push(GhComment {
                    id: c["id"].as_u64().unwrap_or(0),
                    body: c["body"].as_str().unwrap_or("").to_string(),
                    author: c["user"]["login"].as_str().map(str::to_string),
                    author_is_bot: c["user"]["type"].as_str() == Some("Bot"),
                });
            }
            if count < 100 {
                return Ok(out);
            }
            page += 1;
        }
    }

    /// Login of the authenticated identity; `None` when the token cannot
    /// answer `/user` (e.g. the Actions installation token).
    pub async fn viewer_login(&self) -> Option<String> {
        let v = self
            .send(self.http.get(format!("{}/user", self.base)))
            .await
            .ok()?;
        v["login"].as_str().map(str::to_string)
    }

    pub async fn create_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        body: &str,
    ) -> Result<GhComment> {
        let v = self
            .send(
                self.http
                    .post(format!(
                        "{}/repos/{}/{}/issues/{}/comments",
                        self.base, owner, repo, n
                    ))
                    .json(&json!({"body": body})),
            )
            .await?;
        Ok(GhComment {
            id: v["id"].as_u64().unwrap_or(0),
            body: body.to_string(),
            ..Default::default()
        })
    }

    pub async fn update_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<GhComment> {
        let v = self
            .send(
                self.http
                    .patch(format!(
                        "{}/repos/{}/{}/issues/comments/{}",
                        self.base, owner, repo, comment_id
                    ))
                    .json(&json!({"body": body})),
            )
            .await?;
        Ok(GhComment {
            id: v["id"].as_u64().unwrap_or(comment_id),
            body: body.to_string(),
            ..Default::default()
        })
    }

    /// Create a "COMMENT" pull request review with inline comments.
    /// Returns the review id.
    pub async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        commit_id: &str,
        body: &str,
        comments: &[ReviewComment],
    ) -> Result<u64> {
        let comments: Vec<Value> = comments
            .iter()
            .map(|c| {
                let mut v = json!({
                    "path": c.path,
                    "line": c.line.max(1),
                    "side": "RIGHT",
                    "body": c.body,
                });
                if let Some(end) = c.end_line {
                    if end > c.line.max(1) {
                        v["line"] = json!(end);
                        v["start_line"] = json!(c.line.max(1));
                        v["start_side"] = json!("RIGHT");
                    }
                }
                v
            })
            .collect();
        let v = self
            .send(
                self.http
                    .post(format!(
                        "{}/repos/{}/{}/pulls/{}/reviews",
                        self.base, owner, repo, n
                    ))
                    .json(&json!({
                        "commit_id": commit_id,
                        "event": "COMMENT",
                        "body": body,
                        "comments": comments,
                    })),
            )
            .await?;
        Ok(v["id"].as_u64().unwrap_or(0))
    }
}
