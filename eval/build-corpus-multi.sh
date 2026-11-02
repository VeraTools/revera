#!/usr/bin/env bash
# Builds the multi-defect eval repos under eval/corpus/.
# Every defect repo plants 3-5 independent defects in one PR, mixed with
# behaviour-preserving changes, so recall (not just "found the one bug") is
# measurable. Two clean controls carry similar-sized behaviour-preserving
# diffs. Generated repos are ignored; this script is the committed definition.
#
# truth.json line ranges are computed from an anchor regex at build time
# (see `truth`), so editing a file body cannot silently desync the truth.
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

# truth DIR < spec.json
# spec: {"clean": bool, "defects": [{"file", "anchor" (regex, must match
# exactly one line of the head file), "before", "after" (lines around the
# anchor), "keywords", optional "class"}]}. Writes DIR/truth.json with
# line_min/line_max resolved; fails the build on a missing/ambiguous anchor.
TRUTH_PY="$(cat <<'PY'
import json, re, sys
d = sys.argv[1]
spec = json.load(sys.stdin)
out = []
for x in spec["defects"]:
    lines = open(f"{d}/{x['file']}").read().split("\n")
    hits = [i + 1 for i, l in enumerate(lines) if re.search(x["anchor"], l)]
    if len(hits) != 1:
        sys.exit(f"{d}: anchor {x['anchor']!r} in {x['file']} matched {len(hits)} lines")
    n = hits[0]
    last = len(lines) - (1 if lines[-1] == "" else 0)  # trailing newline
    out.append({
        "file": x["file"],
        "line_min": max(1, n - x.get("before", 2)),
        "line_max": min(last, n + x.get("after", 2)),
        "keywords": x["keywords"],
        "class": x.get("class", ""),
    })
json.dump({"defects": out, "clean": spec.get("clean", False),
           "also_valid": spec.get("also_valid", [])},
          open(f"{d}/truth.json", "w"), indent=1)
PY
)"
truth() {
    python3 -c "$TRUTH_PY" "$1"
}

# ============ M1 inventory-rs (Rust) ============
D="$(repo inventory-rs)"
mkdir -p "$D/src"
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "inventory"
version = "0.1.0"
edition = "2021"
EOF
cat > "$D/src/lib.rs" <<'EOF'
pub mod cache;
pub mod page;
pub mod report;
pub mod stock;
EOF
cat > "$D/src/stock.rs" <<'EOF'
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub enum StockError {
    Unknown(String),
    Insufficient { sku: String, available: u32, wanted: u32 },
}

/// Canonical map key for a SKU. Every lookup into `Stock::items` must use it.
pub fn sku_key(sku: &str) -> String {
    format!("SKU:{}", sku)
}

#[derive(Default)]
pub struct Stock {
    pub items: HashMap<String, u32>,
}

impl Stock {
    pub fn add(&mut self, sku: &str, qty: u32) {
        *self.items.entry(sku_key(sku)).or_insert(0) += qty;
    }

    pub fn reserve(&mut self, sku: &str, qty: u32) -> Result<u32, StockError> {
        let available = self
            .items
            .get_mut(&sku_key(sku))
            .ok_or_else(|| StockError::Unknown(sku.to_string()))?;
        if *available < qty {
            return Err(StockError::Insufficient {
                sku: sku.to_string(),
                available: *available,
                wanted: qty,
            });
        }
        *available -= qty;
        Ok(*available)
    }
}
EOF
cat > "$D/src/page.rs" <<'EOF'
/// Number of pages needed to show `total` items, `size` per page.
pub fn page_count(total: usize, size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    (total + size - 1) / size
}

