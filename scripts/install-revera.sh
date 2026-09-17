#!/usr/bin/env bash
set -euo pipefail

version=${1:?usage: install-revera.sh VERSION TARGET_DIR}
target_dir=${2:?usage: install-revera.sh VERSION TARGET_DIR}
base=${REVERA_DOWNLOAD_BASE:-"https://github.com/VeraTools/revera/releases/download/v${version}"}
# 0.1.0 is the only release that shipped a GNU asset; 0.2.0+ ship static musl.
if [ "$version" = "0.1.0" ]; then
  archive="revera-x86_64-unknown-linux-gnu.tar.gz"
else
  archive="revera-x86_64-unknown-linux-musl.tar.gz"
fi
checksum="${archive}.sha256"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

curl -fsSL "$base/$archive" -o "$tmp_dir/$archive"
curl -fsSL "$base/$checksum" -o "$tmp_dir/$checksum"
(
  cd "$tmp_dir"
  sha256sum -c "$checksum"
)
mkdir -p "$target_dir"
tar -xzf "$tmp_dir/$archive" -C "$target_dir"
chmod +x "$target_dir/revera"
"$target_dir/revera" --version
