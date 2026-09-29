use crate::findings::Severity;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    #[default]
    Baseline,
    Delegated,
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum PublishMode {
    #[default]
    DryRun,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    OpenaiChat,
    OpenaiResponses,
    Anthropic,
    Gemini,
    Scripted,
}

impl Protocol {
    /// Whether this protocol goes over the HTTP transport + adapter stack.
    pub fn is_http(&self) -> bool {
        !matches!(self, Protocol::Scripted)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    #[serde(default)]
    pub strategy: Strategy,
    #[serde(default = "default_max_findings")]
    pub max_findings: usize,
    #[serde(default)]
    pub publish: PublishMode,
    #[serde(default)]
    pub publish_uncertain: bool,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_tool_output")]
    pub max_tool_output_bytes: usize,
    #[serde(default = "default_diff_bytes")]
    pub max_diff_bytes: usize,
    #[serde(default)]
    pub min_severity: Severity,
    /// Evaluation-only knob: `false` skips the validation pass. Candidates
    /// are then reported as unvalidated, are never publishable, and never
    /// seed stored state; `publish: comment` is rejected. Default true.
    #[serde(default = "default_true")]
    pub validate: bool,
    /// Repository guidance given to the investigator, read from the base
    /// commit: `off` (default), `review` (`REVIEW.md`) or `agents`
    /// (`REVIEW.md`, else `AGENTS.md`, per directory).
    #[serde(default)]
    pub guidance: GuidanceMode,
    #[serde(default = "default_guidance_bytes")]
    pub guidance_max_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GuidanceMode {
    #[default]
    Off,
    Review,
    Agents,
}

impl GuidanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Review => "review",
            Self::Agents => "agents",
        }
    }
}

fn default_guidance_bytes() -> usize {
    16 * 1024
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    #[serde(default = "default_tool_calls")]
    pub agent_max_tool_calls: u32,
    #[serde(default = "default_agent_seconds")]
    pub agent_max_seconds: u64,
    #[serde(default = "default_run_requests")]
    pub run_max_requests: u32,
    #[serde(default = "default_run_seconds")]
    pub run_max_seconds: u64,
    #[serde(default = "default_retries")]
    pub retries: u32,
}

/// Reasoning effort levels; `none` disables and emits no reasoning fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    /// Effort -> thinking budget in tokens (where the provider takes a budget).
    pub fn budget(&self) -> u64 {
        match self {
            Self::None => 0,
            Self::Minimal => 1024,
            Self::Low => 2048,
            Self::Medium => 8192,
            Self::High => 16384,
            Self::Xhigh => 32768,
            Self::Max => 65536,
        }
    }
    /// Wire spelling (openai has no "xhigh" on non-gpt-5 models — caller maps).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Which wire field openai-chat uses for reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningField {
    #[default]
    Auto,
    Openai,
    Openrouter,
}

/// Long form of `reasoning:`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningSpec {
    #[serde(default)]
    pub effort: ReasoningEffort,
    #[serde(default)]
    pub budget_tokens: Option<u64>,
    #[serde(default)]
    pub field: ReasoningField,
}

/// `reasoning: medium` or the long `{effort, budget_tokens, field}` form.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Reasoning {
    Effort(ReasoningEffort),
    Spec(ReasoningSpec),
}

impl Default for Reasoning {
    fn default() -> Self {
        Reasoning::Effort(ReasoningEffort::Medium)
    }
}

