//! Approved test commands run only on a disposable copy, without credentials or
//! network. This module is a runner, not Rust's unit-test module.
use super::{sandbox, workspace};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotConfigured,
    Passed,
    Failed,
    TimedOut,
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub outcome: Outcome,
    pub output: Option<sandbox::Output>,
    pub reason: Option<String>,
}
pub async fn run(profile: &sandbox::Profile, command: &[String], tree_copy: &Path) -> TestResult {
    if command.is_empty() {
        return TestResult {
            outcome: Outcome::NotConfigured,
            output: None,
            reason: None,
        };
    }
    let status = sandbox::probe().await;
    let p = sandbox::Profile {
        net: false,
        binds_rw: vec![(tree_copy.into(), "/work".into())],
        limits: profile.limits.clone(),
        ..Default::default()
    };
    match sandbox::run(&status, &p, command).await {
        Ok(output) => TestResult {
            outcome: if output.timed_out {
                Outcome::TimedOut
            } else if output.code == Some(0) {
                Outcome::Passed
            } else {
                Outcome::Failed
            },
            output: Some(output),
            reason: None,
        },
        Err(e) => TestResult {
            outcome: Outcome::Unavailable,
            output: None,
            reason: Some(crate::redact::diagnostic(&e.to_string())),
        },
    }
}
pub async fn run_copy(
    profile: &sandbox::Profile,
    command: &[String],
    tree: &Path,
    copy: &Path,
) -> anyhow::Result<TestResult> {
    workspace::copy_tree(tree, copy)?;
    let result = run(profile, command, copy).await;
    std::fs::remove_dir_all(copy)?;
    Ok(result)
}
