use anyhow::{Context, Result, bail};
use hmac::{Hmac, KeyInit, Mac};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
pub struct App {
    pub id: u64,
    pub slug: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub name: String,
    pub pem: String,
    pub webhook_secret: String,
    pub owner: String,
    pub html_url: String,
}

impl App {
    pub fn from_conversion(v: &Value) -> Result<Self> {
        let string = |key: &str| -> Result<String> {
            Ok(v[key]
                .as_str()
                .with_context(|| format!("app response missing {key}"))?
                .into())
        };
        let app = Self {
            id: v["id"].as_u64().context("app response missing id")?,
            slug: string("slug")?,
            client_id: string("client_id")?,
            client_secret: string("client_secret")?,
            name: v["name"].as_str().unwrap_or("Revera").into(),
            pem: string("pem")?,
            webhook_secret: string("webhook_secret")?,
            owner: v["owner"]["login"]
                .as_str()
                .context("app response missing owner")?
                .into(),
            html_url: string("html_url")?,
        };
        app.register_secrets();
        app.jwt()?;
        Ok(app)
    }

    pub fn register_secrets(&self) {
        crate::redact::register(&self.pem);
        crate::redact::register(&self.webhook_secret);
        crate::redact::register(&self.client_secret);
    }

    pub fn jwt(&self) -> Result<String> {
        let now = chrono::Utc::now().timestamp();
        let jwt = jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &json!({"iat": now - 60, "exp": now + 9 * 60, "iss": self.id.to_string()}),
            &EncodingKey::from_rsa_pem(self.pem.as_bytes())?,
        )?;
        crate::redact::register(&jwt);
        Ok(jwt)
    }
}

pub fn signature_valid(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(signature) = header
        .strip_prefix("sha256=")
        .and_then(|s| hex::decode(s).ok())
    else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

pub fn token_matches(expected: &str, actual: &str) -> bool {
    // Compare fixed-size MACs rather than branching on token bytes or length.
    let mut mac =
        Hmac::<Sha256>::new_from_slice(expected.as_bytes()).expect("HMAC accepts any key");
    mac.update(b"revera setup");
    let signature = mac.finalize().into_bytes();
    let mut other =
        Hmac::<Sha256>::new_from_slice(actual.as_bytes()).expect("HMAC accepts any key");
    other.update(b"revera setup");
    other.verify_slice(&signature).is_ok()
}

pub fn manifest(public_url: &str, name: &str, public: bool, publish_fixes: bool) -> Value {
    json!({
        "name": name, "public": public, "url": "https://github.com/VeraTools/revera",
        "hook_attributes": {"url": format!("{public_url}/webhook")},
        "redirect_url": format!("{public_url}/setup/callback"),
        "setup_url": format!("{public_url}/repositories"),
        "setup_on_update": true,
        "callback_urls": [format!("{public_url}/auth/callback")],
        "default_permissions": {
            "contents": if publish_fixes { "write" } else { "read" },
            "pull_requests": "write", "checks": "write", "metadata": "read",
            "issues": "read"
        },
        "default_events": ["pull_request", "push", "check_run", "issue_comment"]
    })
}

#[derive(Clone)]
pub struct Api {
    base: String,
    http: reqwest::Client,
    oauth_url: String,
}

pub enum Permissions {
    Checks,
    Review,
    Index,
    ReadPulls,
    Comments,
    Fix,
    FixWorkflows,
}

#[derive(Debug)]
pub struct ApiStatus(pub reqwest::StatusCode, bool);

impl ApiStatus {
    pub fn denied(&self) -> bool {
        self.0 == reqwest::StatusCode::NOT_FOUND
            || (self.0 == reqwest::StatusCode::FORBIDDEN && !self.1)
    }
}

impl std::fmt::Display for ApiStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub API returned HTTP {}", self.0.as_u16())
    }
}
impl std::error::Error for ApiStatus {}