impl Reasoning {
    pub fn effort(&self) -> ReasoningEffort {
        match self {
            Reasoning::Effort(e) => *e,
            Reasoning::Spec(s) => s.effort,
        }
    }
    pub fn budget_tokens(&self) -> Option<u64> {
        match self {
            Reasoning::Effort(_) => None,
            Reasoning::Spec(s) => s.budget_tokens,
        }
    }
    pub fn field(&self) -> ReasoningField {
        match self {
            Reasoning::Effort(_) => ReasoningField::Auto,
            Reasoning::Spec(s) => s.field,
        }
    }
    /// Effective thinking budget: explicit budget wins, else effort->budget.
    pub fn effective_budget(&self) -> u64 {
        self.budget_tokens()
            .unwrap_or_else(|| self.effort().budget())
    }
    pub fn enabled(&self) -> bool {
        self.effort() != ReasoningEffort::None
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    pub protocol: Protocol,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub model: String,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// Header name sent on every request with a stable per-run session id.
    /// Defaults to `x-opencode-session` when the base_url host is opencode.ai.
    #[serde(default)]
    pub session_header: Option<String>,
    pub script: Option<PathBuf>,
    /// Reasoning/thinking level; on by default (medium). `none` disables.
    #[serde(default)]
    pub reasoning: Reasoning,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoutRoute {
    pub name: String,
    pub focus: Option<String>,
    #[serde(flatten)]
    pub route: ModelRoute,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    pub investigator: ModelRoute,
    /// Omitted validator inherits the investigator route (fresh context).
    pub validator: Option<ModelRoute>,
    pub lead: Option<ModelRoute>,
    pub workers: Option<Vec<ModelRoute>>,
    pub scouts: Option<Vec<ScoutRoute>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum VeraBackend {
    Api,
    #[default]
    Local,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VeraEndpoint {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    /// Reranker only: `generic` (Jina/Cohere/SiliconFlow-style `/rerank`) or
    /// `voyage`. Unset lets Vera infer it from the base URL.
    #[serde(default)]
    pub protocol: Option<RerankerProtocol>,
    /// Reranker only: path joined onto `base_url` (Vera default `/rerank`).
    #[serde(default)]
    pub endpoint_path: Option<String>,
    /// Reranker only: `true`/`false` send `return_documents` explicitly;
    /// `omit` leaves the field out for providers that reject it. Unset keeps
    /// Vera's default (`false`).
    #[serde(default)]
    pub return_documents: Option<ReturnDocuments>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ReturnDocuments {
    Send(bool),
    Omit(OmitTag),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OmitTag {
    Omit,
}

impl ReturnDocuments {
    /// Value for `vera config set retrieval.reranker_return_documents`.
    pub fn config_value(this: Option<Self>) -> &'static str {
        match this {
            None | Some(Self::Send(false)) => "false",
            Some(Self::Send(true)) => "true",
            Some(Self::Omit(_)) => "null",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RerankerProtocol {
    Generic,
    Voyage,
}

impl RerankerProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Voyage => "voyage",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VeraConfig {
    #[serde(default = "default_vera_exe")]
    pub executable: String,
    pub version: Option<String>,
    /// When false: skip indexing and remove the vera_* tools entirely.
    /// A present `vera:` section opts in (default true); an absent one means
    /// disabled (see `impl Default for VeraConfig`).
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub backend: VeraBackend,
    pub embedding: Option<VeraEndpoint>,
    pub reranker: Option<VeraEndpoint>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Revera-owned Vera home (config + local models). Defaults to
    /// `$REVERA_VERA_HOME`, else `$XDG_CACHE_HOME/revera/vera-home`, else
    /// `~/.cache/revera/vera-home`. Never the user's global Vera home.
    #[serde(default)]
    pub home: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default = "default_marker")]
    pub summary_marker: String,
    /// Login the token posts as when `/user` cannot name it (installation
    /// tokens). Only comments by this author are treated as Revera's own.
    #[serde(default = "default_bot_login")]
    pub bot_login: String,
    /// Allow publishing comments on forked-PR events (default false).
    #[serde(default)]
    pub allow_forks: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedConfig {
    #[serde(default = "default_max_questions")]
    pub max_questions: usize,
    #[serde(default = "default_worker_tool_calls")]
    pub worker_max_tool_calls: u32,
    #[serde(default = "default_worker_seconds")]
    pub worker_max_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    #[serde(default = "default_focuses")]
    pub focuses: Vec<String>,
    #[serde(default = "default_scout_tool_calls")]
    pub scout_max_tool_calls: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOverride {
    pub review: Option<ReviewOverride>,
    pub budget: Option<BudgetOverride>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOverride {
    pub strategy: Option<Strategy>,
    pub max_findings: Option<usize>,
    pub publish: Option<PublishMode>,
    pub publish_uncertain: Option<bool>,
    pub concurrency: Option<usize>,
    pub max_tool_output_bytes: Option<usize>,
    pub max_diff_bytes: Option<usize>,
    pub min_severity: Option<Severity>,
    pub guidance: Option<GuidanceMode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetOverride {
    pub agent_max_tool_calls: Option<u32>,
    pub agent_max_seconds: Option<u64>,
    pub run_max_requests: Option<u32>,
    pub run_max_seconds: Option<u64>,
    pub retries: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    pub models: ModelsConfig,
    #[serde(default)]
    pub vera: VeraConfig,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub delegated: DelegatedConfig,
    #[serde(default)]
    pub panel: PanelConfig,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileOverride>,
}

fn default_true() -> bool {
    true
}
fn default_max_findings() -> usize {
    10
}
fn default_concurrency() -> usize {
    4
}
fn default_tool_output() -> usize {
    12000
}
fn default_diff_bytes() -> usize {
    200_000
}
fn default_tool_calls() -> u32 {
    25
}
fn default_agent_seconds() -> u64 {
    300
}
fn default_run_requests() -> u32 {
    120
}
fn default_run_seconds() -> u64 {
    900
}
fn default_retries() -> u32 {
    3
}
fn default_max_output() -> u32 {
    4000
}
fn default_temperature() -> f64 {
    0.2
}
fn default_vera_exe() -> String {
    "vera".into()
}
fn default_token_env() -> String {
    "GITHUB_TOKEN".into()
}
fn default_marker() -> String {
    "<!-- revera-summary -->".into()
}
fn default_bot_login() -> String {
    "github-actions[bot]".into()
}
fn default_max_questions() -> usize {
    4
}
fn default_worker_tool_calls() -> u32 {
    12
}
fn default_worker_seconds() -> u64 {
    120
}
fn default_focuses() -> Vec<String> {
    vec!["general".into(), "cross-file".into()]
}
fn default_scout_tool_calls() -> u32 {
    15
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            strategy: Strategy::default(),
            max_findings: default_max_findings(),
            publish: PublishMode::default(),
            publish_uncertain: false,
            concurrency: default_concurrency(),
            max_tool_output_bytes: default_tool_output(),
            max_diff_bytes: default_diff_bytes(),
            min_severity: Severity::default(),
            validate: default_true(),
            guidance: GuidanceMode::default(),
            guidance_max_bytes: default_guidance_bytes(),
        }
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            agent_max_tool_calls: default_tool_calls(),
            agent_max_seconds: default_agent_seconds(),
            run_max_requests: default_run_requests(),
            run_max_seconds: default_run_seconds(),
            retries: default_retries(),
        }
    }
}

impl Default for DelegatedConfig {
    fn default() -> Self {
        Self {
            max_questions: default_max_questions(),
            worker_max_tool_calls: default_worker_tool_calls(),
            worker_max_seconds: default_worker_seconds(),
        }
    }
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            focuses: default_focuses(),
            scout_max_tool_calls: default_scout_tool_calls(),
        }
    }
}

impl Default for VeraConfig {
    fn default() -> Self {
        Self {
            executable: default_vera_exe(),
            version: None,
            enabled: false,
            backend: VeraBackend::default(),
            embedding: None,
            reranker: None,
            exclude: vec![],
            home: None,
        }
    }
}

/// Repository paths no model-facing surface (file tools, grep, Vera index,
/// guidance) may read: likely credential material, even when tracked.
pub const SENSITIVE_GLOBS: &[&str] = &[
    ".env",
    ".env.*",
    "**/.env",
    "**/.env.*",
    "*.pem",
    "**/*.pem",
    "*.key",
    "**/*.key",
    "*.p12",
    "**/*.p12",
    "*.pfx",
    "**/*.pfx",
    "**/id_rsa*",
    "**/id_ed25519*",
    "**/id_ecdsa*",
    ".npmrc",
    "**/.npmrc",
    ".pypirc",
    "**/.pypirc",
    ".netrc",
    "**/.netrc",
    ".revera/**",
    ".vera/**",
];

/// Schema of the Revera-side index identity; bump when its meaning changes.
const INDEX_IDENTITY_SCHEMA: u32 = 2;

impl VeraConfig {
    /// Stable identity of everything that shapes the on-disk index: Vera
    /// version, embedding backend/origin/model and the effective corpus
    /// exclusions. Query-time settings (reranker, review models, budgets,
    /// guidance) and credentials are deliberately absent so they never
    /// invalidate a warm index.
    pub fn index_identity(&self) -> serde_json::Value {
        let embedding = match self.backend {
            VeraBackend::Api => self
                .embedding
                .as_ref()
                .map(|e| serde_json::json!({"base_url": e.base_url.trim_end_matches('/'), "model": e.model})),
            VeraBackend::Local => Some(serde_json::json!({"local": LOCAL_VERA_BACKEND})),
        };
        serde_json::json!({
            "schema": INDEX_IDENTITY_SCHEMA,
            "version": self.version,
            "backend": format!("{:?}", self.backend).to_lowercase(),
            "embedding": embedding,
            "exclude": self.effective_excludes(),
        })
    }

    /// Short stable hash of [`Self::index_identity`] (Action cache key).
    pub fn index_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(self.index_identity().to_string().as_bytes());
        hex::encode(&h[..12])
    }

    /// Configured exclusions plus [`SENSITIVE_GLOBS`], deduplicated in order.
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for g in self
            .exclude
            .iter()
            .map(String::as_str)
            .chain(SENSITIVE_GLOBS.iter().copied())
        {
            let g = g.trim();
            if !g.is_empty() && !out.iter().any(|x| x == g) {
                out.push(g.to_string());
            }
        }
        out
    }

    /// Query-time reranker identity: part of the review fingerprint, not
    /// the index identity.
    pub fn reranker_identity(&self) -> serde_json::Value {
        match &self.reranker {
            None => serde_json::Value::Null,
            Some(r) => serde_json::json!({
                "base_url": r.base_url.trim_end_matches('/'),
                "model": r.model,
                "protocol": r.protocol.map(|p| p.as_str()),
                "endpoint_path": r.endpoint_path,
                "return_documents": crate::config::ReturnDocuments::config_value(r.return_documents),
            }),
        }
    }
}

impl VeraConfig {
    /// Revera-owned Vera home; see [`VeraConfig::home`].
    pub fn vera_home(&self) -> PathBuf {
        if let Some(h) = &self.home {
            return h.clone();
        }
        if let Some(h) = std::env::var_os("REVERA_VERA_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(h);
        }
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
            });
        cache.join("revera/vera-home")
    }
}

/// Vera backend used for `vera.backend: local` (CPU static embeddings that
/// need no GPU and no API key).
pub const LOCAL_VERA_BACKEND: &str = "potion-code";

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            token_env: default_token_env(),
            summary_marker: default_marker(),
            bot_login: default_bot_login(),
            allow_forks: false,
        }
    }
}

