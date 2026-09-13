#!/usr/bin/env python3
"""Summarize eval/results.jsonl into a markdown table (stdout)."""
import json
import statistics
import sys
from collections import defaultdict

path = sys.argv[1] if len(sys.argv) > 1 else "eval/results.jsonl"
rows = [json.loads(l) for l in open(path) if l.strip()]

CLEAN = {"clean-refactor", "clean-docs"}

agg = defaultdict(list)
for r in rows:
    agg[r["config"]].append(r)

print("| config | TP | FN | FP | clean-PR commented | incomplete | median wall s | mean req | mean tok | total cost |")
print("|---|---|---|---|---|---|---|---|---|---|")
order = ["A-baseline", "B-baseline-novera", "C-baseline-norerank",
         "D-candidate-only", "E-panel-2scouts", "F-delegated"]
for cfg in order + sorted(set(agg) - set(order)):
    rs = agg.get(cfg, [])
    if not rs:
        continue
    tp = sum(r["tp"] for r in rs)
    fn = sum(max(0, r["fn"]) for r in rs)
    fp = sum(r["fp"] for r in rs)
    clean_commented = sum(
        1 for r in rs if r["corpus"] in CLEAN and r["fp"] > 0
    )
    incomplete = sum(1 for r in rs if r["status"] != "complete")
    walls = [r["wall_ms"] / 1000 for r in rs]
    reqs = [r["requests"] for r in rs]
    toks = [r["prompt_tokens"] + r["completion_tokens"] for r in rs]
    cost = sum(r["est_cost"] for r in rs)
    print(
        f"| {cfg} | {tp} | {fn} | {fp} | {clean_commented} | {incomplete} | "
        f"{statistics.median(walls):.1f} | {statistics.mean(reqs):.1f} | "
        f"{statistics.mean(toks):.0f} | ${cost:.4f} |"
    )

fails = [r for r in rows if r["status"] != "complete"]
if fails:
    print("\nFailed/incomplete runs:")
    for r in fails:
        print(f"- {r['config']} x {r['corpus']} rep {r['rep']}: "
              f"{r['status']} — {r.get('reason')}")
