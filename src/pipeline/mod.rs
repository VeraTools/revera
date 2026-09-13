pub mod anchor;
pub mod baseline;
pub mod validate;

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
