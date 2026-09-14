# Revera

Provider-independent PR reviewer. Deterministic Rust owns review state and
publication; configurable OpenAI-compatible models do the reasoning;
[Vera](https://github.com/VeraTools/Vera) supplies repository-aware retrieval.

## CLI

```sh
revera review --repo <path> --base <rev> [--head <rev>] --config revera.yaml [--profile deep] [--force]
revera doctor [--config revera.yaml]
revera cache-info [--repo <path>]
```

`review` prints a summary to stdout and writes a full JSON report to
`--out` (default `.revera/last-report.json`). The report includes a
`timing` object with per-phase wall-clock breakdowns (indexing, lanes,
validation, publish). Exit codes: 0 complete,
2 partial, 1 failed/config error. With `--event <path>` (a
`pull_request`/`pull_request_target` payload) Revera takes base/head/title
from the event and, with `--publish comment`, posts an inline review plus a
managed summary comment carrying its state blob.

## GitHub Action

```yaml
- uses: actions/checkout@v4
  with:
    fetch-depth: 0
    ref: ${{ github.event.pull_request.head.sha }}
- uses: VeraTools/revera@v0
  with: { config: revera.yaml }
  env:
    REVIEW_API_KEY: ${{ secrets.REVIEW_API_KEY }}
    OPENROUTER_API_KEY: ${{ secrets.OPENROUTER_API_KEY }}
```

`@v1` becomes available with the 1.0.0 release; pin `@v0.1.0` for an exact version.

The checkout must use the PR head SHA so reviewer tools read the exact PR
tree rather than GitHub's synthetic merge ref.

Workflow permissions: `pull-requests: write`, `contents: read`.

## Configuration

See `revera.example.yaml` and `docs/DESIGN.md`. Model roles
(`investigator`, `validator`, optional `lead`/`workers`/`scouts`) each take
a `protocol`, endpoint, and model; Vera API mode reads keys from the env
names given in config.

### Providers

Each route independently picks a protocol:

| protocol | wire format | base_url | reasoning |
|---|---|---|---|
| `openai-chat` | POST `{base}/chat/completions` | required (e.g. OpenRouter) | `reasoning_effort` or `reasoning` (openrouter) |
| `openai-responses` | POST `{base}/responses` | required | `reasoning.effort` + encrypted echo-back |
| `anthropic` | POST `{base}/v1/messages` | optional (default `https://api.anthropic.com`) | `thinking.budget_tokens` |
| `gemini` | POST `{base}/v1beta/models/{model}:generateContent` | optional (default `generativelanguage.googleapis.com`) | `thinkingConfig` (level/budget) |
| `scripted` | offline JSON script (tests) | — | ignored |

Every route defaults to `reasoning: medium`; set `reasoning: none` (or a
long form `{effort, budget_tokens, field}`) to tune or disable. Reasoning
items/thinking blocks/thoughtSignatures are echoed back verbatim across
turns via `provider_state`; a 400 mentioning the reasoning field drops it
and retries once on the same ledger slot.

All HTTP protocols share one transport (retries, Retry-After, request
budget, ledger) with a per-protocol wire adapter, so routes can mix — e.g.
an Anthropic validator with an OpenRouter investigator.

## Evaluation

`eval/` holds a small synthetic corpus plus scoring scripts (not wired into
CI; requires `OPENROUTER_API_KEY`):

```sh
cargo build
bash eval/build-corpus.sh            # 6 repos under eval/corpus/
bash eval/build-corpus-hard.sh       # +7 harder repos (regressions, multi-hop, clean traps)
bash eval/run.sh A-baseline crossfile 2   # one config x corpus x reps
bash eval/run-all.sh 2               # full matrix, <=3 lanes parallel
CORPORA="utf8-truncate trait-contract" CONFIGS="A-baseline F-delegated" bash eval/run-all.sh 2
python3 eval/summarize.py eval/results.jsonl [corpus1,corpus2,...]
```

Measured so far (muse-spark on every route; see docs/EVAL.md): all
configurations find 8/8 easy and 9/10 hard defects; baseline with Vera and
validation has zero false positives and the lowest cost, so it is the default.
Panel/delegated cost 1.8–2x with no recall gain here. Not measured: strong
validator or scout models, large repositories.

## Status

All three strategies (baseline / delegated / panel), GitHub event mode with
`--publish comment` reviewing the exact PR head, composite action + verified
release workflow, and a 13-repo eval harness in `eval/`. `v0.1.0` is released
(`@v0` resolves); see STATUS.md for limitations and docs/EVAL.md for results.
