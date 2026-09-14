#!/usr/bin/env bash
# Builds the large-repo eval corpus: for each case in eval/large/cases.md,
# a fresh ripgrep clone at the pinned sha (tag `base`), with the case's
# synthetic edit committed as `head`. Truth lives in eval/large/truth/
# and is NOT written into the repos.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$HERE/../corpus"
CORPUS_ROOT="$(cd "$HERE/../corpus" && pwd)"
PIN=3fce3b5bb0236da2df6d99672afb8a719642eca7
LOCAL_RIPGREP=/home/ubuntu/ripgrep
REMOTE_RIPGREP=https://github.com/BurntSushi/ripgrep

export GIT_AUTHOR_NAME="eval" GIT_AUTHOR_EMAIL="eval@example.com"
export GIT_COMMITTER_NAME="eval" GIT_COMMITTER_EMAIL="eval@example.com"
export GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z"

src="$LOCAL_RIPGREP"
git -C "$src" cat-file -e "$PIN^{commit}" 2>/dev/null || src="$REMOTE_RIPGREP"

apply() { # apply <repo> <file> <old> <new> — exactly one occurrence required
    python3 - "$1" "$2" "$3" "$4" <<'PY'
import sys
repo, path, old, new = sys.argv[1:5]
p = f"{repo}/{path}"
s = open(p).read()
assert s.count(old) == 1, f"{p}: expected 1 occurrence, found {s.count(old)}"
open(p, "w").write(s.replace(old, new))
PY
}

build() { # build <name> <commit-msg>; edits are done by the caller via `apply`
    local name="$1" msg="$2"
    local d="$CORPUS_ROOT/$name"
    rm -rf "$d"
    git clone -q --no-checkout "$src" "$d"
    git -C "$d" checkout -q "$PIN"
    git -C "$d" tag base
    echo "$d"
}

finish() {
    git -C "$1" add -A
    git -C "$1" commit -qm "$2"
    git -C "$1" tag head
    git -C "$1" checkout -q base
}

mkdir -p "$CORPUS_ROOT"

# ============ L1 linestep-terminator ============
D="$(build linestep-terminator "searcher: LineStep yields lines without their terminator")"
F="$D/crates/searcher/src/lines.rs"
apply "$D" crates/searcher/src/lines.rs \
"    /// The range returned includes the line terminator. Ranges are always
    /// non-empty." \
"    /// The range returned excludes the line terminator."
apply "$D" crates/searcher/src/lines.rs \
'            Some(line_end) => {
                let m = (self.pos, self.pos + line_end + 1);
                assert!(m.0 <= m.1);

                self.pos = m.1;
                Some(m)
            }' \
'            Some(line_end) => {
                let m = (self.pos, self.pos + line_end);
                assert!(m.0 <= m.1);

                self.pos = m.1 + 1;
                Some(m)
            }'
finish "$D" "searcher: LineStep yields lines without their terminator"

# ============ L2 printer-crlf ============
D="$(build printer-crlf "printer: simplify trim_line_terminator")"
apply "$D" crates/printer/src/util.rs \
'        let mut end = line.end() - 1;
        if lineterm.is_crlf() && end > 0 && buf.get(end - 1) == Some(&b'"'"'\r'"'"') {
            end -= 1;
        }' \
'        let end = line.end() - 1;'
finish "$D" "printer: simplify trim_line_terminator"

# ============ L3 globset-dot-ext ============
D="$(build globset-dot-ext "globset: a leading dot is not an extension")"
apply "$D" crates/globset/src/pathutil.rs \
'    let last_dot_at = match name.rfind_byte(b'"'"'.'"'"') {
        None => return None,
        Some(i) => i,
    };
    Some(match *name {' \
'    let last_dot_at = match name.rfind_byte(b'"'"'.'"'"') {
        None => return None,
        Some(i) => i,
    };
    if last_dot_at == 0 {
        return None;
    }
    Some(match *name {'
finish "$D" "globset: a leading dot is not an extension"

# ============ L0 clean-without-terminator (control) ============
D="$(build clean-without-terminator "searcher: use ends_with in without_terminator")"
apply "$D" crates/searcher/src/lines.rs \
'    let line_term = line_term.as_bytes();
    let start = bytes.len().saturating_sub(line_term.len());
    if bytes.get(start..) == Some(line_term) {
        return &bytes[..bytes.len() - line_term.len()];
    }
    bytes' \
'    let line_term = line_term.as_bytes();
    if bytes.ends_with(line_term) {
        return &bytes[..bytes.len() - line_term.len()];
    }
    bytes'
finish "$D" "searcher: use ends_with in without_terminator"

echo "large corpus built in $CORPUS_ROOT"
