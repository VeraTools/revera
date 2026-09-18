# Revera

Provider-independent PR reviewer. Deterministic Rust owns diffing, review
identity, state and publication; configurable OpenAI-compatible models do the
reasoning (an **investigator** finds candidates, a **fresh-context validator**
re-derives each one); [Vera](https://github.com/VeraTools/Vera) optionally
adds semantic repository retrieval on top of the built-in lexical search.

## Quickstart (GitHub Action)

1. Copy `revera.example.yaml` to `revera.yaml` in your repo root and set the
   two model routes (`models.investigator`, `models.validator`). Keys are
   read from the env names you put in `api_key_env`; values never appear in
   config, state or reports.
2. Add the workflow:

```yaml
name: revera
on: pull_request
permissions:
  pull-requests: write
  contents: read
concurrency:                      # one publisher per PR; a new push cancels the old run
  group: revera-${{ github.event.pull_request.number }}
  cancel-in-progress: true
jobs:
  review:
    runs-on: ubuntu-latest        # any ubuntu-22.04+ runner (static musl binary)
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
          ref: ${{ github.event.pull_request.head.sha }}   # exact PR tree, not the merge ref
      - uses: VeraTools/revera@v0
        with:
          config: revera.yaml
          publish: comment        # dry-run to only print the summary
          fail-on: failed         # failed | partial | never
        env:
          OPENROUTER_API_KEY: ${{ secrets.OPENROUTER_API_KEY }}    # Vera embeddings/reranker
          RELAY_FAST_API_KEY: ${{ secrets.RELAY_FAST_API_KEY }}    # validator (and other) routes
          OPENCODE_GO_API_KEY: ${{ secrets.OPENCODE_GO_API_KEY }}  # investigator route
```

3. Run `revera doctor --config revera.yaml` locally (or in a workflow step)
   to check config, key env vars, Vera and the GitHub token before the first
   review. It validates only what the config enables and prints a `next:`
   hint for every failure.

Pin `@v0.2.0` for an exact version; `@v0` follows the latest 0.x release.
Fork PRs are skipped (`github.allow_forks: false`) because secrets are not
available to them; the run is reported as `partial`, not as clean.

### Action outputs and exit codes

| revera exit | Action `status` | meaning |
|---|---|---|
| 0 | `complete` | every stage finished; `findings: 0` means "reviewed, nothing found" |
| 2 | `partial` | something was not checked (budget, provider, retrieval, malformed model output). **Zero findings here is not a clean verdict.** |
| other | `failed` | config/setup error or crash; `report-path` is empty and `findings` is `0` because no trustworthy report exists |

`fail-on` is applied to that normalised status. Stale reports from a previous
step are removed before each run, so a failed run can never expose an old
report as current. Every run also writes the summary to the job summary /
PR comment with the status on the first line.

## CLI

```sh
revera review --repo <path> --base <rev> [--head <rev>] --config revera.yaml [--profile deep] [--force]
revera review --event "$GITHUB_EVENT_PATH" --config revera.yaml --publish comment
revera doctor [--config revera.yaml]
revera cache-key [--config revera.yaml] [--profile <name>]   # Vera index identity, or "disabled"
revera cache-info [--repo <path>]
```

`review` prints the summary to stdout and writes the JSON report to `--out`
(default `.revera/last-report.json`). The report carries `status`, `reason`,
`findings`, `coverage_gaps`, `timing` (per-phase wall clock), `ledger`
(requests/tokens per route) and `stats` (candidates before/after validation,
resolved/reopened, retrieval mode, tool call counts/errors/latency, files
read, malformed-output repairs, reuse). Exit codes match the table above.

## How a review runs

1. Diff base…head (merge-base; two-dot fallback on shallow clones). The
   checked-out tree must be exactly `head`; dirty or mismatched trees are
   refused rather than reviewed approximately.
2. Retrieval: `vera update` on the exact head (cached between runs, only
   when the cache matches backend + embedding model and passed a health
   check). If Vera is disabled, missing or fails, the run continues with
   lexical tools only (`grep_repo`, `find_files`, `read_file`, tracked files,
   `.git`/`.vera`/`.revera` excluded) and the summary says so.
3. Investigator agent loop → candidate findings via `submit_findings`. A
   malformed submission gets exactly one repair round; still-malformed output
   makes the run `partial`, and valid findings in a mixed list are kept.
4. Fresh-context validator per candidate (accept / reject / uncertain).
5. Deterministic anchoring to diff lines, state reconciliation (open →
   resolved when a finding disappears, reopened when it reappears), and
   publication: inline comments for new accepted findings + one managed
   summary comment that carries the state blob. Inline and summary
   publication are reconciled separately so a summary failure never
   duplicates inline comments on retry.

State is versioned and bounded. A prior review is reused only when it was
`complete`, has no unposted open findings, and the exact head tree, base,
and review-affecting config/prompt/engine version all match; partial and
failed runs are always redone. Corrupt state is quarantined to
`state.json.corrupt`; state written by an older engine is re-reviewed but
keeps its publication ids so nothing is re-posted.

## Configuration

See `revera.example.yaml` (every key, commented) and `docs/DESIGN.md`.
Highlights:

- `review.strategy`: `baseline` (default, recommended). `delegated` and
  `panel` are **advanced/experimental** multi-lane strategies — they cost
  1.8–2× and did not improve recall in our evals (docs/EVAL.md).
- `budget.run_max_seconds` is a hard run deadline: provider requests, tool
  calls and Vera subprocesses are bounded by the remaining time, with a
  reserve kept for validation and the final report. `run_max_requests` counts
  every HTTP attempt including retries.
- `vera.enabled: false` runs lexical-only with no Vera binary, key or index
  required.
- `${VAR}` in string values expands from the environment; a referenced but
  missing/empty variable is a config error (`doctor` reports it).

### Providers

Each route independently picks a protocol:

| protocol | wire format | base_url | reasoning |
|---|---|---|---|
| `openai-chat` | POST `{base}/chat/completions` | required (e.g. OpenRouter, relay.fast) | `reasoning_effort` or `reasoning` (openrouter) |
| `openai-responses` | POST `{base}/responses` | required (e.g. OpenCode Go) | `reasoning.effort` + encrypted echo-back |
| `anthropic` | POST `{base}/v1/messages` | optional (default `https://api.anthropic.com`) | `thinking.budget_tokens` |
| `gemini` | POST `{base}/v1beta/models/{model}:generateContent` | optional | `thinkingConfig` (level/budget) |
| `scripted` | offline JSON script (tests, Action smoke) | — | ignored |

Every route defaults to `reasoning: medium`; set `reasoning: none` (or a
long form `{effort, budget_tokens, field}`) to tune or disable. Reasoning
items/thinking blocks are echoed back verbatim across turns. A 400 on the
reasoning field first steps `xhigh`/`max` down to `high`, then drops
reasoning, on the same ledger slot. All HTTP protocols share one transport
(retries, Retry-After, request budget, deadline, ledger), so routes can mix.

## Verification

Offline (no credentials; what CI and the `action-smoke` workflow run):

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
scripts/test-install-revera.sh && scripts/test-install-vera.sh && scripts/test-action-outcome.sh
```

`action-smoke.yml` runs the composite Action itself (`uses: ./`) on
ubuntu-22.04 and ubuntu-24.04 against scripted models with Vera disabled,
asserting the complete / partial / failed outcomes and `fail-on` behaviour.

Live (needs `OPENROUTER_API_KEY` for Vera embeddings): `fixtures/run-fixture.sh`
(break → fix → clean → delegated → panel with scripted models and a real
Vera index) and the `live-fixtures` CI job. Live jobs are **skipped**, not
passed, when secrets are unavailable (fork PRs); the skip is annotated on the
run.

## Evaluation

`eval/` holds a synthetic corpus plus scoring scripts (not in CI; requires
provider keys). See docs/EVAL.md for results and for what is and is not
measured; headline: baseline + Vera + validation has zero false positives
and the lowest cost on the corpus, so it is the default. Model-selection
evidence is 1–2 reps per cell and labelled provisional.

## Status

`v0.2.0` is released (static `x86_64-unknown-linux-musl` asset, `v0` tag).
See STATUS.md for what works, known limitations and next steps.
