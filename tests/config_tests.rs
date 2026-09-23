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
    // load only checks shape; credential checks live in validate_for
    let c = Config::load(f.path()).unwrap();
    let e = c
        .validate_for(revera::config::Strategy::Baseline)
        .unwrap_err();
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
fn panel_with_personas_configuration() {
    let yaml = r#"
review: {strategy: panel}
models:
  investigator: {protocol: scripted, script: /tmp/s, model: inv}
  validator: {protocol: scripted, script: /tmp/s, model: val}
panel:
  personas:
    - name: security_specialist
      focus: security
    - name: custom_api
      prompt: "Check all public API changes for breaking changes and documentation."
      route:
        protocol: scripted
        script: /tmp/s
        model: api_model
vera: {enabled: false}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert!(c.panel.personas.is_some());
    let personas = c.panel.personas.as_ref().unwrap();
    assert_eq!(personas.len(), 2);
    assert_eq!(personas[0].name, "security_specialist");
    assert_eq!(personas[0].focus.as_deref(), Some("security"));
    assert_eq!(personas[1].name, "custom_api");
    assert_eq!(
        personas[1].prompt.as_deref(),
        Some("Check all public API changes for breaking changes and documentation.")
    );
    assert_eq!(personas[1].route.as_ref().unwrap().model, "api_model");

    let lanes = c
        .panel
        .effective_lanes(&c.models.investigator, c.models.scouts.as_deref())
        .unwrap();
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0].name, "security_specialist");
    assert_eq!(lanes[0].route.model, "inv");
    assert_eq!(lanes[1].name, "custom_api");
    assert_eq!(lanes[1].route.model, "api_model");
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
    let v = c.models.validator.as_ref().unwrap();
    assert_eq!(v.reasoning.effort(), ReasoningEffort::Low);
    assert_eq!(v.reasoning.budget_tokens(), Some(4096));
    assert_eq!(v.reasoning.effective_budget(), 4096);
    assert_eq!(v.reasoning.field(), ReasoningField::Openrouter);
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
    assert!(!is_opencode_host("https://example.com/v1"));
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

use revera::config::{PublishMode, Strategy};

#[test]
fn minimal_config_loads_with_defaults() {
    let yaml = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: inv-m}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert_eq!(c.review.strategy, Strategy::Baseline);
    assert!(c.review.validate);
    assert_eq!(c.review.publish, PublishMode::DryRun);
    assert!(!c.vera.enabled);
    assert!(c.validator_inherited());
    assert_eq!(c.effective_validator().model, "inv-m");
    c.validate_for(Strategy::Baseline).unwrap();
}

#[test]
fn explicit_validator_overrides_inheritance() {
    let yaml = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: inv-m}
  validator: {protocol: scripted, script: /tmp/s2, model: val-m}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert!(!c.validator_inherited());
    assert_eq!(c.effective_validator().model, "val-m");
}

#[test]
fn vera_opt_in_only_when_section_present() {
    let no_vera = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
"#;
    let f = write_tmp(no_vera);
    assert!(!Config::load(f.path()).unwrap().vera.enabled);

    let present = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
vera: {backend: local}
"#;
    let f = write_tmp(present);
    assert!(Config::load(f.path()).unwrap().vera.enabled);

    let off = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
vera: {enabled: false}
"#;
    let f = write_tmp(off);
    assert!(!Config::load(f.path()).unwrap().vera.enabled);
}

