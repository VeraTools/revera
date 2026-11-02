#!/usr/bin/env python3
"""SZZ-style miner for real-bug eval cases.

    eval/real/mine.py REPO [--since 2019-01-01] [--max 200] > candidates.jsonl

For each non-merge commit whose message looks like a bug fix and whose diff is
small and touches source (not only tests/docs), blame the lines the fix
removed or changed at fix^ to find the commit that last touched them. A
candidate is kept when every blamed line comes from ONE introducing commit
that is itself small enough to review (non-merge, <= 400 changed lines,
<= 8 files, not the root commit).

Output: one JSON object per candidate with the introducing sha, the fix sha
and message, and the blamed lines mapped to their line numbers in the
introducing commit (the head side a reviewer would comment on).

Two guards against SZZ mislabels: blame runs with -M -C, so lines the
introducing commit only moved or copied are attributed to their origin; and
a candidate is dropped as `predates` when every token the fix deletes was
already present in the lines the introducing commit replaced (the faulty
logic existed before it). `--check cases.json` re-runs the predates guard
on vetted cases. Candidates still need a human pass: keep only bugs a reviewer could have caught from the
introducing diff plus the repository, then copy them into eval/real/cases.json.
"""

import argparse
import difflib
import json
import re
import subprocess
import sys

FIX_RE = re.compile(r"\b(fix(e[sd])?|bug|regression|crash|panic|wrong|incorrect|broken)\b", re.I)
# fixes that are not behaviour bugs a reviewer would flag
NOT_BUG_RE = re.compile(
    r"\b(typos?|docs?|docstring|documentation|formatting|format|changelog|readme|version|release|lint|ruff|clippy|flake8|mypy|typing|type hints?|pre-commit|ci|build error|failing test|tests?)\b|^(doc|ci|pkg|test|tests|chore|style|build)(\(|:)",
    re.I,
)
SKIP_PATH_RE = re.compile(
    r"(^|/)(tests?|testdata|benches|docs?|examples?|ci|pkg|complete)/|[._](test|spec)\.|(^|/)test_[^/]*$|\.(md|rst|txt|toml|lock|json|ya?ml|rb|sh|cfg|ini)$|CHANGE|README",
    re.I,
)
MAX_FIX_LINES = 60
MAX_INTRO_LINES = 400
MAX_INTRO_FILES = 8


def git(repo, *args):
    return subprocess.run(
        # pin the diff header format: a user's diff.noprefix or
        # diff.mnemonicPrefix would shift the paths the parsers read
        [
            "git",
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.mnemonicPrefix=false",
            "-c",
            "core.quotepath=false",
            "-C",
            repo,
            *args,
        ],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="surrogateescape",
    ).stdout


def numstat(repo, sha):
    files, lines = [], 0
    for row in git(repo, "show", "--numstat", "--format=", sha).splitlines():
        parts = row.split("\t")
        if len(parts) != 3 or parts[0] == "-":
            continue
        files.append(parts[2])
        lines += int(parts[0]) + int(parts[1])
    return files, lines


def removed_lines(repo, fix):
    """{path: [old line numbers]} for lines the fix deleted or replaced."""
    out, path, old, in_hunk = {}, None, 0, False
    for line in git(repo, "show", "-U0", "--format=", fix).splitlines():
        if line.startswith("diff --git "):
            path, in_hunk = None, False
        elif not in_hunk and line.startswith("--- "):
            path = None if line == "--- /dev/null" else line[6:]
        elif line.startswith("@@"):
            m = re.match(r"@@ -(\d+)(?:,(\d+))? ", line)
            old, in_hunk = int(m.group(1)), True
        elif path and in_hunk and line.startswith("-"):
            out.setdefault(path, []).append(old)
            old += 1
    return out


def blame(repo, fix, path, lines):
    """[(intro_sha, intro_path, intro_line)] for `lines` of `path` at fix^."""
    res = []
    for n in lines:
        try:
            out = git(
                repo,
                "blame",
                "--porcelain",
                "-w",
                "-M",
                "-C",
                "-L",
                f"{n},{n}",
                f"{fix}^",
                "--",
                path,
            )
        except subprocess.CalledProcessError:
            return None
        head = out.splitlines()[0].split()
        sha, orig = head[0], int(head[1])
        fname = next((ln[9:] for ln in out.splitlines() if ln.startswith("filename ")), path)
        res.append((sha, fname, orig))
    return res


TOKEN_RE = re.compile(r"\w+|[^\s\w]+")


def diff_lines(repo, sha, paths):
    """{path: (removed, added)} token lines of `sha`'s -U0 diff for `paths`,
    keyed by the post-image path (the pre-image one for deletions)."""
    out, path, in_hunk = {}, None, False
    for line in git(repo, "show", "-U0", "--format=", sha, "--", *paths).splitlines():
        if line.startswith("diff --git "):
            path, in_hunk = None, False
        elif line.startswith("@@"):
            in_hunk = True
        elif not in_hunk and line.startswith("--- "):
            # file headers only come before a file's first hunk; inside a
            # hunk, "--- x" is a removed line that reads "-- x"
            path = None if line == "--- /dev/null" else line[6:]
        elif not in_hunk and line.startswith("+++ "):
            if line != "+++ /dev/null":
                path = line[6:]
        elif path and in_hunk and line.startswith("-"):
            out.setdefault(path, ([], []))[0].append(TOKEN_RE.findall(line[1:]))
        elif path and in_hunk and line.startswith("+"):
            out.setdefault(path, ([], []))[1].append(TOKEN_RE.findall(line[1:]))
    return out