/// Half-open item range for zero-based `page`.
pub fn page_bounds(total: usize, page: usize, size: usize) -> (usize, usize) {
    let start = (page * size).min(total);
    let end = (start + size).min(total);
    (start, end)
}
EOF
cat > "$D/src/cache.rs" <<'EOF'
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// On-disk snapshot of stock levels: one `key=qty` per line.
pub fn load(path: &Path) -> io::Result<HashMap<String, u32>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e),
    };
    parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn parse(text: &str) -> Result<HashMap<String, u32>, String> {
    let mut out = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: missing '='", i + 1))?;
        let qty = v
            .trim()
            .parse()
            .map_err(|_| format!("line {}: bad qty", i + 1))?;
        out.insert(k.trim().to_string(), qty);
    }
    Ok(out)
}

pub fn save(path: &Path, items: &HashMap<String, u32>) -> io::Result<()> {
    let mut keys: Vec<_> = items.keys().collect();
    keys.sort();
    let body: String = keys
        .into_iter()
        .map(|k| format!("{}={}\n", k, items[k]))
        .collect();
    fs::write(path, body)
}
EOF
cat > "$D/src/report.rs" <<'EOF'
use crate::stock::Stock;

/// Low-stock report for the given SKUs (as typed by operators).
pub fn low_stock(stock: &Stock, skus: &[&str], threshold: u32) -> Vec<String> {
    skus.iter()
        .filter(|sku| {
            let key = format!("SKU:{}", sku);
            stock.items.get(&key).copied().unwrap_or(0) < threshold
        })
        .map(|s| s.to_string())
        .collect()
}
EOF
commit "$D" "base" base

cat > "$D/src/stock.rs" <<'EOF'
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub enum StockError {
    Unknown(String),
    Insufficient { sku: String, available: u32, wanted: u32 },
}

/// Canonical map key for a SKU. Every lookup into `Stock::items` must use it.
/// SKUs are case-insensitive, so the key is normalised.
pub fn sku_key(sku: &str) -> String {
    format!("sku:{}", sku.trim().to_lowercase())
}

#[derive(Default)]
pub struct Stock {
    pub items: HashMap<String, u32>,
}

impl Stock {
    pub fn add(&mut self, sku: &str, qty: u32) {
        *self.items.entry(sku_key(sku)).or_insert(0) += qty;
    }

    /// Reserve `qty` units and return what is left.
    pub fn reserve(&mut self, sku: &str, qty: u32) -> Result<u32, StockError> {
        let available = self
            .items
            .get_mut(&sku_key(sku))
            .ok_or_else(|| StockError::Unknown(sku.to_string()))?;
        let remaining = available.saturating_sub(qty);
        *available = remaining;
        Ok(remaining)
    }

    /// Reserve several lines at once; stops at the first failure.
    pub fn reserve_batch(&mut self, lines: &[(&str, u32)]) -> Result<Vec<u32>, StockError> {
        lines
            .iter()
            .map(|(sku, qty)| self.reserve(sku, *qty))
            .collect()
    }
}
EOF
cat > "$D/src/page.rs" <<'EOF'
/// Number of pages needed to show `total` items, `size` per page.
pub fn page_count(total: usize, size: usize) -> usize {
    if size == 0 {
        return 0;
    }
    total / size
}

/// Half-open item range for zero-based `page`.
pub fn page_bounds(total: usize, page: usize, size: usize) -> (usize, usize) {
    let start = page.saturating_mul(size).min(total);
    let end = start.saturating_add(size).min(total);
    (start, end)
}

/// True when `page` is the last page that has items.
pub fn is_last(total: usize, page: usize, size: usize) -> bool {
    page + 1 >= page_count(total, size)
}
EOF
cat > "$D/src/cache.rs" <<'EOF'
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// On-disk snapshot of stock levels: one `key=qty` per line.
/// A snapshot that cannot be parsed is treated as empty so startup never fails.
pub fn load(path: &Path) -> io::Result<HashMap<String, u32>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e),
    };
    Ok(parse(&text).unwrap_or_default())
}

