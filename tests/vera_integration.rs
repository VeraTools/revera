//! Real `vera` binary against mock embedding/reranker HTTP services.
//! Skipped (with a note) when no `vera` executable is available; set
//! `REVERA_TEST_VERA` to an executable path to force a specific binary.

use revera::config::Config;
use revera::vera::{RerankState, VeraClient};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// must match .cargo/config.toml, which exports them to the Vera child
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
    for (var, want) in [
        ("REVERA_IT_EMBED_KEY", EMBED_KEY),
        ("REVERA_IT_RERANK_KEY", RERANK_KEY),
    ] {
        assert_eq!(
            std::env::var(var).as_deref(),
            Ok(want),
            "{var} comes from .cargo/config.toml: run through cargo test, with {var} unset in the shell"
        );
    }
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
    // RERANKER_MODEL_BASE_URL is set ambiently (.cargo/config.toml) and
    // must not reach the child
    assert!(std::env::var_os("RERANKER_MODEL_BASE_URL").is_some());
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
    // option-like model queries are positional, not flags
    vera.search("--help", None, None, None, 1).await.unwrap();
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
    assert!(
        vera.rerank_fallbacks
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0,
        "an unreranked fallback must be observable"
    );
}

#[tokio::test]
async fn legacy_cache_without_identity_is_rebuilt() {
    let Some((server, exe)) = setup().await else {
        return;
    };
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let v = VeraClient::from_config(
        &config(&server.uri(), home.path(), &exe, false).vera,
        repo.path(),
    )
    .unwrap();
    let p = VeraClient::cache_info_path(repo.path());
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    // metadata written before index identities were recorded
    std::fs::write(
        &p,
        r#"{"vera_version": "1.4.1", "backend": "api", "embedding_model": "emb", "dim": null, "updated_at": "0"}"#,
    )
    .unwrap();
    assert!(v.cache_incompatibility().is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_reranker_shutdown_is_a_configuration_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    // a vera whose `config set retrieval.*` always fails (embedding
    // settings apply, so the reranker path is what fails)
    let exe = dir.path().join("vera");
    std::fs::write(
        &exe,
        "#!/bin/sh\ncase \"$3\" in retrieval.*) echo 'config locked' >&2; exit 1;; esac\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    for rr in [None, Some("protocol: generic")] {
        let v = VeraClient::from_config(
            &config_with("http://127.0.0.1:9", home.path(), &exe, rr).vera,
            repo.path(),
        )
        .unwrap();
        assert!(
            v.configure().await.is_err(),
            "reranking state unknown ({rr:?}) must not be reported as usable"
        );
    }
}

/// A fake `vera` that logs each invocation and fails `config set` for keys
/// matching the shell pattern `fail`.
#[cfg(unix)]
fn logging_vera(dir: &Path, fail: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let exe = dir.join("vera");
    let log = dir.join("calls.log");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$3\" in {fail}) echo 'refused' >&2; exit 1;; esac\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    (exe, log)
}

#[cfg(unix)]
#[tokio::test]
async fn configure_applies_embedding_throughput_for_api_backend_only() {
    let dir = tempfile::tempdir().unwrap();
    let (exe, log) = logging_vera(dir.path(), "__none__");
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let api = VeraClient::from_config(
        &config_with("http://127.0.0.1:9", home.path(), &exe, None).vera,
        repo.path(),
    )
    .unwrap();
    assert_eq!(api.configure().await.unwrap(), RerankState::Off);
    let calls = std::fs::read_to_string(&log).unwrap();
    for want in [
        "config set embedding.max_concurrent_requests 2",
        "config set embedding.max_in_flight_inputs 128",
        "config set embedding.timeout_secs 120",
        "config set retrieval.reranking_enabled false",
    ] {
        assert!(calls.lines().any(|l| l == want), "{want} missing: {calls}");
    }
    std::fs::remove_file(&log).unwrap();

    let yaml = format!(
        "models:\n  investigator: {{protocol: scripted, script: /dev/null, model: m}}\nvera:\n  executable: \"{}\"\n  backend: local\n  home: \"{}\"\n",
        exe.display(),
        home.path().display()
    );
    let local =
        VeraClient::from_config(&Config::parse(&yaml, "t").unwrap().vera, repo.path()).unwrap();
    assert_eq!(local.configure().await.unwrap(), RerankState::Off);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("embedding."), "{calls}");
    assert!(calls.contains("retrieval.reranking_enabled"), "{calls}");
}

#[cfg(unix)]
#[tokio::test]
async fn failed_embedding_setting_is_a_configuration_error() {
    let dir = tempfile::tempdir().unwrap();
    let (exe, _) = logging_vera(dir.path(), "embedding.*");
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    // with a reranker configured this must not be mistaken for a degraded
    // reranker
    let v = VeraClient::from_config(
        &config_with(
            "http://127.0.0.1:9",
            home.path(),
            &exe,
            Some("protocol: generic"),
        )
        .vera,
        repo.path(),
    )
    .unwrap();
    let e = v.configure().await.unwrap_err();
    assert!(format!("{e:#}").contains("embedding"), "{e:#}");
}

#[tokio::test]
async fn embedding_throughput_reaches_the_vera_home() {
    let Some((server, exe)) = setup().await else {
        return;
    };
    let repo = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let cfg = config(&server.uri(), home.path(), &exe, false);
    let vera = VeraClient::from_config(&cfg.vera, repo.path()).unwrap();
    vera.configure().await.unwrap();
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("config.json")).unwrap())
            .unwrap();
    let emb = &stored["core_config"]["embedding"];
    assert_eq!(emb["max_concurrent_requests"], 2, "{stored}");
    assert_eq!(emb["max_in_flight_inputs"], 128, "{stored}");
    assert_eq!(emb["timeout_secs"], 120, "{stored}");
}

fn commit_all(dir: &Path, msg: &str) {
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

#[tokio::test]
async fn prepared_run_reports_active_reranker() {
    use revera::pipeline::common::{PrepareOut, ReviewRequest, prepare};
    let Some((server, exe)) = setup().await else {
        return;
    };
    let repo = fixture_repo();
    git(repo.path(), &["add", "src/lib.rs"]);
    commit_all(repo.path(), "base");
    let base = String::from_utf8(
        std::process::Command::new("git")
            .current_dir(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn apply_discount(price: u32, pct: u32) -> u32 {\n    price - price * pct / 10\n}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "src/lib.rs"]);
    commit_all(repo.path(), "head");
    let home = tempfile::tempdir().unwrap();
    let cfg = config(&server.uri(), home.path(), &exe, true);
    let req = ReviewRequest {
        repo: repo.path().to_path_buf(),
        base,
        head: None,
        title: None,
        body: String::new(),
        strategy_override: None,
        force: true,
    };
    let PrepareOut::Ready(p) = prepare(&cfg, &req, "baseline").await.unwrap() else {
        panic!("a fresh review must not short-circuit");
    };
    assert_eq!(p.stats.retrieval, "vera+rerank");
    assert!(p.retrieval_unavailable.is_none());
    assert!(
        !p.partial_reasons.iter().any(|r| r.contains("reranker")),
        "{:?}",
        p.partial_reasons
    );
}
