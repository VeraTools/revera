#!/usr/bin/env python3
"""Score one revera report against a real-PR case; prints one JSON line.

usage: score.py REPORT CASES_JSON CASE_ID CONFIG REP

Recall counts only `confirmed` defects. Accepted findings that match no
labeled entry are listed under `unlabeled` for manual judgment; add a
`valid`, `invalid` or `nit` entry to the case to label them.
"""

import json
import re
import sys

GOOD_EXTRA = ("plausible", "valid")
KNOWN_FP = ("neutral", "invalid")


def text_of(f):
    return " ".join(
        [f.get("title", ""), f.get("claim", ""), f.get("trigger", ""), f.get("impact", "")]
    ).lower()


def overlaps(f, loc, slack=3):
    if f.get("file") != loc["file"]:
        return False
    start = f.get("start_line") or 0
    end = f.get("end_line") or start
    lo, hi = loc["lines"]
    return start <= hi + slack and end >= lo - slack


def matches(f, d):
    text = text_of(f)
    keys = [k.lower() for k in d.get("keys", [])]
    for loc in d.get("locations", []):
        if overlaps(f, loc) and (not keys or any(k in text for k in keys)):
            return True
    groups = d.get("terms") or []
    return bool(groups) and all(any(t.lower() in text for t in g) for g in groups)


def max_matching(findings, defects):
    """Maximum bipartite matching finding->defect; returns {defect: finding}."""
    adj = [[di for di, d in enumerate(defects) if matches(f, d)] for f in findings]
    owner = {}

    def augment(fi, seen):
        for di in adj[fi]:
            if di in seen:
                continue
            seen.add(di)
            if di not in owner or augment(owner[di], seen):
                owner[di] = fi
                return True
        return False

    for fi in range(len(findings)):
        augment(fi, set())
    return owner, adj


def classify(findings, defects):
    confirmed = [i for i, d in enumerate(defects) if d["kind"] == "confirmed"]
    sub = [defects[i] for i in confirmed]
    owner, _ = max_matching(findings, sub)
    hit = {confirmed[k]: v for k, v in owner.items()}
    used = set(hit.values())
    out = {"tp": [], "dup": [], "extra": [], "known_fp": [], "nit": [], "unlabeled": []}
    for di in hit:
        out["tp"].append(defects[di]["id"])
    for fi, f in enumerate(findings):
        if fi in used:
            continue
        labels = [d for d in defects if matches(f, d)]
        kinds = {d["kind"] for d in labels}
        if "confirmed" in kinds:
            out["dup"].append(next(d["id"] for d in labels if d["kind"] == "confirmed"))
        elif kinds & set(GOOD_EXTRA):
            out["extra"].append(next(d["id"] for d in labels if d["kind"] in GOOD_EXTRA))
        elif kinds & set(KNOWN_FP):
            out["known_fp"].append(next(d["id"] for d in labels if d["kind"] in KNOWN_FP))
        elif "nit" in kinds:
            out["nit"].append(next(d["id"] for d in labels if d["kind"] == "nit"))
        else:
            out["unlabeled"].append(
                {
                    "file": f.get("file"),
                    "start_line": f.get("start_line"),
                    "end_line": f.get("end_line"),
                    "severity": f.get("severity"),
                    "title": f.get("title"),
                    "claim": re.sub(r"\s+", " ", f.get("claim", ""))[:400],
                }
            )
    return out


def main():
    report_path, cases_path, case_id, config, rep = sys.argv[1:6]
    r = json.load(open(report_path))
    case = next(c for c in json.load(open(cases_path))["cases"] if c["id"] == case_id)
    defects = case["defects"]
    findings = r.get("findings", [])
    # a finding the duplicate check removed keeps its verdict but is not published
    accepted = [
        f
        for f in findings
        if f.get("validation_status") == "accepted" and not f.get("duplicate_of")
    ]
    acc = classify(accepted, defects)
    cand = classify(findings, defects)
    n_confirmed = sum(1 for d in defects if d["kind"] == "confirmed")
    statuses = [f.get("validation_status") for f in findings]
    led = r.get("ledger", {}) or {}
    print(
        json.dumps(
            {
                "config": config,
                "case": case_id,
                "rep": int(rep),
                "status": r.get("status", "failed"),
                "reason": r.get("reason"),
                "confirmed": n_confirmed,
                "tp": len(acc["tp"]),
                "fn": n_confirmed - len(acc["tp"]),
                "dup": len(acc["dup"]),
                "extra": len(acc["extra"]),
                "known_fp": len(acc["known_fp"]),
                "nit": len(acc["nit"]),
                "unlabeled": len(acc["unlabeled"]),
                "accepted": len(accepted),
                "candidates": (r.get("stats") or {}).get("candidates", len(findings)),
                "candidate_tp": len(cand["tp"]),
                "deduplicated": sum(1 for f in findings if f.get("duplicate_of")),
                "rejected": statuses.count("rejected"),
                "uncertain": statuses.count("uncertain"),
                "tp_ids": acc["tp"],
                "candidate_tp_ids": cand["tp"],
                "unlabeled_findings": acc["unlabeled"],
                "requests": led.get("requests", 0),
                "prompt_tokens": led.get("prompt_tokens", 0),
                "completion_tokens": led.get("completion_tokens", 0),
                "wall_ms": led.get("wall_ms", 0),
                "coverage_gaps": r.get("coverage_gaps", []),
            }
        )
    )


if __name__ == "__main__":
    main()
