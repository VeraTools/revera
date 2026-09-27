//! Real `vera` binary against mock embedding/reranker HTTP services.
//! Skipped (with a note) when no `vera` executable is available; set
//! `REVERA_TEST_VERA` to an executable path to force a specific binary.

use revera::config::Config;
use revera::vera::{RerankState, VeraClient};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const EMBED_KEY: &str = "sk-embed-test-0123456789abcdef";
const RERANK_KEY: &str = "sk-rerank-test-0123456789abcdef";

fn vera_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("REVERA_TEST_VERA") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join("vera"))
        .find(|p| p.is_file())
}

struct Embed;
impl Respond for Embed {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let v: Value = serde_json::from_slice(&req.body).unwrap();
        let n = v["input"].as_array().map(|a| a.len()).unwrap_or(1);
        let data: Vec<Value> = (0..n)
            .map(|i| {
                let e: Vec<f32> = (0..8).map(|j| ((i + j) % 5) as f32 + 0.5).collect();
                json!({"object": "embedding", "index": i, "embedding": e})
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data}))
    }
}

struct Rerank;
impl Respond for Rerank {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let v: Value = serde_json::from_slice(&req.body).unwrap();
        let n = v["documents"].as_array().map(|a| a.len()).unwrap_or(0);
        let results: Vec<Value> = (0..n)
            .map(|i| json!({"index": i, "relevance_score": 1.0 - i as f64 / (n as f64 + 1.0)}))
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({"results": results}))
    }
}

fn git(dir: &Path, args: &[&str]) {
    let s = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));
}

fn fixture_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/lib.rs"),
        "pub fn apply_discount(price: u32, pct: u32) -> u32 {\n    price - price * pct / 100\n}\n\npub fn total(items: &[u32]) -> u32 {\n    items.iter().sum()\n}\n",
    )
    .unwrap();
    std::fs::write(d.path().join(".env"), "API_TOKEN=do-not-index-me\n").unwrap();
    d
}

fn config(server: &str, home: &Path, exe: &Path, reranker: bool) -> Config {
    let rr = reranker.then_some("protocol: generic, endpoint_path: /rerank");
    config_with(server, home, exe, rr)
}

/// `rr` holds extra reranker keys; `None` configures no reranker.
fn config_with(server: &str, home: &Path, exe: &Path, rr: Option<&str>) -> Config {
    let rr = match rr {
        Some(extra) => format!(
            "  reranker: {{base_url: \"{server}/v1\", model: rr, api_key_env: REVERA_IT_RERANK_KEY, {extra}}}\n"
        ),
        None => String::new(),
    };
    let yaml = format!(
        "models:\n  investigator: {{protocol: scripted, script: /dev/null, model: m}}\nvera:\n  executable: \"{}\"\n  backend: api\n  home: \"{}\"\n  embedding: {{base_url: \"{server}/v1\", model: emb, api_key_env: REVERA_IT_EMBED_KEY}}\n{rr}",
        exe.display(),
        home.display()
    );
    Config::parse(&yaml, "test").unwrap()
}

async fn setup() -> Option<(MockServer, PathBuf)> {
    let Some(exe) = vera_exe() else {
        eprintln!("skipping: no vera executable (set REVERA_TEST_VERA)");
        return None;
    };
    std::env::set_var("REVERA_IT_EMBED_KEY", EMBED_KEY);
    std::env::set_var("REVERA_IT_RERANK_KEY", RERANK_KEY);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(Embed)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(Rerank)
        .mount(&server)
        .await;
    Some((server, exe))
}

async fn requests_to(server: &MockServer, p: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == p)
        .collect()
}

#[tokio::test]
async fn reranker_is_activated_in_isolated_home_and_invoked() {
    let Some((server, exe)) = setup().await else {
        return;
    };
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    // ambient overrides must not reach the child
    std::env::set_var("RERANKER_MODEL_BASE_URL", "http://127.0.0.1:9/ambient");
    let cfg = config(&server.uri(), home.path(), &exe, true);
    let vera = VeraClient::from_config(&cfg.vera, repo.path()).unwrap();
    assert_eq!(vera.home, home.path());
    assert_eq!(vera.configure().await.unwrap(), RerankState::Enabled);

    let stored = std::fs::read_to_string(home.path().join("config.json")).unwrap();
    let stored_v: Value = serde_json::from_str(&stored).unwrap();
    let retrieval = &stored_v["core_config"]["retrieval"];
    assert_eq!(retrieval["reranking_enabled"], true);
    assert_eq!(retrieval["reranker_protocol"], "generic");
    assert_eq!(retrieval["reranker_endpoint_path"], "/rerank");
    assert!(!stored.contains(EMBED_KEY) && !stored.contains(RERANK_KEY));

    vera.ensure_index().await.unwrap();
    let emb = requests_to(&server, "/v1/embeddings").await;
    assert!(!emb.is_empty(), "index must call the embedding service");
    for r in &emb {
        assert_eq!(
            r.headers.get("authorization").unwrap().to_str().unwrap(),
            format!("Bearer {EMBED_KEY}")
        );
        let body = String::from_utf8_lossy(&r.body);
        assert!(
            !body.contains("do-not-index-me"),
            "sensitive file was indexed"
        );
    }

    let hits = vera
        .search(
            "where is the discount applied to the price",
            Some("find the discount arithmetic"),
            None,
            None,
            // Vera only reranks when there are more candidates than results
            1,
        )
        .await
        .unwrap();
    assert!(hits.to_string().contains("apply_discount"), "{hits}");
    let rr = requests_to(&server, "/v1/rerank").await;
    assert!(!rr.is_empty(), "search must invoke the configured reranker");
    let body: Value = serde_json::from_slice(&rr[0].body).unwrap();
    assert_eq!(body["model"], "rr");
    assert_eq!(body["return_documents"], false);
    assert_eq!(
        rr[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {RERANK_KEY}")
    );
    std::env::remove_var("RERANKER_MODEL_BASE_URL");
}

#[tokio::test]
async fn no_reranker_means_reranking_explicitly_disabled() {
    let Some((server, exe)) = setup().await else {
        return;
    };
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let cfg = config(&server.uri(), home.path(), &exe, false);
    let vera = VeraClient::from_config(&cfg.vera, repo.path()).unwrap();
    assert_eq!(vera.configure().await.unwrap(), RerankState::Off);
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        stored["core_config"]["retrieval"]["reranking_enabled"],
        false
    );
    vera.ensure_index().await.unwrap();
    vera.search("discount", None, None, None, 3).await.unwrap();
    assert!(requests_to(&server, "/v1/rerank").await.is_empty());
}

