#!/usr/bin/env python3
"""Check vetted cases (eval/tmp/vet-*.json, written by vetters following eval/real/VETTING.md) and append the valid ones to
eval/real/cases.json. Prints every rejection with its reason.

    python3 eval/real/merge-vetted.py [--write]
"""

import glob
import importlib.util
import json
import subprocess
import sys

ROOT = "eval/tmp/mirrors"
spec = importlib.util.spec_from_file_location("mine", "eval/real/mine.py")
mine = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mine)


def git(repo, *a):
    return subprocess.run(["git", "-C", f"{ROOT}/{repo}", *a], capture_output=True, text=True)


def problems(c):
    out = []
    for k in ("name", "repo", "url", "intro", "subject", "defects"):
        if k not in c:
            out.append(f"missing {k}")
    if out:
        return out
    if git(c["repo"], "cat-file", "-e", c["intro"] + "^{commit}").returncode:
        return ["intro sha not found"]
    if c.get("clean"):
        return [] if not c["defects"] else ["clean case with defects"]
    if not c.get("fix") or git(c["repo"], "cat-file", "-e", c["fix"] + "^{commit}").returncode:
        return ["fix sha not found"]
    if not c["defects"]:
        return ["no defects"]
    for d in c["defects"]:
        f = git(c["repo"], "show", f"{c['intro']}:{d['file']}")
        if f.returncode:
            out.append(f"{d['file']} not in intro")
            continue
        n = len(f.stdout.splitlines())
        if not (1 <= d["line_min"] <= d["line_max"] <= n) or d["line_max"] - d["line_min"] > 12:
            out.append(f"bad line window {d['line_min']}-{d['line_max']} (file has {n})")
        if len(d.get("keywords", [])) < 3:
            out.append("fewer than 3 keywords")
    repo = f"{ROOT}/{c['repo']}"
    fix_paths = [p for p in mine.numstat(repo, c["fix"])[0] if not mine.SKIP_PATH_RE.search(p)]
    if mine.predates(repo, c["fix"], c["intro"], fix_paths, mine.numstat(repo, c["intro"])[0]):
        out.append("predates guard: faulty logic older than intro")
    return out


def main():
    cases = json.load(open("eval/real/cases.json"))
    names = {c["name"] for c in cases}
    add = []
    for path in sorted(glob.glob("eval/tmp/vet-*.json")):
        for c in json.load(open(path)):
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
