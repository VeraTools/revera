#!/usr/bin/env bash
# eval/prs/run.sh CONFIG [CASE...]
# Reviews real pull requests from eval/prs/cases.json with
# eval/prs/configs/CONFIG.yaml and appends one score line per run to
# eval/prs/results.jsonl. Repositories are mirrored and each case's Vera
# index is warmed once under EVAL_CACHE, so index time stays out of the
# reported wall time.
#
# Environment: REPS (default 1), PROFILE (quick|deep, default unset), LABEL
# (result label, default CONFIG[-PROFILE]),
# PARALLEL (default 3), REVERA_BIN (default target/debug/revera),
# EVAL_CACHE (default ~/.cache/revera-eval), EVAL_WORK (default
# ~/.local/tmp/revera-eval), plus the REVERA_MODEL_* and
# REVERA_EMBEDDING_* variables the config references.
# RUN_REAL_DRY_RUN=1 prints the plan and exits.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
CONFIG_NAME="${1:?usage: run.sh CONFIG [CASE...]}"
shift
CONFIG="$HERE/configs/${CONFIG_NAME}.yaml"
CASES_JSON="$HERE/cases.json"
REPS="${REPS:-1}"
PARALLEL="${PARALLEL:-3}"
PROFILE="${PROFILE:-}"
BIN="${REVERA_BIN:-$ROOT/target/debug/revera}"
CACHE="${EVAL_CACHE:-$HOME/.cache/revera-eval}"
WORK="${EVAL_WORK:-$HOME/.local/tmp/revera-eval}"
RESULTS="$HERE/results.jsonl"
REPORTS="$HERE/reports"
LABEL="${LABEL:-$CONFIG_NAME${PROFILE:+-$PROFILE}}"

export PATH="$HOME/.local/bin:$PATH"
export VERA_HOME="${VERA_HOME:-$HOME/.vera-revera}"

[ -f "$CONFIG" ] || { echo "no config $CONFIG" >&2; exit 1; }

case_field() { python3 - "$CASES_JSON" "$1" "$2" <<'PY'
import json, sys
cases = {c["id"]: c for c in json.load(open(sys.argv[1]))["cases"]}
print(cases[sys.argv[2]][sys.argv[3]])
PY
}

