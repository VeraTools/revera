#!/usr/bin/env bash
# Public-facing files must attribute models to providers/families and must
# not name routing intermediaries. Internal dogfood config
# (.github/revera-self-review.yaml) and raw evaluation configs under eval/
# are reproducibility data and are deliberately out of scope.
set -euo pipefail

cd "$(dirname "$0")/.."

public=(README.md CONTRIBUTING.md SECURITY.md action.yml revera.example.yaml docs prompts)
forbidden=('relay\.fast')

status=0
for pat in "${forbidden[@]}"; do
  if hits=$(grep -rniE "$pat" "${public[@]}" 2>/dev/null); then
    echo "forbidden reference /$pat/ in public files:" >&2
    echo "$hits" >&2
    status=1
  fi
done

if [ "$status" -eq 0 ]; then
  echo "docs hygiene: ok"
fi
exit "$status"
