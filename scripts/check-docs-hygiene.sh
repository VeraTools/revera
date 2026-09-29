#!/usr/bin/env bash
# Check all tracked text files for names of routing intermediaries, stored as
# hashes so this check does not repeat any such name.
set -euo pipefail

cd "$(dirname "$0")/.."

python3 <<'PY'
import hashlib
import re
import subprocess
import sys
from pathlib import Path

FORBIDDEN = {
    9: {
        "d4a62f38047e17888d1e10189cab2ab26ec111d010d08e0e14aec4ae43e08ef5",
    },
}
NON_TEXT = re.compile(r"[^a-z0-9]")
status = 0

paths = subprocess.run(
    ["git", "ls-files", "-z"],
    check=True,
    stdout=subprocess.PIPE,
).stdout.split(b"\0")
for raw_path in paths:
    if not raw_path:
        continue
    path = Path(raw_path.decode("utf-8"))
    try:
        data = path.read_bytes()
    except OSError as exc:
        print(f"docs hygiene: could not read {path}: {exc}", file=sys.stderr)
        status = 1
        continue
    if b"\0" in data:
        continue
    normalized = NON_TEXT.sub("", data.decode("utf-8", errors="replace").lower())
    for length, hashes in FORBIDDEN.items():
        if any(
            hashlib.sha256(normalized[i : i + length].encode("ascii")).hexdigest()
            in hashes
            for i in range(len(normalized) - length + 1)
        ):
            print(
                f"docs hygiene: forbidden routing-intermediary name in {path}",
                file=sys.stderr,
            )
            status = 1
            break

if status == 0:
    print("docs hygiene: ok")
sys.exit(status)
PY
