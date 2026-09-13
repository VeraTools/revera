pub mod anchor;
pub mod baseline;
pub mod common;
pub mod delegated;
pub mod panel;
pub mod validate;

use crate::config::{Config, Strategy};
use crate::report::RunReport;
use crate::state::ReviewState;
use common::ReviewRequest;

/// Dispatch on the configured (or --strategy-overridden) strategy.
pub async fn run(cfg: &Config, req: &ReviewRequest) -> anyhow::Result<(RunReport, ReviewState)> {
    let strategy = req.strategy_override.unwrap_or(cfg.review.strategy);
    match strategy {
        Strategy::Baseline => baseline::run(cfg, req).await,
        Strategy::Delegated => delegated::run(cfg, req).await,
        Strategy::Panel => panel::run(cfg, req).await,
    }
}

use crate::config::{ModelRoute, Protocol};
use crate::provider::openai_chat::OpenAiChatClient;
use crate::provider::scripted::ScriptedClient;
use crate::provider::{LedgerHandle, ModelClient, ProviderError};
use std::sync::Arc;

pub fn make_client(
    route: &ModelRoute,
    role: &str,
    ledger: LedgerHandle,
    max_requests: u32,
    retries: u32,
    terminal_tool: &str,
) -> Result<Arc<dyn ModelClient>, ProviderError> {
    match route.protocol {
        Protocol::OpenaiChat => Ok(Arc::new(OpenAiChatClient::new(
            route.clone(),
            ledger,
            max_requests,
            retries,
        )?)),
        Protocol::Scripted => {
            let p = route.script.clone().ok_or_else(|| {
                ProviderError::Other(format!("role {role}: scripted route needs script path"))
            })?;
            Ok(Arc::new(ScriptedClient::new(
                &p,
                role,
                ledger,
                terminal_tool,
            )?))
        }
    }
}
