#!/usr/bin/env bash
# eval/run.sh CONFIG CORPUS [REPS]
# Fresh copy of eval/corpus/$CORPUS (with its warm .vera index) into a temp
# dir per rep, checkout head, run revera review --force, score into
# eval/results.jsonl.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
CONFIG_NAME="$1"; CORPUS="$2"; REPS="${3:-1}"
CONFIG="$HERE/configs/${CONFIG_NAME}.yaml"
SRC="$HERE/corpus/$CORPUS"
BIN="$ROOT/target/debug/revera"
RESULTS="$HERE/results.jsonl"

export PATH="$HOME/.local/bin:$PATH"
export VERA_HOME="${VERA_HOME:-$HOME/.vera-revera}"

[ -f "$CONFIG" ] || { echo "no config $CONFIG" >&2; exit 1; }
[ -d "$SRC" ] || { echo "no corpus $SRC" >&2; exit 1; }

for rep in $(seq 1 "$REPS"); do
    TMP="$(mktemp -d)"
    cp -a "$SRC" "$TMP/repo"
    git -C "$TMP/repo" checkout -q head 2>/dev/null || true
    rm -f "$TMP/repo/.revera/state.json"
    OUT="$TMP/repo/report.json"
    "$BIN" review --repo "$TMP/repo" --base base --head head \
        --config "$CONFIG" --title "eval $CORPUS" --force \
        --out "$OUT" > "$TMP/stdout.txt" 2> "$TMP/stderr.log"
    RC=$?
    if [ -f "$OUT" ]; then
        python3 "$HERE/score.py" "$OUT" "$SRC/truth.json" \
            "$CONFIG_NAME" "$CORPUS" "$rep" >> "$RESULTS"
    else
        python3 - "$CONFIG_NAME" "$CORPUS" "$rep" "$RC" "$TMP/stderr.log" <<'PY' >> "$RESULTS"
import json, sys
config, corpus, rep, rc, errlog = sys.argv[1:6]
err = open(errlog).read()[-300:]
print(json.dumps({"config": config, "corpus": corpus, "rep": int(rep),
    "status": "failed", "reason": f"review exited {rc}: {err}",
    "tp": 0, "fp": 0, "fn": -1, "requests": 0, "prompt_tokens": 0,
    "completion_tokens": 0, "wall_ms": 0, "est_cost": 0}))
PY
    fi
    rm -rf "$TMP"
done
