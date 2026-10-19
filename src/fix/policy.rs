//! Deterministic controller-side patch policy. Never trust the agent's summary.
use super::workspace::{Patch, Status};
use serde::{Deserialize, Serialize};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Explicit CI configuration set, including nested workflow/job definitions.
pub const CI_GLOBS: &[&str] = &[
    ".github/workflows/**",
    ".github/actions/**",
    ".circleci/**",
    ".buildkite/**",
    ".gitlab-ci.yml",
    ".gitlab/**",
    "azure-pipelines*.yml",
    "azure-pipelines*.yaml",
    "Jenkinsfile",
    "**/Jenkinsfile",
    ".travis.yml",
    "appveyor.yml",
    ".drone.yml",
    ".woodpecker*",
    "bitbucket-pipelines.yml",
    "cloudbuild*.yaml",
    "cloudbuild*.yml",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub allow_workflow_changes: bool,
    pub max_patch_bytes: usize,
    pub max_files: usize,
    pub max_deleted_files: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_workflow_changes: false,
            max_patch_bytes: 200_000,
            max_files: 20,
            max_deleted_files: 3,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    pub path: Option<String>,
    pub rule: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}
impl Report {
    pub fn allowed(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Plain UTF-8 Git paths only. Reject platform aliases too, even on Linux.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0', '\r', '\n', ':'])
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}
pub fn sensitive(path: &str) -> bool {
    static SET: std::sync::OnceLock<globset::GlobSet> = std::sync::OnceLock::new();
    SET.get_or_init(|| case_insensitive_globs(crate::config::SENSITIVE_GLOBS))
        .is_match(path)
}

fn case_insensitive_globs(patterns: &[&str]) -> globset::GlobSet {
    let mut set = globset::GlobSetBuilder::new();
    for pattern in patterns {
        set.add(
            globset::GlobBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .unwrap(),
        );
    }
    set.build().unwrap()
}

pub fn check(patch: &Patch, policy: &Policy) -> Report {
    let mut out = Report::default();
    let mut error = |path: Option<&str>, rule: &str| {
        out.errors.push(Issue {
            path: path.map(str::to_owned),
            rule: rule.into(),
        })
    };
    if patch.unified.len() > policy.max_patch_bytes {
        error(None, "patch_size");
    }
    if patch.files.len() > policy.max_files {
        error(None, "file_count");
    }
    if patch
        .files
        .iter()
        .filter(|f| f.status == Status::Deleted)
        .count()
        > policy.max_deleted_files
    {
        error(None, "deleted_file_count");
    }
    let ci = case_insensitive_globs(CI_GLOBS);
    for f in &patch.files {
        let path = Some(f.path.as_str());
        if !valid_path(&f.path) {
            error(path, "path");
        }
        if f.path
            .split('/')
            .any(|c| c.eq_ignore_ascii_case(".gitmodules"))
        {
            error(path, "gitmodules");
        }
        if sensitive(&f.path) {
            error(path, "sensitive");
        }
        if ci.is_match(&f.path) && !policy.allow_workflow_changes {
            error(path, "workflow");
        }
        if [f.old_mode, f.new_mode].contains(&Some(0o120000)) {
            error(path, "symlink");
        }
        if [f.old_mode, f.new_mode].contains(&Some(0o160000)) {
            error(path, "gitlink");
        }
        if f.new_mode == Some(0o100755) && f.old_mode != Some(0o100755) {
            error(path, "executable");
        }
        if f.binary {
            error(path, "binary");
        }
        if f.size > MAX_FILE_BYTES as u64
            || f.old_content
                .as_ref()
                .is_some_and(|s| s.len() > MAX_FILE_BYTES)
            || f.new_content
                .as_ref()
                .is_some_and(|s| s.len() > MAX_FILE_BYTES)
        {
            error(path, "file_size");
        }
        if f.new_content.as_ref().is_some_and(|s| s.contains('\0')) {
            error(path, "nul");
        }
        if [&f.old_content, &f.new_content]
            .into_iter()
            .flatten()
            .any(|s| crate::redact::text(s) != s.as_str())
        {
            error(path, "credential_material");
        }
        if let Some(text) = &f.new_content {
            for (rule, yes) in [
                (
                    "conflict_markers",
                    text.lines().any(|l| {
                        l.starts_with("<<<<<<<")
                            || l.starts_with("=======")
                            || l.starts_with(">>>>>>>")
                    }),
                ),
                (
                    "crlf_change",
                    f.old_content
                        .as_ref()
                        .is_some_and(|old| old.contains("\r\n") != text.contains("\r\n")),
                ),
                (
                    "trailing_whitespace_only",
                    f.old_content.as_ref().is_some_and(|old| {
                        old != text
                            && old
                                .lines()
                                .map(str::trim_end)
                                .eq(text.lines().map(str::trim_end))
                    }),
                ),
            ] {
                if yes {
                    out.warnings.push(Issue {
                        path: Some(f.path.clone()),
                        rule: rule.into(),
                    });
                }
            }
        }
    }
    out
}