/// A credential env var counts as set only when it has a non-blank value.
pub fn env_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.trim().is_empty())
}

/// Expand `${VAR}` occurrences; error names the unset var.
pub fn expand_env(s: &str) -> Result<String> {
    let re = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    let mut missing: Option<String> = None;
    let out = re
        .replace_all(s, |c: &regex::Captures| {
            let name = c[1].to_string();
            match std::env::var(&name) {
                Ok(v) => v,
                Err(_) => {
                    missing = Some(name.clone());
                    String::new()
                }
            }
        })
        .into_owned();
    if let Some(v) = missing {
        bail!(
            "environment variable ${} referenced by config is not set",
            v
        );
    }
    Ok(out)
}

fn expand_route(r: &mut ModelRoute) -> Result<()> {
    if let Some(b) = &mut r.base_url {
        *b = expand_env(b)?;
    }
    if let Some(s) = &mut r.script {
        let e = expand_env(&s.to_string_lossy())?;
        *s = PathBuf::from(e);
    }
    for v in r.extra_headers.values_mut() {
        *v = expand_env(v)?;
    }
    if r.session_header.is_none() && r.base_url.as_deref().is_some_and(is_opencode_host) {
        r.session_header = Some("x-opencode-session".into());
    }
    Ok(())
}

/// `[A-Za-z_][A-Za-z0-9_]*`
pub fn is_env_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch == '_' || ch.is_ascii_alphabetic())
        && c.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Credential-bearing endpoints must be absolute https URLs without
