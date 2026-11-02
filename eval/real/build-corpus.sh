#!/usr/bin/env bash
# Builds the real-bug eval corpus from eval/real/cases.json.
#
# Each case is a bug-introducing commit found by eval/real/mine.py and kept
# after a human check (the bug must be catchable from the diff plus the
# repository). The repo is rebuilt as two fresh commits from `git archive` of
# intro^ (tag `base`) and intro (tag `head`), so the later fix is not in the
# history a reviewer could read. Clean cases use a commit with no known
# defect. Truth goes to eval/real/truth/<name>.json, outside the repos.
# Cases marked "excluded" in cases.json are skipped entirely.
#
# Sources are cloned once into eval/tmp/mirrors/ (gitignored).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
EVAL="$(cd "$HERE/.." && pwd)"
mkdir -p "$EVAL/corpus" "$EVAL/tmp/mirrors" "$HERE/truth"
CORPUS_ROOT="$(cd "$EVAL/corpus" && pwd)"
MIRRORS="$(cd "$EVAL/tmp/mirrors" && pwd)"

export GIT_AUTHOR_NAME="eval" GIT_AUTHOR_EMAIL="eval@example.com"
export GIT_COMMITTER_NAME="eval" GIT_COMMITTER_EMAIL="eval@example.com"
export GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z"

python3 - "$HERE/cases.json" <<'PY' > "$EVAL/tmp/real-cases.tsv"
import json, sys
for c in json.load(open(sys.argv[1])):
    if c.get("excluded"):
        continue  # vetted out (e.g. an SZZ mislabel): never built or scored
    print("\t".join([c["name"], c["repo"], c["url"], c["intro"], c["subject"]]))
PY

python3 - "$HERE/cases.json" "$HERE/truth" <<'PY'
import json, sys
cases, out = sys.argv[1:3]
for c in json.load(open(cases)):
    if c.get("excluded"):
        continue
    json.dump({"defects": c.get("defects", []), "clean": c.get("clean", False),
               "also_valid": c.get("also_valid", []),
               "source": {"repo": c["url"], "intro": c["intro"], "fix": c.get("fix")}},
              open(f"{out}/{c['name']}.json", "w"), indent=1)
PY

while IFS=$'\t' read -r name repo url intro subject; do
    m="$MIRRORS/$repo"
    [ -d "$m" ] || git clone -q "$url" "$m"
    git -C "$m" cat-file -e "$intro^{commit}" 2>/dev/null || git -C "$m" fetch -q origin
    d="$CORPUS_ROOT/$name"
    rm -rf "$d"
    mkdir -p "$d"
    git -C "$d" init -q -b main
    git -C "$m" archive "$intro^" | tar -x -C "$d"
    git -C "$d" add -A
    git -C "$d" commit -qm "base" && git -C "$d" tag base
    git -C "$d" rm -rq . && find "$d" -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +
    git -C "$m" archive "$intro" | tar -x -C "$d"
    git -C "$d" add -A
    # the intro commit's full message is the PR's stated intent in eval
    msg="$(git -C "$m" log -1 --format=%B "$intro")"
    [ -n "$msg" ] || msg="$subject"
    GIT_COMMITTER_DATE="${GIT_COMMITTER_DATE} +1 hour" git -C "$d" commit -q -F - <<<"$msg"
    git -C "$d" tag head
    echo "built $name"
done < "$EVAL/tmp/real-cases.tsv"

echo "real corpus built in $CORPUS_ROOT"