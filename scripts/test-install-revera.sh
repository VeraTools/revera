#!/usr/bin/env bash
# Offline smoke test for install-revera.sh: fake releases served over
# file:// via REVERA_DOWNLOAD_BASE. Asserts the musl/gnu archive selection
# and that a corrupted checksum fails the install.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

make_release() { # <version> <flavor gnu|musl>
    local ver="$1" flavor="$2"
    local rel="$tmp/v$ver" arch="revera-x86_64-unknown-linux-${flavor}.tar.gz"
    mkdir -p "$rel/pkg"
    cat > "$rel/pkg/revera" <<EOF
#!/usr/bin/env bash
echo "revera $ver-$flavor"
EOF
    chmod +x "$rel/pkg/revera"
    tar -czf "$rel/$arch" -C "$rel/pkg" revera
    # checksum file must name the archive exactly as install expects
    (cd "$rel" && sha256sum "$arch" > "$arch.sha256")
    rm -rf "$rel/pkg"
}

make_release 0.1.0 gnu
make_release 0.2.0 musl

out=$(REVERA_DOWNLOAD_BASE="file://$tmp/v0.2.0" \
    "$HERE/install-revera.sh" 0.2.0 "$tmp/bin-0.2.0" 2>&1)
[[ "$out" == *"revera 0.2.0-musl"* ]] || {
    echo "FAIL: 0.2.0 did not install the musl asset: $out" >&2
    exit 1
}

out=$(REVERA_DOWNLOAD_BASE="file://$tmp/v0.1.0" \
    "$HERE/install-revera.sh" 0.1.0 "$tmp/bin-0.1.0" 2>&1)
[[ "$out" == *"revera 0.1.0-gnu"* ]] || {
    echo "FAIL: 0.1.0 did not install the gnu asset: $out" >&2
    exit 1
}

# corrupt the 0.2.0 checksum file; install must fail
cp -r "$tmp/v0.2.0" "$tmp/vbad"
echo "deadbeef  revera-x86_64-unknown-linux-musl.tar.gz" \
    > "$tmp/vbad/revera-x86_64-unknown-linux-musl.tar.gz.sha256"
if REVERA_DOWNLOAD_BASE="file://$tmp/vbad" \
    "$HERE/install-revera.sh" 0.2.0 "$tmp/bin-bad" >/dev/null 2>&1; then
    echo "FAIL: corrupted sha256 was accepted" >&2
    exit 1
fi

echo "install-revera.sh OK"
