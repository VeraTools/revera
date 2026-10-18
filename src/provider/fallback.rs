//! Ordered, sticky alternatives around protocol-neutral ModelClient requests.
use super::{
    ChatMessage, Completion, FallbackEvent, FallbackReason, LedgerHandle, ModelClient,
    ProviderError, ToolSpec, UnavailableRoute,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub struct FallbackClient {
    routes: Vec<(String, Option<Arc<dyn ModelClient>>)>,
    role: String,
    ledger: LedgerHandle,
    max_requests: u32,
    position: Arc<AtomicUsize>,
}

impl FallbackClient {
    pub fn new(
        routes: Vec<(String, Option<Arc<dyn ModelClient>>)>,
        role: &str,
        ledger: LedgerHandle,
        max_requests: u32,
    ) -> Self {
        let position = {
            let mut run = ledger.0.lock().unwrap();
            for (model, client) in &routes {
                if client.is_none()
                    && !run
                        .unavailable_routes
                        .iter()
                        .any(|r| r.role == role && r.model == *model)
                {
                    tracing::warn!(
                        role = %crate::redact::text(role),
                        model = %crate::redact::text(model),
                        "model route unavailable: API key missing or empty; skipping route"
                    );
                    run.unavailable_routes.push(UnavailableRoute {
                        role: crate::redact::text(role).into_owned(),
                        model: crate::redact::text(model).into_owned(),
                        reason: FallbackReason::MissingKey,
                    });
                }
            }
            // Roles such as delegated workers can hold different chains, so the
            // sticky position belongs to the role and its exact route list.
            let mut key = role.to_string();
            for (model, client) in &routes {
                let label = client.as_ref().map(|c| c.route_label());
                key.push_str(&format!("\0{model}\0{}", label.unwrap_or_default()));
            }
            run.fallback_positions.entry(key).or_default().clone()
        };
        Self {
            routes,
            role: role.into(),
            ledger,
            max_requests,
            position,
        }
    }

    fn has_budget(&self) -> bool {
        let run = self.ledger.0.lock().unwrap();
        run.entries.len() as u32 + run.reserved < self.max_requests
    }
}

#[async_trait::async_trait]
impl ModelClient for FallbackClient {
    fn route_label(&self) -> String {
        self.routes[self.position.load(Ordering::Acquire)]
            .1
            .as_ref()
            .map(|c| c.route_label())
            .unwrap_or_else(|| "unavailable".into())
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<Completion, ProviderError> {
        let start = self.position.load(Ordering::Acquire);
        let mut failed = None;
        let mut last_error = ProviderError::MissingKey("no available route".into());
        for (index, (model, client)) in self.routes.iter().enumerate().skip(start) {
            let Some(client) = client else {
                if failed.is_none() {
                    failed = Some((model.clone(), FallbackReason::MissingKey));
                }
                continue;
            };
            if !self.has_budget() {
                return Err(ProviderError::BudgetExhausted);
            }
            // Opaque state is route-owned, not protocol-neutral. Strip state from
            // other routes, including same-protocol models at different endpoints.
            // All adapters receive the FULL transcript; none resumes server-side state.
            let replay: Vec<_> = messages
                .iter()
                .cloned()
                .map(|mut message| {
                    message.provider_state = message.provider_state.and_then(|state| {
                        (state["revera_route"].as_u64() == Some(index as u64))
                            .then(|| state["state"].clone())
                    });
                    message
                })
                .collect();
            let result = client.complete(&replay, tools).await;
            if !matches!(result, Err(ProviderError::BudgetExhausted))
                && let Some((from_model, reason)) = &failed
            {
                self.ledger
                    .0
                    .lock()
                    .unwrap()
                    .fallback_events
                    .push(FallbackEvent {
                        role: crate::redact::text(&self.role).into_owned(),
                        from_model: crate::redact::text(from_model).into_owned(),
                        to_model: crate::redact::text(model).into_owned(),
                        reason: *reason,
                    });
            }
            match result {
                Ok(mut completion) => {
                    self.position.fetch_max(index, Ordering::AcqRel);
                    completion.message.provider_state = completion
                        .message
                        .provider_state
                        .map(|state| serde_json::json!({"revera_route": index, "state": state}));
                    return Ok(completion);
                }
                Err(error) => {
                    let Some(reason) = error.fallback_reason() else {
                        return Err(error);
                    };
                    failed = Some((model.clone(), reason));
                    last_error = error;
                }
            }
        }
        Err(last_error)
    }
}
