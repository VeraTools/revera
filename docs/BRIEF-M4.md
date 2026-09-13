# Brief: M4 — `delegated` and `panel` strategies (+ CI fixture step)

Branch: `devin/1789294091-revera-core-m1-m3` (PR #1). Commit there, push.

## 0. CI fix
`.github/workflows/ci.yml` fixture step runs without installing vera. Install
vera 1.4.1 (same sha-verified snippet as self-review.yml) before
`fixtures/run-fixture.sh`. Keep the secret gate.

## 1. Refactor baseline.rs into shared stages (src/pipeline/{mod,common,baseline,delegated,panel}.rs)
Shared: `Prepared { repo, base_sha, head_sha, diff, patch_id, vera, toolbox, ledger, state, rechecks }`,
`prepare(cfg, req) -> Prepared | ShortCircuit(report)`, `finish(cfg, prepared, candidates: Vec<Finding>, lane_labels) -> (RunReport, ReviewState)`
which does collapse → min_severity → validate → anchor → state → report exactly as today.
Strategies only differ in how `candidates` are produced. Baseline must produce byte-identical fixture output after the refactor.

## 2. Config
```yaml
models:
  lead:      {…}   # delegated: plans + synthesizes (defaults to investigator route if absent)
  workers:   [{…}] # delegated: pool; question i uses workers[i % len]; defaults to [investigator]
  scouts:    [{…}] # panel: one lane per entry; defaults to [investigator]
delegated:
  max_questions: 4
  worker_max_tool_calls: 12
  worker_max_seconds: 120
panel:
  focuses: [general, cross-file]   # names from prompts/scout_focus.md; len must == scouts.len() or 1 scout repeated per focus
  scout_max_tool_calls: 15
```
`review.concurrency` bounds simultaneous lanes (futures::stream buffer_unordered). All lanes share the run ledger/budget; when the run budget trips, remaining lanes are skipped and `partial_reasons` gets "run budget exhausted before N lanes".

## 3. delegated
1. Lead (`prompts/lead_plan.md`, terminal tool `submit_plan { questions: [{id, question, symbols[], expected_evidence, stop_condition, files_hint[]}] }`, max_tool_calls 6) sees the diff + changed-file list + prior rechecks, may call vera_overview/vera_search. Cap to `max_questions`; if 0 questions → run baseline investigator instead (log it).
2. Workers concurrently: `prompts/worker.md` + the contract; terminal `submit_worker_result { result: answered|blocked|no_issue, answer, evidence[], candidate_findings[Finding], gaps[] }`. Worker output stored verbatim (bounded to 6k chars each).
3. Lead synthesis (`prompts/lead_synthesize.md`, terminal `submit_findings`, no tools except read_file; max 4 calls) receives the worker reports and emits final candidates. Blocked/gap items → appended to `partial_reasons`? NO — to report `coverage_gaps: Vec<String>` (new field, rendered under "Not checked").
4. `finish(...)`.
Finding.source = "delegated:worker:<id>" or "delegated:lead".

## 4. panel
Scouts run concurrently, each = investigator prompt + focus addendum from `prompts/scout_focus.md` (parse the `## <focus>` sections). Each returns findings; union all, `collapse` merges same-id (keep the union of evidence, record `source = "panel:<focus>"` list). Nothing is dropped for being a minority; validator decides. Panel-specific report line: "panel: N scouts, M raw candidates → K unique".

## 5. Fixtures + tests
Scripted fixtures for both strategies on the crossfile repo (`fixtures/configs/scripted-delegated.yaml`, `scripted-panel.yaml`, scripts under fixtures/scripts/): delegated: lead plans 2 questions, worker 1 finds the caller bug, worker 2 says no_issue, lead synthesizes 1 finding, validator accepts → same inline plan as baseline. panel: 2 scouts, one finds the bug, one finds a duplicate + a junk finding the validator rejects → 1 inline finding. Extend `run-fixture.sh` to run all three strategies. Unit tests: plan cap, `i % len` worker routing, focus-section parsing, panel union/collapse preserving minority, run-budget lane skipping (stub client).

## 6. Live check
Run delegated and panel on the crossfile `break` checkout with `meta/muse-spark-1.3-contributor` for EVERY route (fixtures/configs/live-openrouter.yaml variants). Append a table to docs/EVAL.md: strategy | found bug | false findings | requests | prompt/completion tokens | wall s | est. cost. Baseline numbers already exist there.

Verification: cargo test, clippy -D warnings, fmt --check, fixtures/run-fixture.sh (3 strategies), live runs. Update STATUS.md and `revera.example.yaml`. Report the EVAL table verbatim.
