use std::io::Write;
use std::process::Command;

fn write_tmp(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

fn git_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(d.path())
        .output()
        .unwrap();
    d
}

fn doctor(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_revera"))
        .arg("doctor")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

#[test]
fn doctor_minimal_config_inherited_validator() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/scripts/clean.json");
    let yaml =
        format!("models:\n  investigator: {{protocol: scripted, script: {script}, model: m}}\n");
    let f = write_tmp(&yaml);
    let repo = git_dir();
    let out = doctor(&["--config", f.path().to_str().unwrap()], repo.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("validator: inherited from investigator"),
        "{stdout}"
    );
    assert!(stdout.contains("vera: disabled"), "{stdout}");
}

#[test]
fn doctor_missing_investigator_key_fails_with_next_hint() {
    let yaml = "models:\n  investigator: {protocol: openai-chat, base_url: http://x, model: m, api_key_env: REVERA_T_UNSET_DOCTOR_INV}\n";
    let f = write_tmp(yaml);
    let repo = git_dir();
    let out = doctor(&["--config", f.path().to_str().unwrap()], repo.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("config: ok"), "{stdout}");
    assert!(stdout.contains("REVERA_T_UNSET_DOCTOR_INV"), "{stdout}");
    assert!(stdout.contains("next:"), "{stdout}");
}

#[test]
fn doctor_strategy_override_validates_lead() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/scripts/clean.json");
    let yaml = format!(
        "models:\n  investigator: {{protocol: scripted, script: {script}, model: m}}\n  lead: {{protocol: openai-chat, base_url: http://x, model: l, api_key_env: REVERA_T_UNSET_DOCTOR_LEAD}}\n"
    );
    let f = write_tmp(&yaml);
    let cfg = f.path().to_str().unwrap().to_string();
    let repo = git_dir();

    // baseline: the unused lead's missing key must not fail doctor
    let out = doctor(&["--config", &cfg], repo.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(!stdout.contains("models.lead"), "{stdout}");

    // --strategy delegated: the lead credential check fires
    let out = doctor(&["--config", &cfg, "--strategy", "delegated"], repo.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("models.lead"), "{stdout}");
}

#[test]
fn doctor_panel_cardinality_fails() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/scripts/clean.json");
    let yaml = format!(
        "models:\n  investigator: {{protocol: scripted, script: {script}, model: m}}\n  scouts:\n    - {{name: a, protocol: scripted, script: {script}, model: m}}\n    - {{name: b, protocol: scripted, script: {script}, model: m}}\npanel: {{focuses: [general, cross-file, concurrency]}}\n"
    );
    let f = write_tmp(&yaml);
    let repo = git_dir();
    let out = doctor(
        &[
            "--config",
            f.path().to_str().unwrap(),
            "--strategy",
            "panel",
        ],
        repo.path(),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("panel: FAIL"), "{stdout}");
}

#[test]
fn doctor_web_disabled_free_keyed_and_trust_hints_without_network() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/scripts/clean.json");
    let repo = git_dir();
    for (web, key, expected, code) in [
        (
            "{enabled: false, api_key_env: GITHUB_TOKEN}",
            false,
            "web_search: disabled",
            0,
        ),
        (
            "{enabled: true, api_key_env: REVERA_WEB_DOCTOR_KEY}",
            false,
            "free hosted Exa",
            0,
        ),
        (
            "{enabled: true, api_key_env: REVERA_WEB_DOCTOR_KEY}",
            true,
            "keyed Exa REST",
            0,
        ),
        (
            "{enabled: true, api_key_env: REVERA_WEB_DOCTOR_KEY, free_first: true}",
            true,
            "free Exa first",
            0,
        ),
        (
            "{enabled: true, api_key_env: GITHUB_TOKEN}",
            false,
            "web_search: FAIL",
            1,
        ),
    ] {
        let f = write_tmp(&format!(
            "models:\n  investigator: {{protocol: scripted, script: {script}, model: m}}\nweb_search: {web}\n"
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_revera"));
        command
            .args(["doctor", "--config", f.path().to_str().unwrap()])
            .current_dir(repo.path())
            .env_remove("REVERA_WEB_DOCTOR_KEY");
        if key {
            command.env("REVERA_WEB_DOCTOR_KEY", "doctor-exa-secret-never-printed");
        }
        let out = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(code), "{stdout}");
        assert!(stdout.contains(expected), "{stdout}");
        assert!(!stdout.contains("doctor-exa-secret-never-printed"));
    }
}
