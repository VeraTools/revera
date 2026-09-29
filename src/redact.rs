//! Secret containment for every egress surface (model/provider requests,
//! GitHub comments, reports, state, logs): exact configured secret values
//! plus conservative detection of recognizable credential material.

use regex::Regex;
use std::borrow::Cow;
use std::sync::{OnceLock, RwLock};

pub const REDACTED: &str = "[REDACTED]";

/// Shorter values are too likely to collide with ordinary text.
const MIN_SECRET_LEN: usize = 8;

fn registry() -> &'static RwLock<Vec<String>> {
    static R: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    R.get_or_init(|| RwLock::new(Vec::new()))
}

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // PEM private keys (whole block)
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|$)",
            // GitHub tokens
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{40,})\b",
            // OpenAI / Anthropic / OpenRouter style keys
            r"\bsk-(?:ant-|or-v1-|proj-)?[A-Za-z0-9_\-]{20,}\b",
            // AWS access key id
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            // Google API key
            r"\bAIza[0-9A-Za-z_\-]{35}\b",
            // Slack tokens
            r"\bxox[abposr]-[A-Za-z0-9-]{10,}\b",
            // HTTP bearer credentials
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/\-]{20,}=*",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("valid redaction pattern"))
        .collect()
    })
}

/// Register a resolved secret value; every later [`text`] call masks it.
pub fn register(value: &str) {
    let v = value.trim();
    if v.len() < MIN_SECRET_LEN {
        return;
    }
    let mut r = registry().write().unwrap();
    if !r.iter().any(|x| x == v) {
        r.push(v.to_string());
        // longest first so a secret containing another is masked whole
        r.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
}

/// Read an environment variable holding a credential and register it.
pub fn secret_env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok().filter(|v| !v.trim().is_empty())?;
    register(&v);
    Some(v)
}

/// Mask registered secrets and recognizable credential material.
pub fn text(s: &str) -> Cow<'_, str> {
    let mut out: Cow<'_, str> = Cow::Borrowed(s);
    {
        let r = registry().read().unwrap();
        for secret in r.iter() {
            if out.contains(secret.as_str()) {
                out = Cow::Owned(out.replace(secret.as_str(), REDACTED));
            }
        }
    }
    for p in patterns() {
        if p.is_match(&out) {
            out = Cow::Owned(p.replace_all(&out, REDACTED).into_owned());
        }
    }
    out
}

/// Error text from providers and Vera may echo endpoint URLs whose query
/// values are credentials, in any path or encoding: drop every URL query and
/// mask the rest. Only for diagnostics, never for model input or output.
pub fn diagnostic(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(https?://[^\s?#"'<>()]+)\?[^\s#"'<>()]*"#).unwrap());
    text(&re.replace_all(s, "$1?[query removed]")).into_owned()
}

/// Redact every string inside a JSON value in place.
pub fn json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => {
            if let Cow::Owned(r) = text(s) {
                *s = r;
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(json),
        serde_json::Value::Object(o) => o.values_mut().for_each(json),
        _ => {}
    }
}
