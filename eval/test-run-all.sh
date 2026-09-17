#!/usr/bin/env bash
# Smoke test for eval/run-all.sh planning (RUN_ALL_DRY_RUN=1; no cargo, no
# API calls). Usage: bash eval/test-run-all.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

out="$(CORPORA="crossfile linestep-terminator" CONFIGS="A-baseline" \
    RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1)"

grep -q "build: easy" <<<"$out"
grep -q "build: large" <<<"$out"
! grep -q "build: hard" <<<"$out"
grep -q "run: A-baseline crossfile 1" <<<"$out"
grep -q "run: A-baseline linestep-terminator 1" <<<"$out"

if CORPORA="bogus" RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1 2>/dev/null; then
    echo "FAIL: unknown corpus should exit non-zero" >&2
    exit 1
fi

echo "run-all.sh planning OK"
