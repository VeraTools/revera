# Evaluation

What has been measured, how, and what the numbers do and do not show. All
of it is evidence gathered while building Revera on small synthetic corpora
with 1–2 runs per cell; none of it is a benchmark.

## Method

- **Corpora.** `eval/build-corpus.sh` and `eval/build-corpus-hard.sh` build
  small git repositories (3–6 files) with one planted defect each, plus
  clean controls. `eval/large/cases.md` specifies four cases on a pinned
  ripgrep checkout (~50k LOC, 11 crates) where the defect is only visible
  through untouched callers in other crates; truth lives in
  `eval/large/truth/`, outside the repositories.
- **Scoring.** `eval/score.py` matches accepted findings against a
  hand-written truth file per corpus (file + defect key, or keywords for
  looser cases). Every accepted finding was additionally read once by a
  person; borderline cases are called out below. There is no independent
  second adjudicator.
- **Runner.** `eval/run.sh CONFIG CORPUS [REPS]` copies the corpus (with a
  warm `.vera` index), runs `revera review --force`, and appends a scored
  row to `eval/results.jsonl`. `eval/run-all.sh` runs the matrix;
  `eval/test-run-all.sh` smoke-tests its plan with `RUN_ALL_DRY_RUN=1`.
  Configs are in `eval/configs/`.
- **Cost.** Estimated from ledger token counts at list prices of the
  endpoint used; treat as relative.

Caveats that apply everywhere: single-run differences of one finding are
noise; the engine changed between sections, so numbers from different
sections are not comparable; token and latency figures exclude Vera
indexing unless stated.

## Strategy and retrieval comparison (Meta Muse Spark on every route)

### Six-corpus set, 6 configs × 6 corpora × 2 reps

Corpora: crossfile, nullpath, offbyone, lockdrop (defects); clean-refactor,
clean-docs (controls).

| config | TP | FN | FP | clean-PR commented | incomplete | median wall s | mean req | mean tok |
|---|---|---|---|---|---|---|---|---|
| A-baseline (Vera on) | 8 | 0 | 0 | 0 | 0 | 19.2 | 6.2 | 17532 |
| B-baseline-novera | 8 | 0 | 0 | 0 | 0 | 28.3 | 5.8 | 14421 |
| C-baseline-norerank | 8 | 0 | 0 | 0 | 0 | 16.9 | 6.0 | 16761 |
| D-candidate-only (no validation) | 8 | 0 | 0 | 0 | 0 | 11.4 | 3.5 | 10031 |
| E-panel-2scouts | 8 | 0 | 2 | 0 | 0 | 19.6 | 10.9 | 32421 |
| F-delegated | 8 | 0 | 0 | 0 | 0 | 45.8 | 9.2 | 26286 |

The corpus is saturated (every config found every defect), so it ranks
configurations on cost, latency and false positives only. Panel produced
the only false positives at 1.8× cost; delegated cost 1.6× and took 2.4×
longer for no gain.

### Hard corpus, 5 configs × 7 corpora × 2 reps

Seven repositories where the evidence for the defect is outside the diff
(historical Revera regressions and multi-hop cases) plus two clean controls
designed as false-positive traps.

| config | TP | FN | FP | clean-PR commented | rejected by validator | incomplete | median wall s | mean tok |
|---|---|---|---|---|---|---|---|---|
| A-baseline | 9 | 1 | 0 | 0 | 0 | 0 | 33.1 | 24014 |
| B-baseline-novera | 9 | 1 | 0 | 0 | 0 | 0 | 27.2 | 17045 |
| E-panel-2scouts | 9 | 1 | 2 | 0 | 1 | 2 | 31.0 | 42628 |
| F-delegated | 9 | 1 | 1 | 0 | 1 | 0 | 43.4 | 46879 |
| G-baseline-reasoning-high | 9 | 1 | 0 | 0 | 0 | 1 | 26.8 | 23443 |

Recall still ties (9/10 everywhere; the miss is the multi-hop `posted-state`
case at 1/2 for every config), so the hard corpus separates on false
positives, cost and robustness. The two panel/delegated FPs are not junk —
one duplicates a true defect the collapse step missed, one is a valid
observation the truth file does not list — but they would have been posted.
`reasoning: high` (G) was honoured (reasoning tokens in 14/14 runs) and
changed nothing. `posted-state` scoring is loose: several accepted findings
flagged the right lines while describing a neighbouring problem.

Reproduce:
`CORPORA="utf8-truncate modzero-routing retry-after posted-state trait-contract clean-signature clean-dead-helper" CONFIGS="A-baseline B-baseline-novera E-panel-2scouts F-delegated G-baseline-reasoning-high" bash eval/run-all.sh 2`.

### Large repository (ripgrep), 2 configs × 4 cases × 2 reps

Equal budgets (`run_max_seconds: 600`, `agent_max_seconds: 420`,
`agent_max_tool_calls: 40`); the configs differ only in `vera.enabled`.

| config | TP | TP high/crit | FN | FP | cross-file mechanism cited | median wall s | mean req | mean tok |
|---|---|---|---|---|---|---|---|---|
| LA-baseline-large (Vera on) | 5/6 | 4 | 1 | 0 | 5 | 138.5 | 17.8 | 244974 |
| LB-baseline-large-novera | 4/6 | 2 | 2 | 0 | 4 | 118.5 | 16.5 | 208690 |

Misses: `linestep-terminator` rep 2 under both configs, `printer-crlf` rep
2 without Vera. Control: 0 FP in all four runs. This is the first evidence
in Vera's favour — +1 recall and higher severities at ~17 % more wall time
and tokens — but two reps on three defect cases is not strong evidence.