pub fn parse(text: &str) -> Result<HashMap<String, u32>, String> {
    let mut out = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: missing '='", i + 1))?;
        let qty = v
            .trim()
            .parse()
            .map_err(|_| format!("line {}: bad qty", i + 1))?;
        out.insert(k.trim().to_string(), qty);
    }
    Ok(out)
}

/// Write the snapshot. Callers save after every change, starting from `load`.
pub fn save(path: &Path, items: &HashMap<String, u32>) -> io::Result<()> {
    let mut keys: Vec<_> = items.keys().collect();
    keys.sort();
    let body: String = keys
        .into_iter()
        .map(|k| format!("{}={}\n", k, items[k]))
        .collect();
    fs::write(path, body)
}
EOF
commit "$D" "stock: case-insensitive SKUs, batch reservations, tolerant cache" head
truth "$D" <<'EOF'
{"defects": [
 {"file": "src/stock.rs", "anchor": "let remaining = available.saturating_sub\\(qty\\);", "class": "logic",
  "keywords": ["saturating_sub", "insufficient", "over-reserv", "more than available", "never returns an error"]},
 {"file": "src/stock.rs", "anchor": "format!\\(\"sku:\\{\\}\"", "before": 3, "class": "cross-file",
  "keywords": ["report.rs", "low_stock", "key mismatch", "old key format", "SKU:"]},
 {"file": "src/page.rs", "anchor": "^    total / size$", "class": "off-by-one",
  "keywords": ["partial page", "last page", "rounds down", "round up", "ceil", "truncat"]},
 {"file": "src/cache.rs", "anchor": "unwrap_or_default\\(\\)", "class": "error-handling",
  "keywords": ["unwrap_or_default", "corrupt", "data loss", "overwrit", "silently", "parse error"]}
],
 "also_valid": [
  {"file": "src/stock.rs", "keywords": ["snapshot", "on-disk", "orphan", "persisted", "existing keys"]},
  {"file": "src/stock.rs", "keywords": ["rollback", "roll back", "partial", "earlier lines", "atomic"]}
]}
EOF

# ============ M2 billing-py (Python) ============
D="$(repo billing-py)"
mkdir -p "$D/billing"
cat > "$D/billing/__init__.py" <<'EOF'
EOF
cat > "$D/billing/proration.py" <<'EOF'
from decimal import Decimal


def prorate(price: Decimal, days_used: int, days_in_period: int) -> Decimal:
    """Charge for the used part of a billing period."""
    if days_in_period <= 0:
        raise ValueError("empty period")
    return (price * days_used / days_in_period).quantize(Decimal("0.01"))
EOF
cat > "$D/billing/invoice.py" <<'EOF'
from datetime import datetime, timezone

from billing.gateway import charge, ChargeError


class Invoice:
    def __init__(self, customer_id, amount_cents, period_end):
        if period_end.tzinfo is None:
            raise ValueError("period_end must be timezone-aware")
        self.customer_id = customer_id
        self.amount_cents = amount_cents
        self.period_end = period_end
        self.discounts = []
        self.paid = False


def settle(invoice):
    """Charge the customer; mark paid only when the charge succeeds."""
    try:
        charge(invoice.customer_id, invoice.amount_cents)
    except ChargeError:
        return False
    invoice.paid = True
    return True


def is_overdue(invoice):
    return not invoice.paid and datetime.now(timezone.utc) > invoice.period_end
EOF
cat > "$D/billing/gateway.py" <<'EOF'
class ChargeError(Exception):
    pass


def charge(customer_id, amount_cents):
    if amount_cents < 0:
        raise ChargeError("negative charge")
    return {"customer": customer_id, "amount": amount_cents}
EOF
cat > "$D/billing/db.py" <<'EOF'
import sqlite3


def connect(path=":memory:"):
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE IF NOT EXISTS coupons (code TEXT PRIMARY KEY, percent INTEGER)")
    return conn
EOF
commit "$D" "base" base

cat > "$D/billing/proration.py" <<'EOF'
from decimal import Decimal


