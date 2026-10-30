//! Secret containment for every egress surface (model/provider requests,
//! GitHub comments, reports, state, logs): exact configured secret values
//! plus conservative detection of recognizable credential material.

use aho_corasick::{AhoCorasick, MatchKind};
use regex::Regex;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

pub const REDACTED: &str = "[REDACTED]";

/// Shorter values are too likely to collide with ordinary text.
const MIN_SECRET_LEN: usize = 8;

/// How long an expired credential stays masked, so output written after its
/// expiry (late logs, child process errors) still cannot show it.
const EXPIRED_GRACE: Duration = Duration::from_secs(3600);

/// Every registered value, plus a matcher compiled from them. Credentials
/// minted per request expire, so a long-running server holds only the
/// values that are still live (or recently expired) instead of every token
/// it ever minted.
#[derive(Default)]
struct Registry {
    permanent: HashSet<String>,
    expiring: HashMap<String, Instant>,
    scoped: HashMap<String, usize>,
    /// Dropped on every change and rebuilt on the next [`text`] call.
    matcher: Option<Arc<Matcher>>,
}

impl Registry {
    fn register(&mut self, value: &str) {
        let removed = self.expiring.remove(value).is_some();
        if self.permanent.insert(value.into()) || removed {
            self.matcher = None;
        }
    }

    fn register_until(&mut self, value: &str, until: Instant, now: Instant) {
        self.prune(now);
        if self.permanent.contains(value) {
            return;
        }
        let mut changed = false;
        let entry = self.expiring.entry(value.into()).or_insert_with(|| {
            changed = true;
            until
        });
        if *entry < until {
            *entry = until;
            changed = true;
        }
        if changed {
            self.matcher = None;
        }
    }

    fn add_scoped(&mut self, value: &str) {
        let count = self.scoped.entry(value.into()).or_default();
        *count += 1;
        if *count == 1 {
            self.matcher = None;
        }
    }

    fn remove_scoped(&mut self, value: &str) {
        if let Some(count) = self.scoped.get_mut(value) {
            *count -= 1;
            if *count == 0 {
                self.scoped.remove(value);
                self.matcher = None;
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        let before = self.expiring.len();
        self.expiring.retain(|_, until| *until > now);
        if self.expiring.len() != before {
            self.matcher = None;
        }
    }

    fn matcher(&mut self, now: Instant) -> Arc<Matcher> {
        self.prune(now);
        if let Some(matcher) = self.matcher.as_ref().filter(|m| m.current(now)) {
            return matcher.clone();
        }
        let secrets = self
            .permanent
            .iter()
            .chain(self.expiring.keys())
            .chain(self.scoped.keys())
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        let matcher = Arc::new(Matcher::new(secrets, self.expiring.values().min().copied()));
        self.matcher = Some(matcher.clone());
        matcher
    }
}

struct Matcher {
    automaton: Option<AhoCorasick>,
    /// Used only if the automaton cannot be built.
    secrets: Vec<String>,
    /// The earliest expiry among the compiled values.
    valid_until: Option<Instant>,
}

impl Matcher {
    fn new(mut secrets: Vec<String>, valid_until: Option<Instant>) -> Self {
        // longest first so a secret containing another is masked whole
        secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        secrets.dedup();
        let automaton = (!secrets.is_empty())
            .then(|| {
                AhoCorasick::builder()
                    .match_kind(MatchKind::Standard)
                    .build(&secrets)
                    .ok()
            })
            .flatten();
        Self {
            automaton,
            secrets,
            valid_until,
        }
    }

    fn current(&self, now: Instant) -> bool {
        self.valid_until.is_none_or(|until| now < until)
    }

    /// Masks every byte covered by any occurrence of any secret; overlapping
    /// occurrences become a single marker, so no part of either is visible.
    fn mask<'a>(&self, s: &'a str) -> Cow<'a, str> {
        let Some(automaton) = &self.automaton else {
            let mut out: Cow<'_, str> = Cow::Borrowed(s);
            for secret in &self.secrets {
                if out.contains(secret.as_str()) {
                    out = Cow::Owned(out.replace(secret.as_str(), REDACTED));
                }
            }
            return out;
        };
        let mut spans: Vec<(usize, usize)> = automaton
            .find_overlapping_iter(s)
            .map(|m| (m.start(), m.end()))
            .collect();
        if spans.is_empty() {
            return Cow::Borrowed(s);
        }
        spans.sort_unstable();
        let mut out = String::with_capacity(s.len());
        let mut copied = 0;
        let mut current: Option<(usize, usize)> = None;
        for (start, end) in spans {
            match &mut current {
                Some((_, current_end)) if start < *current_end => {
                    *current_end = (*current_end).max(end);
                }
                _ => {
                    if let Some((span_start, span_end)) = current.replace((start, end)) {
                        out.push_str(&s[copied..span_start]);
                        out.push_str(REDACTED);
                        copied = span_end;
                    }
                }
            }
        }
        if let Some((span_start, span_end)) = current {
            out.push_str(&s[copied..span_start]);
            out.push_str(REDACTED);
            copied = span_end;
        }
        out.push_str(&s[copied..]);
        Cow::Owned(out)
    }
}

fn registry() -> &'static RwLock<Registry> {
    static R: OnceLock<RwLock<Registry>> = OnceLock::new();
    R.get_or_init(|| RwLock::new(Registry::default()))
}

fn write_registry() -> std::sync::RwLockWriteGuard<'static, Registry> {
    registry().write().unwrap_or_else(PoisonError::into_inner)
}

