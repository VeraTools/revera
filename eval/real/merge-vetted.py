#!/usr/bin/env python3
"""Check vetted cases (eval/tmp/vet-*.json, written by vetters following eval/real/VETTING.md) and append the valid ones to
eval/real/cases.json. Prints every rejection with its reason.

    python3 eval/real/merge-vetted.py [--write]
    python3 eval/real/merge-vetted.py --recheck   # re-run the checks on cases.json
"""

import glob
import importlib.util
import json
import re
import subprocess
import sys

ROOT = "eval/tmp/mirrors"
MAX_SPAN = 6  # line_max - line_min: "a window of at most ~6 lines" (VETTING.md)
NEAR = 3  # a window may sit this many lines from an intro hunk
spec = importlib.util.spec_from_file_location("mine", "eval/real/mine.py")
mine = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mine)


def git(repo, *a):
    return subprocess.run(["git", "-C", f"{ROOT}/{repo}", *a], capture_output=True, text=True)


def intro_hunks(c, path):
    """(start, end) post-image line ranges the intro commit changed in `path`;
    a pure deletion is the empty range at its position."""
    out = []
    diff = git(
        c["repo"], "-c", "diff.noprefix=false", "show", "-U0", "--format=", c["intro"], "--", path
    )
    for m in re.finditer(r"^@@ -\S+ \+(\d+)(?:,(\d+))? @@", diff.stdout, re.M):
        start, count = int(m.group(1)), int(m.group(2) if m.group(2) is not None else 1)
        out.append((start, start + max(count, 1) - 1))
    return out


def defect_problems(d):
    """Shape errors of one defect record; empty when well formed."""
    if not isinstance(d, dict):
        return ["defect is not an object"]
    out = []
    if not isinstance(d.get("file"), str) or not d.get("file"):
        out.append("defect file missing")
    for k in ("line_min", "line_max"):
        if not isinstance(d.get(k), int) or isinstance(d.get(k), bool):
            out.append(f"defect {k} is not an integer")
    kw = d.get("keywords", [])
    if not isinstance(kw, list) or not all(isinstance(k, str) and k for k in kw):
        out.append("keywords must be non-empty strings")
    return out


def problems(c):
    out = []
    for k in ("name", "repo", "url", "intro", "subject", "defects"):
        if k not in c:
            out.append(f"missing {k}")
    if out:
        return out
    if git(c["repo"], "cat-file", "-e", c["intro"] + "^{commit}").returncode:
        return ["intro sha not found"]
    # before the clean shortcut: a malformed value must not merge either
    if not isinstance(c["defects"], list):
        return ["defects is not a list"]
    if c.get("clean"):
        return [] if not c["defects"] else ["clean case with defects"]
    if not c.get("fix") or git(c["repo"], "cat-file", "-e", c["fix"] + "^{commit}").returncode:
        return ["fix sha not found"]
    if not c["defects"]:
        return ["no defects"]
    for d in c["defects"]:
        shape = defect_problems(d)
        if shape:
            out += shape
            continue
        f = git(c["repo"], "show", f"{c['intro']}:{d['file']}")
        if f.returncode:
            out.append(f"{d['file']} not in intro")
            continue
        n = len(f.stdout.splitlines())
        lo, hi = d["line_min"], d["line_max"]
        if not (1 <= lo <= hi <= n) or hi - lo > MAX_SPAN:
            out.append(f"bad line window {lo}-{hi} (file has {n}; span at most {MAX_SPAN})")
        else:
            hunks = intro_hunks(c, d["file"])
            if not hunks:
                out.append(f"{d['file']} not changed by intro")
            elif not any(lo <= e + NEAR and hi >= s - NEAR for s, e in hunks):
                out.append(f"window {lo}-{hi} is not in or next to an intro hunk of {d['file']}")
        if len(d.get("keywords", [])) < 3:
            out.append("fewer than 3 keywords")
    repo = f"{ROOT}/{c['repo']}"
    fix_paths = [p for p in mine.numstat(repo, c["fix"])[0] if not mine.SKIP_PATH_RE.search(p)]
    if not fix_paths:
        out.append("fix touches only skipped paths (tests, docs, lockfiles)")
    elif mine.predates(repo, c["fix"], c["intro"], fix_paths, mine.numstat(repo, c["intro"])[0]):
        out.append("predates guard: faulty logic older than intro")
    return out


def recheck():
    """Run the merge checks on the cases already in cases.json."""
    bad = 0
    for c in json.load(open("eval/real/cases.json")):
        if c.get("excluded"):
            continue
        p = problems(c)
        if p:
            bad += 1
            print(f"REJ {c['name']}: {'; '.join(p)}")
    print(f"{bad} case(s) fail")
    return bad


def main():
    if "--recheck" in sys.argv:
        sys.exit(1 if recheck() else 0)
    cases = json.load(open("eval/real/cases.json"))
    names = {c["name"] for c in cases}
    add = []
    for path in sorted(glob.glob("eval/tmp/vet-*.json")):
        for c in json.load(open(path)):
            if not isinstance(c, dict):
                print(f"REJ {path}: entry is not an object")
                continue
            p = problems(c)
            if c.get("name") in names:
                p.append("duplicate name")
            tag = "clean" if c.get("clean") else "defect"
            print(
                f"{'OK ' if not p else 'REJ'} {tag:6} {c.get('name')}: {'; '.join(p) or c.get('why', '')[:110]}"
            )
            if not p:
                c["note"] = c.pop("why", "")
                add.append(c)
                names.add(c["name"])
    print(f"{len(add)} case(s) pass")
    if "--write" in sys.argv:
        json.dump(cases + add, open("eval/real/cases.json", "w"), indent=1)
        print("written")


if __name__ == "__main__":
    main()
