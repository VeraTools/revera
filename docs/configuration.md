# Configuration reference

Revera reads `revera.yaml` from the working directory (or `--config <path>` /
the Action's `config` input). Unknown keys are errors. `${VAR}` in any string
value expands from the environment; a referenced variable that is missing or
empty is a configuration error.

The smallest valid file is one investigator route:

```yaml
models:
  investigator:
    protocol: openai-chat
    base_url: https://api.example.com/v1
    api_key_env: REVIEW_API_KEY
    model: your-model-id
```

Everything else below is optional. Credentials are only required for the
routes the effective strategy uses; unused `lead`, `workers` and `scouts`
routes are shape-checked but need no key. `revera doctor` reports exactly
what a run would need.

## `models`

| key | default | notes |
|---|---|---|
| `investigator` | required | route that reads the diff and proposes candidates |
| `validator` | inherits `investigator` | route that re-derives each candidate in a fresh context |
| `lead` | `investigator` | `delegated` only: plans questions and synthesises worker reports |
| `workers` | `[investigator]` | `delegated` only: list of routes answering the lead's questions |
| `scouts` | — | `panel` only: list of named routes (`name` plus route fields) |

Omitting `validator` does not skip validation: the investigator's route is
called again with an empty conversation for every candidate. Set an
explicit validator to use a different model (a stronger one, or one from a
different provider family).

### Route fields

| key | default | notes |
|---|---|---|
| `protocol` | required | `openai-chat` · `openai-responses` · `anthropic` · `gemini` · `scripted` |
| `model` | required | provider model id |
| `base_url` | protocol default | required for `openai-chat` / `openai-responses`; `anthropic` and `gemini` default to the official endpoints |
| `api_key_env` | required for HTTP protocols | *name* of the environment variable holding the key; the value is never logged or fingerprinted |
| `max_output_tokens` | `4000` | |
| `temperature` | `0.2` | |
| `cache` | `true` | send explicit prompt-cache hints where the provider supports them |
| `reasoning` | `medium` | `none` · `minimal` · `low` · `medium` · `high` · `xhigh` · `max`, or the long form below |
| `extra_headers` | `{}` | header name → value (values may use `${VAR}`) |
| `session_header` | — | header that carries a stable per-run session id (set automatically for `opencode.ai` hosts) |
| `script` | — | `scripted` protocol only: path to a JSON transcript (tests, Action smoke) |
| `fallback` | `[]` | ordered alternative routes; entries cannot contain `fallback` |

Reasoning long form:

```yaml
reasoning:
  effort: high
  budget_tokens: 16384      # optional; overrides the effort→budget mapping where budgets are the native knob
  field: auto               # openai-chat only: auto | openai (reasoning_effort) | openrouter (reasoning object)
```

Effort maps to a thinking budget where the provider takes one: minimal 1024,
low 2048, medium 8192, high 16384, xhigh 32768, max 65536. If a provider
rejects the reasoning field with HTTP 400, the route first steps `xhigh`/
`max` down to `high`, then drops reasoning; the ledger records requested vs
effective effort per route.

Protocol wire formats:

| protocol | request | reasoning |
|---|---|---|
| `openai-chat` | `POST {base_url}/chat/completions` | `reasoning_effort` or `reasoning: {effort}` |
| `openai-responses` | `POST {base_url}/responses` | `reasoning.effort`; encrypted reasoning items echoed back |
| `anthropic` | `POST {base_url}/v1/messages` (default `https://api.anthropic.com`) | `thinking.budget_tokens` |
| `gemini` | `POST {base_url}/v1beta/models/{model}:generateContent` | `thinkingConfig` |

All HTTP protocols share one transport: retries with `Retry-After`, the run
request budget, the run deadline and the ledger. Routes can mix protocols.

### Fallback models

Any role can configure ordered alternatives with the same route fields:

```yaml
models:
  investigator:
    protocol: openai-responses
    base_url: https://api.openai.com/v1
    api_key_env: OPENAI_API_KEY
    model: gpt-5.2
    fallback:
      - protocol: anthropic
        api_key_env: ANTHROPIC_API_KEY
        model: claude-sonnet-4-5
```

After a route exhausts its own retries and request-reshaping steps, Revera
tries the next available route on HTTP 429 or 5xx, timeout/connect/transport
errors, HTTP 401/403 or 404, or an empty/malformed provider response. Other
HTTP errors (including an unhandled 400), request-budget or deadline
exhaustion, cancellation, and agent tool/terminal-protocol failures do not
trigger fallback. Missing or empty key environments make only that route
unavailable; a missing primary key is allowed when an alternative has a key.
`revera doctor` lists each alternative's key presence; unavailable routes
are noted in logs and the run ledger.

The last successful route is sticky per role for the rest of the run,
including fresh validator conversations. Switching replays the complete
provider-neutral transcript without the previous route's opaque state.
Every HTTP attempt shares the run request budget and existing deadlines.
An inherited validator inherits the investigator's alternatives; an explicit
validator has only its own. Nested `fallback` is rejected. Fallback routes
affect the review fingerprint, not the Vera index identity. The ledger records
switches and per-route usage; the summary footer shows the served model chain.

### Prompt caching

Each model route has `cache: true` by default. Revera sends
`prompt_cache_key` on OpenAI-compatible chat and Responses requests, and
ephemeral `cache_control` hints on Anthropic requests. If a provider returns
HTTP 400 identifying an unsupported cache hint, Revera retries without that
hint. An endpoint that rejects the hint with an HTTP 400 that does not name it
needs `cache: false` on that route. Cache hints do not affect the review fingerprint, so changing `cache`
alone does not invalidate a completed review. Where providers report them,
cached-prompt and cache-write token usage is recorded in the run ledger,
both per route and in the totals.

## `review`

| key | default | notes |
|---|---|---|
| `strategy` | `baseline` | `baseline` · `delegated` · `panel` (see [strategies.md](strategies.md)) |
| `publish` | `dry-run` | `dry-run` prints the plan; `comment` posts to the PR (the Action passes `comment`) |
| `validate` | `true` | evaluation-only switch; `false` reports candidates as unvalidated, never publishes or stores them, and is rejected with `publish: comment` |
| `min_severity` | `low` | `low` · `medium` · `high`; findings below are dropped |
| `max_findings` | `10` | accepted findings kept per run |
| `publish_uncertain` | `false` | list `uncertain` verdicts in the summary as unconfirmed |
| `concurrency` | `4` | concurrent lanes / validations |
| `max_tool_output_bytes` | `12000` | truncation cap on each tool result |
| `max_diff_bytes` | `200000` | diffs larger than this are truncated per file with a note |
| `guidance` | `off` | `off` · `review` · `agents`; see below |
| `guidance_max_bytes` | `16384` | cap on guidance text given to the investigator |

`guidance` gives the investigator repository review conventions read from
the **base commit** (never the PR head): the root directory and each
ancestor directory of a changed file are searched; `review` reads
`REVIEW.md`, `agents` reads `REVIEW.md` or, when a directory has none,
`AGENTS.md`. A `REVIEW.md` that exists but cannot be used (over 256 KiB,
unreadable or not UTF-8) is recorded as `skipped` and its directory
contributes nothing; it never falls back to `AGENTS.md`. Sources, their
sha256 and truncation are recorded under
`stats.guidance` in the run report and are never rendered into PR comments.
The mode is part of the review fingerprint, not the Vera index identity.
It stays `off` by default; evaluation found no benefit so far
([evaluation](evaluation.md)). Guidance is text the investigator may
follow: in one evaluation run, a base `AGENTS.md` claiming a file was
externally audited and out of scope made it skip a real high-severity defect while the
review still reported `complete`. Enable it only for guidance you would
accept as review policy.

## `budget`

Hard limits. Breaching one ends the affected stage and marks the run
`partial` with the reason in the summary.

| key | default | notes |
|---|---|---|
| `agent_max_tool_calls` | `25` | per agent (investigator, validator, scout, worker) |
| `agent_max_seconds` | `300` | per agent wall clock |
| `run_max_requests` | `120` | HTTP attempts across the whole run, retries included |
| `run_max_seconds` | `900` | run deadline; provider calls, tools and Vera subprocesses are bounded by the remaining time, with a reserve for validation and the report |
| `retries` | `3` | per request, honouring `Retry-After` |

For repositories above ~20k LOC, `agent_max_seconds: 420`–`600` avoids
investigator time-outs (the 300 s default produced the only misses in our
large-repository runs).

## `vera`

Absent: disabled; repository search is lexical-only and no `vera` binary,
key or index is needed. Present: enabled unless `enabled: false`.

| key | default | notes |
|---|---|---|
| `enabled` | `true` when the block is present | |
| `executable` | `vera` | binary name or path |
| `version` | — | expected Vera version; mismatch is reported by `doctor` |
| `backend` | `local` | `local` (on-device embeddings) or `api` |
| `embedding` | — | `api` backend: `{base_url, model, api_key_env, max_concurrent_requests?, max_in_flight_inputs?, timeout_secs?}` |
| `reranker` | — | optional `{base_url, model, api_key_env, protocol?, endpoint_path?, return_documents?}` |
| `exclude` | `[]` | glob patterns excluded from indexing and lexical tools |
| `home` | see below | Revera-owned Vera home (settings and local models) |

Reranker options: `protocol` is `generic` (`/rerank`, `top_n`) or `voyage`
(`top_k`); unset lets Vera infer it from the base URL. `endpoint_path` is
joined onto `base_url` (default `/rerank`). `return_documents` is `true` or
`false` (sent explicitly; default `false`) or `omit` (field left out for
providers that reject it). These options are rejected on `embedding`.

Embedding options (`api` backend only; rejected on `reranker`):
`max_concurrent_requests` (default `8`), `max_in_flight_inputs` (default
`256`) and `timeout_secs` (default `120`) are positive integers applied with
`vera config set embedding.*`. The defaults are Vera 2's own, so a cold index
sends two 128-input requests at a time; Revera sets them explicitly so its
persistent Vera home never keeps values from an earlier run. They change
indexing speed only, so they are not part of the index identity or the
review fingerprint. Use Vera 2.0 or later: with Vera 1.x a single failed
embedding request discards the whole index, and the next run starts over.

Revera runs Vera with its own `VERA_HOME` (`vera.home`, else
`$REVERA_VERA_HOME`, else `$XDG_CACHE_HOME/revera/vera-home`, else
`~/.cache/revera/vera-home`), strips ambient
`VERA_*`/`EMBEDDING_MODEL_*`/`RERANKER_MODEL_*` variables from the child,
and applies the reranker with `vera config set retrieval.*`; with no
reranker configured, reranking is explicitly disabled. The report's
`retrieval` field distinguishes `vera`, `vera+rerank` and
`vera (rerank degraded)`, and counts searches where Vera fell back to
keyword-only or unreranked results, for example
`vera+rerank (keyword-only on 2 searches)`.

Credential-bearing paths (`.env*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, …)
are always excluded from indexing, lexical tools, `read_file` and the diff
shown to models; excluded changed files are listed as coverage gaps.

```yaml
vera:
  backend: api
  embedding:
    base_url: https://api.example.com/v1
    model: your-embedding-model
    api_key_env: EMBEDDING_API_KEY
  reranker:
    base_url: https://api.example.com/v1
    model: your-reranker-model
    api_key_env: RERANKER_API_KEY
```

`revera cache-key` prints a hash of the index-shaping settings (backend,
embedding origin and model, excludes, Vera version) or `disabled`; the
Action uses it to cache `.vera` between runs. Reranker, model routes,
prompts, budgets and credential variable names are deliberately absent, so
changing them reuses the warm index; they are part of the review
fingerprint instead.

### Warm index for new pull requests

GitHub lets a pull request restore caches saved by its own earlier runs or
by its base branch, never by other pull requests. With only a
`pull_request` trigger, every new PR therefore builds its index from
scratch, which on a large repository means minutes and a full round of
embedding requests. Add a push trigger for the base branch:

```yaml
on:
  pull_request:
  push:
    branches: [main]
```

On a push the Action does not review. It restores `main`'s previous index,
runs `revera index` on the pushed commit (only changed files are
re-embedded) and saves the result under `main`, where every new PR into
`main` finds it. The checkout step from the quickstart needs no change: on
a push, `github.event.pull_request.head.sha` is empty and it checks out the
pushed commit. Index runs need the embedding credentials and any variable
the config references; a failed index fails the step under the default
`fail-on: failed` and saves nothing. Busy repositories can use a
`schedule:` trigger instead of `push`; PRs then re-embed only what changed
since the last refresh. With Vera disabled an index run does nothing.

## Event mode trust

With `--event` (the Action), a `--config` path inside the checkout is read
from the PR's base commit, never the PR head, so a pull request cannot
change the endpoints, executables or credential names its own review uses.
A path outside the checkout is read as-is (operator-provided). Before any
credential is resolved, event mode requires `https` endpoints (loopback
excepted), valid credential variable names that are not GitHub token
variables, and `review.validate: true` for `publish: comment`.

Index runs (`revera index`, and the Action on push, schedule or manual
events) read the config from the checked-out commit: they have no pull
request, so the code being indexed is already on a branch the repository's
writers control. They apply the same credential-name and endpoint checks
before resolving any key, and never review or publish. `pull_request_target`
runs restore the base branch's index but never save one, because their
cache scope is the base branch's and their tree is the PR head.

## `github`

| key | default | notes |
|---|---|---|
| `token_env` | `GITHUB_TOKEN` | needs `pull-requests: write` for `publish: comment` |
| `summary_marker` | `<!-- revera-summary -->` | marker identifying the managed summary comment |
| `bot_login` | `github-actions[bot]` | author treated as Revera when the token cannot name itself |
| `allow_forks` | `false` | publish on fork PRs (secrets are normally unavailable there; the run is reported `partial`) |

## `delegated` and `panel`

| key | default | notes |
|---|---|---|
| `delegated.max_questions` | `4` | questions the lead may plan |
| `delegated.worker_max_tool_calls` | `12` | |
| `delegated.worker_max_seconds` | `120` | |
| `panel.focuses` | `[general, cross-file]` | one focus label per scout, in order; a single scout runs once per label |
| `panel.scout_max_tool_calls` | `15` | |

## `profiles`

Named overrides of `review` and `budget` keys, applied with `--profile <name>`
(CLI) or the `profile` input (Action) before the strategy is resolved and
credentials are checked:

```yaml
profiles:
  deep:
    review: { strategy: panel, max_findings: 20 }
    budget: { agent_max_tool_calls: 40, agent_max_seconds: 600, run_max_seconds: 1500 }
```

`--strategy` on the command line overrides both the file and the profile.

## Command line

```sh
revera review --repo <path> --base <rev> [--head <rev>] [--config <file>] [--profile <name>] [--strategy <s>] [--publish dry-run|comment] [--out <report.json>] [--force]
revera review --event "$GITHUB_EVENT_PATH" [--config <file>] --publish comment
revera doctor [--config <file>] [--profile <name>] [--strategy <s>] [--publish dry-run|comment]
revera index [--repo <path>] [--config <file>] [--profile <name>]
revera cache-key [--config <file>] [--profile <name>]
revera cache-info [--repo <path>]
```

`--force` re-reviews even when the previous review of the same head, base
and effective configuration was `complete`.

## Action inputs

| input | default | notes |
|---|---|---|
| `config` | `revera.yaml` | |
| `profile` | — | |
| `strategy` | — | |
| `publish` | `comment` | `dry-run` to only print the plan |
| `fail-on` | `failed` | `failed` · `partial` · `never` |
| `revera-version` | pinned to the Action release | Revera binary to install |
| `vera-version` | `2.1.4` | Vera binary to install (skipped when Vera is disabled) |
| `github-token` | `${{ github.token }}` | |
| `cache` | `true` | cache the `.vera` index between runs |

Outputs: `status` (`complete` / `partial` / `failed`), `findings` (accepted
count), `report-path`.

The event decides what the Action does: `pull_request` and
`pull_request_target` run a review; `push`, `schedule` and
`workflow_dispatch` only build or refresh the Vera index and cache it (see
[Warm index for new pull requests](#warm-index-for-new-pull-requests));
other events fail. Index runs report `complete` or `failed`, with no
findings and no report.
