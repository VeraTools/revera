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
`--out` (default `.revera/last-report.json`). Exit codes: 0 complete,
2 partial, 1 failed/config error. With `--event <path>` (a
`pull_request`/`pull_request_target` payload) Revera takes base/head/title
from the event and, with `--publish comment`, posts an inline review plus a
managed summary comment carrying its state blob.

## GitHub Action

```yaml
- uses: actions/checkout@v4
  with: { fetch-depth: 0 }
- uses: VeraTools/revera@v1
  with: { config: revera.yaml }
  env:
    REVIEW_API_KEY: ${{ secrets.REVIEW_API_KEY }}
    OPENROUTER_API_KEY: ${{ secrets.OPENROUTER_API_KEY }}
```

Workflow permissions: `pull-requests: write`, `contents: read`.

## Configuration

See `revera.example.yaml` and `docs/DESIGN.md`. Model roles
(`investigator`, `validator`, optional `lead`/`workers`/`scouts`) each take
a `protocol` (`openai-chat` or `scripted`), endpoint, and model; Vera API
mode reads keys from the env names given in config.

## Evaluation

`eval/` holds a small synthetic corpus plus scoring scripts (not wired into
CI; requires `OPENROUTER_API_KEY`):

```sh
cargo build
bash eval/build-corpus.sh            # 6 repos under eval/corpus/
bash eval/run.sh A-baseline crossfile 2   # one config x corpus x reps
bash eval/run-all.sh 2               # full matrix, <=3 lanes parallel
python3 eval/summarize.py eval/results.jsonl
```

## Status

M5: all three strategies (baseline / delegated / panel), GitHub event mode
with `--publish comment`, composite action + release workflow, and a 6-repo
eval harness in `eval/` (see docs/EVAL.md).
