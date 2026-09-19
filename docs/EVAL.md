# Revera model evaluation log

## How to read this document

Everything below is **historical evidence gathered while building Revera**,
not a benchmark. Read each section with its own "what this does and does not
show" notes and these global caveats:

- Cells are 1–2 reps on small synthetic repos; single-run differences of one
  finding are noise. No confidence intervals are reported because the sample
  sizes do not support them.
- Most cells used one model family (Muse Spark) on every route; the model
  screening section is the only cross-model comparison and it is 1–2 reps.
- "TP" is scored by `eval/score.py` against a hand-written truth file per
  corpus (file + defect key). Every accepted finding in the tables was also
  read by a human once; borderline ones are listed in the section text. There
  is no independent second adjudicator.
- The engine changed between sections (terminal handling, retrieval fallback,
  identity/reuse). Numbers from different sections are **not** comparable to
  each other and none of them were re-run on the current engine.
- Token and latency figures come from the run ledger and exclude Vera
  indexing unless a section says otherwise (`run-all.sh` warm-indexes first).

### What a fair evaluation of the current engine looks like

The next measurement should **not** be another broad investigator-model
tournament (the last one did not separate single models on small repos).
In order of value:

1. **Validator comparison on frozen candidates.** Record the investigator's
   candidate list and evidence once per corpus (the `stats.candidates` /
   `findings[]` in the run report), then run only the validator with
   different models against the *same* candidates. Score: accepted true
   defects, rejected false claims, incorrectly rejected true defects,
   uncertain verdicts, validator latency and tokens. This isolates the stage
   that decides false positives from investigator variance.
2. **Vera on vs lexical-only under equal budgets** (`A-baseline` vs
   `B-baseline-novera`, same `agent_max_tool_calls`/`agent_max_seconds`/
   `run_max_seconds`), reporting cold and warm indexing time separately from
   review wall time (`timing.vera_index_ms` vs `timing.total_ms`). The engine now
   degrades to lexical-only automatically, so this also measures what a user
   gets when Vera is unavailable.
3. Large repos (>20k LOC) only after 1–2; that is where recall separated.

Manual adjudication rule for all of the above: every accepted finding is
read by a person and labelled TP / FP / duplicate-of-TP before it counts; an
"uncertain" verdict is neither and is reported in its own column.

### Live evaluation status for the current engine

No live evaluation was run on the current engine (v0.2.x with checked
terminal handling, exact review identity, deadline-bounded runtime and
lexical fallback). The offline evidence for it is `cargo test`
(`tests/finish_plan_tests.rs` covers terminal parsing/repair, reuse
identity, lifecycle, owned summary comments, lexical fallback and the
summary presentation of every outcome), `fixtures/run-fixture.sh` (scripted
models, real Vera index) and `action-smoke.yml` (the composite Action with
scripted models, Vera disabled). Treat the model recommendations below as
provisional until item 1 above has been run.

## Model selection (2026-09-13, openrouter.ai/api/v1/models)

Cheap candidates (tools supported, prompt <= $0.30/M, completion <= $1.0/M):

| id | prompt $/M | completion $/M |
|---|---|---|
| deepseek/deepseek-v4-flash | 0.049 | 0.099 |
| qwen/qwen3.5-flash-02-23 | 0.065 | 0.260 |
| mistralai/mistral-small-3.2-24b-instruct | 0.075 | 0.200 |
| google/gemini-2.5-flash-lite | 0.100 | 0.400 |
| meta/muse-spark-1.3-contributor | 0.10 | 0.20 |

Strong candidates (in brief preference order): anthropic/claude-sonnet-4.6
($3.00/$15.00), openai/gpt-5.4 ($2.50/$15.00), google/gemini-2.5-pro
($1.25/$10.00).

**Selected (user directive): `meta/muse-spark-1.3-contributor` for ALL routes**
(investigator, validator, and later scouts/workers). Verified: id present in
/api/v1/models, `"tools"` in supported_parameters, $0.10/M prompt, $0.20/M
completion.

## Live probe — crossfile fixture (dry-run)

