use crate::findings::Severity;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
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
    /// Eval-only knob: skip the validation pass and treat candidates as
    /// accepted (warns in the log). Default true.
    #[serde(default = "default_true")]
    pub validate: bool,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
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
    pub validator: ModelRoute,
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VeraConfig {
    #[serde(default = "default_vera_exe")]
    pub executable: String,
    pub version: Option<String>,
    /// When false: skip indexing and remove the vera_* tools entirely.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub backend: VeraBackend,
    pub embedding: Option<VeraEndpoint>,
    pub reranker: Option<VeraEndpoint>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default = "default_marker")]
    pub summary_marker: String,
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
    pub review: ReviewConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    pub models: ModelsConfig,
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

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            token_env: default_token_env(),
            summary_marker: default_marker(),
            allow_forks: false,
        }
    }
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
    if r.session_header.is_none()
        && r.base_url.as_deref().is_some_and(|b| {
            b.split("://")
                .nth(1)
                .unwrap_or(b)
                .split('/')
                .next()
                .unwrap_or("")
                .ends_with("opencode.ai")
        })
    {
        r.session_header = Some("x-opencode-session".into());
    }
    Ok(())
}

fn validate_route(name: &str, r: &ModelRoute) -> Result<()> {
    match r.protocol {
        Protocol::OpenaiChat | Protocol::OpenaiResponses => {
            if r.base_url.as_deref().is_none_or(|s| s.is_empty()) {
                bail!("models.{name}: protocol {:?} requires base_url", r.protocol);
            }
        }
        // anthropic/gemini have default base_urls; api key still required
        Protocol::Anthropic | Protocol::Gemini => {}
        Protocol::Scripted => {
            if r.script.is_none() {
                bail!("models.{name}: protocol scripted requires script path");
            }
            return Ok(());
        }
    }
    // every HTTP protocol requires api_key_env
    if r.protocol.is_http() {
        let env = r.api_key_env.as_deref().unwrap_or_default();
        if env.is_empty() {
            bail!(
                "models.{name}: protocol {:?} requires api_key_env",
                r.protocol
            );
        }
        if std::env::var(env).is_err() {
            bail!("models.{name}: api_key_env {env} is not set in the environment");
        }
    }
    Ok(())
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let text = expand_env(&text)?;
        let mut cfg: Config = serde_yaml::from_str(&text)
            .with_context(|| format!("invalid config {}", path.display()))?;
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
        expand_route(&mut self.models.validator)?;
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
        validate_route("investigator", &self.models.investigator)?;
        validate_route("validator", &self.models.validator)?;
        if let Some(l) = &self.models.lead {
            validate_route("lead", l)?;
        }
        if let Some(ws) = &self.models.workers {
            for (i, w) in ws.iter().enumerate() {
                validate_route(&format!("workers[{i}]"), w)?;
            }
        }
        if let Some(ss) = &self.models.scouts {
            for s in ss.iter() {
                validate_route(&format!("scouts.{}", s.name), &s.route)?;
            }
        }
        Ok(())
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
