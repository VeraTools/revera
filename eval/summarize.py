#!/usr/bin/env python3
"""Summarize eval/results.jsonl into a markdown table (stdout)."""
import json
import statistics
import sys
from collections import defaultdict

path = sys.argv[1] if len(sys.argv) > 1 else "eval/results.jsonl"
rows = [json.loads(l) for l in open(path) if l.strip()]

CLEAN = {"clean-refactor", "clean-docs"}
CORPORA = set(sys.argv[2].split(",")) if len(sys.argv) > 2 else None

agg = defaultdict(list)
for r in rows:
    if CORPORA is not None and r["corpus"] not in CORPORA:
        continue
    agg[r["config"]].append(r)


def is_clean(r):
    return r.get("clean", r["corpus"] in CLEAN)


print("| config | TP | TP high/crit | FN | FP | clean-PR commented | rejected | uncertain | incomplete | median wall s | median first-validated s | incomplete phases | mean req | mean tok | total cost |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
order = ["A-baseline", "B-baseline-novera", "C-baseline-norerank",
         "D-candidate-only", "E-panel-2scouts", "F-delegated"]
for cfg in order + sorted(set(agg) - set(order)):
    rs = agg.get(cfg, [])
    if not rs:
        continue
    tp = sum(r["tp"] for r in rs)
    fn = sum(max(0, r["fn"]) for r in rs)
    fp = sum(r["fp"] for r in rs)
    tp_high = sum(r.get("tp_high", 0) for r in rs)
    rejected = sum(r.get("rejected", 0) for r in rs)
    uncertain = sum(r.get("uncertain", 0) for r in rs)
    clean_commented = sum(1 for r in rs if is_clean(r) and r["fp"] > 0)
    incomplete = sum(1 for r in rs if r["status"] != "complete")
    walls = [r["wall_ms"] / 1000 for r in rs]
    fv = [r["first_validated_s"] for r in rs if r.get("first_validated_s") is not None]
    fv_str = f"{statistics.median(fv):.1f}" if fv else "-"
    inc_phases = sum(r.get("incomplete_phases") or 0 for r in rs)
    reqs = [r["requests"] for r in rs]
    toks = [r["prompt_tokens"] + r["completion_tokens"] for r in rs]
    cost = sum(r["est_cost"] for r in rs)
    print(
        f"| {cfg} | {tp} | {tp_high} | {fn} | {fp} | {clean_commented} | {rejected} | {uncertain} | {incomplete} | "
        f"{statistics.median(walls):.1f} | {fv_str} | {inc_phases} | "
        f"{statistics.mean(reqs):.1f} | "
        f"{statistics.mean(toks):.0f} | ${cost:.4f} |"
    )

fails = [r for r in rows if r["status"] != "complete"]
if fails:
    print("\nFailed/incomplete runs:")
    for r in fails:
        print(f"- {r['config']} x {r['corpus']} rep {r['rep']}: "
              f"{r['status']} — {r.get('reason')}")
