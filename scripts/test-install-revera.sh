#!/usr/bin/env bash
# Offline smoke test for install-revera.sh: fake releases served over
# file:// via REVERA_DOWNLOAD_BASE. Asserts architecture and musl/gnu archive
# selection, rejects unsupported architectures and corrupted checksums.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

export TEST_REAL_UNAME
TEST_REAL_UNAME=$(command -v uname)
mkdir -p "$tmp/mock-bin"
cat > "$tmp/mock-bin/uname" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == -m ]]; then
    printf '%s\n' "$TEST_UNAME_M"
else
    exec "$TEST_REAL_UNAME" "$@"
fi
EOF
chmod +x "$tmp/mock-bin/uname"
export PATH="$tmp/mock-bin:$PATH"

make_release() { # <version> <arch> <flavor gnu|musl>
    local ver="$1" cpu="$2" flavor="$3"
    local rel="$tmp/v$ver" arch="revera-${cpu}-unknown-linux-${flavor}.tar.gz"
    mkdir -p "$rel/pkg"
    cat > "$rel/pkg/revera" <<EOF
#!/usr/bin/env bash
echo "revera $ver-$cpu-$flavor"
EOF
    chmod +x "$rel/pkg/revera"
    tar -czf "$rel/$arch" -C "$rel/pkg" revera
    # checksum file must name the archive exactly as install expects
    (cd "$rel" && sha256sum "$arch" > "$arch.sha256")
    rm -rf "$rel/pkg"
}

make_release 0.1.0 x86_64 gnu
make_release 0.2.0 x86_64 musl
make_release 0.2.0 aarch64 musl

check_install() { # <machine> <version> <arch> <flavor>
    local machine="$1" ver="$2" cpu="$3" flavor="$4" out
    out=$(TEST_UNAME_M="$machine" REVERA_DOWNLOAD_BASE="file://$tmp/v$ver" \
        "$HERE/install-revera.sh" "$ver" "$tmp/bin-$ver-$machine" 2>&1)
    [[ "$out" == *"revera $ver-$cpu-$flavor"* ]] || {
        echo "FAIL: $machine $ver did not install the $cpu $flavor asset: $out" >&2
        exit 1
    }
}

check_install x86_64 0.2.0 x86_64 musl
check_install x86_64 0.1.0 x86_64 gnu
check_install aarch64 0.2.0 aarch64 musl
check_install arm64 0.2.0 aarch64 musl

if out=$(TEST_UNAME_M=riscv64 REVERA_DOWNLOAD_BASE="file://$tmp/v0.2.0" \
    "$HERE/install-revera.sh" 0.2.0 "$tmp/bin-unsupported" 2>&1); then
    echo "FAIL: unsupported architecture was accepted" >&2
    exit 1
fi
[[ "$out" == *"unsupported architecture: riscv64"* && ! -e "$tmp/bin-unsupported" ]] || {
    echo "FAIL: unsupported architecture was not rejected before installation: $out" >&2
    exit 1
}

# Corrupt a valid-length checksum for each architecture; install must fail.
cp -r "$tmp/v0.2.0" "$tmp/vbad"
for cpu in x86_64 aarch64; do
    archive="revera-${cpu}-unknown-linux-musl.tar.gz"
    printf '%064d  %s\n' 0 "$archive" > "$tmp/vbad/$archive.sha256"
    if TEST_UNAME_M="$cpu" REVERA_DOWNLOAD_BASE="file://$tmp/vbad" \
        "$HERE/install-revera.sh" 0.2.0 "$tmp/bin-bad-$cpu" >/dev/null 2>&1; then
        echo "FAIL: corrupted $cpu sha256 was accepted" >&2
        exit 1
    fi
    [[ ! -e "$tmp/bin-bad-$cpu" ]] || {
        echo "FAIL: corrupted $cpu archive was extracted" >&2
        exit 1
    }
done

echo "install-revera.sh OK"
