use super::{ChatMessage, Completion, LedgerEntry, LedgerHandle, ProviderError, ToolSpec, Usage};
use crate::config::{ModelRoute, ReasoningEffort};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Stable id for this process run, used by routes that set `session_header`
/// (e.g. OpenCode Go's `x-opencode-session`).
fn run_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Headers shared by all four HTTP adapters: the route's extra_headers plus
/// its `session_header` carrying the per-run id.
pub fn route_headers(route: &ModelRoute) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = route
        .extra_headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(name) = &route.session_header {
        headers.push((name.clone(), run_session_id().to_string()));
    }
    headers
}

/// Per-request mutable state carried across attempts (retry / same-slot
/// fallbacks). E.g. openai-chat flips `tokens_key` from `max_tokens` to
/// `max_completion_tokens`; openai-responses sets `drop_temperature`.
#[derive(Debug, Clone)]
pub struct AttemptState {
    pub tokens_key: String,
    pub drop_temperature: bool,
    pub drop_reasoning: bool,
    /// After a 400 that rejected the requested effort, cap subsequent
    /// attempts to this level before falling back to dropping reasoning.
    pub reasoning_cap: Option<ReasoningEffort>,
}

impl Default for AttemptState {
    fn default() -> Self {
        Self {
            tokens_key: "max_tokens".into(),
            drop_temperature: false,
            drop_reasoning: false,
            reasoning_cap: None,
        }
    }
}

/// Requested effort after applying `attempt.reasoning_cap`.
pub fn capped_effort(requested: ReasoningEffort, attempt: &AttemptState) -> ReasoningEffort {
    match attempt.reasoning_cap {
        Some(cap) if requested > cap => cap,
        _ => requested,
    }
}

/// What the adapter asks the transport to do with a response.
pub enum Parse {
    /// Successfully parsed assistant message + token usage.
    Ok(ChatMessage, Usage),
    /// Retry this same request shape after mutating AttemptState
    /// (e.g. max_tokens -> max_completion_tokens). Consumes a new
    /// ledger slot like any attempt.
    RetrySameSlot(String),
    /// Fatal parse/HTTP error.
    Err(ProviderError),
}

/// Shared 400 fallback detection for reasoning/thinking fields and
/// temperature (reasoning models reject it). Returns Some(RetrySameSlot)
/// after mutating `attempt`, else None. `reason_keys` are the wire
/// spellings the protocol uses (e.g. &["reasoning"], &["thinking"]).
pub fn detect_400_fallback(
    status: u16,
    body: &str,
    attempt: &mut AttemptState,
    reason_keys: &[&str],
    requested_effort: ReasoningEffort,
) -> Option<Parse> {
    if status != 400 {
        return None;
    }
    if !attempt.drop_reasoning && reason_keys.iter().any(|k| body.contains(k)) {
        // effort step-down: a provider that rejects xhigh/max often still
        // accepts high; only fall through to dropping reasoning when we're
        // already capped or never asked above high.
        if attempt.reasoning_cap.is_none() && requested_effort > ReasoningEffort::High {
            attempt.reasoning_cap = Some(ReasoningEffort::High);
            return Some(Parse::RetrySameSlot(
                "400: retrying with reasoning effort high".into(),
            ));
        }
        attempt.drop_reasoning = true;
        return Some(Parse::RetrySameSlot(
            "400: retrying without reasoning".into(),
        ));
    }
    if !attempt.drop_temperature && body.contains("temperature") {
        attempt.drop_temperature = true;
        return Some(Parse::RetrySameSlot(
            "400: retrying without temperature".into(),
        ));
    }
    None
}

/// A fully materialized HTTP request.
pub struct HttpRequestSpec {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// Synchronous wire-format adapter: builds request specs and interprets
/// responses. No I/O — HttpTransport owns the socket and retry loop.
pub trait ProtocolAdapter: Send + Sync {
    fn label(&self) -> String;
    fn model(&self) -> &str;
    /// Reasoning effort the route was configured with ("none" when off).
    fn requested_reasoning(&self) -> String;
    /// The effort `build()` would emit for this attempt state ("none" when
    /// reasoning is disabled or dropped).
    fn effective_reasoning(&self, attempt: &AttemptState) -> String;
    fn build(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError>;
    fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse;
}

const MAX_RETRY_AFTER_SECS: f64 = 60.0;

/// Per-request ledger identity, captured once per `send()` call.
struct Telemetry {
    label: String,
    model: String,
    requested: String,
}

/// Shared attempt loop: ledger reservation per attempt, 429/5xx + transient
/// retry with Retry-After + exponential backoff + jitter, ledger entries for
/// every attempt (success and failure), latency accounting.
pub struct HttpTransport {
    pub http: reqwest::Client,
    pub ledger: LedgerHandle,
    pub max_requests: u32,
    pub retries: u32,
    /// Pipeline role recorded on every ledger entry.
    pub role: String,
}

impl HttpTransport {
    pub fn new(
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
        role: &str,
    ) -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("revera/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(Self {
            http,
            ledger,
            max_requests,
            retries,
            role: role.to_string(),
        })
    }

    fn record(&self, tel: &Telemetry, effective_reasoning: &str, e: LedgerEntry) {
        self.ledger.record(LedgerEntry {
            role: self.role.clone(),
            route: tel.label.clone(),
            model: tel.model.clone(),
            requested_reasoning: tel.requested.clone(),
            effective_reasoning: effective_reasoning.to_string(),
            ..e
        });
    }

    fn backoff(attempt: u32) -> Duration {
        let backoff = 0.5 * 2f64.powi(attempt.saturating_sub(1) as i32);
        Duration::from_secs_f64(backoff.min(MAX_RETRY_AFTER_SECS))
    }

