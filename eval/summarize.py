#!/usr/bin/env python3
"""Summarize eval/results.jsonl into a markdown table (stdout)."""

import json
import statistics
import sys
from collections import defaultdict

path = sys.argv[1] if len(sys.argv) > 1 else "eval/results.jsonl"
rows = [json.loads(line) for line in open(path) if line.strip()]

CLEAN = {"clean-refactor", "clean-docs"}
CORPORA = set(sys.argv[2].split(",")) if len(sys.argv) > 2 else None

agg = defaultdict(list)
for r in rows:
    if CORPORA is not None and r["corpus"] not in CORPORA:
        continue
    agg[r["config"]].append(r)


def is_clean(r):
    return r.get("clean", r["corpus"] in CLEAN)


HEADER = "| config | TP | TP high/crit | FN | FP | xf | clean-PR commented | clean-PR inline | inline TP | inline FP | rejected | uncertain | incomplete | median wall s | median first-validated s | incomplete phases | mean req | mean tok | cache hit % | total cost |"
DELIM = "|" + "---|" * 20
assert len(HEADER.split("|")) == len(DELIM.split("|")), (
    len(HEADER.split("|")),
    len(DELIM.split("|")),
)
print(HEADER)
print(DELIM)
order = [
    "A-baseline",
    "B-baseline-novera",
    "C-baseline-norerank",
    "D-candidate-only",
    "E-panel-2scouts",
    "F-delegated",
]
for cfg in order + sorted(set(agg) - set(order)):
    rs = agg.get(cfg, [])
    if not rs:
        continue
    tp = sum(r["tp"] for r in rs)
    fn = sum(max(0, r["fn"]) for r in rs)
    fp = sum(r["fp"] for r in rs)
    xf = sum(r.get("xf", 0) for r in rs)
    tp_high = sum(r.get("tp_high", 0) for r in rs)
    rejected = sum(r.get("rejected", 0) for r in rs)
    uncertain = sum(r.get("uncertain", 0) for r in rs)
    clean_commented = sum(1 for r in rs if is_clean(r) and r["fp"] > 0)
    # rows scored before inline placement was measured carry no inline
    # fields: show "-" for them rather than a measured-looking zero
    inline_rs = [r for r in rs if "tp_inline" in r]
    comment_rs = [r for r in rs if "inline_comments" in r]
    clean_inline = (
        sum(1 for r in comment_rs if is_clean(r) and r["inline_comments"] > 0)
        if comment_rs
        else "-"
    )
    if inline_rs:
        tp_inline = sum(r["tp_inline"] for r in inline_rs)
        fp_inline = sum(r.get("fp_inline", 0) for r in inline_rs)
    else:
        tp_inline = fp_inline = "-"
    incomplete = sum(1 for r in rs if r["status"] != "complete")
    walls = [r["wall_ms"] / 1000 for r in rs]
    fv = [r["first_validated_s"] for r in rs if r.get("first_validated_s") is not None]
    fv_str = f"{statistics.median(fv):.1f}" if fv else "-"
    inc_phases = sum(r.get("incomplete_phases") or 0 for r in rs)
    reqs = [r["requests"] for r in rs]
    toks = [r["prompt_tokens"] + r["completion_tokens"] for r in rs]
    cache_prompt_tokens = sum(r.get("prompt_tokens", 0) for r in rs)
    cache_hit = (
        f"{sum(r.get('cached_prompt_tokens', 0) for r in rs) / cache_prompt_tokens * 100:.0f}%"
        if cache_prompt_tokens
        else "-"
    )
    cost = sum(r["est_cost"] for r in rs)
    row = (
        f"| {cfg} | {tp} | {tp_high} | {fn} | {fp} | {xf} | {clean_commented} | {clean_inline} | {tp_inline} | {fp_inline} | {rejected} | {uncertain} | {incomplete} | "
        f"{statistics.median(walls):.1f} | {fv_str} | {inc_phases} | "
        f"{statistics.mean(reqs):.1f} | "
        f"{statistics.mean(toks):.0f} | {cache_hit} | ${cost:.4f} |"
    )
    assert len(row.split("|")) == len(HEADER.split("|")), row
    print(row)

# Miss attribution and diff-size recall need rows scored with miss_stages
# (eval/score.py after the funnel landed); older rows are skipped. Engines
# that record no funnel report candidate-less misses as "unattributed".
STAGES = [
    "never_proposed",
    "unattributed",
    "matched_other_defect",
    "merged_away",
    "severity_dropped",
    "already_tracked",
    "rejected",
    "uncertain",
]
staged = {c: [r for r in rs if "miss_stages" in r] for c, rs in agg.items()}
if any(staged.values()):
    print("\nWhere misses died (defects not accepted, by stage):\n")
    print("| config | defects | missed | " + " | ".join(STAGES) + " |")
    print("|" + "---|" * (3 + len(STAGES)))
    for cfg in sorted(c for c in staged if staged[c]):
        rs = staged[cfg]
        stages = [s for r in rs for s in r["miss_stages"]]
        cells = " | ".join(str(stages.count(s)) for s in STAGES)
        print(f"| {cfg} | {sum(r['defects'] for r in rs)} | {len(stages)} | {cells} |")

    BUCKETS = [("<10", 0, 9), ("10-150", 10, 150), (">150", 151, 10**9)]
    print("\nRecall by diff size (changed lines, base..head):\n")
    print("| config | " + " | ".join(b[0] for b in BUCKETS) + " |")
    print("|" + "---|" * (1 + len(BUCKETS)))
    for cfg in sorted(c for c in staged if staged[c]):
        cells = []
        for _, lo, hi in BUCKETS:
            rs = [
                r
                for r in staged[cfg]
                if r.get("diff_lines") is not None
                and lo <= r["diff_lines"] <= hi
                and r["defects"] > 0
            ]
            d = sum(r["defects"] for r in rs)
            cells.append(f"{sum(r['tp'] for r in rs)}/{d}" if d else "-")
        print(f"| {cfg} | " + " | ".join(cells) + " |")

fails = [r for r in rows if r["status"] != "complete"]
if fails:
    print("\nFailed/incomplete runs:")
    for r in fails:
        print(f"- {r['config']} x {r['corpus']} rep {r['rep']}: {r['status']} — {r.get('reason')}")