Config: `fixtures/configs/live.yaml` (Muse via OpenCode Go, glm-5.3-flash
validator via relay.fast, Vera retrieval via OpenRouter).

### muse-spark runs (authoritative)

| run | summary | investigator tools used | caller bug found | validator verdict | requests | prompt tok | compl tok | wall | est. cost |
|---|---|---|---|---|---|---|---|---|---|
| break | 1 finding: **[high]** src/checkout.rs:3 discount unit change breaks final_price (negative totals) | vera_references, vera_grep, read_file | yes | accepted | 8 | 20758 | 2858 | 24.1s | ~$0.0026 |
| fix | 0 findings | vera_references, read_file, search | n/a (not present) | n/a (no candidates) | 9 | 30459 | 3353 | 32.7s | ~$0.0037 |
| clean | 0 findings (docs-only) | read_file, references | n/a | n/a | 4 | 9240 | 820 | 8.7s | ~$0.0011 |

Notes:
- muse-spark emits well-formed OpenAI tool_calls; no adapter issues observed.
- On `break`, the finding targets `src/checkout.rs` which is NOT in the diff, so
  it correctly lands in "Findings outside the diff" (summary placement).

### Earlier runs against other models (superseded by the muse-only directive; kept for reference)

- qwen/qwen3.5-flash-02-23 (investigator) + claude-sonnet-4.6 (validator):
  initially found the bug but it was silently dropped (see adapter bugs below);
  after the fix, found + validated (8 requests, ~24s).
- deepseek/deepseek-v4-flash: correct tool calls, 19 requests over 3m19s, missed
  the caller bug (reasoned checkout was already correct — but see worktree
  caveat).
- google/gemini-2.5-flash-lite: first run returned content-part arrays which the
  parser read as `null` → NoTerminalCall (1 request); worked after the
  content-array fix.
- mistralai/mistral-small-3.2-24b-instruct: clean tool use, missed the bug.
- claude-sonnet-4.6 as investigator: also reported no findings.

IMPORTANT CAVEAT on the earlier "missed" verdicts: those runs were executed with
the fixture worktree checked out at `fix` while --head pointed at `break`, so
Vera's index and read_file reflected the FIXED source — the models literally
could not see the bug via tools. After `git checkout break` (worktree == head),
muse-spark found it on the first try. Lesson recorded: review runs must be done
with the PR head checked out (event mode does this naturally).

## Adapter bugs found and fixed during the probe

1. `openai_chat.rs` `parse_message`: `content` returned as a parts array
   (`[{"type":"text","text":...}]`, seen from Gemini) was read as null → agent
   loop saw empty non-tool reply → NoTerminalCall. Fixed: join text parts.
2. `submit_findings` schema declared findings items as opaque objects, so live
   models invented field names (`path: "file.rs:6"`, `description`) that failed
   `Finding` deserialization and were silently dropped — zero findings despite a
   correct terminal call. Fixed: full JSON schema for finding items
   (required: defect_key,severity,file,start_line,title,claim) plus tolerant
   parsing (`path:"file:line"` → file+start_line, `description` → `claim`) and a
   warn log for dropped findings.

## M4 strategy comparison — crossfile fixture `break` (live, muse-spark all routes)

| strategy | found bug | false findings | requests | prompt tok | completion tok | wall s | est. cost |
|---|---|---|---|---|---|---|---|
| baseline | yes (checkout.rs:3, outside-diff) | 0 | 8 | 20,758 | 2,858 | 24.1 | ~$0.0026 |
| delegated | yes (checkout.rs:4, outside-diff) | 0 | 10 | 31,100 | 5,710 | 66.5 | ~$0.0043 |
| panel | yes (checkout.rs:4, outside-diff) | 0 | 13 | 40,084 | 5,324 | 46.2 | ~$0.0051 |

Notes:
- All three strategies found the real defect with zero false findings; both new
  strategies anchored at the caller (checkout.rs, not in the diff) → "Findings
  outside the diff", same as baseline's muse run.
- Delegated: lead planned 2 questions, both workers answered, synthesis
  re-derived the finding (one worker candidate was dropped as malformed —
  missing `severity` — but the lead still produced the correct finding).
