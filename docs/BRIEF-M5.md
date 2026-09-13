# Brief: M5 — practical evaluation and provisional default

Branch: same PR branch. Model for ALL routes: `meta/muse-spark-1.3-contributor`.
Consequence: comparisons that require a distinct strong model (package 09 items
4 and 6) are recorded as "not run — single-model constraint" in EVAL.md.

## Eval corpus: `eval/corpus/` — 6 small synthetic repos, each a git repo built by
`eval/build-corpus.sh` (deterministic: fixed author/date env so SHAs are stable),
with branches `base` and `head`. Ground truth in `eval/corpus/<name>/truth.json`:
`{ "defects": [{"file","line_min","line_max","keywords":[..]}], "clean": bool }`.

1. `crossfile` — existing fixture (percent/fraction unit break in a caller). 1 defect.
2. `nullpath` — Python: `parse_config()` now returns `None` when the file is
   missing (previously raised); unchanged caller `load()` does `cfg["port"]`. 1 defect.
3. `offbyone` — Go: pagination helper changes `end := start + size` to
   `end := start + size + 1` in `pages.go`; test file unchanged. 1 defect.
4. `lockdrop` — Rust: refactor moves a `counter += 1` outside the `Mutex` guard
   scope into a section reading a stale local copy; concurrency defect. 1 defect.
5. `clean-refactor` — TypeScript: rename `getUser`→`fetchUser` across 3 files +
   extracted helper, behavior-preserving. clean.
6. `clean-docs` — Markdown + comment-only changes across 2 files. clean.

## Configs: `eval/configs/*.yaml` (all muse-spark, publish dry-run, min_severity low)
- `A-baseline` (vera on, reranker on)
- `B-baseline-novera` (`vera.enabled: false` — add this config key if missing: skips index and removes vera_* tools)
- `C-baseline-norerank` (vera on, no reranker block)
- `D-candidate-only` (add `review.validate: false`: candidates skip validation and are treated as accepted — eval-only knob, warn in log)
- `E-panel-2scouts` (focuses general, cross-file)
- `F-delegated`

## Runner: `eval/run.sh CONFIG CORPUS [REPS]` and `eval/run-all.sh` (REPS=2, lanes in parallel ≤3)
Per run: fresh copy of the corpus repo in a temp dir (checkout head, `.vera`
warm-indexed once per corpus and copied in so index time is excluded), run
`revera review --base base --head head --config … --out report.json --force`,
then `eval/score.py report.json truth.json` → one JSON line appended to
`eval/results.jsonl` with: config, corpus, rep, status, tp, fp, fn, requests,
prompt_tokens, completion_tokens, wall_ms, est_cost (from $0.10/$0.20 per M).
Scoring: a reported accepted finding is TP if `file` matches and `start_line`
within [line_min-3, line_max+3] OR any keyword appears (case-insensitive) in
title+claim; otherwise FP. FN = defects with no TP. For clean corpora every
accepted finding is FP. Uncertain findings are not counted (they don't publish).

## Report: `eval/summarize.py` → markdown table into docs/EVAL.md
Per config: TP/FN over all defects, FP total, clean-PR-commented count, incomplete
runs, median wall s, mean requests, mean tokens, total cost. Then a short
"Decision" section written by ME after you report — leave a `TODO(lead)` marker.

## Also
- `revera eval` is NOT a CLI subcommand; keep this as scripts.
- Add `eval/` to the CI? No. Document in README how to run it.
- STATUS.md: update.

Verification: cargo test/clippy/fmt clean; `eval/run-all.sh` completes; paste the
summary table verbatim in your report along with results.jsonl path and any
runs that failed (with the reason). Commit + push; report at the push.
