#!/usr/bin/env bash
# Smoke test for eval/run-all.sh planning (RUN_ALL_DRY_RUN=1; no cargo, no
# API calls). Usage: bash eval/test-run-all.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
out="$(CORPORA="crossfile linestep-terminator" CONFIGS="A-baseline" \
    RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1)"

grep -q "build: easy" <<<"$out"
grep -q "build: large" <<<"$out"
if grep -q "build: hard" <<<"$out"; then echo "FAIL: hard corpus built" >&2; exit 1; fi
grep -q "run: A-baseline crossfile 1" <<<"$out"
grep -q "run: A-baseline linestep-terminator 1" <<<"$out"

out="$(CORPORA="billing-py clean-multi-rs" CONFIGS="A-baseline" \
    RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1)"
grep -q "build: multi" <<<"$out"
if grep -q "build: easy" <<<"$out"; then echo "multi corpora must not build easy" >&2; exit 1; fi
grep -q "run: A-baseline clean-multi-rs 1" <<<"$out"
grep -q "run: A-baseline billing-py 1" <<<"$out"

out="$(CORPORA="real-hono-e9830c6b" CONFIGS="A-baseline" \
    RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1)"
grep -q "build: real" <<<"$out"
grep -q "run: A-baseline real-hono-e9830c6b 1" <<<"$out"
grep -q "needs: REVERA_MODEL_API_KEY" <<<"$out"

# a Responses endpoint config needs the model endpoint and credentials
out="$(CORPORA="real-hono-e9830c6b" CONFIGS="P1-glm" \
    RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1)"
grep -q "needs: REVERA_MODEL_BASE_URL" <<<"$out"
grep -q "needs: REVERA_MODEL_API_KEY" <<<"$out"

# an excluded (vetted-out) case is not a runnable corpus
if CORPORA="real-ripgrep-361698b9" RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1 2>/dev/null; then
    echo "FAIL: excluded case should be unknown" >&2
    exit 1
fi
if CONFIGS="bogus" RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1 2>/dev/null; then
    echo "FAIL: unknown config should exit non-zero" >&2
    exit 1
fi

if CORPORA="bogus" RUN_ALL_DRY_RUN=1 bash "$HERE/run-all.sh" 1 2>/dev/null; then
    echo "FAIL: unknown corpus should exit non-zero" >&2
    exit 1
fi

echo "run-all.sh planning OK"
python3 "$HERE/test_score.py"
bash "$HERE/prs/test-run.sh"
