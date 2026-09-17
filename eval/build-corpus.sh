#!/usr/bin/env bash
# Builds the M5 eval corpus: 6 small synthetic git repos under eval/corpus/,
# each with branches `base` and `head`. Deterministic: fixed author/committer
# dates so SHAs are stable. Ground truth lives in <name>/truth.json.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$HERE/corpus"
ROOT="$(cd "$HERE/corpus" && pwd)"

export GIT_AUTHOR_NAME="eval" GIT_AUTHOR_EMAIL="eval@example.com"
export GIT_COMMITTER_NAME="eval" GIT_COMMITTER_EMAIL="eval@example.com"
export GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z"

repo() { # repo <name>; then cd-style ops
    local d="$ROOT/$1"
    rm -rf "$d"
    mkdir -p "$d"
    git -C "$d" init -q -b main
    echo "$d"
}

commit() { # commit <dir> <msg> <tag?>
    git -C "$1" add -A
    GIT_COMMITTER_DATE="${GIT_COMMITTER_DATE} +1 hour" git -C "$1" commit -qm "$2"
    [ -n "${3:-}" ] && git -C "$1" tag "$3"
}

# ============ 1. crossfile (percent/fraction unit break) ============
D="$(repo crossfile)"
mkdir -p "$D/src" "$D/tests"
cat > "$D/src/lib.rs" <<'EOF'
pub mod checkout;
pub mod pricing;
EOF
cat > "$D/src/pricing.rs" <<'EOF'
/// Returns the discount for a customer tier as a fraction (0.0-1.0).
pub fn discount_for_tier(tier: &str) -> f64 {
    match tier {
        "gold" => 0.2,
        "silver" => 0.1,
        _ => 0.0,
    }
}
EOF
cat > "$D/src/checkout.rs" <<'EOF'
use crate::pricing::discount_for_tier;

/// Final price after applying the tier discount (fraction of base).
pub fn final_price(base: f64, tier: &str) -> f64 {
    let d = discount_for_tier(tier);
    base * (1.0 - d)
}
EOF
cat > "$D/tests/pricing_test.rs" <<'EOF'
use crossfile::pricing::discount_for_tier;

#[test]
fn gold_is_twenty_percent() {
    assert!((discount_for_tier("gold") - 0.2).abs() < 1e-9);
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "crossfile"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" base base

cat > "$D/src/pricing.rs" <<'EOF'
/// Returns the discount for a customer tier as a percentage (0-100).
pub fn discount_for_tier(tier: &str) -> f64 {
    match tier {
        "gold" => 20.0,
        "silver" => 10.0,
        _ => 0.0,
    }
}
EOF
cat > "$D/tests/pricing_test.rs" <<'EOF'
use crossfile::pricing::discount_for_tier;

#[test]
fn gold_is_twenty_percent() {
    assert!((discount_for_tier("gold") - 20.0).abs() < 1e-9);
}
EOF
commit "$D" "break: discount returns percent" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/pricing.rs", "line_min": 1, "line_max": 8,
  "keywords": ["percent", "fraction", "discount", "final_price", "checkout"]}],
 "clean": false}
EOF

# ============ 2. nullpath (Python: parse_config now returns None) ============
D="$(repo nullpath)"
cat > "$D/config_loader.py" <<'EOF'
import json

def parse_config(path):
    """Parse the JSON config file. Raises IOError if missing."""
    with open(path) as f:
        return json.load(f)

def load(path):
    cfg = parse_config(path)
    return cfg["port"]
EOF
cat > "$D/app.py" <<'EOF'
from config_loader import load

def serve(path):
    return f"listening on {load(path)}"
EOF
commit "$D" base base

cat > "$D/config_loader.py" <<'EOF'
import json
import os

def parse_config(path):
    """Parse the JSON config file. Returns None if the file is missing."""
    if not os.path.exists(path):
        return None
    with open(path) as f:
        return json.load(f)

def load(path):
    cfg = parse_config(path)
    return cfg["port"]
EOF
commit "$D" "config: return None for missing file" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "config_loader.py", "line_min": 4, "line_max": 9,
  "keywords": ["none", "missing", "subscript", "cfg[\"port\"]", "load", "parse_config"]}],
 "clean": false}
EOF

# ============ 3. offbyone (Go pagination end +1) ============
D="$(repo offbyone)"
cat > "$D/pages.go" <<'EOF'
package pages

