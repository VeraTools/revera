#!/usr/bin/env bash
set -euo pipefail

cargo_version=$(
  sed -n '/^\[package\]/,/^\[/ s/^version = "\([^"]*\)"/\1/p' Cargo.toml |
    head -n 1
)
action_version=$(
  sed -n '/^  revera-version:/,/^  [^ ]/ s/^    default: "\([^"]*\)"/\1/p' action.yml |
    head -n 1
)

if [[ -z "$cargo_version" || -z "$action_version" ]]; then
  echo "could not determine Cargo or Action Revera version" >&2
  exit 1
fi

if [[ "$cargo_version" != "$action_version" ]]; then
  echo "Revera version mismatch: Cargo.toml=$cargo_version action.yml=$action_version" >&2
  exit 1
fi

echo "Revera versions match: $cargo_version"

# Every place that installs or documents the default Vera must agree with the
# Action's `vera-version` default.
vera_version=$(
  sed -n '/^  vera-version:/,/^  [^ ]/ s/^    default: "\([^"]*\)"/\1/p' action.yml |
    head -n 1
)
if [[ -z "$vera_version" ]]; then
  echo "could not determine the Action's Vera version" >&2
  exit 1
fi
mismatch=0
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  found=${line##*install-vera.sh }
  found=${found%% *}
  if [[ "$found" != "$vera_version" ]]; then
    echo "Vera version mismatch: ${line%%:*} installs $found, action.yml=$vera_version" >&2
    mismatch=1
  fi
done < <(grep -Ho 'scripts/install-vera.sh [^ ]*' .github/workflows/ci.yml .github/workflows/self-review.yml || true)
self_review=$(sed -n 's/^  version: "\([^"]*\)"/\1/p' .github/revera-self-review.yaml)
if [[ "$self_review" != "$vera_version" ]]; then
  echo "Vera version mismatch: .github/revera-self-review.yaml=$self_review action.yml=$vera_version" >&2
  mismatch=1
fi
if ! grep -q "^| \`vera-version\` | \`$vera_version\` |" docs/configuration.md; then
  echo "Vera version mismatch: docs/configuration.md does not list vera-version $vera_version" >&2
  mismatch=1
fi
if [[ "$mismatch" != 0 ]]; then
  exit 1
fi
echo "Vera versions match: $vera_version"