def prorate(price: Decimal, days_used: int, days_in_period: int) -> Decimal:
    """Charge for the used part of a billing period."""
    if days_in_period <= 0:
        raise ValueError("empty period")
    return (price * days_used / days_in_period).quantize(Decimal("0.01"))


def prorate_cents(price_cents: int, days_used: int, days_in_period: int) -> int:
    """Integer-cents variant used by the invoice path."""
    if days_in_period <= 0:
        raise ValueError("empty period")
    return days_used // days_in_period * price_cents
EOF
cat > "$D/billing/coupons.py" <<'EOF'
def find_coupon(conn, code):
    """Look up a coupon by the code the customer typed at checkout."""
    row = conn.execute(
        f"SELECT code, percent FROM coupons WHERE code = '{code}'"
    ).fetchone()
    return None if row is None else {"code": row[0], "percent": row[1]}


def apply_coupons(invoice, coupons, applied=[]):
    """Apply each coupon once; returns the list of applied codes."""
    for c in coupons:
        if c["code"] in applied:
            continue
        invoice.amount_cents -= invoice.amount_cents * c["percent"] // 100
        applied.append(c["code"])
    invoice.discounts = list(applied)
    return applied
EOF
cat > "$D/billing/invoice.py" <<'EOF'
from datetime import datetime, timezone

from billing.gateway import charge, ChargeError
from billing.proration import prorate_cents


class Invoice:
    def __init__(self, customer_id, amount_cents, period_end):
        if period_end.tzinfo is None:
            raise ValueError("period_end must be timezone-aware")
        self.customer_id = customer_id
        self.amount_cents = amount_cents
        self.period_end = period_end
        self.discounts = []
        self.paid = False


def settle(invoice):
    """Charge the customer; mark paid only when the charge succeeds."""
    try:
        charge(invoice.customer_id, invoice.amount_cents)
    except Exception:
        pass
    invoice.paid = True
    return True


def prorated(invoice, days_used, days_in_period):
    invoice.amount_cents = prorate_cents(invoice.amount_cents, days_used, days_in_period)
    return invoice


def is_overdue(invoice):
    return not invoice.paid and datetime.now() > invoice.period_end
EOF
commit "$D" "billing: coupons and proration" head
truth "$D" <<'EOF'
{"defects": [
 {"file": "billing/proration.py", "anchor": "return days_used // days_in_period \\* price_cents", "class": "arithmetic",
  "keywords": ["integer division", "floor division", "always zero", "always 0", "evaluates to 0", "order of operations"]},
 {"file": "billing/coupons.py", "anchor": "WHERE code = '\\{code\\}'", "class": "security",
  "keywords": ["sql injection", "injection", "parameteriz", "placeholder"]},
 {"file": "billing/coupons.py", "anchor": "def apply_coupons\\(invoice, coupons, applied=\\[\\]\\)", "class": "state",
  "keywords": ["mutable default", "shared between calls", "shared across calls", "default argument"]},
 {"file": "billing/invoice.py", "anchor": "except Exception:", "after": 3, "class": "error-handling",
  "keywords": ["marked paid", "marks the invoice paid", "paid even", "swallow", "charge fail"]},
 {"file": "billing/invoice.py", "anchor": "datetime.now\\(\\) > invoice.period_end", "class": "type",
  "keywords": ["naive", "offset-naive", "timezone-aware", "tzinfo", "aware datetime"]}
]}
EOF

# ============ M3 session-ts (TypeScript) ============
D="$(repo session-ts)"
mkdir -p "$D/src"
cat > "$D/package.json" <<'EOF'
{ "name": "session-ts", "version": "0.1.0", "type": "module" }
EOF
cat > "$D/src/store.ts" <<'EOF'
export interface Session {
  id: string;
  userId: string;
  token: string;
  /** Expiry as a Unix timestamp in milliseconds. */
  expiresAt: number;
}