    pub fn retry_delay(header: Option<&str>, attempt: u32, now: DateTime<Utc>) -> Duration {
        if let Some(value) = header.map(str::trim) {
            if let Ok(seconds) = value.parse::<f64>() {
                if seconds.is_finite() && seconds >= 0.0 {
                    return Duration::from_secs_f64(seconds.min(MAX_RETRY_AFTER_SECS));
                }
            } else if let Ok(date) = DateTime::parse_from_rfc2822(value) {
                let seconds = (date.with_timezone(&Utc) - now)
                    .to_std()
                    .unwrap_or_default();
                return seconds.min(Duration::from_secs_f64(MAX_RETRY_AFTER_SECS));
            }
        }
        Self::backoff(attempt)
    }

    pub async fn send<A: ProtocolAdapter>(
        &self,
        adapter: &A,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let tel = Telemetry {
            label: adapter.label(),
            model: adapter.model().to_string(),
            requested: adapter.requested_reasoning(),
        };
        let mut attempt = AttemptState::default();
        let mut attempts = 0u32;
        let mut retries_used = 0u32;
        let start = Instant::now();
        loop {
            // reserve a slot for every HTTP attempt, including retries and
            // same-slot fallbacks
            if !self.ledger.try_reserve(self.max_requests) {
                return Err(ProviderError::BudgetExhausted);
            }
            attempts += 1;
            let spec = match adapter.build(messages, tools, &mut attempt) {
                Ok(s) => s,
                Err(e) => {
                    self.ledger.release();
                    self.record(
                        &tel,
                        &adapter.effective_reasoning(&attempt),
                        LedgerEntry {
                            latency_ms: start.elapsed().as_millis() as u64,
                            retries: retries_used,
                            error: Some(e.to_string()),
                            ..Default::default()
                        },
                    );
                    return Err(e);
                }
            };
            // effort actually sent on this attempt; captured right after
            // build() so later parse-time mutations don't rewrite history
            let effective = adapter.effective_reasoning(&attempt);
            let mut req = self.http.post(&spec.url).json(&spec.body);
            for (k, v) in &spec.headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let result = req.send().await;
            match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .map(str::to_string);
                    let text = resp.text().await.unwrap_or_default();
                    if status == 429 || status >= 500 {
                        if attempts <= self.retries {
                            self.record(
                                &tel,
                                &effective,
                                LedgerEntry {
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    error: Some(format!("HTTP {status} (will retry)")),
                                    ..Default::default()
                                },
                            );
                            retries_used += 1;
                            let d =
                                Self::retry_delay(retry_after.as_deref(), retries_used, Utc::now());
                            tokio::time::sleep(d).await;
                            continue;
                        }
                        let err = ProviderError::RetryExhausted(format!(
                            "HTTP {status}: {}",
                            crate::text::excerpt_bytes(&text, 400)
                        ));
                        self.record(
                            &tel,
                            &effective,
                            LedgerEntry {
                                latency_ms: start.elapsed().as_millis() as u64,
                                retries: retries_used,
                                error: Some(err.to_string()),
                                ..Default::default()
                            },
                        );
                        return Err(err);
                    }
                    match adapter.parse(status, &text, &mut attempt) {
                        Parse::Ok(message, usage) => {
                            let latency = start.elapsed().as_millis() as u64;
                            self.record(
                                &tel,
                                &effective,
                                LedgerEntry {
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                    reasoning_tokens: usage.reasoning_tokens,
                                    cached_tokens: usage.cached_tokens,
                                    latency_ms: latency,
                                    retries: retries_used,
                                    error: None,
                                    ..Default::default()
                                },
                            );
                            tracing::debug!(
                                route = tel.label,
                                model = tel.model,
                                prompt_tokens = usage.prompt_tokens,
                                latency_ms = latency,
                                "model request"
                            );
                            return Ok(Completion {
                                message,
                                usage,
                                model: tel.model.clone(),
                                latency_ms: latency,
                            });
                        }
                        Parse::RetrySameSlot(reason) => {
                            self.record(
                                &tel,
                                &effective,
                                LedgerEntry {
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    error: Some(reason),
                                    ..Default::default()
                                },
                            );
                            continue;
                        }
                        Parse::Err(e) => {
                            self.record(
                                &tel,
                                &effective,
                                LedgerEntry {
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    retries: retries_used,
                                    error: Some(e.to_string()),
                                    ..Default::default()
                                },
                            );
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    let transient = e.is_timeout() || e.is_connect();
                    if transient && attempts <= self.retries {
                        self.record(
                            &tel,
                            &effective,
                            LedgerEntry {
                                latency_ms: start.elapsed().as_millis() as u64,
                                error: Some(format!("{e} (will retry)")),
                                ..Default::default()
                            },
                        );
                        retries_used += 1;
                        tokio::time::sleep(Self::retry_delay(None, retries_used, Utc::now())).await;
                        continue;
                    }
                    let err = ProviderError::Http(e.to_string());
                    self.record(
                        &tel,
                        &effective,
                        LedgerEntry {
                            latency_ms: start.elapsed().as_millis() as u64,
                            retries: retries_used,
                            error: Some(err.to_string()),
                            ..Default::default()
                        },
                    );
                    return Err(err);
                }
            }
        }
    }
}

/// A ModelClient = adapter (wire format) + transport (I/O + retries).
pub struct HttpClient<A: ProtocolAdapter> {
    pub adapter: A,
    pub transport: HttpTransport,
}

#[async_trait::async_trait]
impl<A: ProtocolAdapter> super::ModelClient for HttpClient<A> {
    fn route_label(&self) -> String {
        self.adapter.label()
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        self.transport.send(&self.adapter, messages, tools).await
    }
}