#[tokio::test]
async fn warm_index_survives_reranker_change() {
    let Some((server, exe)) = setup().await else {
        return;
    };
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let a = VeraClient::from_config(
        &config(&server.uri(), home.path(), &exe, false).vera,
        repo.path(),
    )
    .unwrap();
    a.configure().await.unwrap();
    a.ensure_index().await.unwrap();
    let before = requests_to(&server, "/v1/embeddings").await.len();
    let b = VeraClient::from_config(
        &config(&server.uri(), home.path(), &exe, true).vera,
        repo.path(),
    )
    .unwrap();
    assert_eq!(a.index_key, b.index_key);
    assert!(b.cache_incompatibility().is_none());
    b.configure().await.unwrap();
    b.ensure_index().await.unwrap();
    let after = requests_to(&server, "/v1/embeddings").await.len();
    assert_eq!(before, after, "an unchanged tree must not be re-embedded");
}

async fn rerank_body(rr: &str, rerank_path: &str) -> Option<(Value, usize)> {
    let (server, exe) = setup().await?;
    Mock::given(method("POST"))
        .and(path(rerank_path))
        .respond_with(Rerank)
        .mount(&server)
        .await;
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let vera = VeraClient::from_config(
        &config_with(&server.uri(), home.path(), &exe, Some(rr)).vera,
        repo.path(),
    )
    .unwrap();
    assert_eq!(vera.configure().await.unwrap(), RerankState::Enabled);
    vera.ensure_index().await.unwrap();
    let hits = vera
        .search(
            "where is the discount applied to the price",
            Some("find the discount arithmetic"),
            None,
            None,
            1,
        )
        .await
        .unwrap();
    let reqs = requests_to(&server, rerank_path).await;
    let body = reqs
        .first()
        .map(|r| serde_json::from_slice(&r.body).unwrap())?;
    let _ = hits;
    Some((body, reqs.len()))
}

#[tokio::test]
async fn voyage_protocol_custom_path_and_omitted_return_documents() {
    let Some((body, _)) = rerank_body(
        "protocol: voyage, endpoint_path: /custom/rr, return_documents: omit",
        "/v1/custom/rr",
    )
    .await
    else {
        return;
    };
    let o = body.as_object().unwrap();
    assert!(
        o.contains_key("top_k") && !o.contains_key("top_n"),
        "{body}"
    );
    assert!(!o.contains_key("return_documents"), "{body}");
    assert!(!o.contains_key("instruction"), "{body}");
}

#[tokio::test]
async fn generic_protocol_sends_explicit_return_documents_true() {
    let Some((body, _)) =
        rerank_body("protocol: generic, return_documents: true", "/v1/rerank").await
    else {
        return;
    };
    assert_eq!(body["return_documents"], true);
    assert!(body.get("top_n").is_some(), "{body}");
}

#[tokio::test]
async fn reranker_outage_degrades_to_unreranked_results() {
    let Some((server, exe)) = setup().await else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/v1/broken"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let vera = VeraClient::from_config(
        &config_with(
            &server.uri(),
            home.path(),
            &exe,
            Some("protocol: generic, endpoint_path: /broken"),
        )
        .vera,
        repo.path(),
    )
    .unwrap();
    vera.configure().await.unwrap();
    vera.ensure_index().await.unwrap();
    let hits = vera
        .search(
            "where is the discount applied to the price",
            Some("find the discount arithmetic"),
            None,
            None,
            1,
        )
        .await
        .unwrap();
    assert!(hits.to_string().contains("src/lib.rs"), "{hits}");
    assert!(!requests_to(&server, "/v1/broken").await.is_empty());
}
