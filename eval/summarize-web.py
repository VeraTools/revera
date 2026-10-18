#!/usr/bin/env python3
"""Print raw paired-review rows; no quality conclusions or cost estimates."""

import json
from pathlib import Path


def main():
    here = Path(__file__).resolve().parent
    rows = [json.loads(line) for line in (here / "results.jsonl").read_text().splitlines()]
    print("| arm | corpus | rep | TP | FP | uncertain | rejected | tools | web attempts | seconds | tokens | status |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for row in rows:
        if row["config"] not in ("WEB-off", "WEB-on"):
            continue
        path = here / "reports" / f"{row['config']}-{row['corpus']}-{row['rep']}.json"
        report = json.loads(path.read_text()) if path.exists() else {}
        tools = report.get("stats", {}).get("tools", [])
        calls = sum(tool["calls"] for tool in tools)
        web = sum(stat["calls"] for tool in tools for stat in tool.get("backends", {}).values())
        tokens = row.get("prompt_tokens", 0) + row.get("completion_tokens", 0)
        values = [row["config"], row["corpus"], row["rep"], row["tp"], row["fp"],
                  row.get("uncertain", 0), row.get("rejected", 0), calls, web,
                  round(row.get("wall_ms", 0) / 1000, 1), tokens, row["status"]]
        print("| " + " | ".join(map(str, values)) + " |")
    print("External-call prices are not estimated; the free endpoint has no API-key charge.")


if __name__ == "__main__":
    main()