export interface SessionStore {
  get(id: string): Promise<Session | undefined>;
  put(s: Session): Promise<void>;
  delete(id: string): Promise<void>;
}
EOF
cat > "$D/src/session.ts" <<'EOF'
import { timingSafeEqual } from "node:crypto";
import type { Session, SessionStore } from "./store.js";

const TTL_MS = 30 * 60 * 1000;

export function newSession(id: string, userId: string, token: string): Session {
  return { id, userId, token, expiresAt: Date.now() + TTL_MS };
}

export function isExpired(s: Session): boolean {
  return Date.now() > s.expiresAt;
}

export function tokenMatches(s: Session, presented: string): boolean {
  const a = Buffer.from(s.token);
  const b = Buffer.from(presented);
  return a.length === b.length && timingSafeEqual(a, b);
}

export async function logout(store: SessionStore, id: string): Promise<void> {
  await store.delete(id);
}
EOF
commit "$D" "base" base

cat > "$D/src/session.ts" <<'EOF'
import type { Session, SessionStore } from "./store.js";

const TTL_SECONDS = 30 * 60;

export function newSession(id: string, userId: string, token: string): Session {
  return { id, userId, token, expiresAt: Math.floor(Date.now() / 1000) + TTL_SECONDS };
}

/** Sliding expiry: every authenticated request extends the session. */
export function touch(s: Session): Session {
  return { ...s, expiresAt: Math.floor(Date.now() / 1000) + TTL_SECONDS };
}

export function isExpired(s: Session): boolean {
  return Date.now() > s.expiresAt;
}

export function tokenMatches(s: Session, presented: string): boolean {
  return s.token === presented;
}

export async function logout(store: SessionStore, id: string): Promise<void> {
  try {
    store.delete(id);
  } catch (e) {
    console.error("logout failed", e);
  }
}
EOF
cat > "$D/src/ratelimit.ts" <<'EOF'
const WINDOW_MS = 60_000;
const LIMIT = 20;

interface Bucket {
  windowStart: number;
  count: number;
}

const buckets = new Map<string, Bucket>();

/** Fixed-window limiter keyed by client IP. Returns false when over limit. */
export function allow(ip: string, now = Date.now()): boolean {
  let b = buckets.get(ip);
  if (!b || now - b.windowStart >= WINDOW_MS) {
    b = { windowStart: now, count: 0 };
    buckets.set(ip, b);
  }
  b.count += 1;
  return b.count <= LIMIT;
}
EOF
commit "$D" "auth: sliding sessions and per-IP rate limit" head
truth "$D" <<'EOF'
{"defects": [
 {"file": "src/session.ts", "anchor": "return Date.now\\(\\) > s.expiresAt;", "before": 1, "class": "units",
  "keywords": ["seconds", "milliseconds", "always expired", "unit mismatch"]},
 {"file": "src/session.ts", "anchor": "return s.token === presented;", "class": "security",
  "keywords": ["timing", "timingsafeequal", "constant-time", "constant time"]},
 {"file": "src/session.ts", "anchor": "^    store.delete\\(id\\);", "class": "async",
  "keywords": ["await", "unhandled", "floating promise", "not awaited", "rejection"]},
 {"file": "src/ratelimit.ts", "anchor": "^const buckets = new Map", "after": 12, "class": "resource",
  "keywords": ["memory", "never removed", "never evicted", "unbounded", "grows", "evict"]}
]}
EOF

# ============ M4 worker-go (Go) ============
D="$(repo worker-go)"
cat > "$D/go.mod" <<'EOF'
module example.com/worker

go 1.21
EOF
cat > "$D/process.go" <<'EOF'
package worker

import (
	"bufio"
	"os"
)

// CountLines returns the number of lines in each file, in order.
func CountLines(paths []string) (map[string]int, error) {
	out := make(map[string]int, len(paths))
	for _, p := range paths {
		n, err := countFile(p)
		if err != nil {
			return nil, err
		}
		out[p] = n
	}
	return out, nil
}

