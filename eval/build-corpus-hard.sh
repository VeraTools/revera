#!/usr/bin/env bash
# Builds the seven hard eval repos under eval/corpus/.
# Generated repos are ignored; this script is the committed corpus definition.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$HERE/corpus"
mkdir -p "$ROOT"
ROOT="$(cd "$ROOT" && pwd)"

export GIT_AUTHOR_NAME="eval" GIT_AUTHOR_EMAIL="eval@example.com"
export GIT_COMMITTER_NAME="eval" GIT_COMMITTER_EMAIL="eval@example.com"
export GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z"

repo() {
    local d="$ROOT/$1"
    rm -rf "$d"
    mkdir -p "$d"
    git -C "$d" init -q -b main
    echo "$d"
}

commit() {
    git -C "$1" add -A
    GIT_COMMITTER_DATE="${GIT_COMMITTER_DATE} +1 hour" git -C "$1" commit -qm "$2"
    [ -n "${3:-}" ] && git -C "$1" tag "$3"
}

# ============ H1 utf8-truncate ============
D="$(repo utf8-truncate)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod text;
pub mod tools;
EOF
cat > "$D/src/text.rs" <<'EOF'
pub fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
EOF
cat > "$D/src/tools.rs" <<'EOF'
use crate::text::truncate;

/// Tool results may contain arbitrary model-visible file contents, including non-ASCII source and emoji.
pub fn tool_output(s: &str) -> String {
    truncate(s, 4000)
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "utf8-truncate"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" "base" base

cat > "$D/src/text.rs" <<'EOF'
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { return s.to_string(); }
    s[..max].to_string()
}
EOF
commit "$D" "perf: truncate by bytes" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/text.rs", "line_min": 1, "line_max": 6,
  "keywords": ["utf-8", "utf8", "char boundary", "multibyte", "multi-byte", "panic", "byte index", "is_char_boundary"]}],
 "clean": false}
EOF

# ============ H2 modzero-routing ============
D="$(repo modzero-routing)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod config;
pub mod dispatch;
EOF
cat > "$D/src/config.rs" <<'EOF'
#[derive(Clone, serde::Deserialize)]
pub struct Route {
    pub name: String,
}

#[derive(Clone, serde::Deserialize)]
pub struct Config {
    pub primary: Route,
    /// Optional. When absent, all tasks go to `primary`.
    pub workers: Option<Vec<Route>>,
}

pub fn default_config() -> Config {
    Config {
        primary: Route { name: "primary".into() },
        workers: None,
    }
}
EOF
cat > "$D/src/dispatch.rs" <<'EOF'
use crate::config::{Config, Route};

pub fn assign(cfg: &Config, n: usize) -> Vec<Route> {
    let routes = cfg.workers.clone().unwrap_or_else(|| vec![cfg.primary.clone()]);
    (0..n).map(|i| routes[i % routes.len()].clone()).collect()
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "modzero-routing"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
EOF
commit "$D" "base" base

cat > "$D/src/config.rs" <<'EOF'
#[derive(Clone, serde::Deserialize)]
pub struct Route {
    pub name: String,
}

#[derive(Clone, serde::Deserialize)]
pub struct Config {
    pub primary: Route,
    /// An empty list means no workers.
    #[serde(default)]
    pub workers: Vec<Route>,
}

pub fn default_config() -> Config {
    Config {
        primary: Route { name: "primary".into() },
        workers: vec![],
    }
}
EOF
cat > "$D/src/dispatch.rs" <<'EOF'
use crate::config::{Config, Route};

pub fn assign(cfg: &Config, n: usize) -> Vec<Route> {
    let routes = cfg.workers.clone();
    (0..n).map(|i| routes[i % routes.len()].clone()).collect()
}
EOF
commit "$D" "config: make workers non-optional" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/dispatch.rs", "line_min": 1, "line_max": 6,
  "keywords": ["empty", "zero", "modulo", "remainder", "division by zero", "len() == 0", "panic", "primary", "fallback"]}],
 "clean": false}
EOF

# ============ H3 retry-after ============
D="$(repo retry-after)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod http;
pub mod retry;
EOF
cat > "$D/src/retry.rs" <<'EOF'
use std::time::Duration;

pub fn delay_for(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(6))
}
EOF
cat > "$D/src/http.rs" <<'EOF'
use std::time::Duration;

use crate::retry;

/// Servers may send Retry-After either as delta-seconds or as an HTTP-date (RFC 7231 §7.1.3).
pub fn send_with_retry(header: Option<&str>, attempt: u32) -> Duration {
    let _ = header;
    retry::delay_for(attempt)
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "retry-after"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" "base" base

cat > "$D/src/retry.rs" <<'EOF'
use std::time::Duration;

pub fn delay_for(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(6))
}

