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
multi_corpora="inventory-rs billing-py session-ts worker-go ledger-rs etl-py clean-multi-py clean-multi-rs"
real_corpora="$(python3 -c 'import json,sys; print(" ".join(c["name"] for c in json.load(open(sys.argv[1])) if "excluded" not in c))' "$HERE/real/cases.json")"
large_corpora="$(basename -s .json "$HERE/large/truth"/*.json | tr '\n' ' ')"

in_set() { [[ " $2 " == *" $1 "* ]]; }

build_easy=0 build_hard=0 build_multi=0 build_real=0 build_large=0
for c in $CORPORA; do
    if in_set "$c" "$easy_corpora"; then
        build_easy=1
    elif in_set "$c" "$hard_corpora"; then
        build_hard=1
    elif in_set "$c" "$multi_corpora"; then
        build_multi=1
    elif in_set "$c" "$real_corpora"; then
        build_real=1
    elif in_set "$c" "$large_corpora"; then
        build_large=1
    else
        echo "unknown corpus: $c" >&2
        exit 1
    fi
done

# credentials: exactly the variables the selected configs reference, as
# ${VAR} interpolations or api_key_env names
needed=()
for cfg in $CONFIGS; do
    f="$HERE/configs/$cfg.yaml"
    if [ ! -f "$f" ]; then
        echo "unknown config: $cfg" >&2
        exit 1
    fi
    while read -r v; do
        in_set "$v" "${needed[*]:-}" || needed+=("$v")
    done < <(grep -v '^[[:space:]]*#' "$f" |
        grep -oE '\$\{[A-Za-z_][A-Za-z0-9_]*\}|api_key_env:[[:space:]]*[A-Za-z_][A-Za-z0-9_]*' |
        sed -E 's/^\$\{//; s/\}$//; s/^api_key_env:[[:space:]]*//')
done

if [ "${RUN_ALL_DRY_RUN:-0}" = "1" ]; then
    for v in "${needed[@]}"; do echo "needs: $v"; done
    [ "$build_easy" -eq 1 ] && echo "build: easy"
    [ "$build_hard" -eq 1 ] && echo "build: hard"
    [ "$build_multi" -eq 1 ] && echo "build: multi"
    [ "$build_real" -eq 1 ] && echo "build: real"
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

missing=()
for var in "${needed[@]}"; do
    [ -n "${!var:-}" ] || missing+=("$var")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo "SKIP: evaluation requires ${missing[*]}" >&2
    exit 0
fi

(cd "$ROOT" && cargo build -q)

[ "$build_easy" -eq 1 ] && bash "$HERE/build-corpus.sh"
[ "$build_hard" -eq 1 ] && bash "$HERE/build-corpus-hard.sh"
[ "$build_multi" -eq 1 ] && bash "$HERE/build-corpus-multi.sh"
[ "$build_real" -eq 1 ] && bash "$HERE/real/build-corpus.sh"
[ "$build_large" -eq 1 ] && bash "$HERE/large/build-corpus.sh"

# warm index once per corpus (vera enabled configs reuse the copied .vera);
# configs with Vera off need no embedding endpoint, so this is optional
if [ -n "${REVERA_EMBEDDING_BASE_URL:-}" ] && [ -n "${REVERA_EMBEDDING_API_KEY:-}" ]; then
    export VERA_BACKEND=api \
        EMBEDDING_MODEL_BASE_URL="${REVERA_EMBEDDING_BASE_URL}" \
        EMBEDDING_MODEL_ID="qwen/qwen3-embedding-8b" \
        EMBEDDING_MODEL_API_KEY="$REVERA_EMBEDDING_API_KEY"
    for c in $CORPORA; do
        d="$HERE/corpus/$c"
        git -C "$d" checkout -q head 2>/dev/null || true
        (cd "$d" && vera index . >/dev/null 2>&1) || true
    done
else
    echo "no embedding endpoint: skipping the warm Vera index" >&2
fi

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