func countFile(p string) (int, error) {
	f, err := os.Open(p)
	if err != nil {
		return 0, err
	}
	defer f.Close()
	s := bufio.NewScanner(f)
	n := 0
	for s.Scan() {
		n++
	}
	return n, s.Err()
}
EOF
commit "$D" "base" base

cat > "$D/process.go" <<'EOF'
package worker

import (
	"bufio"
	"os"
	"sync"
)

// CountLines returns the number of lines in each file. Files are counted
// concurrently; the first error wins.
func CountLines(paths []string) (map[string]int, error) {
	out := make(map[string]int, len(paths))
	var wg sync.WaitGroup
	var firstErr error
	var errOnce sync.Once
	for _, p := range paths {
		go func() {
			wg.Add(1)
			defer wg.Done()
			n, err := countFile(p)
			if err != nil {
				errOnce.Do(func() { firstErr = err })
				return
			}
			out[p] = n
		}()
	}
	wg.Wait()
	if firstErr != nil {
		return nil, firstErr
	}
	return out, nil
}

func countFile(p string) (int, error) {
	f, err := os.Open(p)
	if err != nil {
		return 0, err
	}
	defer f.Close()
	s := bufio.NewScanner(f)
	n := 0
	for s.Scan() {
		n++
	}
	return n, s.Err()
}

// HeadLines returns the first line of every file.
func HeadLines(paths []string) ([]string, error) {
	var out []string
	for _, p := range paths {
		f, err := os.Open(p)
		if err != nil {
			return nil, err
		}
		defer f.Close()
		s := bufio.NewScanner(f)
		if s.Scan() {
			out = append(out, s.Text())
		}
	}
	return out, nil
}
EOF
commit "$D" "worker: count files concurrently, add HeadLines" head
truth "$D" <<'EOF'
{"defects": [
 {"file": "process.go", "anchor": "^\t\tgo func\\(\\) \\{", "before": 1, "after": 1, "class": "concurrency",
  "keywords": ["loop variable", "captur", "closure", "same path", "go 1.21"]},
 {"file": "process.go", "anchor": "^\t\t\twg.Add\\(1\\)", "class": "concurrency",
  "keywords": ["wg.add", "add inside", "inside the goroutine", "wait returns", "before all"]},
 {"file": "process.go", "anchor": "^\t\t\tout\\[p\\] = n", "class": "concurrency",
  "keywords": ["concurrent map", "map write", "data race", "mutex", "fatal error"]},
 {"file": "process.go", "anchor": "^\t\tdefer f.Close\\(\\)", "class": "resource",
  "keywords": ["defer in", "defer inside", "file descriptor", "until the function returns", "too many open files"]}
],
 "also_valid": [
  {"file": "process.go", "keywords": ["empty file", "misalign", "nondeterministic", "non-deterministic"]},
  {"file": "process.go", "keywords": ["s.err", "scanner error", "err()", "read error"]}
]}
EOF

# ============ M5 ledger-rs (Rust, cross-file) ============
D="$(repo ledger-rs)"
mkdir -p "$D/src"
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "ledger"
version = "0.1.0"
edition = "2021"
EOF
cat > "$D/src/lib.rs" <<'EOF'
pub mod export;
pub mod money;
EOF
cat > "$D/src/money.rs" <<'EOF'
/// A monetary amount in dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Amount {
    pub value: f64,
}

impl Amount {
    pub fn parse(s: &str) -> Option<Amount> {
        s.trim().parse::<f64>().ok().map(|value| Amount { value })
    }

    pub fn add(self, other: Amount) -> Amount {
        Amount { value: self.value + other.value }
    }
}
EOF
cat > "$D/src/export.rs" <<'EOF'
use crate::money::Amount;

/// One CSV row per entry: `label,amount` with the amount in dollars.
pub fn to_csv(entries: &[(String, Amount)]) -> String {
    entries
        .iter()
        .map(|(label, a)| format!("{},{:.2}\n", label, a.value as f64))
        .collect()
}
EOF
commit "$D" "base" base

