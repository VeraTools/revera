#!/usr/bin/env bash
# Controlled opt-in comparison: only web_search.enabled differs between arms.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
: "${REVERA_MODEL_BASE_URL:?set the evaluation model endpoint}"
: "${REVERA_MODEL_API_KEY:?set the evaluation model key}"
export TMPDIR="${TMPDIR:-$HOME/.local/tmp}" CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-5}"
mkdir -p "$TMPDIR"
bash "$HERE/build-corpus-web.sh"
(cd "$ROOT" && cargo build -q)
for config in WEB-off WEB-on; do
    for corpus in web-contract web-version-change web-wrong-version web-clean-libraries web-injected-leak; do
        while [ "$(jobs -rp | wc -l)" -ge 2 ]; do wait -n; done
        bash "$HERE/run.sh" "$config" "$corpus" "${1:-1}" &
    done
done
wait
python3 "$HERE/summarize-web.py"
