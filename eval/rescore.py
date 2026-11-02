#!/usr/bin/env python3
"""Re-score saved reports against the current scorer and truth files, offline.

    python3 eval/rescore.py --out "$HOME/.local/tmp/rescored.jsonl" P0-glm P1-glm P2-glm

Reads eval/results.jsonl and eval/reports/ by default (--results and --reports
override those inputs). Exact config labels select rows to re-score with
eval/score.py; unselected rows and rows missing reports or truth are copied
unchanged. Writes a NEW output file, never overwriting eval/results.jsonl or
the input. Use after scoring/truth fixes instead of paying for new reviews.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results.jsonl"


def truth_for(corpus):
    for p in (
        HERE / "corpus" / corpus / "truth.json",
        HERE / "large" / "truth" / f"{corpus}.json",
        HERE / "real" / "truth" / f"{corpus}.json",
    ):
        if p.is_file():
            return p
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path, help="new output JSONL file")
    parser.add_argument("--results", type=Path, default=RESULTS)
    parser.add_argument("--reports", type=Path, default=HERE / "reports")
    parser.add_argument("configs", nargs="+", help="exact config labels to rescore")
    args = parser.parse_args()
    if args.out.resolve() in (RESULTS.resolve(), args.results.resolve()):
        parser.error("--out must not overwrite results.jsonl or the input results")

    out, changed = [], 0
    for line in args.results.read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        report = args.reports / f"{row['config']}-{row['corpus']}-{row['rep']}.json"
        truth = truth_for(row["corpus"])
        if (
            row["config"] in args.configs
            and row.get("status") != "failed"
            and report.is_file()
            and truth
        ):
            new = subprocess.run(
                [
                    sys.executable,
                    str(HERE / "score.py"),
                    str(report),
                    str(truth),
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
    with args.out.open("x") as output:
        output.write("\n".join(out) + "\n")
    print(f"rescored {changed} of {len(out)} rows into {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