A cold `vera index` of ripgrep through an API embedding backend took 22
minutes (229 files, 5,259 chunks), most of it idle on one embedding
connection; relocating a warm `.vera` and running `vera update` took 5–6 s,
which is the path the Action's restored cache takes.

## Investigator model screening (9 configs × 7 hard corpora × 2 reps)

Configs `eval/configs/M*.yaml` (generated by `eval/configs/gen-screening.py`).
Every config uses the same validator — Z.ai GLM (`glm-5.3-flash`) at
`reasoning: high` — so only the investigator/scout side varies.
`reasoning: max` was requested where supported; on a provider 400 the
adapter steps down to `high`, then drops reasoning, and the ledger records
requested vs effective effort. Models are named by family; the endpoint
each config used is recorded in its YAML for reproduction.

Hard corpus (10 possible TPs, 2 clean controls):

| config | investigator | TP | FN | FP | clean FP | incomplete | median wall s | mean req | total tok |
|---|---|---|---|---|---|---|---|---|---|
| M1-glm | Z.ai GLM `glm-5.3` | 10 | 0 | 1 | 0 | 0 | 77 | 7.1 | 248k |
| M2-flash | Z.ai GLM `glm-5.3-flash` | 10 | 0 | 1 | 0 | 0 | 82 | 7.4 | 290k |
| M3-terra | OpenAI GPT `gpt-5.6-terra` @ max | 10 | 0 | 1 | 1 | 0 | 74 | 6.8 | 257k |
| M4-hy4 | `hy4` | 9 | 1 | 2 | 0 | 0 | 82 | 8.0 | 343k |
| M5-muse | Meta Muse Spark `muse-spark-1.3-contributor` | 10 | 0 | 0 | 0 | 0 | 41 | 7.3 | 322k |
| M6-panel-2xflash | 2 × GLM flash (identical lanes) | 9 | 1 | 5 | 1 | 0 | 71 | 12.1 | 462k |
| M7-panel-3het | GLM + Google Gemini `gemini-3.8-flash` + DeepSeek `deepseek-v4.1-flash` | 10 | 0 | 10 | 0 | 1 | 149 | 26.5 | 1237k |
| M8-panel-3xflash | 3 × GLM flash | 10 | 0 | 8 | 0 | 0 | 115 | 17.9 | 678k |
| M9-panel-5het | M7 lanes + xAI Grok `grok-4.6` + GLM flash | 10 | 0 | 35 | 1 | 1 | 137 | 46.0 | 2119k |

Large subset (ripgrep cases, 1 rep, `agent_max_seconds: 300`):

| config | TP | FN | FP | cross-file cited | incomplete | median wall s | total tok |
|---|---|---|---|---|---|---|---|
| M1-glm | 2 | 1 | 0 | 2 | 1 (agent time budget) | 358 | 699k |
| M2-flash | 0 | 3 | 0 | 0 | 3 (2 provider errors, 1 time budget) | 429 | 762k |
| M3-terra | 1 | 2 | 2 | 1 | 1 | 179 | 1230k |
| M5-muse | 2 | 1 | 0 | 2 | 1 | 149 | 761k |

### What this shows

- **Recall does not separate single-model configurations on small
  repositories** — every single lane except `hy4` scored 10/10. Small
  corpora separate on false positives and cost; large repositories separate
  on recall.
- **Panels are not worth it as configured.** More lanes produce more raw
  candidates and more accepted false positives (5, 8, 10, 35) at 1.5–8× the
  cost with no recall gain; two identical cheap lanes were worse than one.
  The validator did not absorb the extra noise.
- **The cheapest model is not free on large repositories.** GLM flash went
  0/3 with three incomplete runs on ripgrep; GLM `glm-5.3` and Muse Spark
  each found 2/3 with cross-file evidence; the GPT variant was fastest but
  produced the only clean-repository false positives.
- **Provisional defaults (best measured, weak evidence):** `baseline`;
  investigator Muse Spark or GLM `glm-5.3`; validator GLM `glm-5.3-flash`
  at `high`; Vera on for repositories above ~20k LOC with
  `agent_max_seconds` raised to 420–600 (the 300 s cap produced the only
  M1/M5 large-repository misses).
- Not measured: validator choice (one validator model throughout), more
  than two reps, real-dollar cost.

## What a fair next evaluation looks like

1. **Validator comparison on frozen candidates.** Record the investigator's
   candidates once per corpus (`findings[]` in the run report), then run
   only the validator with different models against the same candidates.
   Score accepted true defects, rejected false claims, incorrectly rejected
   true defects, uncertain verdicts, validator latency and tokens. This
   isolates the stage that decides false positives from investigator
   variance.
2. **Vera on vs lexical-only under equal budgets** on repositories larger
   than the ripgrep cases, reporting cold and warm indexing time separately
   from review wall time (`timing.vera_index_ms` vs `timing.total_ms`) and
   per-tool usage.
3. Deeper multi-hop cases only after 1–2.

Every accepted finding is read by a person and labelled TP / FP /
duplicate-of-TP before it counts; `uncertain` is reported in its own column.

## Live evaluation status of the current engine

No live evaluation has been run on the current engine (implicit baseline,
validator inheritance, strategy-aware credential checks). Offline evidence
is `cargo test` (terminal parsing and repair, review identity, lifecycle,
publication reconciliation, lexical fallback, every outcome's summary),
`fixtures/run-fixture.sh` (scripted models with a real Vera index) and
`action-smoke.yml` (the composite Action with scripted models). Treat the
model recommendations above as provisional until item 1 has been run.