cat > "$D/src/money.rs" <<'EOF'
/// A monetary amount in integer cents (no float rounding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amount {
    pub value: i64,
}

impl Amount {
    /// Parse "12", "12.5", "12.50" or "-0.75".
    pub fn parse(s: &str) -> Option<Amount> {
        let s = s.trim();
        let (whole, frac) = match s.split_once('.') {
            Some((w, f)) => (w, f),
            None => (s, "0"),
        };
        if frac.len() > 2 {
            return None;
        }
        let whole: i64 = whole.parse().ok()?;
        let frac: i64 = frac.parse().ok()?;
        let value = if whole < 0 {
            whole * 100 - frac
        } else {
            whole * 100 + frac
        };
        Some(Amount { value })
    }

    pub fn add(self, other: Amount) -> Amount {
        Amount {
            value: self.value + other.value,
        }
    }
}
EOF
commit "$D" "money: store amounts as integer cents" head
truth "$D" <<'EOF'
{"defects": [
 {"file": "src/money.rs", "anchor": "pub value: i64,", "before": 2, "class": "cross-file",
  "keywords": ["export.rs", "to_csv", "100x", "100 times", "in cents instead of dollars", "csv"]},
 {"file": "src/money.rs", "anchor": "whole \\* 100 \\+ frac", "before": 6, "class": "parsing",
  "keywords": ["12.5", "single digit", "one digit", "pad", "tenths", "1205"]},
 {"file": "src/money.rs", "anchor": "let value = if whole < 0", "after": 4, "class": "parsing",
  "keywords": ["-0.", "negative zero", "sign is lost", "sign lost", "loses the sign", "minus sign"]}
],
 "also_valid": [
  {"file": "src/money.rs", "keywords": ["accepts a sign", "12.-5", "signed fraction", "fraction part accepts"]},
  {"file": "src/money.rs", "keywords": ["overflow", "checked_mul", "unchecked i64"]}
]}
EOF

# ============ M6 etl-py (Python, performance/resource) ============
D="$(repo etl-py)"
mkdir -p "$D/etl"
cat > "$D/etl/__init__.py" <<'EOF'
EOF
cat > "$D/etl/sync.py" <<'EOF'
import json


def fetch_users(db, ids):
    """Load users for the given ids in one query."""
    if not ids:
        return {}
    marks = ",".join("?" for _ in ids)
    rows = db.execute(f"SELECT id, name FROM users WHERE id IN ({marks})", list(ids))
    return {r[0]: r[1] for r in rows}


def dedupe(events):
    seen = set()
    out = []
    for e in events:
        if e["id"] not in seen:
            seen.add(e["id"])
            out.append(e)
    return out


def sync(db, events, checkpoint_path):
    events = dedupe(events)
    users = fetch_users(db, [e["user_id"] for e in events])
    with db:
        for e in events:
            db.execute("INSERT INTO facts VALUES (?, ?)", (e["id"], users.get(e["user_id"])))
    with open(checkpoint_path, "w") as f:
        json.dump({"last_id": events[-1]["id"] if events else None}, f)
EOF
commit "$D" "base" base

cat > "$D/etl/sync.py" <<'EOF'
import json


def fetch_user(db, user_id):
    row = db.execute("SELECT id, name FROM users WHERE id = ?", (user_id,)).fetchone()
    return None if row is None else row[1]


def dedupe(events):
    seen = []
    out = []
    for e in events:
        if e["id"] not in seen:
            seen.append(e["id"])
            out.append(e)
    return out


def load_batches(paths):
    """Yield the events from each batch file, in order."""
    for p in paths:
        f = open(p)
        yield from json.load(f)


