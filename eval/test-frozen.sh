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
verdict() { printf '[{"tool_calls": [{"name": "submit_verdict", "arguments": {"validation_status": "%s", "rationale": "r", "counterevidence_checked": ["src/lib.rs callers"]}}]}]' "$1"; }
printf '{"roles": {"validator": [%s, %s]}}' "$(verdict accepted)" "$(verdict accepted)" > "$W/accept.json"
printf '{"roles": {"validator": [%s, %s]}}' "$(verdict rejected)" "$(verdict rejected)" > "$W/reject.json"
for arm in accept reject; do
cat > "$W/$arm.yaml" <<YAML
review: {min_severity: low}
models:
  investigator: {protocol: scripted, script: /dev/null, model: ignored}
  validator: {protocol: scripted, script: $W/$arm.json, model: $arm}
YAML
done

out="$(python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" \
    --repo "$R" --base HEAD~1 --head HEAD --out "$W/out" "$W/accept.yaml" "$W/reject.yaml")"
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
PY

# a validated report is refused as frozen input
sed 's/"disabled"/"fresh"/' "$W/candidates.json" > "$W/bad.json"
if python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/bad.json" --repo "$R" \
    --base HEAD~1 --out "$W/out2" "$W/accept.yaml" 2>/dev/null; then
    echo "FAIL: validated report accepted as candidates" >&2; exit 1
fi
echo "frozen-candidate harness OK"