/// embedded userinfo; plain http is allowed only for loopback hosts.
/// `strict` (event mode, where CI secrets are present): https only, plain
/// http only for loopback. Always: absolute URL without embedded credentials.
pub fn check_endpoint_url(what: &str, u: &str, strict: bool) -> Result<()> {
    let parsed =
        url::Url::parse(u).with_context(|| format!("{what}: {u:?} is not an absolute URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("{what}: credentials must not be embedded in the URL");
    }
    let loopback = matches!(
        parsed.host(),
        Some(url::Host::Domain("localhost"))
            | Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
            | Some(url::Host::Ipv6(std::net::Ipv6Addr::LOCALHOST))
    );
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if loopback || !strict => Ok(()),
        s => bail!("{what}: scheme {s:?} is not allowed (use https; http only for localhost)"),
    }
}

/// Whether `base_url` points at opencode.ai or a subdomain. Anything that
/// does not parse as an absolute URL, or has no host, is false.
pub fn is_opencode_host(base_url: &str) -> bool {
    let Ok(u) = url::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = u.host_str().map(str::to_lowercase) else {
        return false;
    };
    host == "opencode.ai" || host.ends_with(".opencode.ai")
}

/// Syntactic shape of a route: required fields present, header names valid.
/// Runs for every route at load time; credentials are checked separately by
/// `validate_for` so an unused route cannot block a run.
fn check_route_shape(name: &str, r: &ModelRoute) -> Result<()> {
    match r.protocol {
        Protocol::OpenaiChat | Protocol::OpenaiResponses => {
            if r.base_url.as_deref().is_none_or(|s| s.is_empty()) {
                bail!("models.{name}: protocol {:?} requires base_url", r.protocol);
            }
        }
        // anthropic/gemini have default base_urls
        Protocol::Anthropic | Protocol::Gemini => {}
        Protocol::Scripted => {
            if r.script.is_none() {
                bail!("models.{name}: protocol scripted requires script path");
            }
            return Ok(());
        }
    }
    if let Some(h) = &r.session_header
        && (h.trim().is_empty() || reqwest::header::HeaderName::from_bytes(h.as_bytes()).is_err())
    {
        bail!("models.{name}: session_header {h:?} is not a valid HTTP header name");
    }
    if r.protocol.is_http() && r.api_key_env.as_deref().unwrap_or_default().is_empty() {
        bail!(
            "models.{name}: protocol {:?} requires api_key_env",
            r.protocol
        );
    }
    Ok(())
}

