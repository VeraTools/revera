use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct GitHubHttpError {
    pub status: u16,
    pub body: String,
}

impl GitHubHttpError {
    /// A 422 caused by inline comment placement (line/path/hunk not in the
    /// diff), as opposed to any other validation failure.
    pub fn is_placement_rejection(&self) -> bool {
        if self.status != 422 {
            return false;
        }
        let b = self.body.to_lowercase();
        [
            "part of the diff",
            "could not be resolved",
            "pull_request_review_thread",
            "start_line",
            "same hunk",
            "position",
        ]
        .iter()
        .any(|k| b.contains(k))
    }

    fn is_transient(&self) -> bool {
        self.status == 429 || self.status >= 500
    }
}

impl fmt::Display for GitHubHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GitHub HTTP {}: {}",
            self.status,
            crate::redact::text(&excerpt(&self.body))
        )
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

/// Retry policy for idempotent requests (GET/PATCH). POSTs are never
/// replayed: an ambiguous create is reconciled by the caller instead.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub deadline: Duration,
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            deadline: Duration::from_secs(60),
            max_wait: Duration::from_secs(20),
        }
    }
}

pub struct GitHubApi {
    pub base: String,
    token: String,
    http: reqwest::Client,
    pub retry: RetryPolicy,
}

fn excerpt(s: &str) -> String {
    s.chars().take(300).collect()
}

/// Upper bound on pages walked by list endpoints (100 items per page).
const MAX_PAGES: u32 = 50;

