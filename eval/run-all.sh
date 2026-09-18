#!/usr/bin/env bash
# eval/run-all.sh [REPS] — every config x corpus x rep, <=3 lanes parallel.
# Warm-indexes each corpus once (vera index) so index time is excluded from
# reported wall time. RUN_ALL_DRY_RUN=1 prints the build/run plan and exits.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
REPS="${1:-2}"

export PATH="$HOME/.local/bin:$PATH"
export VERA_HOME="${VERA_HOME:-$HOME/.vera-revera}"

CORPORA="${CORPORA:-crossfile nullpath offbyone lockdrop clean-refactor clean-docs}"
CONFIGS="${CONFIGS:-A-baseline B-baseline-novera C-baseline-norerank D-candidate-only E-panel-2scouts F-delegated}"

easy_corpora="crossfile nullpath offbyone lockdrop clean-refactor clean-docs"
hard_corpora="utf8-truncate modzero-routing retry-after posted-state trait-contract clean-signature clean-dead-helper"
large_corpora="$(basename -s .json "$HERE/large/truth"/*.json | tr '\n' ' ')"

in_set() { [[ " $2 " == *" $1 "* ]]; }

build_easy=0 build_hard=0 build_large=0
for c in $CORPORA; do
    if in_set "$c" "$easy_corpora"; then
        build_easy=1
    elif in_set "$c" "$hard_corpora"; then
        build_hard=1
    elif in_set "$c" "$large_corpora"; then
        build_large=1
    else
        echo "unknown corpus: $c" >&2
        exit 1
    fi
done

if [ "${RUN_ALL_DRY_RUN:-0}" = "1" ]; then
    [ "$build_easy" -eq 1 ] && echo "build: easy"
    [ "$build_hard" -eq 1 ] && echo "build: hard"
    [ "$build_large" -eq 1 ] && echo "build: large"
    for cfg in $CONFIGS; do
        for c in $CORPORA; do
            for rep in $(seq 1 "$REPS"); do
                echo "run: $cfg $c $rep"
            done
        done
    done
    exit 0
fi

if [ -z "${OPENROUTER_API_KEY:-}" ]; then
    echo "SKIP: OPENROUTER_API_KEY not set" >&2
    exit 0
fi

(cd "$ROOT" && cargo build -q)

[ "$build_easy" -eq 1 ] && bash "$HERE/build-corpus.sh"
[ "$build_hard" -eq 1 ] && bash "$HERE/build-corpus-hard.sh"
[ "$build_large" -eq 1 ] && bash "$HERE/large/build-corpus.sh"

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
