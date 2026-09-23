use crate::config::RuleConfig;
use crate::diff::{DiffLineKind, DiffSet};
use crate::findings::{Evidence, Finding, Severity};
use globset::{Glob, GlobSetBuilder};
use regex::Regex;
use std::sync::LazyLock;

static SECRET_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \bAKIA[0-9A-Z]{16}\b |
        \bgh[pousr]_[A-Za-z0-9_]{36,255}\b |
        \bgithub_pat_[A-Za-z0-9_]{22,255}\b |
        \bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b |
        -----BEGIN[A-Z\x20]*PRIVATE\x20KEY-----
    "#,
    )
    .unwrap()
});

/// Credentials vendors publish as documentation examples; they match the
/// patterns but are not secrets (secret scanners allowlist them too).
const DOC_EXAMPLE_SECRETS: &[&str] = &["AKIAIOSFODNN7EXAMPLE", "AKIAI44QH8DHBEXAMPLE"];

/// A secret-pattern match that is not a published documentation example.
fn has_secret(text: &str) -> bool {
    SECRET_REGEX
        .find_iter(text)
        .any(|m| !DOC_EXAMPLE_SECRETS.contains(&m.as_str()))
}

static DEBUG_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \bdbg!\s*\( |
        \bconsole\.(?:log|debug|dir)\s*\( |
        \bprintln!\s*\(\s*"debug |
        \bbinding\.pry\b |
        \bbreakpoint\s*\(\)
    "#,
    )
    .unwrap()
});

static UNSAFE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bunsafe\s*\{"#).unwrap());

static SQL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)format!\s*\(\s*"[^"]*\b(SELECT|INSERT|UPDATE|DELETE)\b[^"]*\{"#).unwrap()
});

static SLOP_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \b(?:todo!|unimplemented!)\s*\( |
        (?i)(?://|\#|/\*)\s*(?:TODO|FIXME|XXX)\s*:\s*(?:implement|fill\s+in|add\s+logic|placeholder)
    "#,
    )
    .unwrap()
});

