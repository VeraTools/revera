#!/usr/bin/env bash
# Offline test of eval/frozen.py: two scripted validator arms over one frozen
# candidate set (no network). Usage: bash eval/test-frozen.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
BIN="${REVERA_BIN:-$ROOT/target/debug/revera}"
[ -x "$BIN" ] || cargo build -q --manifest-path "$ROOT/Cargo.toml"
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
[ -n "${KEEP:-}" ] && trap - EXIT && echo "work dir: $W"
R="$W/repo"; mkdir -p "$R/src"
g() { git -C "$R" -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }
g init -q
printf 'pub fn div(a: i32, b: i32) -> i32 {\n    a / b\n}\n' > "$R/src/lib.rs"
g add -A; g commit -qm base
printf 'pub fn div(a: i32, b: i32) -> i32 {\n    a / (b - 1)\n}\n' > "$R/src/lib.rs"
g commit -qam head

cat > "$W/candidates.json" <<'JSON'
{"stats": {"validation": "disabled"}, "findings": [
 {"defect_key": "div-zero", "severity": "high", "file": "src/lib.rs", "start_line": 2,
  "title": "division by zero when b == 1", "claim": "b - 1 is zero for b == 1"},
 {"defect_key": "overflow", "severity": "low", "file": "src/lib.rs", "start_line": 2,
  "title": "overflow", "claim": "i32::MIN / -1 overflows"}]}
JSON
verdict() { printf '[{"tool_calls": [{"name": "submit_verdict", "arguments": {"validation_status": "%s", "rationale": "r", "counterevidence_checked": ["src/lib.rs callers"]%s}}]}]' "$1" "${2:-}"; }
# the accept arm also corrects a line and downgrades below its threshold:
# the candidate identity must not change
printf '{"roles": {"validator": [%s, %s]}}' "$(verdict accepted ', "start_line": 3, "severity": "low"')" "$(verdict accepted ', "severity": "low"')" > "$W/accept.json"
printf '{"roles": {"validator": [%s, %s]}}' "$(verdict rejected)" "$(verdict rejected)" > "$W/reject.json"
for arm in accept reject; do
cat > "$W/$arm.yaml" <<YAML
review: {min_severity: low}
models:
  investigator: {protocol: scripted, script: /dev/null, model: ignored}
  validator: {protocol: scripted, script: $W/$arm.json, model: $arm}
YAML
done
sed -i 's/min_severity: low/min_severity: medium/' "$W/accept.yaml"

# --out inside --repo must not be copied into each arm's repository
out="$(python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" \
    --repo "$R" --base HEAD~1 --head HEAD --out "$R/out" "$W/accept.yaml" "$W/reject.yaml")"
[ ! -e "$R/out/accept.repo/out" ] || { echo "FAIL: output dir copied into arm repo" >&2; exit 1; }
echo "$out"
python3 - "$out" <<'PY'
import json, sys
rows = {r["arm"]: r for r in map(json.loads, sys.argv[1].splitlines())}
a, r = rows["accept"], rows["reject"]
assert a["identical_candidates"] and r["identical_candidates"], rows
assert a["candidate_digest"] == r["candidate_digest"], rows
assert (a["candidates"], a["accepted"], a["rejected"]) == (2, 2, 0), a
assert (r["candidates"], r["accepted"], r["rejected"]) == (2, 0, 2), r
assert a["validation"] == r["validation"] == "fresh", rows
assert (a["min_severity"], a["accepted_at_min_severity"]) == ("medium", 0), a
assert (r["min_severity"], r["accepted_at_min_severity"]) == ("low", 0), r
PY

# arms whose config file names collide are refused before any arm runs
mkdir -p "$W/dup"; cp "$W/accept.yaml" "$W/dup/accept.yaml"
if python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" --repo "$R" \
    --base HEAD~1 --out "$W/out4" "$W/accept.yaml" "$W/dup/accept.yaml" 2>/dev/null; then
    echo "FAIL: duplicate arm names accepted" >&2; exit 1
fi
[ ! -e "$W/out4/accept.repo" ] || { echo "FAIL: arm ran despite duplicate names" >&2; exit 1; }

# a validated report is refused as frozen input
sed 's/"disabled"/"fresh"/' "$W/candidates.json" > "$W/bad.json"
if python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/bad.json" --repo "$R" \
    --base HEAD~1 --out "$W/out2" "$W/accept.yaml" 2>/dev/null; then
    echo "FAIL: validated report accepted as candidates" >&2; exit 1
fi
# a validator that fails (malformed verdicts) invalidates the comparison
printf '{"roles": {"validator": [[{"tool_calls": [{"name": "submit_verdict", "arguments": {"rationale": "r"}}]}]]}}' > "$W/broken.json"
sed "s#$W/accept.json#$W/broken.json#" "$W/accept.yaml" > "$W/broken.yaml"
if python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" --repo "$R" \
    --base HEAD~1 --out "$W/out3" "$W/broken.yaml" >/dev/null 2>&1; then
    echo "FAIL: failed validation counted as a valid comparison" >&2; exit 1
fi
echo "frozen-candidate harness OK"
