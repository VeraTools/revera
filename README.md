# Revera

Provider-agnostic AI code review for GitHub pull requests, with independent
validation and optional repository-aware retrieval.

[![ci](https://github.com/VeraTools/revera/actions/workflows/ci.yml/badge.svg)](https://github.com/VeraTools/revera/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Revera runs as a GitHub Action (or a local CLI). Deterministic Rust owns the
diff, review identity, state and publication; the models you configure do
the reasoning:

- **Bring your own model.** Any OpenAI-compatible chat endpoint, plus native
  Anthropic, Google Gemini and OpenAI Responses protocols. One route is
  enough to start.
- **Nothing is published unvalidated.** An *investigator* proposes candidate
  findings; a *fresh-context validator* re-derives each one from the code
  before it can appear on the PR.
- **Repository-aware.** Built-in lexical search over the exact PR head, and
  optional semantic search and reference lookup through
  [Vera](https://github.com/VeraTools/Vera).
- **Truthful status.** A run is `complete`, `partial` or `failed`; budget
  breaches and provider errors are reported, never presented as a clean
  review. Findings are tracked across pushes (resolved, reopened) without
  duplicate comments.

## Quickstart (GitHub Action)

1. Create `revera.yaml` in your repository root with one model route:

   ```yaml
   models:
     investigator:
       protocol: openai-chat
       base_url: https://api.example.com/v1
       api_key_env: REVIEW_API_KEY
       model: your-model-id
   ```

   That is a full configuration: baseline strategy, fresh validation using
   the same route, Vera off. [`revera.example.yaml`](revera.example.yaml)
   shows the optional validator and Vera blocks.

2. Add the API key as a repository secret (`REVIEW_API_KEY` here) and a
   workflow:

   ```yaml
   name: revera
   on: pull_request
   permissions:
     contents: read
     pull-requests: write
   jobs:
     review:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
           with:
             fetch-depth: 0
             ref: ${{ github.event.pull_request.head.sha }}
         - uses: VeraTools/revera@v0
           env:
             REVIEW_API_KEY: ${{ secrets.REVIEW_API_KEY }}
   ```

3. Open a pull request. Accepted findings are posted as inline comments plus
   one managed summary comment; later pushes update them instead of
   re-posting.

`@v0` follows the latest 0.x release; pin `@vX.Y.Z` for an exact version.
Check a configuration before the first run with
`revera doctor --config revera.yaml`; it validates only what the effective
configuration uses and prints a `next:` hint for every failure.

## How it works

```text
diff (base…head)  →  investigator  →  candidates  →  fresh validator  →  anchor / dedupe / publish
                        │ tools                          │ tools
                  lexical search (+ Vera)          lexical search (+ Vera)
```

1. Revera diffs base and head (the checked-out tree must be exactly the PR
   head) and, if Vera is enabled, refreshes the repository index.
2. The investigator explores the change with read-only tools and submits
   structured candidate findings.
3. Each candidate is handed to a validator that starts from an empty context
   and must re-establish the claim from the code. This is what makes the
   default safe with a single model: the validator inherits the
   investigator's route but never its conversation.
4. Rust anchors accepted findings to diff lines, reconciles them with the
   previous review (resolved / reopened / still open) and publishes.

Models never post to GitHub directly, run shell commands, or write files.
Details: [docs/how-it-works.md](docs/how-it-works.md).

## Configuration at a glance

| key | default | notes |
|---|---|---|
| `models.investigator` | required | protocol, `base_url`, `api_key_env`, `model`, optional `reasoning` |
| `models.validator` | investigator route | set to validate with a different (e.g. stronger) model |
| `review.strategy` | `baseline` | `delegated` / `panel` are advanced multi-lane strategies |
| `review.publish` | `dry-run` (the Action passes `comment`) | |
| `review.min_severity` | `low` | |
| `vera` | off | add a `vera:` block with an embedding endpoint to enable |
| `budget.*` | 25 tool calls / 300 s per agent, 120 requests / 900 s per run | hard limits; breaches make the run `partial` |

`${VAR}` in string values expands from the environment. Full reference:
[docs/configuration.md](docs/configuration.md).

### Providers

| protocol | endpoint | examples |
|---|---|---|
| `openai-chat` | `{base_url}/chat/completions` | OpenAI, OpenRouter, Z.ai, DeepSeek, most self-hosted servers |
| `openai-responses` | `{base_url}/responses` | OpenAI Responses API and compatible hosts |
| `anthropic` | `{base_url}/v1/messages` | Anthropic Claude |
| `gemini` | `{base_url}/v1beta/models/{model}:generateContent` | Google Gemini |

Routes can mix protocols and providers. Reasoning/thinking is on (`medium`)
by default for every route and degrades automatically on providers that
reject the field.

## Strategies

`baseline` (one investigator, one validator) is the default and the
recommended setting. `delegated` (lead plans questions, workers answer,
lead synthesises) and `panel` (independent scouts, findings unioned) are
available through `review.strategy` or the Action's `strategy` input. In
our evaluation they cost 1.5–2× more and did not improve recall; panels
produced more false positives. See [docs/strategies.md](docs/strategies.md).

## Evidence

Measured on synthetic corpora with hand-written truth files, 1–2 runs per
cell (small samples; treat as provisional):

- Baseline with validation produced **zero false positives** across 13
  small corpora (four clean-control PRs included) with the same model on
  both routes.
- On a ~50k LOC repository (ripgrep) with cross-crate defects, Vera
  retrieval found 5/6 defects vs 4/6 lexical-only at ~17 % more time.
- Across eight investigator models (Meta Muse Spark, Z.ai GLM, OpenAI GPT,
  Google Gemini, DeepSeek, xAI Grok and others), recall did not separate
  single-model configurations on small repositories; false positives and
  cost did.

Method, tables and caveats: [docs/evaluation.md](docs/evaluation.md).

## Supported environment and limitations

- Action: Linux x86_64 runners (`ubuntu-22.04`, `ubuntu-24.04`/`latest`).
  The binary is a static `x86_64-unknown-linux-musl` build.
- Fork PRs are skipped by default (secrets are unavailable to them) and
  reported as `partial`, not clean.
- Inline comments require the finding to sit on a diff line; other accepted
  findings appear in the summary under "Findings outside the diff".
- Vera indexing of a large repository is slow on first run; the Action caches
  the index and updates it incrementally.

### Action outputs and exit codes

| revera exit | Action `status` | meaning |
|---|---|---|
| 0 | `complete` | every stage finished; `findings: 0` means "reviewed, nothing found" |
| 2 | `partial` | something was not checked (budget, provider, retrieval, malformed model output) — zero findings is not a clean verdict |
| other | `failed` | config/setup error or crash; no trustworthy report |

`fail-on: failed | partial | never` (default `failed`) decides which of
these fail the step.

## CLI

```sh
revera review --repo . --base origin/main --config revera.yaml      # local dry-run
revera review --event "$GITHUB_EVENT_PATH" --publish comment        # inside Actions
revera doctor [--config revera.yaml] [--profile deep] [--strategy panel]
revera cache-key [--config revera.yaml]                              # Vera index identity, or "disabled"
```

Prebuilt binaries are attached to
[releases](https://github.com/VeraTools/revera/releases); or
`cargo install --git https://github.com/VeraTools/revera`.

## Documentation

- [How it works](docs/how-it-works.md) — architecture, boundaries, state and re-review
- [Configuration](docs/configuration.md) — every key with defaults
- [Strategies](docs/strategies.md) — baseline, delegated, panel
- [Evaluation](docs/evaluation.md) — what was measured and how to reproduce it
- [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## License

[MIT](LICENSE).
