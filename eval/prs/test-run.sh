#!/usr/bin/env bash
# Offline regression: every requested case/rep gets exactly one truthful row.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "${TMPDIR:-$HOME/.local/tmp}"
scratch="$(mktemp -d "${TMPDIR:-$HOME/.local/tmp}/revera-prs-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/eval/prs/configs" "$scratch/bin"
cp "$HERE/run.sh" "$HERE/score.py" "$scratch/eval/prs/"
cp "$HERE/configs/server.yaml" "$scratch/eval/prs/configs/"
python3 - "$scratch/eval/prs/cases.json" <<'PY'
import json, sys
names = ["checkout-failed", "review-failed", "worker-killed", "scorer-failed", "partial-report"]
with open(sys.argv[1], "w") as out:
    json.dump({"cases": [{"id": name, "repo": "test/repo", "base": "base", "head": "head",
        "title": name, "defects": []} for name in names]}, out)
PY
cat > "$scratch/bin/git" <<'SH'
#!/usr/bin/env bash
[[ "$*" == *checkout-failed* ]] && exit 1
if [[ "${1:-}" == clone ]]; then mkdir -p "${@: -1}"; fi
exit 0
SH
cat > "$scratch/bin/revera" <<'SH'
#!/usr/bin/env bash
command="$1"; shift
repo='' out=''
while [ $# -gt 0 ]; do
    case "$1" in
        --repo) repo="$2"; shift ;;
        --out) out="$2"; shift ;;
    esac
    shift
done
if [ "$command" = index ]; then mkdir -p "$repo/.vera"; exit 0; fi
case "$repo" in
    *review-failed*) echo "fixture review failure" >&2; exit 17 ;;
    *worker-killed*) kill -KILL "$PPID"; exit 1 ;;
    *scorer-failed*) echo 'invalid report' > "$out"; exit 1 ;;
    *partial-report*) echo '{"status":"partial","reason":"fixture incomplete","findings":[]}' > "$out"; exit 23 ;;
esac
exit 1
SH
chmod +x "$scratch/bin/git" "$scratch/bin/revera"
PATH="$scratch/bin:$PATH" REVERA_BIN="$scratch/bin/revera" \
    REVERA_MODEL_BASE_URL=fixture REVERA_MODEL_API_KEY=fixture \
    REVERA_EMBEDDING_BASE_URL=fixture REVERA_EMBEDDING_API_KEY=fixture \
    EVAL_CACHE="$scratch/cache" EVAL_WORK="$scratch/work" REPS=2 PARALLEL=3 \
    bash "$scratch/eval/prs/run.sh" server > "$scratch/run.log" 2>&1
python3 - "$scratch/eval/prs/results.jsonl" <<'PY'
import json, sys
from collections import Counter
rows = [json.loads(line) for line in open(sys.argv[1])]
assert len(rows) == 10, rows
counts = Counter((r["case"], r["rep"]) for r in rows)
assert len(counts) == 10 and set(counts.values()) == {1}, counts
for row in rows:
    if row["case"] == "partial-report":
        # A nonzero CLI exit must not replace the report's own status.
        assert row["status"] == "partial" and row["reason"] == "fixture incomplete", row
        assert row["tp"] == 0 and row["fn"] == 0, row
    else:
        assert row["status"] == "failed" and row["reason"], row
        assert "tp" not in row and "fn" not in row, row
print("PASS: checkout, review, killed-worker and scorer failures; nonzero-exit report status; one row per case/rep")
PY
