//! Dashboard connectivity probes reuse the review adapters and HTTP transport.
use super::http::{
    AttemptState, HttpClient, HttpRequestSpec, HttpTransport, Parse, ProtocolAdapter,
};
use super::{ChatMessage, LedgerHandle, ModelClient, ProviderError, Usage};
use crate::config::{ModelRoute, Protocol, VeraEndpoint};
use serde_json::{Value, json};

fn transport(role: &str, requests: u32, retries: u32) -> Result<HttpTransport, ProviderError> {
    let mut transport = HttpTransport::new(LedgerHandle::new(), requests, retries, role)?;
    transport.http = crate::tls::with_bundled_roots(reqwest::Client::builder())
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("revera/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| ProviderError::Other(error.to_string()))?;
    Ok(transport)
}

pub async fn model(route: ModelRoute, key: String) -> Result<(), ProviderError> {
    let transport = transport("connectivity", 3, 1)?;
    let client: Box<dyn ModelClient> = match route.protocol {
        Protocol::OpenaiChat => Box::new(HttpClient {
            adapter: super::openai_chat::OpenAiChatAdapter::with_api_key(route, key),
            transport,
        }),
        Protocol::OpenaiResponses => Box::new(HttpClient {
            adapter: super::openai_responses::OpenAiResponsesAdapter::with_api_key(route, key),
            transport,
        }),
        Protocol::Anthropic => Box::new(HttpClient {
            adapter: super::anthropic::AnthropicAdapter::with_api_key(route, key),
            transport,
        }),
        Protocol::Gemini => Box::new(HttpClient {
            adapter: super::gemini::GeminiAdapter::with_api_key(route, key),
            transport,
        }),
        Protocol::Scripted => {
            return Err(ProviderError::Other(
                "connectivity tests require an HTTP protocol".into(),
            ));
        }
    };
    client
        .complete(&[ChatMessage::user("Reply with OK.")], &[])
        .await?;
    Ok(())
}

struct Embedding {
    endpoint: VeraEndpoint,
    key: String,
}
impl ProtocolAdapter for Embedding {
    fn label(&self) -> String {
        "embedding-connectivity".into()
    }
    fn model(&self) -> &str {
        &self.endpoint.model
    }
    fn requested_reasoning(&self) -> String {
        "none".into()
    }
    fn effective_reasoning(&self, _: &AttemptState) -> String {
        "none".into()
    }
    fn build(
        &self,
        _: &[ChatMessage],
        _: &[super::ToolSpec],
        _: &mut AttemptState,
    ) -> Result<HttpRequestSpec, ProviderError> {
        Ok(HttpRequestSpec {
            url: format!(
                "{}/embeddings",
                self.endpoint.base_url.trim_end_matches('/')
            ),
            headers: vec![("Authorization".into(), format!("Bearer {}", self.key))],
            body: json!({"model": self.endpoint.model, "input": ["connectivity test"]}),
        })
    }
    fn parse(&self, status: u16, body: &str, _: &mut AttemptState) -> Parse {
        let value = serde_json::from_str::<Value>(body).ok();
        if (200..300).contains(&status)
            && value
                .as_ref()
                .and_then(|v| v["data"][0]["embedding"].as_array())
                .is_some_and(|v| !v.is_empty())
        {
            Parse::Ok(ChatMessage::user("ok"), Usage::default())
        } else {
            Parse::Err(ProviderError::Http(format!(
                "embedding probe: HTTP {status}"
            )))
        }
    }
}

pub async fn embedding(endpoint: VeraEndpoint, key: String) -> Result<(), ProviderError> {
    crate::redact::register(&key);
    let client = HttpClient {
        adapter: Embedding { endpoint, key },
        transport: transport("embedding-connectivity", 3, 1)?,
    };
    client.complete(&[], &[]).await?;
    Ok(())
}

pub async fn models(route: &ModelRoute, key: &str) -> Result<Vec<String>, ProviderError> {
    let transport = transport("model-list", 1, 0)?;
    let base = route
        .base_url
        .as_deref()
        .unwrap_or(match route.protocol {
            Protocol::Anthropic => "https://api.anthropic.com",
            Protocol::Gemini => "https://generativelanguage.googleapis.com",
            _ => "",
        })
        .trim_end_matches('/');
    let (url, auth) = match route.protocol {
        Protocol::OpenaiChat | Protocol::OpenaiResponses => (
            format!("{base}/models"),
            ("Authorization", format!("Bearer {key}")),
        ),
        Protocol::Anthropic => (
            format!("{}/v1/models", base.trim_end_matches("/v1")),
            ("x-api-key", key.into()),
        ),
        Protocol::Gemini => (
            format!("{base}/v1beta/models"),
            ("x-goog-api-key", key.into()),
        ),
        Protocol::Scripted => return Ok(vec![]),
    };
    let mut request = transport.http.get(url).header(auth.0, auth.1);
    if route.protocol == Protocol::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    for (name, value) in super::http::route_headers(route) {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| ProviderError::Http(crate::redact::diagnostic(&e.to_string())))?;
    if !response.status().is_success() {
        return Err(ProviderError::Http(format!(
            "model list: HTTP {}",
            response.status()
        )));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;
    let items = value["data"]
        .as_array()
        .or_else(|| value["models"].as_array());
    Ok(items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item["id"]
                .as_str()
                .or_else(|| item["name"].as_str())
                .map(str::to_owned)
        })
        .collect())
}
