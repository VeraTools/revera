#!/usr/bin/env python3
"""Frozen-candidate comparison: validate one fixed candidate set under
several arms so validator-side differences are not confounded by
investigator variance.

  eval/frozen.py --candidates REPORT.json --repo REPO --base REV --head REV \
      --out DIR ARM.yaml [ARM.yaml ...]

REPORT.json must come from a `review.validate: false` run (its findings are
unvalidated candidates). Each arm config is used as-is except that its
investigator route is replaced by a scripted route replaying the frozen
candidates and `review.validate` is forced on. The script fails unless every
arm validated the identical candidate set (same digest) with one fresh
validator session per candidate; it prints one JSON line per arm.
"""
import argparse, hashlib, json, os, shutil, subprocess, sys
import yaml

CANDIDATE_FIELDS = ("defect_key", "severity", "file", "start_line", "end_line",
                    "title", "claim", "trigger", "impact", "introduced_by_change",
                    "supporting_evidence", "suggested_fix")


def candidates(report):
    if report.get("stats", {}).get("validation") != "disabled":
        sys.exit("candidates report must come from a review.validate: false run")
    out = []
    for f in report["findings"]:
        out.append({k: f[k] for k in CANDIDATE_FIELDS if f.get(k) is not None})
    return out


def digest(items):
    keyed = sorted(json.dumps({k: c.get(k) for k in ("defect_key", "file", "start_line", "claim")},
                              sort_keys=True) for c in items)
    return hashlib.sha256("\n".join(keyed).encode()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidates", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--head", default="HEAD")
    ap.add_argument("--out", required=True)
    ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "..", "target", "debug", "revera"))
    ap.add_argument("arms", nargs="+")
    a = ap.parse_args()

    frozen = candidates(json.load(open(a.candidates)))
    want = digest(frozen)
    a.out = os.path.abspath(a.out)
    os.makedirs(a.out, exist_ok=True)
    script = os.path.abspath(os.path.join(a.out, "frozen-investigator.json"))
    json.dump({"roles": {"investigator": [[{"tool_calls": [{
        "name": "submit_findings",
        "arguments": {"findings": frozen, "coverage": "frozen candidates"}}]}]]}},
        open(script, "w"))

    rows, ok = [], True
    for arm in a.arms:
        name = os.path.splitext(os.path.basename(arm))[0]
        cfg = yaml.safe_load(open(arm))
        cfg.setdefault("models", {})["investigator"] = {
            "protocol": "scripted", "script": script, "model": "frozen"}
        review = cfg.setdefault("review", {})
        review.update({"validate": True, "publish": "dry-run", "strategy": "baseline"})
        cfg.pop("profiles", None)
        arm_cfg = os.path.join(a.out, f"{name}.config.yaml")
        yaml.safe_dump(cfg, open(arm_cfg, "w"), sort_keys=False)
        report_path = os.path.join(a.out, f"{name}.report.json")
        # each arm reviews its own copy without prior state, so no arm sees
        # another arm's verdicts (rechecks would otherwise consume sessions)
        repo = os.path.join(a.out, f"{name}.repo")
        shutil.rmtree(repo, ignore_errors=True)
        shutil.copytree(a.repo, repo, symlinks=True)
        state = os.path.join(repo, ".revera", "state.json")
        if os.path.lexists(state):
            os.remove(state)
        rc = subprocess.run([a.bin, "review", "--repo", repo, "--base", a.base,
                             "--head", a.head, "--config", arm_cfg, "--force",
                             "--out", report_path],
                            stdout=subprocess.DEVNULL,
                            stderr=open(os.path.join(a.out, f"{name}.stderr.log"), "w")).returncode
        if not os.path.exists(report_path):
            rows.append({"arm": name, "status": "failed", "exit": rc})
            ok = False
            continue
        rep = json.load(open(report_path))
        st = rep.get("stats", {})
        got = digest(rep["findings"])
        # every candidate gets its own validator session, so validator
        # requests can never be fewer than candidates
        vreq = sum(r.get("requests", 0) for r in rep.get("ledger", {}).get("by_route", [])
                   if r.get("role") == "validator")
        row = {"arm": name, "status": rep["status"], "exit": rc,
               "candidates": st.get("candidates"), "accepted": st.get("accepted"),
               "rejected": st.get("rejected"), "uncertain": st.get("uncertain"),
               "rechecked": st.get("resolved", 0) + st.get("reopened", 0),
               "validation": st.get("validation"), "validator_requests": vreq,
               "candidate_digest": got,
               "identical_candidates": got == want and st.get("candidates") == len(frozen)}
        if (not row["identical_candidates"] or st.get("validation") != "fresh"
                or vreq < len(frozen) or row["rechecked"]):
            ok = False
        rows.append(row)
        print(json.dumps(row))
    json.dump(rows, open(os.path.join(a.out, "summary.json"), "w"), indent=2)
    if not ok:
        sys.exit("frozen comparison invalid: arms did not validate the identical candidate set")


if __name__ == "__main__":
    main()