pub fn delay_from_header(value: Option<&str>, attempt: u32) -> Duration {
    match value {
        Some(v) => Duration::from_secs(v.trim().parse::<u64>().unwrap()),
        None => delay_for(attempt),
    }
}
EOF
cat > "$D/src/http.rs" <<'EOF'
use std::time::Duration;

use crate::retry;

/// Servers may send Retry-After either as delta-seconds or as an HTTP-date (RFC 7231 §7.1.3).
pub fn send_with_retry(header: Option<&str>, attempt: u32) -> Duration {
    retry::delay_from_header(header, attempt)
}
EOF
commit "$D" "retry: parse Retry-After" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/retry.rs", "line_min": 1, "line_max": 12,
  "keywords": ["unwrap", "http-date", "http date", "panic", "parse", "retry-after", "malformed", "non-numeric"]}],
 "clean": false}
EOF

# ============ H4 posted-state ============
D="$(repo posted-state)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod publish;
pub mod run;
pub mod state;
EOF
cat > "$D/src/state.rs" <<'EOF'
pub struct Finding {
    pub id: String,
    pub posted: bool,
    pub inline: bool,
}

pub struct State {
    pub patch_id: String,
    pub findings: Vec<Finding>,
}

impl State {
    pub fn mark_posted(&mut self, ids: &[String]) {
        for finding in &mut self.findings {
            if ids.iter().any(|id| id == &finding.id) {
                finding.posted = true;
            }
        }
    }

    pub fn all_posted(&self) -> bool {
        self.findings.iter().all(|finding| finding.posted)
    }
}
EOF
cat > "$D/src/publish.rs" <<'EOF'
use crate::state::State;

pub struct Plan {
    pub inline: Vec<String>,
    pub summary: Vec<String>,
}

pub fn publish(state: &mut State, plan: &Plan) {
    let posted_inline = plan.inline.clone();
    state.mark_posted(&posted_inline);
    let _summary_lists_all_findings = &plan.summary;
}
EOF
cat > "$D/src/run.rs" <<'EOF'
use crate::publish::{publish, Plan};
use crate::state::State;

pub enum Outcome {
    Unchanged,
    Reviewed,
}

pub fn run(state: &mut State, patch_id: &str, plan: &Plan) -> Outcome {
    publish(state, plan);
    state.patch_id = patch_id.to_string();
    Outcome::Reviewed
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "posted-state"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" "base" base

cat > "$D/src/run.rs" <<'EOF'
use crate::publish::{publish, Plan};
use crate::state::State;

pub enum Outcome {
    Unchanged,
    Reviewed,
}

pub fn run(state: &mut State, patch_id: &str, plan: &Plan) -> Outcome {
    if state.patch_id == patch_id && state.all_posted() {
        return Outcome::Unchanged;
    }
    publish(state, plan);
    state.patch_id = patch_id.to_string();
    Outcome::Reviewed
}
EOF
commit "$D" "run: skip unchanged patches" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/run.rs", "line_min": 1, "line_max": 30,
  "keywords": ["summary", "mark_posted", "posted", "never", "short-circuit", "short circuit", "re-run", "rerun", "re-review", "all_posted"]}],
 "clean": false}
EOF

# ============ V1 trait-contract ============
D="$(repo trait-contract)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod cache;
pub mod memory;
pub mod storage;
EOF
cat > "$D/src/storage.rs" <<'EOF'
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotFound;

/// Storage contract: `get` MUST return `Err(NotFound)` for a missing key;
/// callers rely on the error to trigger a fill.
pub trait Storage {
    fn get(&self, key: &str) -> Result<Vec<u8>, NotFound>;
    fn put(&self, key: &str, value: Vec<u8>);
}
EOF
cat > "$D/src/memory.rs" <<'EOF'
use std::collections::HashMap;
use std::sync::Mutex;

use crate::storage::{NotFound, Storage};

pub struct MemoryStorage {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self { values: Mutex::new(HashMap::new()) }
    }
}

impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Result<Vec<u8>, NotFound> {
        self.values.lock().unwrap().get(key).cloned().ok_or(NotFound)
    }

    fn put(&self, key: &str, value: Vec<u8>) {
        self.values.lock().unwrap().insert(key.to_string(), value);
    }
}
EOF
cat > "$D/src/cache.rs" <<'EOF'
use crate::storage::{NotFound, Storage};

pub struct Cache<S: Storage> {
    storage: S,
}

impl<S: Storage> Cache<S> {
    pub fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn get_or_fill<F>(&self, key: &str, fill: F) -> Vec<u8>
    where
        F: FnOnce() -> Vec<u8>,
    {
        match self.storage.get(key) {
            Err(NotFound) => {
                let value = fill();
                self.storage.put(key, value.clone());
                value
            }
            Ok(value) => value,
        }
    }
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "trait-contract"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" "base" base

cat > "$D/src/memory.rs" <<'EOF'
use std::collections::HashMap;
use std::sync::Mutex;

use crate::storage::{NotFound, Storage};

pub struct MemoryStorage {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self { values: Mutex::new(HashMap::new()) }
    }
}

impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Result<Vec<u8>, NotFound> {
        Ok(self.values.lock().unwrap().get(key).cloned().unwrap_or_default())
    }

    fn put(&self, key: &str, value: Vec<u8>) {
        self.values.lock().unwrap().insert(key.to_string(), value);
    }
}
EOF
commit "$D" "memory: avoid error allocation on miss" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/memory.rs", "line_min": 1, "line_max": 30,
  "keywords": ["contract", "caller", "cache", "get_or_fill", "fill", "notfound", "not found", "empty vec", "empty value", "never"]}],
 "clean": false}
EOF

# ============ V2 clean-signature ============
D="$(repo clean-signature)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod cli;
pub mod main_cfg;
pub mod parse;
pub mod server;
EOF
cat > "$D/src/parse.rs" <<'EOF'
pub struct Cfg {
    pub port: Option<u16>,
}

pub struct ParseError;

impl Cfg {
    pub fn validate(&self) -> Result<(), ParseError> {
        if self.port.is_none() { Err(ParseError) } else { Ok(()) }
    }
}

pub fn parse(s: &str) -> Result<Cfg, ParseError> {
    let port = s.parse::<u16>().ok();
    Ok(Cfg { port })
}
EOF
cat > "$D/src/main_cfg.rs" <<'EOF'
use crate::parse::{parse, Cfg, ParseError};

pub fn load(text: &str) -> Result<Cfg, ParseError> {
    parse(text)
}
EOF
cat > "$D/src/server.rs" <<'EOF'
use crate::parse::{parse, ParseError};

pub fn serve(text: &str) -> Result<u16, ParseError> {
    let cfg = parse(text)?;
    let port = cfg.port.unwrap_or(0);
    Ok(port)
}
EOF
cat > "$D/src/cli.rs" <<'EOF'
use crate::parse::{parse, ParseError};

pub fn command(text: &str) -> Result<bool, ParseError> {
    let cfg = parse(text)?;
    Ok(cfg.port.is_some())
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "clean-signature"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" "base" base

cat > "$D/src/parse.rs" <<'EOF'
pub struct Cfg {
    pub port: Option<u16>,
}

pub enum ParseError {
    MissingPort,
}

impl Cfg {
    pub fn validate(&self) -> Result<(), ParseError> {
        if self.port.is_none() { Err(ParseError::MissingPort) } else { Ok(()) }
    }
}

pub fn parse(s: &str, strict: bool) -> Result<Cfg, ParseError> {
    let port = s.parse::<u16>().ok();
    if strict && port.is_none() {
        return Err(ParseError::MissingPort);
    }
    Ok(Cfg { port })
}
EOF
cat > "$D/src/main_cfg.rs" <<'EOF'
use crate::parse::{parse, Cfg, ParseError};

pub fn load(text: &str) -> Result<Cfg, ParseError> {
    parse(text, true)
}
EOF
cat > "$D/src/server.rs" <<'EOF'
use crate::parse::{parse, ParseError};

pub fn serve(text: &str) -> Result<u16, ParseError> {
    let cfg = parse(text, true)?;
    cfg.validate()?;
    let port = cfg.port.unwrap(); // validated non-None by Cfg::validate() above
    Ok(port)
}
EOF
cat > "$D/src/cli.rs" <<'EOF'
use crate::parse::{parse, ParseError};

pub fn command(text: &str) -> Result<bool, ParseError> {
    let cfg = parse(text, false)?;
    Ok(cfg.port.is_some())
}
EOF
commit "$D" "parse: add strict mode" head
cat > "$D/truth.json" <<'EOF'
{"defects": [], "clean": true}
EOF

# ============ V3 clean-dead-helper ============
D="$(repo clean-dead-helper)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod legacy;
pub mod render;
pub mod report;
EOF
cat > "$D/src/legacy.rs" <<'EOF'
pub fn format_row(name: &str) -> String {
    format!("legacy:{name}")
}

pub fn legacy_marker() -> &'static str {
    "legacy"
}
EOF
cat > "$D/src/render.rs" <<'EOF'
pub fn format_row(name: &str) -> String {
    format!("render:{name}")
}
EOF
cat > "$D/src/report.rs" <<'EOF'
use crate::render::format_row;

pub fn report(name: &str) -> String {
    println!("legacy format_row kept for reference");
    format_row(name)
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "clean-dead-helper"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" "base" base

cat > "$D/src/legacy.rs" <<'EOF'
pub fn legacy_marker() -> &'static str {
    "legacy"
}
EOF
commit "$D" "legacy: remove unused formatter" head
cat > "$D/truth.json" <<'EOF'
{"defects": [], "clean": true}
EOF

echo "hard corpus built in $ROOT"
