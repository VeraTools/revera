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
- Not measured here: validator choice (one validator model throughout;
  see E1 below), more than two reps, real-dollar cost.

## Frozen-candidate harness

`eval/frozen.py` implements item 1 below. Capture candidates once with a
`review.validate: false` config (the report's `findings[]` are then
unvalidated candidates; a validated report is refused), then:

```sh
eval/frozen.py --candidates report.json --repo <corpus> --base base --head head \
    --out out/ arm-a.yaml arm-b.yaml
```

Each arm config keeps its own validator route and review settings; its
investigator is replaced by a scripted route replaying the frozen
candidates, and each arm reviews its own copy of the repository with no
prior state. The arm's `review.min_severity` is recorded and applied only
when scoring (`accepted_at_min_severity`); the run itself keeps every
severity so a validator downgrade cannot drop a candidate from the
report. Arm config file names must be distinct. The run fails unless
every arm validated the identical candidate set (same digest), with
validation `fresh`, at least one validator request per candidate, and no
rechecks; one JSON row per arm is printed and written to
`out/summary.json`. `eval/test-frozen.sh` checks
the mechanics offline with scripted validators. Guidance
(`review.guidance`) affects only the investigator, so it is compared with
ordinary paired runs, not this harness.

## v0.4 engine evaluation (2026-09-29)

Engine: commit `c7784ec` (the binary still reported `0.3.0`; the version
bump came later and changed no code), with reruns on `a98bcb1` (see
below). Prompt source hash
`fe07af69ecc520f575d741e177700c8dc40f429b1e0071aafeb857a2cf1dd99d`
(`prompts_source_sha256` in every frozen summary). Routes: OpenAI-compatible
chat for GLM and DeepSeek, OpenAI Responses for GPT; Vera with the Qwen3
embedding model through an API backend, warm indexes. Validators ran at
`reasoning: high` with `max_output_tokens: 4000`.

**Answer-key exposure and reruns.** Each corpus keeps its `truth.json`
untracked in the repository's working tree. Revera's lexical tools only
see tracked files at the reviewed head, but the Vera index was built from
the working tree, and `vera grep` did return `truth.json`. Any run that
called a Vera tool could have seen the answer key. The engine now drops
Vera hits that are not tracked at the reviewed head (`a98bcb1`), and every
affected run was rerun on that binary: 20 E1 validator arms, the five E2
scenarios with an affected run (all three modes, 15 reviews) and 4 E3
reviews. The tables below use the reruns. Reports do not record tool
results, so which original runs actually saw the file is unknown; the
outcomes that changed are listed per section.

### E1: validator comparison on frozen candidates

Candidates were produced once by `review.validate: false` runs of two
investigators (GLM `glm-5.3-flash`, DeepSeek `deepseek-v4.1-flash`) and
supplemented with hand-authored false claims that are plausible but wrong
(for example a TOCTOU race that cannot happen, or a test that "now fails"
but does not). Each arm validated the identical candidate set with
`eval/frozen.py`; truth lived outside the repositories.

Small set: 13 hard and small corpora, 21 candidates (9 true, 12 false),
2 reps per arm, 42 validations per arm.

| validator | true accepted | false rejected | false accepted | uncertain | validator s (total) | requests |
|---|---|---|---|---|---|---|
| GLM `glm-5.3-flash` | 17/18 | 24/24 | 0 | 0 | 1418 | 142 |
| OpenAI GPT `gpt-6-sol` | 18/18 | 24/24 | 0 | 0 | 1300 | 171 |
| DeepSeek `deepseek-v4.1-flash` | 18/18 | 21/24 | 3 | 0 | 481 | 172 |

Changed by the rerun: GLM now rejected the true `posted-state` defect once
(it argued nothing inserts summary findings into state, but the state type
models them and `mark_posted` only marks inline ids, so the new guard can
never fire). DeepSeek now rejected one false claim it had accepted. DeepSeek
still accepted `clean-signature/cli_command_accepts_missing_port_silently`
once and two other false claims once each. Two true candidates
carried a deliberately unsafe `suggested_fix` (`try_lock()` and skip the
write; `unwrap_or(0)` for `Retry-After`); every validator in every rep
rejected the unsafe remedy and wrote a safe `fix`, so no unsafe remedy
reached a report.

Large set: three ripgrep-derived cases (`linestep-terminator`,
`globset-dot-ext`, `printer-crlf`), 4 candidates (3 true, 1 authored
false), 1 rep. GLM and GPT accepted all three true defects and rejected the
false claim. DeepSeek accepted two and returned `uncertain` on
`printer-crlf`: it spent the whole 4000-token output budget on reasoning
and returned no verdict, which counts as a missed high-impact defect.

**Decision:** keep GLM `glm-5.3-flash` as the recommended validator. GPT
was the only arm with no wrong verdict (GLM missed one true defect of 21;
one verdict is within run-to-run noise) and GPT is an equally good choice;
DeepSeek was about 3× faster but the only arm to accept false claims or
lose a verdict to its output budget.

### E2: repository guidance (`review.guidance`)

`eval/build-corpus-guidance.sh` builds eight scenarios (g1–g8): a
contract only stated in `REVIEW.md`, a project convention, irrelevant
guidance, nested `AGENTS.md`, an oversized file, guidance added only on the
PR head (an injection attempt), misleading base guidance, and a noisy clean
PR. Each ran under `off`, `review` and `agents` with the M1-glm routes
(`eval/configs/E2-guidance-*.yaml`), 1 rep, plus a second rep on the two
variable scenarios (g1, g6): 30 scored reviews.