impl Api {
    pub fn base(&self) -> &str {
        &self.base
    }
    /// Anonymous, read-only external metadata path. It cannot mint tokens or write.
    pub async fn public_get(&self, path: &str, etag: Option<&str>) -> Result<reqwest::Response> {
        let mut request = self
            .http
            .get(format!("{}{path}", self.base))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        Ok(request.send().await?)
    }

    pub async fn oauth_token(&self, app: &App, code: &str, redirect: &str) -> Result<String> {
        let response = self
            .http
            .post(&self.oauth_url)
            .header("Accept", "application/json")
            .json(&json!({
                "client_id": app.client_id, "client_secret": app.client_secret,
                "code": code, "redirect_uri": redirect
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("GitHub OAuth exchange failed");
        }
        let value: Value = response.json().await?;
        let token = value["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("GitHub OAuth did not return a token")?;
        Ok(token.into())
    }
    pub fn new(base: String) -> Result<Self> {
        Ok(Self {
            oauth_url: "https://github.com/login/oauth/access_token".into(),
            base: base.trim_end_matches('/').into(),
            http: crate::tls::with_bundled_roots(reqwest::Client::builder())
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("revera/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    #[cfg(test)]
    pub fn with_oauth_url(mut self, url: String) -> Self {
        self.oauth_url = url;
        self
    }

    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if !token.is_empty() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request.send().await?;
        if !response.status().is_success() {
            // Conversion and token responses may contain credentials; do not echo bodies.
            let status = response.status();
            let mut rate_limited = response.headers().contains_key("retry-after")
                || response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0");
            if status == reqwest::StatusCode::FORBIDDEN && !rate_limited {
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await? {
                    let remaining = 64 * 1024 - body.len();
                    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if body.len() == 64 * 1024 {
                        break;
                    }
                }
                rate_limited = String::from_utf8_lossy(&body)
                    .to_ascii_lowercase()
                    .contains("rate limit");
            }
            return Err(ApiStatus(status, rate_limited).into());
        }
        Ok(response.json().await?)
    }

    pub async fn list(&self, path: &str, token: &str, field: Option<&str>) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        for page in 1..=50 {
            let separator = if path.contains('?') { '&' } else { '?' };
            let response = self
                .request(
                    reqwest::Method::GET,
                    &format!("{path}{separator}per_page=100&page={page}"),
                    token,
                    None,
                )
                .await?;
            let values = field
                .map_or(&response, |f| &response[f])
                .as_array()
                .context("GitHub list response is not an array")?;
            items.extend(values.iter().cloned());
            if values.len() < 100 {
                return Ok(items);
            }
        }
        bail!("GitHub pagination exceeds 5000 items")
    }

    pub async fn installation_token(
        &self,
        app: &App,
        installation: u64,
        repo: Option<u64>,
        permissions: Permissions,
    ) -> Result<String> {
        let permissions = match permissions {
            Permissions::Comments => {
                // Replies and reactions on PR conversations need Pull requests
                // write; Issues write alone is refused there.
                json!({"pull_requests": "write", "metadata": "read"})
            }
            Permissions::Review => {
                json!({
                    "contents": "read", "pull_requests": "write",
                    "checks": "write", "metadata": "read"
                })
            }
            Permissions::Index => json!({"contents": "read", "metadata": "read"}),
            Permissions::Checks => json!({"checks": "write", "metadata": "read"}),
            Permissions::ReadPulls => {
                json!({"contents": "read", "pull_requests": "read", "metadata": "read"})
            }
            Permissions::Fix => {
                json!({"contents":"write","pull_requests":"write","metadata":"read"})
            }
            Permissions::FixWorkflows => {
                json!({"contents":"write","pull_requests":"write","metadata":"read","workflows":"write"})
            }
        };
        let mut body = json!({"permissions": permissions});
        if let Some(id) = repo {
            body["repository_ids"] = json!([id]);
        }
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/app/installations/{installation}/access_tokens"),
                &app.jwt()?,
                Some(body),
            )
            .await?;
        let token = response["token"]
            .as_str()
            .context("GitHub token response missing token")?
            .to_string();
        crate::redact::register(&token);
        Ok(token)
    }
}