- Panel: `2 scouts, 2 raw candidates → 1 unique` (both scouts converged on the
  same defect; collapse merged them).
- Cost estimate: $0.10/M prompt + $0.20/M completion (muse-spark pricing).

## M5 eval corpus — 6 configs x 6 corpora x 2 reps (muse-spark all routes)

Corpus: `eval/corpus/` (built by `eval/build-corpus.sh`; defects in
crossfile/nullpath/offbyone/lockdrop, clean in clean-refactor/clean-docs).
Strong-model comparisons (package 09 items 4, 6): not run — single-model
constraint (all routes are `meta/muse-spark-1.3-contributor`).

| config | TP | FN | FP | clean-PR commented | incomplete | median wall s | mean req | mean tok | total cost |
|---|---|---|---|---|---|---|---|---|---|
| A-baseline | 8 | 0 | 0 | 0 | 0 | 19.2 | 6.2 | 17532 | $0.0235 |
| B-baseline-novera | 8 | 0 | 0 | 0 | 0 | 28.3 | 5.8 | 14421 | $0.0201 |
| C-baseline-norerank | 8 | 0 | 0 | 0 | 0 | 16.9 | 6.0 | 16761 | $0.0223 |
| D-candidate-only | 8 | 0 | 0 | 0 | 0 | 11.4 | 3.5 | 10031 | $0.0133 |
| E-panel-2scouts | 8 | 0 | 2 | 0 | 0 | 19.6 | 10.9 | 32421 | $0.0432 |
| F-delegated | 8 | 0 | 0 | 0 | 0 | 45.8 | 9.2 | 26286 | $0.0372 |

All 72 runs completed; none failed. muse-spark detected every defect under
every configuration — including without retrieval (B) and without the
validation pass (D), and produced zero comments on the two clean corpora.
Panel produced the only false positives (2) and delegated was slowest
(~46s median). Raw results: `eval/results.jsonl`.

### Decision

Provisional default: **`baseline` with Vera retrieval and fresh validation on**
(config A), fast profile budgets.

Reasoning and caveats:

- The corpus is saturated: every configuration found all 8 defects, so this
  run cannot rank configurations on recall. It *can* rank them on cost,
  latency and false positives, and on those baseline dominates: panel costs
  1.8x and produced the only false positives; delegated costs 1.6x and takes
  2.4x longer with no quality gain. Neither becomes a default. They stay
  available under the `deep` profile / `--strategy` for hard PRs.
- Validation (A vs D) showed no FP difference here because the investigator
  produced no junk; the fixture suite shows validation rejecting junk when
  it does occur (panel scripted fixture) and it is the only defence before
  publication, so it stays on. `review.validate: false` remains eval-only.
- Vera (A vs B) did not change recall on these single-hop defects but
  reduced wall time (19s vs 28s median), consistent with the model reaching
  the caller via retrieval instead of `read_file` exploration (not verified
  per-transcript). Retrieval
  stays on; a corpus with multi-hop, larger-repo defects is needed to measure
  its recall effect and is the first follow-up.
- Reranker (A vs C) is within noise at this scale (2 reps); keep it
  configurable, default on when the user supplies a reranker route.
- Strong-vs-cheap portfolio comparisons (package items 4 and 6) are not run
  under the single-model constraint; re-run `eval/run-all.sh` with a strong
  validator route when one is available.

## Hard corpus — 5 configs x 7 corpora x 2 reps (muse-spark all routes, 2026-09-13)

Motivation: the 6-repo corpus above is saturated (every config found every
defect), so it cannot separate configurations on recall. `eval/build-corpus-hard.sh`
adds seven repos where the evidence for the bug is outside the diff:

| corpus | kind | defect (all in files the diff does not explain) |
|---|---|---|
| utf8-truncate | historical Revera regression | `&s[..max]` byte slice on model-visible text; caller doc says non-ASCII is expected |
| modzero-routing | historical (empty worker list) | `routes[i % routes.len()]` after `workers` became a defaulted `Vec` |
| retry-after | historical (Retry-After parse) | `parse::<u64>().unwrap()` while http.rs documents HTTP-date values |
| posted-state | historical, multi-hop | new same-patch short-circuit depends on `all_posted()`, but publish.rs never marks summary-only findings |
| trait-contract | multi-hop Vera case | impl returns `Ok(empty)` on miss; trait doc + `Cache::get_or_fill` rely on `Err(NotFound)` |
| clean-signature | clean control + FP trap | signature change with all callers updated; `unwrap()` guarded by `validate()?` |
| clean-dead-helper | clean control + FP trap | deletes unused `legacy::format_row`; a same-name `render::format_row` remains used |

Configs: A-baseline, B-baseline-novera, E-panel-2scouts, F-delegated,
G-baseline-reasoning-high (same model, `reasoning: high` on investigator and
validator). All routes are `meta/muse-spark-1.3-contributor`; strong-model
validator/scout comparisons were **not run** (user directive: muse-spark only).

| config | TP | TP high/crit | FN | FP | clean-PR commented | rejected | uncertain | incomplete | median wall s | mean req | mean tok | total cost |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A-baseline | 9 | 9 | 1 | 0 | 0 | 0 | 0 | 0 | 33.1 | 6.9 | 24014 | $0.0388 |
| B-baseline-novera | 9 | 9 | 1 | 0 | 0 | 0 | 0 | 0 | 27.2 | 5.9 | 17045 | $0.0289 |
| E-panel-2scouts | 9 | 9 | 1 | 2 | 0 | 1 | 0 | 2 | 31.0 | 12.1 | 42628 | $0.0685 |
| F-delegated | 9 | 8 | 1 | 1 | 0 | 1 | 0 | 0 | 43.4 | 14.1 | 46879 | $0.0762 |
| G-baseline-reasoning-high | 9 | 8 | 1 | 0 | 0 | 0 | 0 | 1 | 26.8 | 6.7 | 23443 | $0.0376 |

Per-corpus: utf8-truncate, modzero-routing, retry-after and trait-contract were
found 2/2 by every config; both clean controls drew zero comments from every
config (0 clean-PR comments in 70 runs); posted-state was found 1/2 by every
config. Incomplete runs (status `partial`, findings still reported): E x
posted-state rep 2 (scout ProviderError), E x clean-signature rep 2 (scout
ToolBudget), G x posted-state rep 1 (investigator ProviderError).
Reproduce with `CORPORA="utf8-truncate modzero-routing retry-after posted-state
trait-contract clean-signature clean-dead-helper" CONFIGS="A-baseline
B-baseline-novera E-panel-2scouts F-delegated G-baseline-reasoning-high" bash
eval/run-all.sh 2`; per-run reports land in `eval/reports/` (gitignored).
`eval/test-run-all.sh` smoke-tests the run-all build/run plan via
`RUN_ALL_DRY_RUN=1` (no build, no API calls).

### What this does and does not show

- Recall still ties (9/10 everywhere), so the hard corpus separates configs on
  false positives, cost and robustness, not on recall. A and B (0 FP, cheapest)
  dominate E (2 FP, 1.8x cost, 2 partial runs) and F (1 FP, 2x cost, slowest).
  The provisional default (baseline + Vera + validation) stands.
- Vera on vs off (A vs B): no recall difference on these repos either; B was
  cheaper (17k vs 24k tokens) and faster. The repos are 3–6 files, so
  `read_file` exploration covers them without retrieval; this corpus is still
  too small to measure Vera's recall effect. A large-repo case remains the
  open follow-up.
- `reasoning: high` (G) was honoured by muse-spark (reasoning tokens > 0 in
  14/14 runs, 25.6k total) but changed nothing on recall/FP and had one
  provider error.
- The two panel/delegated FPs are not junk: one is a duplicate of the
  modzero-routing defect (collapse missed it), one is an additional valid
  observation on retry-after (server delay bypasses the backoff cap) that the
  truth file does not list. Validators rejected 2 other candidates (both
  posted-state), which is the first time this corpus shows validation doing
  work.