// Page returns items[start:end] clamped to len(items).
func Page(items []int, start, size int) []int {
	end := start + size
	if end > len(items) {
		end = len(items)
	}
	if start < 0 || start > end {
		return nil
	}
	return items[start:end]
}
EOF
cat > "$D/pages_test.go" <<'EOF'
package pages

import "testing"

func TestPage(t *testing.T) {
	got := Page([]int{1, 2, 3}, 0, 2)
	if len(got) != 2 {
		t.Fatalf("want 2 got %d", len(got))
	}
}
EOF
commit "$D" base base

python3 - "$D/pages.go" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("end := start + size\n", "end := start + size + 1\n")
open(p, "w").write(s)
PY
commit "$D" "pages: inclusive end bound" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "pages.go", "line_min": 4, "line_max": 6,
  "keywords": ["off-by-one", "off by one", "end", "size", "page"]}],
 "clean": false}
EOF

# ============ 4. lockdrop (Rust: increment outside Mutex guard) ============
D="$(repo lockdrop)"
mkdir -p "$D/src"
cat > "$D/src/lib.rs" <<'EOF'
pub mod counter;
EOF
cat > "$D/src/counter.rs" <<'EOF'
use std::sync::Mutex;

pub struct Counter {
    inner: Mutex<u64>,
}

impl Counter {
    /// Increment the counter under the lock.
    pub fn bump(&self) {
        let mut n = self.inner.lock().unwrap();
        *n += 1;
    }
}
EOF
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "lockdrop"
version = "0.1.0"
edition = "2021"
EOF
commit "$D" base base

cat > "$D/src/counter.rs" <<'EOF'
use std::sync::Mutex;

pub struct Counter {
    inner: Mutex<u64>,
}

impl Counter {
    /// Increment the counter.
    pub fn bump(&self) {
        // read current value while holding the lock
        let mut n = *self.inner.lock().unwrap();
        n += 1; // update the local copy
        *self.inner.lock().unwrap() = n;
    }
}
EOF
commit "$D" "counter: hold lock only for the read" head
cat > "$D/truth.json" <<'EOF'
{"defects": [{"file": "src/counter.rs", "line_min": 9, "line_max": 14,
  "keywords": ["mutex", "lock", "race", "stale", "counter", "atomic", "guard"]}],
 "clean": false}
EOF

# ============ 5. clean-refactor (TypeScript rename + extract) ============
D="$(repo clean-refactor)"
mkdir -p "$D/src"
cat > "$D/src/api.ts" <<'EOF'
export function getUser(id: string): { id: string; name: string } {
  return { id, name: "user-" + id };
}
EOF
cat > "$D/src/profile.ts" <<'EOF'
import { getUser } from "./api";

export function profileLabel(id: string): string {
  return getUser(id).name;
}
EOF
cat > "$D/src/index.ts" <<'EOF'
import { getUser } from "./api";

export function main(): string {
  return getUser("42").name;
}
EOF
cat > "$D/package.json" <<'EOF'
{"name": "clean-refactor", "version": "0.1.0"}
EOF
commit "$D" base base

cat > "$D/src/api.ts" <<'EOF'
function toUser(id: string, name: string): { id: string; name: string } {
  return { id, name };
}

export function fetchUser(id: string): { id: string; name: string } {
  return toUser(id, "user-" + id);
}
EOF
cat > "$D/src/profile.ts" <<'EOF'
import { fetchUser } from "./api";

export function profileLabel(id: string): string {
  return fetchUser(id).name;
}
EOF
cat > "$D/src/index.ts" <<'EOF'
import { fetchUser } from "./api";

export function main(): string {
  return fetchUser("42").name;
}
EOF
commit "$D" "refactor: rename getUser -> fetchUser, extract toUser" head
cat > "$D/truth.json" <<'EOF'
{"defects": [], "clean": true}
EOF

# ============ 6. clean-docs (Markdown + comments only) ============
D="$(repo clean-docs)"
cat > "$D/README.md" <<'EOF'
# widget

A small widget library.
EOF
cat > "$D/widget.py" <<'EOF'
def widget_size(w):
    # return the width
    return w.width
EOF
commit "$D" base base

cat > "$D/README.md" <<'EOF'
# widget

A small widget library.

## Usage

Call `widget_size(w)` to get the width.
EOF
cat > "$D/widget.py" <<'EOF'
def widget_size(w):
    # return the width of the widget in pixels
    return w.width
EOF
commit "$D" "docs: usage notes, clarify comment" head
cat > "$D/truth.json" <<'EOF'
{"defects": [], "clean": true}
EOF

echo "corpus built in $ROOT"