#[test]
fn baseline_ignores_unused_route_credentials() {
    let yaml = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  lead: {protocol: openai-chat, base_url: http://x, model: l, api_key_env: REVERA_T_UNSET_LEAD}
  workers:
    - {protocol: openai-chat, base_url: http://x, model: w, api_key_env: REVERA_T_UNSET_WORKER}
  scouts:
    - {name: general, protocol: openai-chat, base_url: http://x, model: s, api_key_env: REVERA_T_UNSET_SCOUT}
vera: {enabled: false}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    c.validate_for(Strategy::Baseline).unwrap();
    let e = c.validate_for(Strategy::Delegated).unwrap_err();
    assert!(e.to_string().contains("models.lead"), "{e}");
    let e = c.validate_for(Strategy::Panel).unwrap_err();
    assert!(e.to_string().contains("models.scouts.general"), "{e}");
}

#[test]
fn shape_errors_fail_at_load() {
    // openai-chat without base_url
    let yaml = r#"
models:
  investigator: {protocol: openai-chat, model: m, api_key_env: REVERA_T_UNSET_SHAPE}
"#;
    let f = write_tmp(yaml);
    let e = Config::load(f.path()).unwrap_err();
    assert!(e.to_string().contains("requires base_url"), "{e}");

    // scripted without script
    let yaml = r#"
models:
  investigator: {protocol: scripted, model: m}
"#;
    let f = write_tmp(yaml);
    let e = Config::load(f.path()).unwrap_err();
    assert!(e.to_string().contains("requires script path"), "{e}");
}

#[test]
fn profile_strategy_checked_after_apply() {
    let yaml = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  lead: {protocol: openai-chat, base_url: http://x, model: l, api_key_env: REVERA_T_UNSET_PROFLEAD}
vera: {enabled: false}
profiles:
  deep:
    review: {strategy: delegated}
"#;
    let f = write_tmp(yaml);
    let mut c = Config::load(f.path()).unwrap();
    c.apply_profile("deep").unwrap();
    assert_eq!(c.review.strategy, Strategy::Delegated);
    let e = c.validate_for(c.review.strategy).unwrap_err();
    assert!(e.to_string().contains("models.lead"), "{e}");
    // a CLI --strategy baseline override validates only baseline routes
    c.validate_for(Strategy::Baseline).unwrap();
}

#[test]
fn fingerprint_same_for_inherited_and_identical_validator() {
    let inherited = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
vera: {enabled: false}
"#;
    let explicit = r#"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
  validator: {protocol: scripted, script: /tmp/s, model: m}
vera: {enabled: false}
"#;
    let f1 = write_tmp(inherited);
    let c1 = Config::load(f1.path()).unwrap();
    let f2 = write_tmp(explicit);
    let c2 = Config::load(f2.path()).unwrap();
    assert_eq!(
        c1.review_fingerprint("baseline"),
        c2.review_fingerprint("baseline")
    );
    let different = explicit.replace("model: m}", "model: other}");
    let f3 = write_tmp(&different);
    let c3 = Config::load(f3.path()).unwrap();
    assert_ne!(
        c1.review_fingerprint("baseline"),
        c3.review_fingerprint("baseline")
    );
}

#[test]
fn example_config_loads() {
    std::env::set_var("REVIEW_BASE_URL", "https://api.example.com/v1");
    let p = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/revera.example.yaml"));
    Config::load(p).unwrap();
}

#[test]
fn path_instructions_and_review_profiles_work() {
    let yaml = r#"
review:
  strategy: baseline
  review_profile: chill
  path_instructions:
    - path: "src/api/**/*.rs"
      instructions: "Validate all request bodies and query parameters."
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
vera: {enabled: false}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert_eq!(c.review.min_severity, revera::findings::Severity::Medium);
    assert_eq!(c.review.path_instructions.len(), 1);
    assert!(c.review.path_instructions[0].matches("src/api/auth/login.rs"));
    assert!(!c.review.path_instructions[0].matches("tests/auth_tests.rs"));

    // Quiet profile sets High severity
    let quiet_yaml = yaml.replace("review_profile: chill", "review_profile: quiet");
    let f_quiet = write_tmp(&quiet_yaml);
    let c_quiet = Config::load(f_quiet.path()).unwrap();
    assert_eq!(
        c_quiet.review.min_severity,
        revera::findings::Severity::High
    );

    // Assertive profile sets Low severity
    let assertive_yaml = yaml.replace("review_profile: chill", "review_profile: assertive");
    let f_assertive = write_tmp(&assertive_yaml);
    let c_assertive = Config::load(f_assertive.path()).unwrap();
    assert_eq!(
        c_assertive.review.min_severity,
        revera::findings::Severity::Low
    );
}

#[test]
fn investigator_user_appends_targeted_path_guidance() {
    use revera::config::PathInstruction;
    use revera::diff::parse_unified;
    use revera::pipeline::common::{investigator_user, ReviewRequest};

    let diff_text = r#"diff --git a/src/api/handler.rs b/src/api/handler.rs
index 0000000..1111111 100644
--- a/src/api/handler.rs
+++ b/src/api/handler.rs
@@ -1,1 +1,2 @@
+pub async fn handle() {}
"#;
    let diff = parse_unified(diff_text);
    let req = ReviewRequest {
        repo: std::path::PathBuf::from("."),
        base: "main".into(),
        head: Some("feature".into()),
        title: Some("New API Handler".into()),
        body: "Adding handler".into(),
        strategy_override: None,
        force: false,
        uncommitted: false,
    };
    let instructions = vec![PathInstruction {
        path: "src/api/**/*.rs".into(),
        instructions: "Verify endpoint rate limiting and authentication.".into(),
    }];

    let prompt = investigator_user(&req, &diff, 100_000, &instructions, &[], "");
    assert!(prompt.contains("Targeted Path Guidance:"));
    assert!(
        prompt.contains("- [src/api/**/*.rs] Verify endpoint rate limiting and authentication.")
    );
}

#[test]
fn knowledge_base_and_fail_on_severity_config_loads() {
    let yaml = r#"
review:
  strategy: baseline
  fail_on_severity: high
  knowledge_base:
    - "README.md"
models:
  investigator: {protocol: scripted, script: /tmp/s, model: m}
vera: {enabled: false}
"#;
    let f = write_tmp(yaml);
    let c = Config::load(f.path()).unwrap();
    assert_eq!(
        c.review.fail_on_severity,
        Some(revera::findings::Severity::High)
    );
    assert_eq!(c.review.knowledge_base, vec!["README.md"]);
}
