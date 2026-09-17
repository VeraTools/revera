use revera::config::{expand_env, Config};
use std::io::Write;

fn write_tmp(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[test]
fn env_expansion_and_missing_var() {
    std::env::set_var("REVERA_TEST_X", "hello");
    assert_eq!(expand_env("a-${REVERA_TEST_X}-b").unwrap(), "a-hello-b");
    let e = expand_env("x-${REVERA_TEST_MISSING_VAR}-y").unwrap_err();
    assert!(e.to_string().contains("REVERA_TEST_MISSING_VAR"), "{e}");
}

#[test]
fn openai_chat_requires_key_env() {
    let yaml = r#"
review: {strategy: baseline}
models:
  investigator:
    protocol: openai-chat
    base_url: http://x
    api_key_env: REVERA_DEFINITELY_UNSET_KEY_123
    model: m
  validator:
    protocol: scripted
    script: /tmp/s.json
    model: m
vera: {backend: local}
"#;
    let f = write_tmp(yaml);
    let e = Config::load(f.path()).unwrap_err();
    assert!(
        e.to_string().contains("REVERA_DEFINITELY_UNSET_KEY_123"),
        "{e}"
    );
}

#[test]
fn unknown_field_rejected() {
    let yaml = r#"
review: {strategy: baseline, bogus_field: 1}
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  validator: {protocol: scripted, script: /tmp/s, model: m}
vera: {backend: local}
"#;
    let f = write_tmp(yaml);
    assert!(Config::load(f.path()).is_err());
}

#[test]
fn profile_override_applies() {
    let yaml = r#"
review: {strategy: baseline}
budget: {agent_max_tool_calls: 25, run_max_requests: 120}
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  validator: {protocol: scripted, script: /tmp/s, model: m}
vera: {backend: local}
profiles:
  deep:
    budget: {agent_max_tool_calls: 60, run_max_requests: 400, run_max_seconds: 2400}
"#;
    let f = write_tmp(yaml);
    let mut c = Config::load(f.path()).unwrap();
    c.apply_profile("deep").unwrap();
    assert_eq!(c.budget.agent_max_tool_calls, 60);
    assert_eq!(c.budget.run_max_requests, 400);
    assert_eq!(c.budget.run_max_seconds, 2400);
    assert!(c.apply_profile("nope").is_err());
}

#[test]
fn reasoning_bare_and_long_forms_parse() {
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    let yaml = r#"
review: {strategy: baseline}
models:
  investigator:
    protocol: openai-chat
    base_url: http://x
    api_key_env: REVERA_TEST_KEY
    model: m
    reasoning: high
  validator:
    protocol: scripted
    script: /tmp/s
    model: m
    reasoning: {effort: low, budget_tokens: 4096, field: openrouter}
vera: {backend: local}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    use revera::config::{ReasoningEffort, ReasoningField};
    assert_eq!(
        c.models.investigator.reasoning.effort(),
        ReasoningEffort::High
    );
    assert_eq!(c.models.investigator.reasoning.effective_budget(), 16384);
    assert_eq!(c.models.validator.reasoning.effort(), ReasoningEffort::Low);
    assert_eq!(c.models.validator.reasoning.budget_tokens(), Some(4096));
    assert_eq!(c.models.validator.reasoning.effective_budget(), 4096);
    assert_eq!(
        c.models.validator.reasoning.field(),
        ReasoningField::Openrouter
    );
}

#[test]
fn reasoning_default_is_medium_and_unknown_fails() {
    let yaml = r#"
review: {strategy: baseline}
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  validator: {protocol: scripted, script: /tmp/s, model: m}
vera: {backend: local}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert_eq!(
        c.models.investigator.reasoning.effort(),
        revera::config::ReasoningEffort::Medium
    );
    let bad = yaml.replace(
        "model: m}\n  validator",
        "model: m, reasoning: ultra}\n  validator",
    );
    let f = write_tmp(&bad);
    assert!(Config::load(f.path()).is_err(), "unknown effort must fail");
}

#[test]
fn empty_workers_and_scouts_normalize_to_none() {
    let yaml = r#"
review: {strategy: baseline}
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  validator: {protocol: scripted, script: /tmp/s, model: m}
  workers: []
  scouts: []
vera: {backend: local}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert!(c.models.workers.is_none());
    assert!(c.models.scouts.is_none());
}

#[test]
fn opencode_ai_base_url_defaults_session_header() {
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    let yaml = r#"
review: {strategy: baseline}
models:
  investigator:
    protocol: openai-chat
    base_url: https://opencode.ai/zen/go/v1
    api_key_env: REVERA_TEST_KEY
    model: m
  validator: {protocol: scripted, script: /tmp/s, model: m}
vera: {backend: local}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert_eq!(
        c.models.investigator.session_header.as_deref(),
        Some("x-opencode-session")
    );
    let other = yaml.replace(
        "https://opencode.ai/zen/go/v1",
        "https://openrouter.ai/api/v1",
    );
    let f = write_tmp(&other);
    let c = Config::load(f.path()).unwrap();
    assert!(c.models.investigator.session_header.is_none());
}

#[test]
fn is_opencode_host_matches_exact_and_subdomain() {
    use revera::config::is_opencode_host;
    assert!(is_opencode_host("https://opencode.ai/zen/go/v1"));
    assert!(is_opencode_host("https://opencode.ai:443/zen/go/v1"));
    assert!(is_opencode_host("https://OpenCode.AI/x"));
    assert!(is_opencode_host("https://api.opencode.ai/v1"));
    assert!(!is_opencode_host("https://evilopencode.ai/v1"));
    assert!(!is_opencode_host("https://opencode.ai.evil.com/v1"));
    assert!(!is_opencode_host("https://relay.fast/v1"));
    assert!(!is_opencode_host("not a url"));
}

#[test]
fn session_header_must_be_a_valid_http_header_name() {
    std::env::set_var("REVERA_TEST_KEY", "sk-test");
    let yaml = |header: &str| {
        format!(
            r#"
review: {{strategy: baseline}}
models:
  investigator: {{protocol: scripted, script: /tmp/s, model: m}}
  validator:
    protocol: openai-chat
    base_url: http://x
    api_key_env: REVERA_TEST_KEY
    model: m
    session_header: "{header}"
vera: {{backend: local}}
"#
        )
    };
    for bad in ["", "bad header"] {
        let f = write_tmp(&yaml(bad));
        let e = Config::load(f.path()).unwrap_err();
        assert!(
            e.to_string().contains("models.validator: session_header"),
            "{e}"
        );
    }
    let f = write_tmp(&yaml("x-ok"));
    assert!(Config::load(f.path()).is_ok());
}