/// >1 configured scouts requires one `panel.focuses` entry per scout.
pub(crate) fn check_lane_cardinality(focuses: usize, scouts: usize) -> Result<()> {
    if scouts > 1 && scouts != focuses {
        bail!(
            "panel: {focuses} focuses but {scouts} scouts (must be equal, or use a single scout)"
        );
    }
    Ok(())
}

/// Credential check: HTTP routes need their api_key_env populated.
fn check_route_credentials(name: &str, r: &ModelRoute) -> Result<()> {
    if r.protocol.is_http() {
        let env = r.api_key_env.as_deref().unwrap_or_default();
        if !env.is_empty() && !env_is_set(env) {
            bail!("models.{name}: api_key_env {env} is not set (or empty) in the environment");
        }
    }
    Ok(())
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse config text (`label` names its origin in errors).
    pub fn parse(text: &str, label: &str) -> Result<Self> {
        let text = expand_env(text)?;
        let mut cfg: Config =
            serde_saphyr::from_str(&text).with_context(|| format!("invalid config {label}"))?;
        cfg.expand_and_validate()?;
        Ok(cfg)
    }

    pub fn find(path: Option<&Path>) -> Result<PathBuf> {
        if let Some(p) = path {
            return Ok(p.to_path_buf());
        }
        let d = Path::new("./revera.yaml");
        if d.exists() {
            return Ok(d.to_path_buf());
        }
        bail!("no config: pass --config <path> or create ./revera.yaml");
    }

    fn expand_and_validate(&mut self) -> Result<()> {
        let m = &self.github.summary_marker;
        if !(m.starts_with("<!-- revera") && m.trim_end().ends_with("-->")) {
            bail!(
                "github.summary_marker must be an HTML comment starting with '<!-- revera' (got {m:?})"
            );
        }
        if self.models.workers.as_ref().is_some_and(|ws| ws.is_empty()) {
            self.models.workers = None;
        }
        if self.models.scouts.as_ref().is_some_and(|ss| ss.is_empty()) {
            self.models.scouts = None;
        }
        expand_route(&mut self.models.investigator)?;
        if let Some(v) = &mut self.models.validator {
            expand_route(v)?;
        }
        if let Some(l) = &mut self.models.lead {
            expand_route(l)?;
        }
        if let Some(ws) = &mut self.models.workers {
            for w in ws.iter_mut() {
                expand_route(w)?;
            }
        }
        if let Some(ss) = &mut self.models.scouts {
            for s in ss.iter_mut() {
                expand_route(&mut s.route)?;
            }
        }
        check_route_shape("investigator", &self.models.investigator)?;
        if let Some(v) = &self.models.validator {
            check_route_shape("validator", v)?;
        }
        if let Some(l) = &self.models.lead {
            check_route_shape("lead", l)?;
        }
        if let Some(ws) = &self.models.workers {
            for (i, w) in ws.iter().enumerate() {
                check_route_shape(&format!("workers[{i}]"), w)?;
            }
        }
        if let Some(ss) = &self.models.scouts {
            for s in ss.iter() {
                check_route_shape(&format!("scouts.{}", s.name), &s.route)?;
            }
        }
        Ok(())
    }

    /// The validator route a run would use: explicit validator, else the
    /// investigator route re-run in a fresh context.
    pub fn effective_validator(&self) -> &ModelRoute {
        self.models
            .validator
            .as_ref()
            .unwrap_or(&self.models.investigator)
    }

    /// True when no explicit validator is configured (inherits investigator).
    pub fn validator_inherited(&self) -> bool {
        self.models.validator.is_none()
    }

    /// Panel cardinality rule used by `doctor` and the panel pipeline.
    pub fn check_panel_lanes(&self) -> Result<()> {
        let n = self.models.scouts.as_ref().map_or(0, |s| s.len());
        check_lane_cardinality(self.panel.focuses.len(), n)
    }

    /// Credential check for only the routes `strategy` would actually use:
    /// always the investigator, plus the effective validator when
    /// `review.validate`, plus the strategy's extra routes.
    pub fn validate_for(&self, strategy: Strategy) -> Result<()> {
        check_route_credentials("investigator", &self.models.investigator)?;
        if self.review.validate {
            check_route_credentials("validator", self.effective_validator())?;
        }
        match strategy {
            Strategy::Baseline => {}
            Strategy::Delegated => {
                if let Some(l) = &self.models.lead {
                    check_route_credentials("lead", l)?;
                }
                if let Some(ws) = &self.models.workers {
                    for (i, w) in ws.iter().enumerate() {
                        check_route_credentials(&format!("workers[{i}]"), w)?;
                    }
                }
            }
            Strategy::Panel => {
                if let Some(ss) = &self.models.scouts {
                    for s in ss.iter() {
                        check_route_credentials(&format!("scouts.{}", s.name), &s.route)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Publication precondition: comments are only ever published from a
    /// run whose candidates went through fresh-context validation.
    pub fn check_publication(&self, publish: PublishMode) -> Result<()> {
        if publish == PublishMode::Comment && !self.review.validate {
            bail!(
                "review.validate=false is evaluation-only and cannot be combined with publish = comment"
            );
        }
        Ok(())
    }

    /// Trust checks that must pass before any credential is resolved:
    /// endpoints must be https (plain http only to loopback), credential
    /// env var names must be well-formed and must not name the GitHub
    /// token, and in event mode the Vera executable must not be a
    /// repository-relative path (PR content must never be executed).
    pub fn check_trust(&self, event_mode: bool) -> Result<()> {
        let gh = self.github.token_env.as_str();
        let check_env = |what: &str, env: &str| -> Result<()> {
            if !is_env_name(env) {
                bail!("{what}: {env:?} is not a valid environment variable name");
            }
            if env == gh || env == "GITHUB_TOKEN" || env == "ACTIONS_RUNTIME_TOKEN" {
                bail!(
                    "{what}: {env} is reserved for GitHub and cannot be sent to a model provider"
                );
            }
            Ok(())
        };
        if !is_env_name(gh) {
            bail!("github.token_env: {gh:?} is not a valid environment variable name");
        }
        let mut routes: Vec<(String, &ModelRoute)> =
            vec![("models.investigator".into(), &self.models.investigator)];
        if let Some(v) = &self.models.validator {
            routes.push(("models.validator".into(), v));
        }
        if let Some(l) = &self.models.lead {
            routes.push(("models.lead".into(), l));
        }
        for (i, w) in self.models.workers.iter().flatten().enumerate() {
            routes.push((format!("models.workers[{i}]"), w));
        }
        for s in self.models.scouts.iter().flatten() {
            routes.push((format!("models.scouts.{}", s.name), &s.route));
        }
        for (name, r) in routes {
            if !r.protocol.is_http() {
                continue;
            }
            if let Some(u) = &r.base_url {
                check_endpoint_url(&format!("{name}.base_url"), u, event_mode)?;
            }
            if let Some(env) = &r.api_key_env {
                check_env(&format!("{name}.api_key_env"), env)?;
            }
        }
        if self.vera.enabled {
            for (name, e) in [
                ("vera.embedding", &self.vera.embedding),
                ("vera.reranker", &self.vera.reranker),
            ] {
                if let Some(e) = e {
                    check_endpoint_url(&format!("{name}.base_url"), &e.base_url, event_mode)?;
                    check_env(&format!("{name}.api_key_env"), &e.api_key_env)?;
                    if let Some(p) = &e.endpoint_path
                        && (!p.starts_with('/') || p.contains("://") || p.contains(".."))
                    {
                        bail!("{name}.endpoint_path must be an absolute path like /rerank");
                    }
                }
            }
            if let Some(e) = &self.vera.embedding
                && (e.protocol.is_some()
                    || e.endpoint_path.is_some()
                    || e.return_documents.is_some())
            {
                bail!(
                    "vera.embedding: protocol/endpoint_path/return_documents apply to vera.reranker only"
                );
            }
            if self.vera.backend == VeraBackend::Api && self.vera.embedding.is_none() {
                bail!("vera.backend: api requires vera.embedding");
            }
            if event_mode {
                let exe = Path::new(&self.vera.executable);
                if !exe.is_absolute() && self.vera.executable.contains(['/', '\\']) {
                    bail!(
                        "vera.executable {:?} is repository-relative; in event mode it must be a bare command name or an absolute path",
                        self.vera.executable
                    );
                }
                if let Some(h) = &self.vera.home
                    && !h.is_absolute()
                {
                    bail!("vera.home must be an absolute path in event mode");
                }
            }
        }
        Ok(())
    }

    /// Review-affecting configuration, hashed into the review identity so a
    /// stored result is only reused when the same review would run again.
    /// Contains no secrets: credential values and their env var names stay out.
    pub fn review_fingerprint(&self, strategy: &str) -> serde_json::Value {
        let route = |r: &ModelRoute| {
            // header names only: values may be interpolated from env vars
            let mut headers: Vec<&str> = r.extra_headers.keys().map(String::as_str).collect();
            headers.sort_unstable();
            serde_json::json!({
                "extra_headers": headers,
                "session_header": r.session_header,
                "protocol": format!("{:?}", r.protocol).to_lowercase(),
                "base_url": r.base_url,
                "model": r.model,
                "max_output_tokens": r.max_output_tokens,
                "temperature": r.temperature,
                "reasoning": r.reasoning.effort().as_str(),
                "reasoning_budget": r.reasoning.effective_budget(),
                "script": r.script.as_ref().map(|p| p.to_string_lossy().to_string()),
            })
        };
        serde_json::json!({
            "engine": env!("CARGO_PKG_VERSION"),
            "prompts": crate::prompts::prompt_version(),
            "strategy": strategy,
            "max_findings": self.review.max_findings,
            "publish_uncertain": self.review.publish_uncertain,
            "min_severity": format!("{:?}", self.review.min_severity).to_lowercase(),
            "validate": self.review.validate,
            "guidance": self.review.guidance.as_str(),
            "guidance_max_bytes": self.review.guidance_max_bytes,
            "concurrency": self.review.concurrency,
            "max_tool_output_bytes": self.review.max_tool_output_bytes,
            "max_diff_bytes": self.review.max_diff_bytes,
            "budget": {
                "agent_max_tool_calls": self.budget.agent_max_tool_calls,
                "agent_max_seconds": self.budget.agent_max_seconds,
                "run_max_requests": self.budget.run_max_requests,
                "run_max_seconds": self.budget.run_max_seconds,
                "retries": self.budget.retries,
            },
            "delegated": {
                "max_questions": self.delegated.max_questions,
                "worker_max_tool_calls": self.delegated.worker_max_tool_calls,
                "worker_max_seconds": self.delegated.worker_max_seconds,
            },
            "panel": {
                "focuses": self.panel.focuses,
                "scout_max_tool_calls": self.panel.scout_max_tool_calls,
            },
            "investigator": route(&self.models.investigator),
            "validator": route(self.effective_validator()),
            "lead": self.models.lead.as_ref().map(route),
            "workers": self.models.workers.as_ref().map(|ws| ws.iter().map(route).collect::<Vec<_>>()),
            "scouts": self.models.scouts.as_ref().map(|ss| {
                ss.iter().map(|s| serde_json::json!({"name": s.name, "focus": s.focus, "route": route(&s.route)})).collect::<Vec<_>>()
            }),
            "vera": {
                "enabled": self.vera.enabled,
                "index": self.vera.index_identity(),
                "reranker": self.vera.reranker_identity(),
            },
        })
    }

    pub fn apply_profile(&mut self, name: &str) -> Result<()> {
        let Some(p) = self.profiles.get(name) else {
            bail!("unknown profile {name:?}");
        };
        if let Some(r) = &p.review {
            if let Some(v) = r.strategy {
                self.review.strategy = v;
            }
            if let Some(v) = r.max_findings {
                self.review.max_findings = v;
            }
            if let Some(v) = r.publish {
                self.review.publish = v;
            }
            if let Some(v) = r.publish_uncertain {
                self.review.publish_uncertain = v;
            }
            if let Some(v) = r.concurrency {
                self.review.concurrency = v;
            }
            if let Some(v) = r.max_tool_output_bytes {
                self.review.max_tool_output_bytes = v;
            }
            if let Some(v) = r.max_diff_bytes {
                self.review.max_diff_bytes = v;
            }
            if let Some(v) = r.min_severity {
                self.review.min_severity = v;
            }
            if let Some(v) = r.guidance {
                self.review.guidance = v;
            }
        }
        if let Some(b) = &p.budget {
            if let Some(v) = b.agent_max_tool_calls {
                self.budget.agent_max_tool_calls = v;
            }
            if let Some(v) = b.agent_max_seconds {
                self.budget.agent_max_seconds = v;
            }
            if let Some(v) = b.run_max_requests {
                self.budget.run_max_requests = v;
            }
            if let Some(v) = b.run_max_seconds {
                self.budget.run_max_seconds = v;
            }
            if let Some(v) = b.retries {
                self.budget.retries = v;
            }
        }
        Ok(())
    }
}
