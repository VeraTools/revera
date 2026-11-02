#!/usr/bin/env python3
"""Score one revera report against a corpus truth.json; prints one JSON line."""

import hashlib
import json
import re
import sys

PROMPT_COST = 0.10 / 1_000_000
COMPLETION_COST = 0.20 / 1_000_000


def is_tp(f, defect):
    # same file is required: a keyword hit in another file's finding is a
    # different defect that happens to share vocabulary (often the PR's own)
    if f.get("file") != defect["file"]:
        return False
    line_min = max(1, defect["line_min"] - 3)
    line_max = defect["line_max"] + 3
    if line_min <= (f.get("start_line") or 0) <= line_max:
        return True
    text = (f.get("title", "") + " " + f.get("claim", "")).lower()
    return any(re.search(re.escape(k.lower()), text) for k in defect["keywords"])


DROP_STAGE = {
    "merged": "merged_away",
    "below_severity": "severity_dropped",
    "already_tracked": "already_tracked",
}


def miss_stage(r, defect):
    """Where a missed defect was lost: the validator verdict of a matching
    candidate, an accepted finding that the matching gave to another defect,
    the pre-validation drop reason recorded in stats.funnel, or never_proposed
    when no candidate matched at all. Reports without stats.funnel (engines
    that do not record pre-validation drops) cannot tell a candidate that was
    never proposed from one dropped before validation: those are
    unattributed."""
    for f in r.get("findings", []):
        st = f.get("validation_status")
        if st in ("rejected", "uncertain") and is_tp(f, defect):
            return st
    for f in r.get("findings", []):
        if f.get("validation_status") == "accepted" and is_tp(f, defect):
            return "matched_other_defect"
    funnel = (r.get("stats") or {}).get("funnel")
    for d in (funnel or {}).get("dropped", []):
        if is_tp(
            {
                "file": d.get("file"),
                "start_line": d.get("start_line"),
                "title": d.get("title", ""),
                "claim": d.get("defect_key", ""),
            },
            defect,
        ):
            return DROP_STAGE.get(d.get("reason"), d.get("reason", "dropped"))
    return "never_proposed" if funnel is not None else "unattributed"


def match_findings(findings, defects):
    # defect index -> matched finding index. Maximum bipartite matching
    # (augmenting paths), so a loosely matching finding cannot take the only
    # finding another defect could use. Keeps the association so per-defect
    # fields like cross_file_keywords stay attributable.
    cand = [[i for i, f in enumerate(findings) if is_tp(f, d)] for d in defects]
    owner = {}  # finding index -> defect index

    def augment(di, seen):
        for i in cand[di]:
            if i in seen:
                continue
            seen.add(i)
            if i not in owner or augment(owner[i], seen):
                owner[i] = di
                return True
        return False

    for di in range(len(defects)):
        augment(di, set())
    return {di: i for i, di in owner.items()}


def finding_id(f):
    if f.get("id"):
        return f["id"]
    if "file" in f and "defect_key" in f:
        # src/findings.rs::finding_id: hex(sha256(file + NUL + defect_key))[..12]
        return hashlib.sha256((f["file"] + "\0" + f["defect_key"]).encode()).hexdigest()[:12]
    return None


def inline_findings(findings, comments):
    ids = {
        m[1]
        for c in comments
        if (m := re.search(r"<!-- revera-id:([0-9a-f]{12}) -->\s*\Z", c.get("body", "")))
    }
    if ids:
        return [f for f in findings if finding_id(f) in ids]
    # Legacy reports with no inline identity markers can only use anchors.
    inline_at = {(c.get("file"), c.get("line")) for c in comments}
    return [f for f in findings if (f.get("file"), f.get("start_line")) in inline_at]


