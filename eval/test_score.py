#!/usr/bin/env python3
"""Offline scoring regressions; also run by eval/test-run-all.sh."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from score import finding_id, inline_findings, match_findings

HERE = Path(__file__).resolve().parent


def finding(key, line=20, **fields):
    return {
        "file": "src/a.rs",
        "defect_key": key,
        "start_line": line,
        "title": key,
        "claim": "",
        "validation_status": "accepted",
        **fields,
    }


def comment(f, **fields):
    return {
        "file": f["file"],
        "line": f["start_line"],
        "body": f"comment\n<!-- revera-id:{finding_id(f)} -->",
        **fields,
    }


def score_report(findings, comments, truth):
    with tempfile.TemporaryDirectory() as tmp:
        report = Path(tmp) / "report.json"
        truth_path = Path(tmp) / "truth.json"
        report.write_text(json.dumps({"findings": findings, "plan": {"inline": comments}}))
        truth_path.write_text(json.dumps(truth))
        return json.loads(
            subprocess.check_output(
                [
                    sys.executable,
                    str(HERE / "score.py"),
                    str(report),
                    str(truth_path),
                    "T",
                    "C",
                    "1",
                ],
                text=True,
            )
        )


class ScoreTests(unittest.TestCase):
    def test_collocated_summary_cannot_take_inline_match(self):
        summary, inline = finding("summary"), finding("inline")
        truth = {"defects": [{"file": "src/a.rs", "line_min": 20, "line_max": 20, "keywords": []}]}
        for findings in ([summary, inline], [inline, summary]):
            with self.subTest(order=[f["defect_key"] for f in findings]):
                row = score_report(findings, [comment(inline)], truth)
                self.assertEqual((row["tp"], row["fp"]), (1, 1))
                self.assertEqual((row["tp_inline"], row["fp_inline"]), (1, 0))

    def test_collocated_summary_does_not_become_inline_fp(self):
        summary, inline = finding("summary"), finding("inline")
        row = score_report([summary, inline], [comment(inline)], {"defects": []})
        self.assertEqual((row["fp"], row["fp_inline"]), (2, 1))

    def test_inline_identity_survives_reanchoring_and_requires_final_marker(self):
        f = finding("inline")
        c = comment(f, line=80)
        self.assertEqual(inline_findings([f], [c]), [f])
        other = finding("other")
        embedded = comment(other, body=comment(other)["body"] + "\nnot the end")
        self.assertEqual(inline_findings([f, other], [c, embedded]), [f])
        self.assertEqual(inline_findings([f], [comment(f, body="legacy")]), [f])
        explicit = {**f, "id": "0123456789ab"}
        self.assertEqual(inline_findings([explicit], [comment(explicit)]), [explicit])

    def test_matching_uses_augmenting_paths(self):
        flexible, narrow = finding("both", line=80), finding("first", line=80)
        defects = [
            {"file": "src/a.rs", "line_min": 1, "line_max": 1, "keywords": ["both", "first"]},
            {"file": "src/a.rs", "line_min": 10, "line_max": 10, "keywords": ["both"]},
        ]
        self.assertEqual(len(match_findings([flexible, narrow], defects)), 2)
        row = score_report(
            [flexible, narrow], [comment(flexible), comment(narrow)], {"defects": defects}
        )
        self.assertEqual((row["tp"], row["tp_inline"], row["fp_inline"]), (2, 2, 0))

    def test_also_valid_requires_keyword_and_overlapping_window(self):
        truth = {
            "defects": [],
            "also_valid": [{"file": "src/a.rs", "lines": [10, 20], "keywords": ["typeof handler"]}],
        }
        for start, end, title, exempt in (
            (9, 10, "removed typeof handler guard", True),
            (20, None, "removed typeof handler guard", True),
            (9, None, "removed typeof handler guard", False),
            (21, 25, "removed typeof handler guard", False),
            (15, None, "unrelated string buffer issue", False),
        ):
            with self.subTest(start=start, end=end, title=title):
                f = finding("test", line=start, end_line=end, title=title)
                row = score_report([f], [comment(f)], truth)
                self.assertEqual(row["extra_valid"], int(exempt))
                self.assertEqual((row["fp"], row["fp_inline"]), (int(not exempt),) * 2)

    def test_hono_exemption_does_not_match_bare_string(self):
        truth = json.loads((HERE / "real/truth/real-clean-hono-61bee9f6.json").read_text())
        f = finding("string buffer issue", file="src/hono-base.ts")
        row = score_report([f], [comment(f)], truth)
        self.assertEqual((row["fp"], row["extra_valid"]), (1, 0))

    def test_clean_inline_metric_counts_actual_comments_not_false_positives(self):
        f = finding("typeof handler")
        truth = {
            "clean": True,
            "defects": [],
            "also_valid": [{"file": f["file"], "keywords": ["typeof handler"]}],
        }
        row = score_report([f], [comment(f)], truth)
        self.assertEqual((row["inline_comments"], row["fp_inline"]), (1, 0))
        legacy = {k: v for k, v in row.items() if k != "inline_comments"}
        legacy["config"] = "legacy"
        with tempfile.TemporaryDirectory() as tmp:
            results = Path(tmp) / "results.jsonl"
            results.write_text(json.dumps(row) + "\n" + json.dumps(legacy) + "\n")
            summary = subprocess.check_output(
                [sys.executable, str(HERE / "summarize.py"), str(results)], text=True
            )
        lines = summary.splitlines()
        headers = [h.strip() for h in lines[0].split("|")[1:-1]]
        cells = {
            values[0]: dict(zip(headers, values, strict=True))
            for line in lines[2:4]
            if (values := [v.strip() for v in line.split("|")[1:-1]])
        }
        self.assertEqual(cells["T"]["clean-PR inline"], "1")
        self.assertEqual(cells["T"]["inline FP"], "0")
        self.assertEqual(cells["legacy"]["clean-PR inline"], "-")

    def test_rescore_writes_separate_output_for_exact_config_labels(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            reports = root / "reports"
            reports.mkdir()
            source, output = root / "results.jsonl", root / "rescored.jsonl"
            rows = [
                {"config": cfg, "corpus": "real-clean-hono-61bee9f6", "rep": 1, "fp": 99}
                for cfg in ("P0-glm", "P1-glm", "P0-glm-extra", "missing")
            ]
            original = "".join(json.dumps(r) + "\n" for r in rows)
            source.write_text(original)
            f = finding("unrelated string buffer issue", file="src/hono-base.ts")
            report = {"status": "complete", "findings": [f], "plan": {"inline": [comment(f)]}}
            for row in rows[:-1]:
                (reports / f"{row['config']}-{row['corpus']}-1.json").write_text(json.dumps(report))
            args = [
                sys.executable,
                str(HERE / "rescore.py"),
                "--results",
                str(source),
                "--reports",
                str(reports),
                "--out",
                str(output),
                "P0-glm",
                "P1-glm",
                "missing",
            ]
            subprocess.run(args, check=True, capture_output=True, text=True)
            rescored = [json.loads(line) for line in output.read_text().splitlines()]
            self.assertEqual(source.read_text(), original)
            for row in rescored[:2]:
                self.assertEqual((row["fp"], row["fp_inline"], row["inline_comments"]), (1, 1, 1))
            self.assertEqual(rescored[2:], rows[2:])
            # An existing output, the input itself, and the tracked results
            # are all refused rather than overwritten.
            for forbidden in (output, source, HERE / "results.jsonl"):
                args[args.index("--out") + 1] = str(forbidden)
                result = subprocess.run(args, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
            self.assertEqual(source.read_text(), original)


if __name__ == "__main__":
    unittest.main()