fn parse_comment(c: &Value, what: &str) -> Result<GhComment> {
    let id = c["id"]
        .as_u64()
        .filter(|&id| id > 0)
        .with_context(|| format!("malformed GitHub {what}: missing id"))?;
    Ok(GhComment {
        id,
        body: c["body"].as_str().unwrap_or("").to_string(),
        author: c["user"]["login"].as_str().map(str::to_string),
        author_is_bot: c["user"]["type"].as_str() == Some("Bot"),
    })
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
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
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        crate::redact::register(token);
        Self {
            base,
            token: token.to_string(),
            http,
            retry: RetryPolicy::default(),
        }
    }

    /// Constructor for tests/custom endpoints.
    pub fn with_base(base: &str, token: &str) -> Self {
        let mut a = Self::new(token);
        a.base = base.trim_end_matches('/').to_string();
        a
    }

    /// Send once and parse a JSON body. Non-JSON success bodies are errors:
    /// a malformed acknowledgement must never read as success.
    async fn send_once(&self, req: reqwest::RequestBuilder) -> Result<(Value, Option<Duration>)> {
        let resp = req
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("github request failed")?;
        let status = resp.status().as_u16();
        let wait = retry_after(&resp);
        let text = resp.text().await.unwrap_or_default();
        if status >= 400 {
            return Err(GitHubHttpError { status, body: text }.into_anyhow_with_wait(wait));
        }
        let v = serde_json::from_str(&text)
            .with_context(|| format!("malformed GitHub response (HTTP {status}): not JSON"))?;
        Ok((v, wait))
    }

    /// Idempotent request with bounded retries on 429/5xx/transport errors,
    /// honouring `Retry-After` within the policy deadline.
    async fn send_idempotent(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let start = Instant::now();
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let this = req
                .try_clone()
                .context("github request cannot be retried")?;
            let err = match self.send_once(this).await {
                Ok((v, _)) => return Ok(v),
                Err(e) => e,
            };
            let (transient, hint) = match err.downcast_ref::<RetryableHttp>() {
                Some(r) => (r.inner.is_transient(), r.wait),
                None => (
                    err.downcast_ref::<reqwest::Error>().is_some() || is_transport(&err),
                    None,
                ),
            };
            let backoff = hint
                .unwrap_or_else(|| Duration::from_millis(500 * 2u64.pow(attempt - 1)))
                .min(self.retry.max_wait);
            if !transient
                || attempt >= self.retry.attempts
                || start.elapsed() + backoff > self.retry.deadline
            {
                return Err(unwrap_retryable(err));
            }
            tracing::warn!("github request failed ({err:#}); retrying in {backoff:?}");
            tokio::time::sleep(backoff).await;
        }
    }

    async fn send_post(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        self.send_once(req)
            .await
            .map(|(v, _)| v)
            .map_err(unwrap_retryable)
    }

    /// Current head sha of a pull request.
    pub async fn get_pull(&self, owner: &str, repo: &str, n: u64) -> Result<String> {
        let v = self
            .send_idempotent(self.http.get(format!(
                "{}/repos/{}/{}/pulls/{}",
                self.base, owner, repo, n
            )))
            .await?;
        v["head"]["sha"]
            .as_str()
            .filter(|s| crate::git::is_oid(s))
            .map(|s| s.to_string())
            .context("malformed GitHub pull payload: missing head.sha")
    }

    async fn list_comments(&self, url: String, what: &str) -> Result<Vec<GhComment>> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let v = self
                .send_idempotent(self.http.get(format!("{url}?per_page=100&page={page}")))
                .await?;
            let arr = v
                .as_array()
                .with_context(|| format!("malformed GitHub {what} list: not an array"))?;
            for c in arr {
                out.push(parse_comment(c, what)?);
            }
            if arr.len() < 100 {
                return Ok(out);
            }
        }
        bail!("GitHub {what} list exceeds {MAX_PAGES} pages");
    }

    /// All issue comments on the PR (paginated, per_page=100).
    pub async fn list_issue_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<GhComment>> {
        self.list_comments(
            format!(
                "{}/repos/{}/{}/issues/{}/comments",
                self.base, owner, repo, n
            ),
            "issue comment",
        )
        .await
    }

    /// All inline review comments on the PR (paginated), with authors.
    pub async fn list_review_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<GhComment>> {
        self.list_comments(
            format!(
                "{}/repos/{}/{}/pulls/{}/comments",
                self.base, owner, repo, n
            ),
            "review comment",
        )
        .await
    }

    /// Login of the authenticated identity; `None` when the token cannot
    /// answer `/user` (e.g. the Actions installation token).
    pub async fn viewer_login(&self) -> Option<String> {
        let v = self
            .send_idempotent(self.http.get(format!("{}/user", self.base)))
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
        let body = crate::redact::text(body);
        let v = self
            .send_post(
                self.http
                    .post(format!(
                        "{}/repos/{}/{}/issues/{}/comments",
                        self.base, owner, repo, n
                    ))
                    .json(&json!({"body": body})),
            )
            .await?;
        let c = parse_comment(&v, "created comment")?;
        Ok(GhComment {
            body: body.into_owned(),
            ..c
        })
    }

    pub async fn update_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<GhComment> {
        let body = crate::redact::text(body);
        let v = self
            .send_idempotent(
                self.http
                    .patch(format!(
                        "{}/repos/{}/{}/issues/comments/{}",
                        self.base, owner, repo, comment_id
                    ))
                    .json(&json!({"body": body})),
            )
            .await?;
        let c = parse_comment(&v, "updated comment")?;
        if c.id != comment_id {
            bail!(
                "malformed GitHub response: updated comment {} but asked for {comment_id}",
                c.id
            );
        }
        Ok(GhComment {
            body: body.into_owned(),
            ..c
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
                    "body": crate::redact::text(&c.body),
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
            .send_post(
                self.http
                    .post(format!(
                        "{}/repos/{}/{}/pulls/{}/reviews",
                        self.base, owner, repo, n
                    ))
                    .json(&json!({
                        "commit_id": commit_id,
                        "event": "COMMENT",
                        "body": crate::redact::text(body),
                        "comments": comments,
                    })),
            )
            .await?;
        v["id"]
            .as_u64()
            .filter(|&id| id > 0)
            .context("malformed GitHub review response: missing id")
    }
}

/// Internal carrier for an HTTP error plus its `Retry-After` hint.
#[derive(Debug)]
struct RetryableHttp {
    inner: GitHubHttpError,
    wait: Option<Duration>,
}

impl fmt::Display for RetryableHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl std::error::Error for RetryableHttp {}

impl GitHubHttpError {
    fn into_anyhow_with_wait(self, wait: Option<Duration>) -> anyhow::Error {
        RetryableHttp { inner: self, wait }.into()
    }
}

/// Callers downcast to [`GitHubHttpError`]; strip the retry carrier.
fn unwrap_retryable(e: anyhow::Error) -> anyhow::Error {
    match e.downcast::<RetryableHttp>() {
        Ok(r) => r.inner.into(),
        Err(e) => e,
    }
}

fn is_transport(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<reqwest::Error>().is_some())
}

/// Whether a failed POST may have been applied server-side (transport
/// failure or 5xx): the caller must reconcile before retrying.
pub fn is_ambiguous(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<GitHubHttpError>() {
        Some(h) => h.status >= 500,
        None => is_transport(e),
    }
}