def main():
    report_path, truth_path, config, corpus, rep = sys.argv[1:6]
    diff_lines = int(sys.argv[6]) if len(sys.argv) > 6 and sys.argv[6] else None
    r = json.load(open(report_path))
    truth = json.load(open(truth_path))

    accepted = [f for f in r.get("findings", []) if f.get("validation_status") == "accepted"]
    matches = match_findings(accepted, truth["defects"])
    tp = len(matches)
    fn = len(truth["defects"]) - tp
    fp = len(accepted) - len(matches)

    # Real defects outside the planted set: same file, overlapping lines
    # when a window is given, and a defect-specific keyword hit.
    # They count as neither TP nor FP, and are reported as extra_valid.
    def also_valid(f):
        text = (f.get("title", "") + " " + f.get("claim", "")).lower()
        return any(
            f.get("file") == a["file"]
            and (
                not a.get("lines")
                or (
                    (f.get("start_line") or 0) <= a["lines"][1]
                    and (f.get("end_line") or f.get("start_line") or 0) >= a["lines"][0]
                )
            )
            and any(k.lower() in text for k in a["keywords"])
            for a in truth.get("also_valid", [])
        )

    unmatched = [f for i, f in enumerate(accepted) if i not in matches.values()]
    extra_valid = sum(1 for f in unmatched if also_valid(f))
    fp -= extra_valid
    # Match independently: a summary finding must not consume a defect that
    # an actual inline comment also found, nor inherit its collocated anchor.
    inline = inline_findings(accepted, r.get("plan", {}).get("inline", []))
    inline_matches = match_findings(inline, truth["defects"])
    tp_inline = len(inline_matches)
    fp_inline = sum(
        1 for i, f in enumerate(inline) if i not in inline_matches.values() and not also_valid(f)
    )
    miss_stages = [miss_stage(r, d) for di, d in enumerate(truth["defects"]) if di not in matches]
    tp_high = sum(
        1 for i in matches.values() if accepted[i].get("severity") in ("high", "critical")
    )
    statuses = [f.get("validation_status") for f in r.get("findings", [])]
    # cross-file evidence: a matched finding counts when its text mentions
    # a keyword from ITS OWN defect's cross_file_keywords
    xf = 0
    for di, i in matches.items():
        xf_kw = [k.lower() for k in truth["defects"][di].get("cross_file_keywords", [])]
        if not xf_kw:
            continue
        f = accepted[i]
        text = (
            f.get("title", "")
            + " "
            + f.get("claim", "")
            + " "
            + json.dumps(f.get("supporting_evidence", []))
        ).lower()
        if any(k in text for k in xf_kw):
            xf += 1
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
    cached_prompt_tokens = led.get("cached_prompt_tokens", 0)
    cache_write_tokens = led.get("cache_write_tokens", 0)
    cache_hit_rate = cached_prompt_tokens / ptok if ptok else None
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
        "tp_inline": tp_inline,
        "fp_inline": fp_inline,
        "inline_comments": len(r.get("plan", {}).get("inline", [])),
        "rejected": rejected,
        "uncertain": uncertain,
        "requests": led.get("requests", 0),
        "prompt_tokens": ptok,
        "completion_tokens": ctok,
        "reasoning_tokens": rtok,
        "cached_prompt_tokens": cached_prompt_tokens,
        "cache_write_tokens": cache_write_tokens,
        "cache_hit_rate": cache_hit_rate,
        "wall_ms": led.get("wall_ms", 0),
        "xf": xf,
        "defects": len(truth["defects"]),
        "extra_valid": extra_valid,
        "miss_stages": miss_stages,
        "funnel": {
            k: v for k, v in (r.get("stats", {}).get("funnel", {}) or {}).items() if k != "dropped"
        },
        "diff_lines": diff_lines,
        "first_validated_s": _t("first_validated_ms"),
        "lanes_s": _t("lanes_ms"),
        "validate_s": _t("validate_ms"),
        "incomplete_phases": timing.get("incomplete_phases"),
        "est_cost": ptok * PROMPT_COST + ctok * COMPLETION_COST,
    }
    print(json.dumps(out))


if __name__ == "__main__":
    main()
