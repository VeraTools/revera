#!/usr/bin/env bash
# Creates two fixture git repos under $1 (default /tmp/revera-fixtures).
#   crossfile: base -> break (contract change breaks unchanged caller) -> fix
#   clean:     base -> docs (comment-only change)
set -euo pipefail

ROOT="${1:-/tmp/revera-fixtures}"
rm -rf "$ROOT"
mkdir -p "$ROOT"

mkbase() {
    local dir="$1"
    mkdir -p "$dir/src" "$dir/tests"
    cat > "$dir/src/lib.rs" <<'EOF'
pub mod checkout;
pub mod pricing;
EOF
    cat > "$dir/src/pricing.rs" <<'EOF'
/// Returns the discount for a customer tier as a fraction (0.0-1.0).
pub fn discount_for_tier(tier: &str) -> f64 {
    match tier {
        "gold" => 0.2,
        "silver" => 0.1,
        _ => 0.0,
    }
}
EOF
    cat > "$dir/src/checkout.rs" <<'EOF'
use crate::pricing::discount_for_tier;

/// Final price after applying the tier discount (fraction of base).
pub fn final_price(base: f64, tier: &str) -> f64 {
    let d = discount_for_tier(tier);
    base * (1.0 - d)
}
EOF
    cat > "$dir/tests/pricing_test.rs" <<'EOF'
use crossfile::pricing::discount_for_tier;

#[test]
fn gold_is_twenty_percent() {
    assert!((discount_for_tier("gold") - 0.2).abs() < 1e-9);
}
EOF
    cat > "$dir/Cargo.toml" <<'EOF'
[package]
name = "crossfile"
version = "0.1.0"
edition = "2021"
EOF
    git -C "$dir" init -q -b main
    git -C "$dir" add -A
    git -C "$dir" -c user.email=t@t -c user.name=t commit -qm base
    git -C "$dir" tag base
}

# --- crossfile ---
CF="$ROOT/crossfile"
mkbase "$CF"

# break: pricing returns percent 0..100; checkout.rs unchanged.
cat > "$CF/src/pricing.rs" <<'EOF'
/// Returns the discount for a customer tier as a percentage (0-100).
pub fn discount_for_tier(tier: &str) -> f64 {
    match tier {
        "gold" => 20.0,
        "silver" => 10.0,
        _ => 0.0,
    }
}
EOF
cat > "$CF/tests/pricing_test.rs" <<'EOF'
use crossfile::pricing::discount_for_tier;

#[test]
fn gold_is_twenty_percent() {
    assert!((discount_for_tier("gold") - 20.0).abs() < 1e-9);
}
EOF
git -C "$CF" add -A
git -C "$CF" -c user.email=t@t -c user.name=t commit -qm "break: discount returns percent"
git -C "$CF" tag break

# fix: checkout divides by 100.
cat > "$CF/src/checkout.rs" <<'EOF'
use crate::pricing::discount_for_tier;

/// Final price after applying the tier discount (percent of base).
pub fn final_price(base: f64, tier: &str) -> f64 {
    let d = discount_for_tier(tier) / 100.0;
    base * (1.0 - d)
}
EOF
git -C "$CF" add -A
git -C "$CF" -c user.email=t@t -c user.name=t commit -qm "fix: convert percent to fraction at call site"
git -C "$CF" tag fix

# --- clean ---
CL="$ROOT/clean"
mkbase "$CL"
cat > "$CL/src/pricing.rs" <<'EOF'
/// Returns the discount for a customer tier as a fraction (0.0-1.0).
/// Tiers without a discount return 0.0.
pub fn discount_for_tier(tier: &str) -> f64 {
    match tier {
        "gold" => 0.2,
        "silver" => 0.1,
        _ => 0.0,
    }
}
EOF
git -C "$CL" add -A
git -C "$CL" -c user.email=t@t -c user.name=t commit -qm "docs: clarify discount_for_tier contract"
git -C "$CL" tag docs

echo "fixtures written to $ROOT"
