use std::io::Write;
use std::process::Command;

fn write_tmp(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[test]
fn fork_guard_fires_before_credential_check() {
    // investigator key unset: a fork event must still get the partial
    // "review skipped" report (exit 2), not a credential failure (exit 1)
    let yaml = "models:\n  investigator: {protocol: openai-chat, base_url: http://x, model: m, api_key_env: REVERA_T_UNSET_FORK_INV}\n";
    let cfg = write_tmp(yaml);
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pr_event.json")).unwrap();
    v["pull_request"]["head"]["repo"]["full_name"] = serde_json::json!("contributor/widgets");
    let ev = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ev.path(), v.to_string()).unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = repo.path().join("report.json");

    let res = Command::new(env!("CARGO_BIN_EXE_revera"))
        .args([
            "review",
            "--repo",
            repo.path().to_str().unwrap(),
            "--event",
            ev.path().to_str().unwrap(),
            "--config",
            cfg.path().to_str().unwrap(),
            "--publish",
            "comment",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        res.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&res.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["status"], "partial");
    assert!(
        report["reason"]
            .as_str()
            .unwrap()
            .contains("fork PR: review skipped"),
        "{report}"
    );
}
