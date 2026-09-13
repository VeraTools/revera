#!/usr/bin/env python3
"""Score one revera report against a corpus truth.json; prints one JSON line."""
import json
import re
import sys

PROMPT_COST = 0.10 / 1_000_000
COMPLETION_COST = 0.20 / 1_000_000


def is_tp(f, defect):
    if f.get("file") == defect["file"]:
        line_min = max(1, defect["line_min"] - 3)
        line_max = defect["line_max"] + 3
        if line_min <= (f.get("start_line") or 0) <= line_max:
            return True
    text = (f.get("title", "") + " " + f.get("claim", "")).lower()
    return any(re.search(re.escape(k.lower()), text) for k in defect["keywords"])


def main():
    report_path, truth_path, config, corpus, rep = sys.argv[1:6]
    r = json.load(open(report_path))
    truth = json.load(open(truth_path))

    accepted = [
        f for f in r.get("findings", [])
        if f.get("validation_status") == "accepted"
    ]
    tp = fp = 0
    used = set()
    for d in truth["defects"]:
        hit = next(
            (i for i, f in enumerate(accepted) if i not in used and is_tp(f, d)),
            None,
        )
        if hit is not None:
            tp += 1
            used.add(hit)
    fn = len(truth["defects"]) - tp
    fp = len(accepted) - len(used)
    tp_high = sum(
        1 for i in used if accepted[i].get("severity") in ("high", "critical")
    )
    statuses = [f.get("validation_status") for f in r.get("findings", [])]
    rejected = statuses.count("rejected")
    uncertain = statuses.count("uncertain")

    timing = r.get("timing", {}) or {}

    def _t(key):
        v = timing.get(key)
        return v / 1000 if v is not None else None

    led = r.get("ledger", {})
    ptok = led.get("prompt_tokens", 0)
    ctok = led.get("completion_tokens", 0)
    rtok = led.get("reasoning_tokens", 0)
    out = {
        "config": config,
        "corpus": corpus,
        "rep": int(rep),
        "status": r.get("status", "failed"),
        "reason": r.get("reason"),
        "clean": bool(truth.get("clean", False)),
        "tp": tp,
        "tp_high": tp_high,
        "fp": fp,
        "fn": fn,
        "rejected": rejected,
        "uncertain": uncertain,
        "requests": led.get("requests", 0),
        "prompt_tokens": ptok,
        "completion_tokens": ctok,
        "reasoning_tokens": rtok,
        "wall_ms": led.get("wall_ms", 0),
        "first_validated_s": _t("first_validated_ms"),
        "lanes_s": _t("lanes_ms"),
        "validate_s": _t("validate_ms"),
        "incomplete_phases": timing.get("incomplete_phases"),
        "est_cost": ptok * PROMPT_COST + ctok * COMPLETION_COST,
    }
    print(json.dumps(out))


if __name__ == "__main__":
    main()
