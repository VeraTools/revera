#!/usr/bin/env bash
# Installs the Vera x86_64 Linux release into TARGET_DIR/vera, verifying the
# archive against the release manifest's sha256. VERA_DOWNLOAD_BASE overrides
# the release URL (tests point it at a file:// directory).
set -euo pipefail

version=${1:?usage: install-vera.sh VERSION TARGET_DIR}
target_dir=${2:?usage: install-vera.sh VERSION TARGET_DIR}
base=${VERA_DOWNLOAD_BASE:-"https://github.com/VeraTools/Vera/releases/download/v${version}"}
target="x86_64-unknown-linux-gnu"
archive="vera-${target}.tar.gz"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

curl -fsSL "$base/$archive" -o "$tmp_dir/$archive"
curl -fsSL "$base/release-manifest.json" -o "$tmp_dir/manifest.json"
want=$(python3 - "$tmp_dir/manifest.json" "$target" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
assets = m.get("assets", m)
entry = assets[sys.argv[2]] if isinstance(assets, dict) else next(
    a for a in assets if sys.argv[2] in json.dumps(a))
print(entry["sha256"])
PY
)
got=$(sha256sum "$tmp_dir/$archive" | cut -d' ' -f1)
if [ -z "$want" ] || [ "$want" != "$got" ]; then
  echo "vera sha256 mismatch: got $got want $want" >&2
  exit 1
fi
mkdir -p "$tmp_dir/x" "$target_dir"
tar -xzf "$tmp_dir/$archive" -C "$tmp_dir/x"
bin=$(find "$tmp_dir/x" -name vera -type f | head -1)
if [ -z "$bin" ]; then
  echo "vera archive did not contain a vera binary" >&2
  exit 1
fi
install -m755 "$bin" "$target_dir/vera"
"$target_dir/vera" --version
