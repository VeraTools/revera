use crate::findings::Severity;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathInstruction {
    pub path: String,
    pub instructions: String,
}

impl PathInstruction {
    pub fn matches(&self, path: &str) -> bool {
        if let Ok(glob) = globset::Glob::new(&self.path) {
            let matcher = glob.compile_matcher();
            matcher.is_match(path)
        } else {
            false
        }
    }
}

/// Review profiles governing sensitivity and publication severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewProfile {
    Quiet,
    Chill,
    Assertive,
}

impl ReviewProfile {
    pub fn default_min_severity(self) -> Severity {
        match self {
            Self::Quiet => Severity::High,
            Self::Chill => Severity::Medium,
            Self::Assertive => Severity::Low,
        }
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
    /// Eval-only knob: skip the validation pass and treat candidates as
    /// accepted (warns in the log). Default true.
    #[serde(default = "default_true")]
    pub validate: bool,
    #[serde(default)]
    pub review_profile: Option<ReviewProfile>,
    #[serde(default)]
    pub path_instructions: Vec<PathInstruction>,
    #[serde(default)]
    pub fail_on_severity: Option<Severity>,
    #[serde(default)]
    pub knowledge_base: Vec<String>,
    /// Add AGENTS.md / CLAUDE.md / REVIEW.md / Copilot instruction files
    /// from the base revision to reviewer prompts.
    #[serde(default = "default_true")]
    pub instruction_files: bool,
    /// Investigator passes for the baseline strategy (1-3); later passes
    /// look for defects the earlier ones did not report.
    #[serde(default = "default_recall_rounds")]
    pub recall_rounds: u32,
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
    /// Resolve Revera's own review threads once the recheck validator marks
    /// their finding resolved (default true).
    #[serde(default = "default_true")]
    pub resolve_threads: bool,
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
pub struct PersonaConfig {
    pub name: String,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub route: Option<ModelRoute>,
}

#[derive(Debug, Clone)]
pub struct EffectiveLane {
    pub name: String,
    pub focus: Option<String>,
    pub custom_prompt: Option<String>,
    pub route: ModelRoute,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    #[serde(default = "default_focuses")]
    pub focuses: Vec<String>,
    #[serde(default = "default_scout_tool_calls")]
    pub scout_max_tool_calls: u32,
    #[serde(default)]
    pub personas: Option<Vec<PersonaConfig>>,
    /// Optional TypeSafe-backed selection of which lanes run per diff.
    #[serde(default)]
    pub lens_router: Option<crate::pipeline::lens_router::LensRouterConfig>,
}

impl PanelConfig {
    /// Resolve the list of effective scout lanes:
    /// (lane_name, focus_or_prompt_key, optional_custom_prompt, route).
    pub fn effective_lanes(
        &self,
        default_route: &ModelRoute,
        scouts: Option<&[ScoutRoute]>,
    ) -> Result<Vec<EffectiveLane>> {
        if let Some(personas) = &self.personas {
            if !personas.is_empty() {
                let mut lanes = Vec::with_capacity(personas.len());
                for p in personas {
                    let route = if let Some(r) = &p.route {
                        r.clone()
                    } else if let Some(scouts_list) = scouts {
                        if let Some(s) = scouts_list.iter().find(|s| {
                            s.name == p.name
                                || p.focus.as_deref() == Some(&s.name)
                                || (p.focus.is_some() && p.focus == s.focus)
                        }) {
                            s.route.clone()
                        } else if scouts_list.len() == 1 {
                            scouts_list[0].route.clone()
                        } else {
                            default_route.clone()
                        }
                    } else {
                        default_route.clone()
                    };
                    lanes.push(EffectiveLane {
                        name: p.name.clone(),
                        focus: p.focus.clone(),
                        custom_prompt: p.prompt.clone(),
                        route,
                    });
                }
                return Ok(lanes);
            }
        }
        let routes = match scouts {
            None | Some([]) => vec![default_route.clone(); self.focuses.len()],
            Some([single]) => vec![single.route.clone(); self.focuses.len()],
            Some(many) => {
                check_lane_cardinality(self.focuses.len(), many.len())?;
                many.iter().map(|s| s.route.clone()).collect()
            }
        };
        Ok(self
            .focuses
            .iter()
            .cloned()
            .zip(routes)
            .map(|(focus, route)| EffectiveLane {
                name: focus.clone(),
                focus: Some(focus),
                custom_prompt: None,
                route,
            })
            .collect())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    pub id: String,
    pub pattern: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub severity: Severity,
    pub message: String,
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
    pub review_profile: Option<ReviewProfile>,
    pub path_instructions: Option<Vec<PathInstruction>>,
    pub fail_on_severity: Option<Severity>,
    pub knowledge_base: Option<Vec<String>>,
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
    pub rules: Option<Vec<RuleConfig>>,
    #[serde(default)]
    pub triage: crate::triage::TriageConfig,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileOverride>,
}

fn default_true() -> bool {
    true
}
fn default_recall_rounds() -> u32 {
    1
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
            review_profile: None,
            path_instructions: Vec::new(),
            fail_on_severity: None,
            knowledge_base: Vec::new(),
            instruction_files: true,
            recall_rounds: default_recall_rounds(),
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
            personas: None,
            lens_router: None,
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
        }
    }
}

impl VeraConfig {
    /// Stable identity of everything that shapes the on-disk index (not the
    /// review models): used for cache keys and compatibility checks.
    pub fn index_identity(&self) -> serde_json::Value {
        let ep = |e: &Option<VeraEndpoint>| {
            e.as_ref()
                .map(|e| serde_json::json!({"base_url": e.base_url, "model": e.model}))
        };
        serde_json::json!({
            "version": self.version,
            "backend": format!("{:?}", self.backend).to_lowercase(),
            "embedding": ep(&self.embedding),
            "reranker": ep(&self.reranker),
            "exclude": self.exclude,
        })
    }
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            token_env: default_token_env(),
            summary_marker: default_marker(),
            bot_login: default_bot_login(),
            allow_forks: false,
            resolve_threads: true,
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
    if let Some(h) = &r.session_header {
        if h.trim().is_empty() || reqwest::header::HeaderName::from_bytes(h.as_bytes()).is_err() {
            bail!("models.{name}: session_header {h:?} is not a valid HTTP header name");
        }
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
        self.triage.validate()?;
        if !(1..=3).contains(&self.review.recall_rounds) {
            bail!(
                "review.recall_rounds must be 1, 2 or 3 (got {})",
                self.review.recall_rounds
            );
        }
        if let Some(r) = &self.panel.lens_router {
            r.validate()?;
        }
        if let Some(rp) = self.review.review_profile {
            if self.review.min_severity == Severity::Low {
                self.review.min_severity = rp.default_min_severity();
            }
        }
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
        if let Some(ps) = &mut self.panel.personas {
            for p in ps.iter_mut() {
                if let Some(r) = &mut p.route {
                    expand_route(r)?;
                }
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
        if let Some(ps) = &self.panel.personas {
            for p in ps.iter() {
                if let Some(r) = &p.route {
                    check_route_shape(&format!("panel.personas.{}", p.name), r)?;
                }
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
        if let Some(ps) = &self.panel.personas {
            if !ps.is_empty() {
                return Ok(());
            }
        }
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
                if let Some(ps) = &self.panel.personas {
                    for p in ps.iter() {
                        if let Some(r) = &p.route {
                            check_route_credentials(&format!("panel.personas.{}", p.name), r)?;
                        }
                    }
                }
                if let Some(r) = &self.panel.lens_router {
                    r.check_credentials()?;
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
            "review_profile": self.review.review_profile.map(|p| match p {
                ReviewProfile::Quiet => "quiet",
                ReviewProfile::Chill => "chill",
                ReviewProfile::Assertive => "assertive",
            }),
            "path_instructions": self.review.path_instructions,
            "knowledge_base": self.review.knowledge_base,
            "instruction_files": self.review.instruction_files,
            "recall_rounds": self.review.recall_rounds,
            "validate": self.review.validate,
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
                "lens_router": self.panel.lens_router.as_ref().map(|r| r.fingerprint()),
                "personas": self.panel.personas.as_ref().map(|ps| {
                    ps.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "focus": p.focus,
                        "prompt": p.prompt,
                        "route": p.route.as_ref().map(route),
                    })).collect::<Vec<_>>()
                }),
            },
            "rules": self.rules.as_ref().map(|rs| {
                rs.iter().map(|r| serde_json::json!({
                    "id": r.id,
                    "pattern": r.pattern,
                    "files": r.files,
                    "severity": r.severity.to_string(),
                    "message": r.message,
                })).collect::<Vec<_>>()
            }),
            "triage": self.triage,
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
            if let Some(rp) = r.review_profile {
                self.review.review_profile = Some(rp);
                if r.min_severity.is_none() {
                    self.review.min_severity = rp.default_min_severity();
                }
            }
            if let Some(v) = &r.path_instructions {
                self.review.path_instructions = v.clone();
            }
            if let Some(v) = r.fail_on_severity {
                self.review.fail_on_severity = Some(v);
            }
            if let Some(v) = &r.knowledge_base {
                self.review.knowledge_base = v.clone();
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