def sync(db, events, checkpoint_path):
    """Incremental sync: write facts, then advance the checkpoint."""
    events = dedupe(events)
    with open(checkpoint_path, "w") as f:
        json.dump({"last_id": events[-1]["id"] if events else None}, f)
    with db:
        for e in events:
            name = fetch_user(db, e["user_id"])
            db.execute("INSERT INTO facts VALUES (?, ?)", (e["id"], name))
EOF
commit "$D" "etl: incremental sync from batch files" head
truth "$D" <<'EOF'
{"defects": [
 {"file": "etl/sync.py", "anchor": "name = fetch_user\\(db, e\\[\"user_id\"\\]\\)", "class": "performance",
  "keywords": ["n+1", "one query per", "query per event", "per row", "batched", "in (...)"]},
 {"file": "etl/sync.py", "anchor": "if e\\[\"id\"\\] not in seen:", "before": 3, "class": "performance",
  "keywords": ["quadratic", "o(n^2)", "o(n²)", "list membership", "linear scan", "use a set"]},
 {"file": "etl/sync.py", "anchor": "^        f = open\\(p\\)", "class": "resource",
  "keywords": ["never closed", "not closed", "file handle", "with open", "leak"]},
 {"file": "etl/sync.py", "anchor": "json.dump\\(\\{\"last_id\"", "before": 2, "class": "ordering",
  "keywords": ["checkpoint", "before the insert", "before commit", "crash", "advanced before"]}
]}
EOF

# ============ C1 clean-multi-py (Python, behaviour-preserving) ============
D="$(repo clean-multi-py)"
mkdir -p "$D/shop"
cat > "$D/shop/__init__.py" <<'EOF'
EOF
cat > "$D/shop/cart.py" <<'EOF'
def total(items):
    t = 0
    for i in items:
        t = t + i["price"] * i["qty"]
    return t


def count(items):
    c = 0
    for i in items:
        c = c + i["qty"]
    return c


def find(items, sku):
    for i in items:
        if i["sku"] == sku:
            return i
    return None
EOF
commit "$D" "base" base
cat > "$D/shop/cart.py" <<'EOF'
"""Cart helpers. Items are dicts with `sku`, `price` (cents) and `qty`."""


def line_total(item):
    """Price of one cart line in cents."""
    return item["price"] * item["qty"]


def total(items):
    """Cart total in cents."""
    return sum(line_total(i) for i in items)


def count(items):
    """Number of units in the cart."""
    return sum(i["qty"] for i in items)


def find(items, sku):
    """First line with `sku`, or None."""
    return next((i for i in items if i["sku"] == sku), None)
EOF
commit "$D" "cart: docstrings and comprehension refactor" head
truth "$D" <<'EOF'
{"defects": [], "clean": true}
EOF

# ============ C2 clean-multi-rs (Rust, behaviour-preserving) ============
D="$(repo clean-multi-rs)"
mkdir -p "$D/src"
cat > "$D/Cargo.toml" <<'EOF'
[package]
name = "clean-multi"
version = "0.1.0"
edition = "2021"
EOF
cat > "$D/src/lib.rs" <<'EOF'
pub fn parse_kv(line: &str) -> Option<(String, String)> {
    let idx = line.find('=')?;
    let k = line[..idx].trim().to_string();
    let v = line[idx + 1..].trim().to_string();
    if k.is_empty() {
        return None;
    }
    Some((k, v))
}

pub fn parse_all(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(kv) = parse_kv(line) {
            out.push(kv);
        }
    }
    out
}
EOF
commit "$D" "base" base
cat > "$D/src/lib.rs" <<'EOF'
/// Parse one `key = value` line. Returns `None` without a `=` or with an
/// empty key; the value may be empty.
pub fn parse_kv(line: &str) -> Option<(String, String)> {
    let (k, v) = line.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

/// Every parseable line of `text`, in order.
pub fn parse_all(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(parse_kv).collect()
}
EOF
commit "$D" "kv: split_once and iterator refactor" head
truth "$D" <<'EOF'
{"defects": [], "clean": true}
EOF

echo "multi corpus built in $ROOT"
