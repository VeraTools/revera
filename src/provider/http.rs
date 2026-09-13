use super::{ChatMessage, Completion, LedgerEntry, LedgerHandle, ProviderError, ToolSpec, Usage};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Per-request mutable state carried across attempts (retry / same-slot
/// fallbacks). E.g. openai-chat flips `tokens_key` from `max_tokens` to
/// `max_completion_tokens`; openai-responses sets `drop_temperature`.
#[derive(Debug, Clone)]
pub struct AttemptState {
    pub tokens_key: String,
    pub drop_temperature: bool,
}

impl Default for AttemptState {
    fn default() -> Self {
        Self {
            tokens_key: "max_tokens".into(),
            drop_temperature: false,
        }
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
    fn build(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError>;
    fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse;
}

/// Shared attempt loop: ledger reservation per attempt, 429/5xx + transient
/// retry with Retry-After + exponential backoff + jitter, ledger entries for
/// every attempt (success and failure), latency accounting.
pub struct HttpTransport {
    pub http: reqwest::Client,
    pub ledger: LedgerHandle,
    pub max_requests: u32,
    pub retries: u32,
}

impl HttpTransport {
    pub fn new(
        ledger: LedgerHandle,
        max_requests: u32,
        retries: u32,
    ) -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(Self {
            http,
            ledger,
            max_requests,
            retries,
        })
    }

    fn record(&self, adapter_label: &str, model: &str, e: LedgerEntry) {
        let _ = (adapter_label, model);
        self.ledger.record(LedgerEntry {
            route: adapter_label.to_string(),
            model: model.to_string(),
            ..e
        });
    }

    fn backoff(attempt: u32) -> Duration {
        let backoff = 0.5 * 2f64.powi(attempt as i32 - 1);
        let jitter = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            % 250) as f64
            / 1000.0;
        Duration::from_secs_f64(backoff + jitter)
    }

    pub async fn send<A: ProtocolAdapter>(
        &self,
        adapter: &A,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let label = adapter.label();
        let model = adapter.model();
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
                        &label,
                        model,
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
                        .and_then(|s| s.parse::<f64>().ok());
                    let text = resp.text().await.unwrap_or_default();
                    if status == 429 || status >= 500 {
                        if attempts <= self.retries {
                            self.record(
                                &label,
                                model,
                                LedgerEntry {
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    error: Some(format!("HTTP {status} (will retry)")),
                                    ..Default::default()
                                },
                            );
                            retries_used += 1;
                            let d = retry_after
                                .map(Duration::from_secs_f64)
                                .unwrap_or_else(|| Self::backoff(retries_used));
                            tokio::time::sleep(d).await;
                            continue;
                        }
                        let err = ProviderError::RetryExhausted(format!(
                            "HTTP {status}: {}",
                            crate::text::excerpt_bytes(&text, 400)
                        ));
                        self.record(
                            &label,
                            model,
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
                                &label,
                                model,
                                LedgerEntry {
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                    latency_ms: latency,
                                    retries: retries_used,
                                    error: None,
                                    ..Default::default()
                                },
                            );
                            tracing::debug!(
                                route = label,
                                model,
                                prompt_tokens = usage.prompt_tokens,
                                latency_ms = latency,
                                "model request"
                            );
                            return Ok(Completion {
                                message,
                                usage,
                                model: model.to_string(),
                                latency_ms: latency,
                            });
                        }
                        Parse::RetrySameSlot(reason) => {
                            self.record(
                                &label,
                                model,
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
                                &label,
                                model,
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
                            &label,
                            model,
                            LedgerEntry {
                                latency_ms: start.elapsed().as_millis() as u64,
                                error: Some(format!("{e} (will retry)")),
                                ..Default::default()
                            },
                        );
                        retries_used += 1;
                        tokio::time::sleep(Self::backoff(retries_used)).await;
                        continue;
                    }
                    let err = ProviderError::Http(e.to_string());
                    self.record(
                        &label,
                        model,
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