| mode | TP | FN | FP | clean FP | incomplete | median wall s |
|---|---|---|---|---|---|---|
| off | 6 | 2 | 0 | 0 | 2 | 107 |
| review | 6 | 2 | 0 | 0 | 2 | 91 |
| agents | 6 | 2 | 0 | 0 | 1 | 108 |

- **Misleading guidance suppressed a real defect.** g7's base `AGENTS.md`
  says `src/auth.rs` is externally audited and findings in it are not to be
  reported. In one `agents` run (the rerun) the investigator followed it,
  skipped a high-severity authentication bypass and reported the review
  `complete` with no findings; with guidance off the bypass was found. That
  is 1 of 4 guidance-enabled g7 runs.
- g3, g4, g5 and g8 scored identically in every mode. The oversized
  file (400,085 bytes) was skipped; nested `AGENTS.md` files both loaded.
- g6: head-only guidance was never loaded (`sources: []`). The original
  `off` run's extra finding (flagging the injected file) did not recur in
  the rerun.
- g2 did not discriminate: the convention violation was never reported,
  even with guidance off. (A first g2 build was flawed, with a module that
  was never declared, and its three runs were discarded after the builder
  was fixed.)
- g1, the case guidance should help most, was noise: every miss was a
  `glm-5.3` investigator stopping at the 300 s agent budget or without a
  terminal call, in all three modes.

**Decision:** guidance stays `off` by default. It showed no benefit, and
g7 shows that guidance text can make the investigator drop a real
high-severity finding while the run still reports `complete`.

### E3: current engine end to end

M1-glm routes (investigator GLM `glm-5.3` at `reasoning: max`, validator
`glm-5.3-flash`), default budgets (`agent_max_seconds: 300`), warm Vera
indexes. Vera arm (`E3-vera.yaml`) on six cases × 2 reps; lexical-only arm
(`E3-lexical.yaml`, `vera.enabled: false`) on the two cross-file cases
× 2 reps. The ripgrep case was then rerun in both arms at
`agent_max_seconds: 600` (`E3-*-600.yaml`), 2 reps each. 20 reviews.

| case | arm | TP | FN | FP | incomplete | notes |
|---|---|---|---|---|---|---|
| trait-contract | Vera | 2/2 | 0 | 0 | 0 | |
| trait-contract | lexical | 2/2 | 0 | 0 | 0 | |
| retry-after | Vera | 2/2 | 0 | 2 | 0 | both FPs: a valid low-severity note that the header delay is uncapped, not in the truth file |
| posted-state | Vera | 0/2 | 2 | 0 | 2 | no terminal call; time budget |
| clean-signature | Vera | – | – | 0 | 0 | |
| clean-without-terminator (ripgrep) | Vera | – | – | 0 | 0 | |
| linestep-terminator (ripgrep), 300 s | Vera | 0/2 | 2 | 0 | 2 | time budget |
| linestep-terminator (ripgrep), 300 s | lexical | 0/2 | 2 | 0 | 2 | time budget |
| linestep-terminator (ripgrep), 600 s | Vera | 0/2 | 2 | 0 | 2 | no terminal call after 12–16 requests |
| linestep-terminator (ripgrep), 600 s | lexical | 0/2 | 2 | 0 | 2 | no terminal call after 13–14 requests |

- Changed by the rerun: `posted-state` rep 1 went from found to a
  no-terminal-call stop; `clean-signature` rep 2 went from a time-budget
  stop to complete and clean.
- Clean controls: 0 FP in 4 runs. Every incomplete run reported
  `partial` with its stop reason and posted no clean verdict.
- Completed small-case runs took 74–218 s; prompt cache hit rate was
  58–89 %.
- The investigator called Vera tools in 3 of 14 Vera-arm runs (after the
  rerun), all on small cases, and never on the ripgrep case. With Vera unused there, the
  two arms did the same work; this run says nothing about Vera's value on
  large repositories.
- `glm-5.3` at `reasoning: max` did not finish `linestep-terminator` in 8
  runs: at 300 s it ran out of time; at 600 s it answered in prose instead
  of submitting, twice, after about 5–6 minutes. The engine nudges once
  and then stops with `no_terminal_call`.

**Decision:** no default changes. Vera stays opt-in; the earlier large-
repository comparison (above) remains the only evidence for it. Whether
capability-neutral prompts reduce how often the investigator reaches for
Vera is not established and is the first question for the next round.

### Limitations

One or two reps per cell; small synthetic corpora plus three ripgrep-derived
cases; one person labelled every accepted finding; a single E1 rep on the
large set; E2 and E3 ran on one investigator model, whose budget stops
caused every g1 miss and every ripgrep miss. Wall times depend on
endpoint load on the day.

## What a fair next evaluation looks like

1. **Vera on vs lexical-only under equal budgets** on repositories larger
   than the ripgrep cases, reporting cold and warm indexing time separately
   from review wall time (`timing.vera_index_ms` vs `timing.total_ms`) and
   per-tool usage, with an investigator that finishes within budget and
   actually calls the Vera tools (E3's did not on the ripgrep case).
2. **Guidance** on cases where the investigator finishes within budget and
   the violation is invisible without guidance.
3. Deeper multi-hop cases.

Every accepted finding is read by a person and labelled TP / FP /
duplicate-of-TP before it counts; `uncertain` is reported in its own column.