- posted-state scoring is loose: `score.py` matches on file/line or keywords,
  and several accepted findings there describe a neighbouring problem (the
  short-circuit ignoring a changed `Plan`) rather than the summary-only
  posting gap; only A rep 1 and E rep 2 name the actual mechanism. Treat
  posted-state TP counts as "flagged the right lines", not "explained the bug".
- Two reps per cell; single-model; no cost in real dollars beyond the
  $0.10/$0.20 per-M estimate. None of the numbers above are statistically
  strong; they are enough to say the corpus is no longer a trivial tie.

## Large-repo multi-hop corpus — ripgrep, 2 configs x 4 corpora x 2 reps (muse-spark, 2026-09-13)

Spec: `eval/large/cases.md`; truth lives outside the repos in
`eval/large/truth/`. Each case is a fresh clone of ripgrep (~50k LOC, 11
crates) at a pinned commit with one synthetic edit to a single file whose
defect only shows through untouched callers in other files/crates (LineStep
terminator contract, CRLF trimming in the printer, globset extension
strategies vs dotfiles) plus one behaviour-preserving control. Budgets were
equal for both configs (`run_max_seconds: 600`, `agent_max_seconds: 420`,
`agent_max_tool_calls: 40`); they differ only in `vera.enabled`.

| config | TP | TP high/crit | FN | FP | xf | clean-PR commented | median wall s | mean req | mean tok | total cost |
|---|---|---|---|---|---|---|---|---|---|---|
| LA-baseline-large (Vera on) | 5/6 | 4 | 1 | 0 | 5 | 0 | 138.5 | 17.8 | 244974 | $0.2074 |
| LB-baseline-large-novera | 4/6 | 2 | 2 | 0 | 4 | 0 | 118.5 | 16.5 | 208690 | $0.1775 |

`xf` = accepted true positives whose text cites the cross-file mechanism
(keywords per case). All 9 TPs did. Misses: LineStep rep 2 under both configs,
CRLF rep 2 without Vera. Control: 0 FP in all four runs.

Historical note: `xf` was previously scored against a pooled keyword set;
it is now per-defect. Reports are not retained (`eval/reports` is
gitignored), so pre-fix rows cannot be recomputed — but every large truth
has a single defect and the small corpora carry no `cross_file_keywords`,
so pooled == per-defect for all retained rows in `eval/results.jsonl`.

Indexing: a cold `vera index` of ripgrep through the OpenRouter embedding
backend took 22 min (229 files, 5,259 chunks); the index files stopped
growing after ~7 min and the process then sat ~15 min on one idle embedding
connection — a Vera api-backend stall worth reporting upstream. Relocating a
warm `.vera` and running `vera update` took 5–6 s per corpus, which is the
path the Action's prefix-restored cache takes.

### What this does and does not show

- First evidence in Vera's favour: +1 recall and higher severities with Vera
  on, at ~17% more wall time and ~17% more tokens. Two reps per cell on three
  defect cases is not statistically strong; label: **Vera on is the
  provisional default for large repos** (best measured, insufficient
  evidence for a strong claim).
- Without Vera the model still found cross-file evidence via `read_file` +
  `vera_grep`-free exploration in 4/6 runs; the corpus is multi-hop but the
  hops are short (one crate boundary). Deeper cases are the next step.
- Tool usage is not yet in the report (the ledger counts requests/tokens
  only); per-tool counters are a follow-up so search efficiency can be
  compared rather than inferred from token counts.
- Cold index cost on a real repo dominates first-run latency; the Action's
  cache restore + `vera update` is what makes this acceptable in CI.

## Model screening — 9 configs x 7 hard corpora x 2 reps + large subset (relay.fast / OpenCode Go, 2026-09-13)

Configs `eval/configs/M*.yaml` (generated by `gen-screening.py`). Every config
uses the same validator (`glm-5.3-flash @ high` via relay.fast) so only the
investigator/scout side varies. `reasoning: max` is sent unchanged to
every route; on a provider 400 the adapter steps down to `high` and only
then drops reasoning entirely. The ledger and summary footer record
per-role requested vs effective reasoning (`role=route:model@req->eff`). Muse Spark runs on OpenCode Go (`openai-responses`,
`x-opencode-session`). Cost column uses OpenRouter rate cards as a relative
proxy; relay pricing was not available.