/// Evaluates static regex patterns against added lines in a diff.
pub fn scan_diff(diff: &DiffSet, custom_rules: Option<&[RuleConfig]>) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Prepare custom rule matchers
    let compiled_custom: Vec<(RuleConfig, Option<globset::GlobSet>, Regex)> = custom_rules
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let re = match Regex::new(&r.pattern) {
                Ok(re) => re,
                Err(e) => {
                    tracing::warn!("invalid regex pattern in custom rule {}: {e}", r.id);
                    return None;
                }
            };
            let globset = if r.files.is_empty() {
                None
            } else {
                let mut builder = GlobSetBuilder::new();
                for f in &r.files {
                    if let Ok(glob) = Glob::new(f) {
                        builder.add(glob);
                    }
                }
                builder.build().ok()
            };
            Some((r.clone(), globset, re))
        })
        .collect();

    for file in &diff.files {
        let is_rust = file.new_path.ends_with(".rs");
        for hunk in &file.hunks {
            for line in &hunk.lines {
                if line.kind != DiffLineKind::Add {
                    continue;
                }
                let line_no = line.new_no.unwrap_or(hunk.new_start);
                let text = line.text.trim();

                // 1. Secret detection
                if has_secret(text) {
                    findings.push(make_finding(StaticMatch {
                        rule_id: "secrets",
                        severity: Severity::High,
                        file: &file.new_path,
                        line_no,
                        title: "Hardcoded credential or API key pattern detected",
                        claim: "A secret pattern (API key, token, or private key) was found in added code. Committing credentials to source control risks immediate exposure.",
                        trigger: "Secret key regex pattern matched in added diff line.",
                        suggested_fix: Some("Remove the credential immediately and store it in an environment variable or secrets manager."),
                    }));
                }

                // 2. Debug leftover detection
                if DEBUG_REGEX.is_match(text) {
                    findings.push(make_finding(StaticMatch {
                        rule_id: "debug-statement",
                        severity: Severity::Low,
                        file: &file.new_path,
                        line_no,
                        title: "Lingering debug statement introduced",
                        claim: "A debugging statement (such as dbg!, console.log, or breakpoint) was added. Debug prints pollute production logs and should be removed before merge.",
                        trigger: "Debug statement matched on added line.",
                        suggested_fix: Some("Remove the debug statement before merging."),
                    }));
                }

                // 3. Unsafe block addition in Rust
                if is_rust && UNSAFE_REGEX.is_match(text) {
                    findings.push(make_finding(StaticMatch {
                        rule_id: "unsafe-block",
                        severity: Severity::Medium,
                        file: &file.new_path,
                        line_no,
                        title: "Unsafe block introduced",
                        claim: "An unsafe block was introduced. Memory safety guarantees are suspended inside unsafe blocks; ensure invariants are documented and validated.",
                        trigger: "Unsafe block added in Rust source.",
                        suggested_fix: Some("Document why unsafe is required and state the safety invariant."),
                    }));
                }

                // 4. SQL string interpolation
                if is_rust && SQL_REGEX.is_match(text) {
                    findings.push(make_finding(StaticMatch {
                        rule_id: "sql-string-interpolation",
                        severity: Severity::High,
                        file: &file.new_path,
                        line_no,
                        title: "Potential SQL string interpolation",
                        claim: "Direct string formatting into a SQL query string risks SQL injection if query inputs are untrusted. Use parameterized query bindings instead.",
                        trigger: "SQL keyword formatted with variable interpolation.",
                        suggested_fix: Some("Replace direct format! string interpolation with parameterized SQL query arguments."),
                    }));
                }

                // 5. Unimplemented slop / placeholder detection
                if SLOP_REGEX.is_match(text) {
                    findings.push(make_finding(StaticMatch {
                        rule_id: "slop-placeholder",
                        severity: Severity::Medium,
                        file: &file.new_path,
                        line_no,
                        title: "Unimplemented placeholder or stub introduced",
                        claim: "An unimplemented placeholder stub (e.g. todo!, unimplemented!, or TODO: implement) was introduced in added code. Leaving placeholder stubs can lead to runtime crashes or incomplete logic.",
                        trigger: "Unimplemented stub pattern matched on added line.",
                        suggested_fix: Some("Implement the required logic or replace the stub before merging."),
                    }));
                }

                // 6. Custom rules evaluation
                for (rule, globs, re) in &compiled_custom {
                    if let Some(gs) = globs {
                        if !gs.is_match(&file.new_path) {
                            continue;
                        }
                    }
                    if re.is_match(text) {
                        findings.push(make_finding(StaticMatch {
                            rule_id: &rule.id,
                            severity: rule.severity,
                            file: &file.new_path,
                            line_no,
                            title: &rule.message,
                            claim: &format!("Static rule '{}' triggered on added line.", rule.id),
                            trigger: &format!("Regex pattern '{}' matched line.", rule.pattern),
                            suggested_fix: None,
                        }));
                    }
                }
            }
        }
    }

    findings
}

struct StaticMatch<'a> {
    rule_id: &'a str,
    severity: Severity,
    file: &'a str,
    line_no: u32,
    title: &'a str,
    claim: &'a str,
    trigger: &'a str,
    suggested_fix: Option<&'a str>,
}

fn make_finding(m: StaticMatch<'_>) -> Finding {
    Finding {
        defect_key: format!("{}:{}", m.rule_id, m.file),
        severity: m.severity,
        file: m.file.to_string(),
        start_line: m.line_no,
        end_line: Some(m.line_no),
        title: m.title.to_string(),
        claim: m.claim.to_string(),
        trigger: m.trigger.to_string(),
        impact: "Deterministic static rule triggered on newly added diff lines.".to_string(),
        introduced_by_change: true,
        supporting_evidence: vec![Evidence {
            path: m.file.to_string(),
            start_line: m.line_no,
            end_line: m.line_no,
            // the matched line may be the very secret the rule detected:
            // point at it, never copy it into state, prompts or reports
            note: format!("matched static rule [{}]", m.rule_id),
        }],
        counterevidence_checked: vec!["static rule matched directly on added diff line".to_string()],
        validation_status: None,
        suggested_fix: m.suggested_fix.map(str::to_string),
        source: format!("static:{}", m.rule_id),
        rationale: Some(format!(
            "Deterministic rule {} triggered on line {}",
            m.rule_id, m.line_no
        )),
        sources: vec![format!("static:{}", m.rule_id)],
        assurance: Some(crate::findings::AssuranceCase {
            rule_id: Some(m.rule_id.to_string()),
            trigger: m.trigger.to_string(),
            rationale: format!(
                "Deterministic rule {} triggered on line {}",
                m.rule_id, m.line_no
            ),
            counterevidence_checked: vec![
                "static rule matched directly on added diff line".to_string()
            ],
            validator_rederivation: None,
            confidence: 1.0,
        }),
        quoted_code: None,
        suggested_replacement: None,
        quote_anchored: false,
    }
}
