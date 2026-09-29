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
  "title": "division by zero when b == 1", "claim": "b - 1 is zero for b == 1",
  "suggested_fix": "Use checked_div and handle None."},
 {"defect_key": "overflow", "severity": "low", "file": "src/lib.rs", "start_line": 2,
  "title": "overflow", "claim": "i32::MIN / -1 overflows"}]}
JSON
verdict() { printf '[{"tool_calls": [{"name": "submit_verdict", "arguments": {"validation_status": "%s", "rationale": "r", "counterevidence_checked": ["src/lib.rs callers"]%s}}]}]' "$1" "${2:-}"; }
# the accept arm also corrects a line and downgrades below its threshold:
# the candidate identity must not change
printf '{"roles": {"validator": [%s, %s]}}' "$(verdict accepted ', "start_line": 3, "severity": "low", "fix": "Use checked_div and handle None."')" "$(verdict accepted ', "severity": "low"')" > "$W/accept.json"
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

cat > "$W/truth.json" <<'JSON'
{"div-zero": {"label": "true", "high_impact": true, "fix_safe": false}, "overflow": {"label": "false"}}
JSON

# the two arms differ in min_severity, a controlled setting: refused
# before any arm runs unless explicitly allowed
if python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" --repo "$R" \
    --base HEAD~1 --out "$W/out5" "$W/accept.yaml" "$W/reject.yaml" 2>/dev/null; then
    echo "FAIL: non-validator config difference accepted" >&2; exit 1
fi
[ ! -e "$W/out5/accept.repo" ] || { echo "FAIL: arm ran despite config difference" >&2; exit 1; }

# --out inside --repo must not be copied into each arm's repository
out="$(python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" \
    --repo "$R" --base HEAD~1 --head HEAD --out "$R/out" --truth "$W/truth.json" \
    --allow-config-diff "$W/accept.yaml" "$W/reject.yaml")"
[ ! -e "$R/out/accept.repo/out" ] || { echo "FAIL: output dir copied into arm repo" >&2; exit 1; }
echo "$out"
python3 - "$out" "$R/out/summary.json" "$(git -C "$R" rev-parse HEAD~1)" "$(git -C "$R" rev-parse HEAD)" <<'PY'
import json, sys
rows = {r["arm"]: r for r in map(json.loads, sys.argv[1].splitlines())}
a, r = rows["accept"], rows["reject"]
assert a["identical_candidates"] and r["identical_candidates"], rows
assert a["candidate_digest"] == r["candidate_digest"], rows
assert a["candidate_payload_sha256"] == r["candidate_payload_sha256"], rows
assert (a["candidates"], a["accepted"], a["rejected"]) == (2, 2, 0), a
assert (r["candidates"], r["accepted"], r["rejected"]) == (2, 0, 2), r
assert a["validation"] == r["validation"] == "fresh", rows
assert a["validator_models"] == r["validator_models"] == ["scripted"], rows
assert (a["min_severity"], a["accepted_at_min_severity"]) == ("medium", 0), a
assert (r["min_severity"], r["accepted_at_min_severity"]) == ("low", 0), r
# the true high-impact defect was accepted but downgraded below the arm's
# threshold: not surfaced, so a miss, not a success
sa, sr = a["score"], r["score"]
assert (sa["true_below_threshold"], sa["high_impact_missed"], sa["false_accepted"]) == (1, 1, 0), sa
assert (sr["true_rejected"], sr["false_rejected"], sr["high_impact_missed"]) == (1, 1, 1), sr
s = json.load(open(sys.argv[2]))
p = s["provenance"]
assert (p["base"], p["head"]) == (sys.argv[3], sys.argv[4]), p
assert p["binary"]["sha256"] and p["binary"]["version"].startswith("revera"), p
assert p["config_differences"] == {"reject": ["review.min_severity"]}, p
assert p["arms"]["accept"]["validator"]["model"] == "accept", p
PY

python3 - "$HERE/frozen.py" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("frozen", sys.argv[1])
frozen = importlib.util.module_from_spec(spec); spec.loader.exec_module(frozen)
finding = {"defect_key": "div-zero", "validation_status": "accepted",
           "severity": "high", "validated_fix": "candidate remedy"}
scores, _ = frozen.score([finding], {"div-zero": {"label": "true", "fix_safe": False}},
                         "low", {"div-zero": "candidate remedy"})
assert scores["unsafe_fix_published"] == 1, scores
PY

# an arm with only an investigator route inherits it as the validator: the
# harness must keep that model after replacing the investigator with replay
PORTF="$W/port"; LOG="$W/mock-requests.jsonl"
python3 - "$PORTF" "$LOG" <<'PY' &
import http.server, json, sys
port_file, log = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        with open(log, "a") as f:
            f.write(json.dumps({"path": self.path, "model": body.get("model")}) + "\n")
        out = json.dumps({"choices": [{"finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": None, "tool_calls": [{
                "id": "c1", "type": "function", "function": {"name": "submit_verdict",
                "arguments": json.dumps({"validation_status": "accepted", "rationale": "r",
                                         "counterevidence_checked": ["src/lib.rs:2"]})}}]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2}}).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
open(port_file, "w").write(str(s.server_port))
s.serve_forever()
PY
MOCK=$!
trap 'kill $MOCK 2>/dev/null || true; [ -n "${KEEP:-}" ] || rm -rf "$W"' EXIT
for _ in $(seq 50); do [ -s "$PORTF" ] && break; sleep 0.1; done
cat > "$W/inherit.yaml" <<YAML
review: {min_severity: low}
models:
  investigator: {protocol: openai-chat, base_url: "http://127.0.0.1:$(cat "$PORTF")/v1", model: inherited-model, api_key_env: FROZEN_TEST_KEY, reasoning: none}
YAML
out="$(FROZEN_TEST_KEY=dummy python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" \
    --repo "$R" --base HEAD~1 --out "$W/out6" --allow-config-diff "$W/inherit.yaml")"
python3 - "$out" "$LOG" <<'PY'
import json, sys
row = json.loads(sys.argv[1])
reqs = [json.loads(l) for l in open(sys.argv[2])]
assert row["validator_models"] == ["inherited-model"], row
assert row["accepted"] == 2 and row["validator_requests"] == 2, row
assert len(reqs) == 2 and all(q["model"] == "inherited-model" for q in reqs), reqs
PY

# malformed truth labels are refused
echo '{"div-zero": {"label": "yes"}}' > "$W/badtruth.json"
if python3 "$HERE/frozen.py" --bin "$BIN" --candidates "$W/candidates.json" --repo "$R" \
    --base HEAD~1 --out "$W/out7" --truth "$W/badtruth.json" "$W/reject.yaml" 2>/dev/null; then
    echo "FAIL: malformed truth label accepted" >&2; exit 1
fi

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