if [ $# -gt 0 ]; then
    CASES=("$@")
else
    mapfile -t CASES < <(python3 -c 'import json,sys; [print(c["id"]) for c in json.load(open(sys.argv[1]))["cases"]]' "$CASES_JSON")
fi

if [ "${RUN_REAL_DRY_RUN:-0}" = "1" ]; then
    for c in "${CASES[@]}"; do
        for rep in $(seq 1 "$REPS"); do echo "run: $LABEL $c $rep"; done
    done
    exit 0
fi

missing=()
for var in REVERA_MODEL_BASE_URL REVERA_MODEL_API_KEY \
    REVERA_EMBEDDING_BASE_URL REVERA_EMBEDDING_API_KEY; do
    [ -n "${!var:-}" ] || missing+=("$var")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo "SKIP: evaluation requires ${missing[*]}" >&2
    exit 0
fi
[ -x "$BIN" ] || { echo "no revera binary at $BIN (cargo build first)" >&2; exit 1; }

mkdir -p "$CACHE/repos" "$CACHE/index" "$WORK" "$REPORTS"

checkout() { # case dir
    local id="$1" dir="$2" repo base head mirror
    repo="$(case_field "$id" repo)"
    base="$(case_field "$id" base)"
    head="$(case_field "$id" head)"
    mirror="$CACHE/repos/${repo//\//_}.git"
    if [ ! -d "$mirror" ]; then
        git clone -q --mirror "https://github.com/$repo.git" "$mirror" || return 1
    fi
    for sha in "$base" "$head"; do
        git -C "$mirror" cat-file -e "$sha^{commit}" 2>/dev/null ||
            git -C "$mirror" fetch -q origin "$sha" || return 1
    done
    rm -rf "$dir"
    git clone -q --shared --no-checkout "$mirror" "$dir" || return 1
    git -C "$dir" checkout -q --detach "$head"
}

# Warm one index per case (serially: concurrent first runs would each embed).
for c in "${CASES[@]}"; do
    idx="$CACHE/index/$c"
    [ -d "$idx/.vera" ] && continue
    dir="$WORK/warm-$c"
    checkout "$c" "$dir" || { echo "checkout failed warming $c; runs will retry" >&2; continue; }
    # Start from another case of the same repository so `vera update` only
    # embeds the files that differ.
    repo="$(case_field "$c" repo)"
    for other in "$CACHE"/index/*; do
        [ -d "$other/.vera" ] && [ -f "$other/repo" ] || continue
        [ "$(cat "$other/repo")" = "$repo" ] || continue
        cp -a "$other/.vera" "$dir/.vera"
        mkdir -p "$dir/.revera"
        cp -a "$other/.revera/." "$dir/.revera/" 2>/dev/null || true
        break
    done
    echo "warming index: $c" >&2
    if "$BIN" index --repo "$dir" --config "$CONFIG" > "$WORK/warm-$c.log" 2>&1; then
        mkdir -p "$idx/.revera"
        cp -a "$dir/.vera" "$idx/.vera"
        cp -a "$dir/.revera/vera-cache.json" "$idx/.revera/" 2>/dev/null || true
        echo "$repo" > "$idx/repo"
    else
        echo "index warm-up failed for $c (see $WORK/warm-$c.log); runs will index cold" >&2
    fi
    rm -rf "$dir"
done

failed_record() {
    python3 - "$LABEL" "$1" "$2" "$3" "${4:-}" <<'PY'
import json, sys
config, case, rep, reason, errlog = sys.argv[1:6]
if errlog:
    with open(errlog, errors="replace") as log:
        reason += ": " + log.read()[-300:]
print(json.dumps({"config": config, "case": case, "rep": int(rep),
    "status": "failed", "reason": reason}))
PY
}

run_one() {
    # Unexpected setup/scorer failures abort this worker; the parent supplies
    # its failure row. A row is committed only after successful scoring.
    set -e
    local c="$1" rep="$2" row="$3" dir rc out
    dir="$WORK/$LABEL-$c-$rep"
    checkout "$c" "$dir/repo" || {
        failed_record "$c" "$rep" "checkout failed: $c" > "$row.tmp"
        mv "$row.tmp" "$row"
        return 1
    }
    if [ -d "$CACHE/index/$c/.vera" ]; then
        cp -a "$CACHE/index/$c/.vera" "$dir/repo/.vera"
        mkdir -p "$dir/repo/.revera"
        cp -a "$CACHE/index/$c/.revera/." "$dir/repo/.revera/" 2>/dev/null || true
    fi
    case_field "$c" title > "$dir/title.txt"
    local body="$HERE/bodies/$c.md"
    local args=(review --repo "$dir/repo" --base "$(case_field "$c" base)"
        --head "$(case_field "$c" head)" --config "$CONFIG"
        --title "$(cat "$dir/title.txt")" --force --out "$dir/report.json")
    [ -f "$body" ] && args+=(--body-file "$body")
    [ -n "$PROFILE" ] && args+=(--profile "$PROFILE")
    rc=0
    "$BIN" "${args[@]}" > "$dir/stdout.txt" 2> "$dir/stderr.log" || rc=$?
    out="$REPORTS/$LABEL-$c-$rep"
    cp "$dir/stderr.log" "$out.stderr.log"
    if [ -f "$dir/report.json" ]; then
        cp "$dir/report.json" "$out.json"
        python3 "$HERE/score.py" "$dir/report.json" "$CASES_JSON" "$c" "$LABEL" "$rep" > "$row.tmp"
        mv "$row.tmp" "$row"
    else
        failed_record "$c" "$rep" "review exited $rc" "$dir/stderr.log" > "$row.tmp"
        mv "$row.tmp" "$row"
    fi
    rm -rf "$dir"
}

# Keep each worker's result separate until it exits. Even a killed worker or
# failed scorer then contributes exactly one failed row, never a missing run.
rows="$(mktemp -d "$WORK/score-lines.XXXXXX")" || exit 1
cases_pending=() reps_pending=()
for c in "${CASES[@]}"; do
    for rep in $(seq 1 "$REPS"); do
        while [ "$(jobs -rp | wc -l)" -ge "$PARALLEL" ]; do wait -n || true; done
        row="$rows/${#cases_pending[@]}.json"
        cases_pending+=("$c") reps_pending+=("$rep")
        run_one "$c" "$rep" "$row" &
    done
done
wait || true
for i in "${!cases_pending[@]}"; do
    row="$rows/$i.json"
    if [ ! -s "$row" ]; then
        failed_record "${cases_pending[$i]}" "${reps_pending[$i]}" \
            "background worker exited without a score (setup or scorer failed)" > "$row" || exit 1
    fi
    flock "$RESULTS.lock" tee -a "$RESULTS" < "$row" > /dev/null || exit 1
done
rm -rf "$rows"
echo "done; results in $RESULTS" >&2
