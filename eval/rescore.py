#!/usr/bin/env python3
"""Re-score saved reports against the current truth files.

    eval/rescore.py [CONFIG_PREFIX ...]

Rewrites eval/results.jsonl in place: every row whose config starts with one
of the prefixes (all rows when none given) and whose report is still in
eval/reports/ is replaced by a fresh eval/score.py line. Rows without a saved
report, or whose truth file is missing, are kept unchanged. Use after a truth
fix (new also_valid entries, corrected keywords) instead of paying for reruns.
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(HERE, "results.jsonl")
prefixes = sys.argv[1:]


def truth_for(corpus):
    for p in (
        os.path.join(HERE, "corpus", corpus, "truth.json"),
        os.path.join(HERE, "large", "truth", f"{corpus}.json"),
        os.path.join(HERE, "real", "truth", f"{corpus}.json"),
    ):
        if os.path.exists(p):
            return p
    return None


out, changed = [], 0
for line in open(RESULTS):
    if not line.strip():
        continue
    row = json.loads(line)
    report = os.path.join(HERE, "reports", f"{row['config']}-{row['corpus']}-{row['rep']}.json")
    truth = truth_for(row["corpus"])
    wanted = not prefixes or any(row["config"].startswith(p) for p in prefixes)
    if wanted and os.path.exists(report) and truth and row.get("status") != "failed":
        new = subprocess.run(
            [
                sys.executable,
                os.path.join(HERE, "score.py"),
                report,
                truth,
                row["config"],
                row["corpus"],
                str(row["rep"]),
                "" if row.get("diff_lines") is None else str(row["diff_lines"]),
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        out.append(new)
        changed += 1
    else:
        out.append(line.strip())
open(RESULTS, "w").write("\n".join(out) + "\n")
print(f"rescored {changed} of {len(out)} rows", file=sys.stderr)