Hard corpus, 5 defect corpora x 2 reps (10 possible TPs) + 2 clean controls:

| config | TP | FN | FP | clean FP | incomplete | median wall s | mean req | total tok |
|---|---|---|---|---|---|---|---|---|
| M1-glm (glm-5.3) | 10 | 0 | 1 | 0 | 0 | 77 | 7.1 | 248k |
| M2-flash (glm-5.3-flash) | 10 | 0 | 1 | 0 | 0 | 82 | 7.4 | 290k |
| M3-terra (gpt-5.6-terra @ max) | 10 | 0 | 1 | 1 | 0 | 74 | 6.8 | 257k |
| M4-hy4 | 9 | 1 | 2 | 0 | 0 | 82 | 8.0 | 343k |
| M5-muse-opencode | 10 | 0 | 0 | 0 | 0 | 41 | 7.3 | 322k |
| M6-panel-2xflash (2 identical lanes) | 9 | 1 | 5 | 1 | 0 | 71 | 12.1 | 462k |
| M7-panel-3het (glm+gemini+deepseek) | 10 | 0 | 10 | 0 | 1 | 149 | 26.5 | 1237k |
| M8-panel-3xflash | 10 | 0 | 8 | 0 | 0 | 115 | 17.9 | 678k |
| M9-panel-5het (+grok, flash) | 10 | 0 | 35 | 1 | 1 | 137 | 46.0 | 2119k |

Large subset (ripgrep corpora from `eval/large/`, 3 defects + 1 control,
1 rep, `agent_max_seconds: 300`):

| config | TP | FN | FP | xf | incomplete | median wall s | total tok |
|---|---|---|---|---|---|---|---|
| M1-glm | 2 | 1 | 0 | 2 | 1 (agent time budget) | 358 | 699k |
| M2-flash | 0 | 3 | 0 | 0 | 3 (2 provider errors, 1 time budget) | 429 | 762k |
| M3-terra | 1 | 2 | 2 | 1 | 1 | 179 | 1230k |
| M5-muse-opencode | 2 | 1 | 0 | 2 | 1 | 149 | 761k |

Provider behaviour seen in 142 runs: no 429s from relay.fast; two HTTP 500
`provider_non_sla` ("upstream stream response contained no choices") and one
transport error, all on the large corpora (flash, terra); HY4 completed every
run here (its earlier 524 on a hard probe did not recur). Panel partials were
single scouts ending without a terminal call.

### What this does and does not show

- **Recall does not separate single-model configs on small repos** — every
  single lane except HY4 scored 10/10. The hard corpus separates on false
  positives and cost only; large repos separate on recall.
- **Panels are not worth it as configured.** More lanes = more raw candidates
  = more accepted FPs (5, 8, 10, 35) at 1.5–8x the cost, with no recall gain;
  two identical cheap lanes (M6) were *worse* than one. The validator did not
  absorb the extra noise. Until the collapse/validation stage is stricter,
  `baseline` stays the default and `panel` is opt-in.
- **Cheapest model is not free on large repos.** glm-5.3-flash went 0/3 with
  3 incomplete runs on ripgrep; glm-5.3 and Muse Spark each found 2/3 with
  cross-file evidence; Terra was fast but produced the only clean-repo FPs.
- **Provisional defaults (best measured, not strong evidence):** single
  investigator = Muse Spark (OpenCode Go) or glm-5.3 (relay), validator =
  glm-5.3-flash @ high, Vera on, `reasoning: high`/`max`. For repos above
  ~20k LOC raise `agent_max_seconds` to 420–600: the 300 s cap produced the
  only M1/M5 misses.
- Two reps on small repos, one rep on large; single validator model; no
  measurement of validator choice. HY4 and Terra `max` were exercised but not
  characterised beyond the tables above.
- Scheduling: median single-lane wall is 41–82 s on small repos, with
  validation running after the lane. No config's incomplete runs were caused
  by validation waiting on lanes, so overlapping validation with lanes is not
  justified by these measurements; revisit if the large-repo lanes (150–430 s)
  become the default path.