def best(line, pool):
    """(ratio, match) of the most similar token line in `pool`."""
    top = (0.0, None)
    for cand in pool:
        r = difflib.SequenceMatcher(None, line, cand, autojunk=False).ratio()
        if r > top[0]:
            top = (r, cand)
    return top


def changed(line, other):
    """(replaced or deleted token indices of `line`, tokens beside pure
    insertions); everything counts as replaced when `other` is None."""
    if other is None:
        return set(range(len(line))), set()
    out, near = set(), set()
    sm = difflib.SequenceMatcher(None, line, other, autojunk=False)
    for tag, i1, i2, _, _ in sm.get_opcodes():
        if tag == "insert":
            near.update(i for i in (i1 - 1, i1) if 0 <= i < len(line))
        elif tag != "equal":
            out.update(range(i1, i2))
    return out, near


def kept(line, before):
    """Token indices of `line` that appear, in runs of >= 2 tokens, in the
    token stream `before` (the lines the introducing commit replaced)."""
    out = set()
    sm = difflib.SequenceMatcher(None, line, before, autojunk=False)
    for blk in sm.get_matching_blocks():
        if blk.size >= 2:
            out.update(range(blk.a, blk.a + blk.size))
    return out


def predates(repo, fix, intro, fix_paths, intro_paths):
    """True when, on every line the fix changes, most tokens the fix touches
    were left alone by the introducing commit (compared with the most similar
    line it replaced): the faulty logic is older than `intro`. Lines the
    introducing commit wrote from scratch (no similar replaced line) count
    as introduced. Each fixed line is compared only with what the
    introducing commit replaced in the same file (all of it when the file is
    not in the introducing diff, e.g. after a rename). None when the fix
    changes no existing line, including a fix with no reviewable paths."""
    if not fix_paths:
        return None
    fix_diff = diff_lines(repo, fix, fix_paths)
    intro_diff = diff_lines(repo, intro, intro_paths)
    everything = [t for rem, _ in intro_diff.values() for ln in rem for t in ln]
    pairs = []
    for path, (frem, fadd) in fix_diff.items():
        if path in intro_diff:
            before = [t for ln in intro_diff[path][0] for t in ln]
        else:
            before = everything
        pairs += [(ln, fadd, before) for ln in frem if any(t.strip() for t in ln)]
    if not pairs:
        return None
    for line, fadd, before in pairs:
        r, after = best(line, fadd)
        by_fix, near = changed(line, after if r >= 0.5 else None)
        same = kept(line, before)
        if not by_fix:
            # a pure insertion says little about which tokens were wrong:
            # require that the introducing commit left the line nearly intact
            if not near or len(same) < 0.8 * len(line):
                return False
            continue
        by_intro = set(range(len(line))) - same
        if len(by_fix & by_intro) * 2 > len(by_fix):
            return False
    return True


def check(repo_root, cases_path):
    """Print the predates verdict for every vetted case with a fix sha."""
    for c in json.load(open(cases_path)):
        if not c.get("fix"):
            continue
        repo = f"{repo_root}/{c['repo']}"
        fix_paths = [f for f in numstat(repo, c["fix"])[0] if not SKIP_PATH_RE.search(f)]
        intro_paths = numstat(repo, c["intro"])[0]
        v = predates(repo, c["fix"], c["intro"], fix_paths, intro_paths)
        print(f"{c['name']:28} predates={v} excluded={'excluded' in c}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("repo")
    ap.add_argument("--since", default="2019-01-01")
    ap.add_argument("--max", type=int, default=200, help="fix commits to examine")
    ap.add_argument(
        "--check",
        metavar="CASES",
        help="run the predates guard on vetted cases; REPO is the mirrors directory",
    )
    a = ap.parse_args()
    if a.check:
        check(a.repo, a.check)
        return
    fixes = git(
        a.repo, "log", "--no-merges", f"--since={a.since}", "--format=%H%x09%s"
    ).splitlines()
    seen, examined = set(), 0
    for row in fixes:
        fix, subject = row.split("\t", 1)
        if not FIX_RE.search(subject) or NOT_BUG_RE.search(subject):
            continue
        files, n = numstat(a.repo, fix)
        src = [f for f in files if not SKIP_PATH_RE.search(f)]
        if not src or n > MAX_FIX_LINES:
            continue
        examined += 1
        if examined > a.max:
            break
        removed = {p: ls for p, ls in removed_lines(a.repo, fix).items() if p in src}
        if not removed:
            continue  # pure additions cannot be blamed
        blamed = []
        for p, ls in removed.items():
            b = blame(a.repo, fix, p, ls)
            if b is None:
                blamed = None
                break
            blamed += b
        if not blamed:
            continue
        intros = {b[0] for b in blamed}
        if len(intros) != 1:
            continue
        intro = intros.pop()
        if intro in seen:
            continue
        parents = git(a.repo, "rev-list", "--parents", "-n1", intro).split()
        if len(parents) != 2:  # root or merge commit
            continue
        ifiles, ilines = numstat(a.repo, intro)
        if ilines > MAX_INTRO_LINES or len(ifiles) > MAX_INTRO_FILES:
            continue
        old = predates(a.repo, fix, intro, list(removed), sorted({b[1] for b in blamed}))
        if old:
            continue
        seen.add(intro)
        print(
            json.dumps(
                {
                    "intro": intro,
                    "intro_subject": git(a.repo, "log", "-1", "--format=%s", intro).strip(),
                    "intro_lines": ilines,
                    "fix": fix,
                    "fix_subject": subject,
                    "fix_body": git(a.repo, "log", "-1", "--format=%b", fix).strip()[:600],
                    "blamed": [{"file": f, "line": ln} for _, f, ln in blamed],
                    "predates": old,
                }
            )
        )
        sys.stdout.flush()


if __name__ == "__main__":
    main()
