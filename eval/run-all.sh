#!/usr/bin/env bash
# eval/run-all.sh [REPS] — every config x corpus x rep, <=3 lanes parallel.
# Warm-indexes each corpus once (vera index) so index time is excluded from
# reported wall time.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
REPS="${1:-2}"

export PATH="$HOME/.local/bin:$PATH"
export VERA_HOME="${VERA_HOME:-$HOME/.vera-revera}"

if [ -z "${OPENROUTER_API_KEY:-}" ]; then
    echo "SKIP: OPENROUTER_API_KEY not set" >&2
    exit 0
fi

(cd "$ROOT" && cargo build -q)

CORPORA="${CORPORA:-crossfile nullpath offbyone lockdrop clean-refactor clean-docs}"
CONFIGS="${CONFIGS:-A-baseline B-baseline-novera C-baseline-norerank D-candidate-only E-panel-2scouts F-delegated}"

hard_corpora="utf8-truncate modzero-routing retry-after posted-state trait-contract clean-signature clean-dead-helper"
build_hard=0
for c in $hard_corpora; do
    if [[ " $CORPORA " == *" $c "* ]]; then
        build_hard=1
        break
    fi
done
if [ "$CORPORA" != "${CORPORA/crossfile/}" ] ||
    [ "$CORPORA" != "${CORPORA/nullpath/}" ] ||
    [ "$CORPORA" != "${CORPORA/offbyone/}" ] ||
    [ "$CORPORA" != "${CORPORA/lockdrop/}" ] ||
    [ "$CORPORA" != "${CORPORA/clean-refactor/}" ] ||
    [ "$CORPORA" != "${CORPORA/clean-docs/}" ]; then
    bash "$HERE/build-corpus.sh"
fi
if [ "$build_hard" -eq 1 ]; then
    bash "$HERE/build-corpus-hard.sh"
fi

# warm index once per corpus (vera enabled configs reuse the copied .vera)
export VERA_BACKEND=api \
    EMBEDDING_MODEL_BASE_URL="https://openrouter.ai/api/v1" \
    EMBEDDING_MODEL_ID="qwen/qwen3-embedding-8b" \
    EMBEDDING_MODEL_API_KEY="$OPENROUTER_API_KEY"
for c in $CORPORA; do
    d="$HERE/corpus/$c"
    git -C "$d" checkout -q head 2>/dev/null || true
    (cd "$d" && vera index . >/dev/null 2>&1) || true
done

jobs_running() { jobs -rp | wc -l; }

for cfg in $CONFIGS; do
    for c in $CORPORA; do
        for rep in $(seq 1 "$REPS"); do
            while [ "$(jobs_running)" -ge 3 ]; do wait -n; done
            bash "$HERE/run.sh" "$cfg" "$c" 1 "$rep" &
        done
    done
done
wait
echo "done; results in $HERE/results.jsonl"