fn matcher() -> Arc<Matcher> {
    let now = Instant::now();
    if let Some(matcher) = registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .matcher
        .as_ref()
        .filter(|m| m.current(now))
    {
        return matcher.clone();
    }
    write_registry().matcher(now)
}

/// Keep a short-lived credential redacted without retaining it after its use.
pub struct ScopedSecret(String);
impl ScopedSecret {
    pub fn new(value: &str) -> Self {
        write_registry().add_scoped(value);
        Self(value.into())
    }
}
impl Drop for ScopedSecret {
    fn drop(&mut self) {
        write_registry().remove_scoped(&self.0);
    }
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
    write_registry().register(v);
}

/// Register a credential that stops working after `valid_for`, such as a
/// GitHub App JWT or installation token. It stays masked for that long plus
/// a grace period, then is forgotten so a long-running server does not keep
/// (and scan for) every credential it ever minted.
pub fn register_expiring(value: &str, valid_for: Duration) {
    let v = value.trim();
    if v.len() < MIN_SECRET_LEN {
        return;
    }
    let now = Instant::now();
    write_registry().register_until(v, now + valid_for + EXPIRED_GRACE, now);
}

/// Read an environment variable holding a credential and register it.
pub fn secret_env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok().filter(|v| !v.trim().is_empty())?;
    register(&v);
    Some(v)
}

/// Mask registered secrets and recognizable credential material.
pub fn text(s: &str) -> Cow<'_, str> {
    let mut out = matcher().mask(s);
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
    let re = RE.get_or_init(|| Regex::new(r#"(https?://[^\s?#"'<>]+)\?[^\s#"'<>]*"#).unwrap());
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

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostic_strips_queries_even_with_parentheses() {
        let out = super::diagnostic("error (url: https://h/v1?key=a(b)c&x=1) end");
        assert!(!out.contains("a(b)c"), "{out}");
        assert!(!out.contains("x=1"), "{out}");
        assert!(out.contains("https://h/v1?[query removed]"), "{out}");
    }

    fn masked(registry: &mut super::Registry, s: &str) -> String {
        registry
            .matcher(std::time::Instant::now())
            .mask(s)
            .into_owned()
    }

    #[test]
    fn overlapping_and_nested_secrets_are_masked_whole() {
        let mut registry = super::Registry::default();
        registry.register("xxabcdefgh");
        registry.register("abcdefghijklmn");
        registry.register("cdefgh12");
        assert_eq!(masked(&mut registry, "<xxabcdefghijklmn>"), "<[REDACTED]>");
        assert_eq!(
            masked(&mut registry, "abcdefghijklmn abcdefghijklmn"),
            "[REDACTED] [REDACTED]"
        );
        assert_eq!(
            masked(&mut registry, "xxabcdefghxxabcdefgh"),
            "[REDACTED][REDACTED]"
        );
        assert_eq!(masked(&mut registry, "é cdefgh12 ü"), "é [REDACTED] ü");
        assert_eq!(masked(&mut registry, "nothing here"), "nothing here");
    }

    #[test]
    fn expiring_secrets_are_forgotten_after_expiry_and_matcher_rebuilds() {
        use std::time::{Duration, Instant};
        let mut registry = super::Registry::default();
        let now = Instant::now();
        registry.register("permanent-secret");
        registry.register_until("short-lived-token", now + Duration::from_secs(60), now);
        let text = "permanent-secret short-lived-token";
        assert_eq!(registry.matcher(now).mask(text), "[REDACTED] [REDACTED]");
        let later = now + Duration::from_secs(61);
        assert_eq!(
            registry.matcher(later).mask(text),
            "[REDACTED] short-lived-token"
        );
        assert!(registry.expiring.is_empty());
        // Re-registering a value permanently promotes it and is never pruned.
        registry.register_until("permanent-secret", later, later);
        assert!(registry.expiring.is_empty());
    }

    #[test]
    fn registering_many_expiring_tokens_stays_bounded() {
        use std::time::{Duration, Instant};
        let mut registry = super::Registry::default();
        let start = Instant::now();
        for i in 0..10_000u64 {
            let now = start + Duration::from_secs(i);
            registry.register_until(
                &format!("token-{i:08}"),
                now + Duration::from_secs(100),
                now,
            );
        }
        assert!(
            registry.expiring.len() <= 101,
            "{}",
            registry.expiring.len()
        );
    }

    #[test]
    fn scoped_secrets_are_masked_only_while_held() {
        let mut registry = super::Registry::default();
        registry.add_scoped("oauth-user-token");
        registry.add_scoped("oauth-user-token");
        assert_eq!(masked(&mut registry, "oauth-user-token"), "[REDACTED]");
        registry.remove_scoped("oauth-user-token");
        assert_eq!(masked(&mut registry, "oauth-user-token"), "[REDACTED]");
        registry.remove_scoped("oauth-user-token");
        assert_eq!(
            masked(&mut registry, "oauth-user-token"),
            "oauth-user-token"
        );
    }

    #[test]
    fn global_text_masks_registered_and_expiring_values() {
        super::register("redact-module-permanent-test-secret");
        super::register_expiring(
            "redact-module-expiring-test-secret",
            std::time::Duration::from_secs(60),
        );
        let out = super::text(
            "a redact-module-permanent-test-secret b redact-module-expiring-test-secret c",
        );
        assert_eq!(out, "a [REDACTED] b [REDACTED] c");
    }
}
