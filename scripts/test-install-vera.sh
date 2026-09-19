#!/usr/bin/env bash
# Offline smoke test for install-vera.sh: fake release served over file://
# via VERA_DOWNLOAD_BASE. Asserts install into a fresh (not-on-PATH)
# directory, manifest checksum enforcement, and archives without a binary.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

rel="$tmp/release"
mkdir -p "$rel/pkg/vera-x86_64-unknown-linux-gnu"
cat > "$rel/pkg/vera-x86_64-unknown-linux-gnu/vera" <<'EOF'
#!/usr/bin/env bash
echo "vera 9.9.9-test"
EOF
chmod +x "$rel/pkg/vera-x86_64-unknown-linux-gnu/vera"
tar -czf "$rel/vera-x86_64-unknown-linux-gnu.tar.gz" -C "$rel/pkg" vera-x86_64-unknown-linux-gnu
sum=$(sha256sum "$rel/vera-x86_64-unknown-linux-gnu.tar.gz" | cut -d' ' -f1)
cat > "$rel/release-manifest.json" <<EOF
{"assets": {"x86_64-unknown-linux-gnu": {"archive": "vera-x86_64-unknown-linux-gnu.tar.gz", "sha256": "$sum"}}}
EOF

# clean PATH: the install must not rely on a vera already on PATH
out=$(PATH=/usr/bin:/bin VERA_DOWNLOAD_BASE="file://$rel" \
    "$HERE/install-vera.sh" 9.9.9 "$tmp/bin/nested/dir" 2>&1)
[[ "$out" == *"vera 9.9.9-test"* ]] || {
    echo "FAIL: install into fresh dir: $out" >&2
    exit 1
}
[ -x "$tmp/bin/nested/dir/vera" ] || { echo "FAIL: binary not installed" >&2; exit 1; }

# tampered manifest checksum
cp -r "$rel" "$tmp/bad"
sed -i "s/$sum/deadbeef/" "$tmp/bad/release-manifest.json"
if VERA_DOWNLOAD_BASE="file://$tmp/bad" "$HERE/install-vera.sh" 9.9.9 "$tmp/bin-bad" >/dev/null 2>&1; then
    echo "FAIL: checksum mismatch was accepted" >&2
    exit 1
fi

# archive without a vera binary
cp -r "$rel" "$tmp/empty"
mkdir -p "$tmp/empty/pkg2/nothing" && touch "$tmp/empty/pkg2/nothing/readme"
tar -czf "$tmp/empty/vera-x86_64-unknown-linux-gnu.tar.gz" -C "$tmp/empty/pkg2" nothing
esum=$(sha256sum "$tmp/empty/vera-x86_64-unknown-linux-gnu.tar.gz" | cut -d' ' -f1)
sed -i "s/$sum/$esum/" "$tmp/empty/release-manifest.json"
if VERA_DOWNLOAD_BASE="file://$tmp/empty" "$HERE/install-vera.sh" 9.9.9 "$tmp/bin-empty" >/dev/null 2>&1; then
    echo "FAIL: archive without binary was accepted" >&2
    exit 1
fi

echo "install-vera.sh OK"
