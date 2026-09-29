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
use crate::provider::anthropic::AnthropicAdapter;
use crate::provider::gemini::GeminiAdapter;
use crate::provider::http::HttpClient;
use crate::provider::openai_chat::OpenAiChatClient;
use crate::provider::openai_responses::OpenAiResponsesAdapter;
use crate::provider::scripted::ScriptedClient;
use crate::provider::{LedgerHandle, ModelClient, ProviderError};
use std::sync::Arc;

pub fn make_client(
    route: &ModelRoute,
    role: &str,
    cache_key: Option<String>,
    ledger: LedgerHandle,
    max_requests: u32,
    retries: u32,
    terminal_tool: &str,
) -> Result<Arc<dyn ModelClient>, ProviderError> {
    match route.protocol {
        Protocol::OpenaiChat => Ok(Arc::new(OpenAiChatClient::new_with_cache_key(
            route.clone(),
            ledger,
            max_requests,
            retries,
            role,
            cache_key,
        )?)),
        Protocol::OpenaiResponses => Ok(Arc::new(HttpClient {
            adapter: OpenAiResponsesAdapter::from_route(route.clone())?.with_cache_key(cache_key),
            transport: crate::provider::http::HttpTransport::new(
                ledger,
                max_requests,
                retries,
                role,
            )?,
        })),
        Protocol::Anthropic => Ok(Arc::new(HttpClient {
            adapter: AnthropicAdapter::from_route(route.clone())?,
            transport: crate::provider::http::HttpTransport::new(
                ledger,
                max_requests,
                retries,
                role,
            )?,
        })),
        Protocol::Gemini => Ok(Arc::new(HttpClient {
            adapter: GeminiAdapter::from_route(route.clone())?,
            transport: crate::provider::http::HttpTransport::new(
                ledger,
                max_requests,
                retries,
                role,
            )?,
        })),
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

/// Deterministic, credential-free cache identity for one review strategy and lane.
pub fn cache_key(cfg: &Config, strategy: &str, role: &str, file: Option<&str>) -> String {
    use sha2::{Digest, Sha256};

    let fingerprint = serde_json::to_string(&cfg.review_fingerprint(strategy))
        .expect("serializing a JSON value cannot fail");
    let material = format!("{fingerprint}\0{role}\0{}", file.unwrap_or(""));
    let digest = hex::encode(Sha256::digest(material.as_bytes()));
    format!("revera-{}", &digest[..16])
}

#[cfg(test)]
mod tests {
    use super::cache_key;
    use crate::config::Config;

    #[test]
    fn cache_key_is_stable_lane_specific_and_credential_free() {
        let cfg = Config::parse(
            r#"
models:
  investigator:
    protocol: openai-chat
    base_url: https://api.example.com/v1
    api_key_env: CACHE_KEY_SECRET_ENV_NAME
    model: test-model
    extra_headers:
      x-test-secret: CACHE_KEY_SECRET_VALUE
vera: {enabled: false}
"#,
            "cache-key-test",
        )
        .unwrap();
        assert!(cfg.models.investigator.cache);
        let fingerprint = cfg.review_fingerprint("baseline");
        let mut cache_disabled = cfg.clone();
        cache_disabled.models.investigator.cache = false;
        assert_eq!(fingerprint, cache_disabled.review_fingerprint("baseline"));
        let first = cache_key(&cfg, "baseline", "investigator", None);
        assert!(first.starts_with("revera-"));
        assert_eq!(first.len(), "revera-".len() + 16);
        assert_eq!(first, cache_key(&cfg, "baseline", "investigator", None));
        assert_ne!(first, cache_key(&cfg, "baseline", "validator", None));
        assert_ne!(
            first,
            cache_key(&cfg, "baseline", "investigator", Some("src/a.rs"))
        );
        assert!(!first.contains("CACHE_KEY_SECRET_ENV_NAME"));
        assert!(!first.contains("CACHE_KEY_SECRET_VALUE"));
    }
}
