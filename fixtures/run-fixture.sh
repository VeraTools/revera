#!/usr/bin/env bash
# Fixture verification: builds fixture repos, runs revera review on break,
# fix, and clean, and asserts on the JSON reports.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
FIX="${1:-/tmp/revera-fixtures}"
export FIXTURE_SCRIPTS="$HERE/scripts"
export PATH="$HOME/.local/bin:$PATH"
VERA_HOME="${VERA_HOME:-$HOME/.vera-revera}"
export VERA_HOME

if [ -z "${OPENROUTER_API_KEY:-}" ]; then
    echo "SKIP: OPENROUTER_API_KEY not set; fixture needs Vera API mode" >&2
    exit 0
fi

BIN="$ROOT/target/debug/revera"
if [ ! -x "$BIN" ]; then
    (cd "$ROOT" && cargo build -q)
fi

bash "$HERE/make-fixtures.sh" "$FIX"

fail() { echo "FIXTURE FAIL: $*" >&2; exit 1; }

jqget() { python3 -c "import json,sys; print(json.load(open(sys.argv[1]))$2)" "$1"; }

# --- break run ---
git -C "$FIX/crossfile" checkout -q break
BREAK_OUT="$FIX/crossfile/.revera/break-report.json"
"$BIN" review --repo "$FIX/crossfile" --base base --head break \
    --config "$HERE/configs/scripted-crossfile-break.yaml" \
    --title "pricing percent refactor" --out "$BREAK_OUT" || fail "break run exited $?"
python3 - "$BREAK_OUT" <<'PY' || fail "break assertions"
import json, sys
r = json.load(open(sys.argv[1]))
acc = [f for f in r["findings"] if f.get("validation_status") == "accepted"]
assert len(acc) == 1, f"expected 1 accepted finding, got {len(acc)}"
f = acc[0]
assert f["file"] == "src/pricing.rs", f["file"]
inline = r["plan"]["inline"]
assert len(inline) == 1 and inline[0]["file"] == "src/pricing.rs", inline
assert "<!-- revera-id:" in inline[0]["body"], "missing revera-id marker"
assert r["status"] == "complete", r["status"]
PY
echo "break run ok: $BREAK_OUT"

# --- fix run (recheck) ---
git -C "$FIX/crossfile" checkout -q fix
FIX_OUT="$FIX/crossfile/.revera/fix-report.json"
"$BIN" review --repo "$FIX/crossfile" --base base --head fix \
    --config "$HERE/configs/scripted-crossfile-fix.yaml" \
    --title "pricing percent refactor v2" --out "$FIX_OUT" || fail "fix run exited $?"
python3 - "$FIX_OUT" <<'PY' || fail "fix assertions"
import json, sys
r = json.load(open(sys.argv[1]))
st = r["plan"]["state"]["findings"]
open_ = [f for f in st if f["status"] == "open"]
resolved = [f for f in st if f["status"] == "resolved"]
assert len(open_) == 0, f"expected 0 open findings, got {open_}"
assert len(resolved) >= 1, f"expected prior finding resolved, got {st}"
assert r["status"] == "complete", r["status"]
PY
echo "fix run ok: $FIX_OUT"

# --- clean control ---
git -C "$FIX/clean" checkout -q docs
CLEAN_OUT="$FIX/clean/.revera/clean-report.json"
"$BIN" review --repo "$FIX/clean" --base base --head docs \
    --config "$HERE/configs/scripted-clean.yaml" \
    --title "docs touch-up" --out "$CLEAN_OUT" || fail "clean run exited $?"
python3 - "$CLEAN_OUT" <<'PY' || fail "clean assertions"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["status"] == "complete", r["status"]
assert len(r["findings"]) == 0, r["findings"]
PY
echo "clean run ok: $CLEAN_OUT"

# --- delegated strategy ---
git -C "$FIX/crossfile" checkout -q break
rm -f "$FIX/crossfile/.revera/state.json"
DEL_OUT="$FIX/crossfile/.revera/delegated-report.json"
"$BIN" review --repo "$FIX/crossfile" --base base --head break \
    --config "$HERE/configs/scripted-delegated.yaml" \
    --title "pricing percent refactor" --out "$DEL_OUT" || fail "delegated run exited $?"
python3 - "$DEL_OUT" <<'PY' || fail "delegated assertions"
import json, sys
r = json.load(open(sys.argv[1]))
acc = [f for f in r["findings"] if f.get("validation_status") == "accepted"]
assert len(acc) == 1, f"expected 1 accepted finding, got {len(acc)}"
assert acc[0]["file"] == "src/pricing.rs", acc[0]["file"]
assert acc[0]["source"].startswith("delegated:"), acc[0]["source"]
assert len(r["plan"]["inline"]) == 1, r["plan"]["inline"]
assert r["status"] == "complete", r["status"]
PY
echo "delegated run ok: $DEL_OUT"

# --- panel strategy ---
rm -f "$FIX/crossfile/.revera/state.json"
PANEL_OUT="$FIX/crossfile/.revera/panel-report.json"
"$BIN" review --repo "$FIX/crossfile" --base base --head break \
    --config "$HERE/configs/scripted-panel.yaml" \
    --title "pricing percent refactor" --out "$PANEL_OUT" || fail "panel run exited $?"
python3 - "$PANEL_OUT" <<'PY' || fail "panel assertions"
import json, sys
r = json.load(open(sys.argv[1]))
acc = [f for f in r["findings"] if f.get("validation_status") == "accepted"]
assert len(acc) == 1, f"expected 1 accepted finding (union collapsed, junk rejected), got {len(acc)}"
assert acc[0]["file"] == "src/pricing.rs", acc[0]["file"]
rej = [f for f in r["findings"] if f.get("validation_status") == "rejected"]
assert len(rej) == 1, f"expected 1 rejected junk finding, got {len(rej)}"
assert len(r["plan"]["inline"]) == 1, r["plan"]["inline"]
assert "panel: 2 scouts," in r["plan"]["summary_markdown"], r["plan"]["summary_markdown"]
assert r["status"] == "complete", r["status"]
PY
echo "panel run ok: $PANEL_OUT"

echo "ALL FIXTURE ASSERTIONS PASSED"
