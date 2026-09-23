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
| `scouts` | — | `panel` only: list of named routes (`name`, optional `focus`, plus route fields) |

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
| `reasoning` | `medium` | `none` · `minimal` · `low` · `medium` · `high` · `xhigh` · `max`, or the long form below |
| `extra_headers` | `{}` | header name → value (values may use `${VAR}`) |
| `session_header` | — | header that carries a stable per-run session id (set automatically for `opencode.ai` hosts) |
| `script` | — | `scripted` protocol only: path to a JSON transcript (tests, Action smoke) |

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

Prompt caching needs no configuration. On `anthropic` routes Revera marks two
cache breakpoints: the system block (caching the tools and system prompt every
validator and lane of a run shares) and the newest turn (so each step of an
agent loop reads the conversation the previous step wrote). OpenAI and Gemini
cache long prefixes automatically. Every protocol reports cache hits, which
the run report's ledger lists as `cached_tokens` (a subset of
`prompt_tokens`, per route and in total).

## `review`

| key | default | notes |
|---|---|---|
| `strategy` | `baseline` | `baseline` · `delegated` · `panel` (see [strategies.md](strategies.md)) |
| `publish` | `dry-run` | `dry-run` prints the plan; `comment` posts to the PR (the Action passes `comment`) |
| `validate` | `true` | evaluation-only switch; `false` publishes candidates unvalidated |
| `min_severity` | `low` | `low` · `medium` · `high`; findings below are dropped |
| `max_findings` | `10` | accepted findings kept per run |
| `publish_uncertain` | `false` | list `uncertain` verdicts in the summary as unconfirmed |
| `concurrency` | `4` | concurrent lanes / validations |
| `max_tool_output_bytes` | `12000` | truncation cap on each tool result |
| `max_diff_bytes` | `200000` | diffs larger than this are truncated per file with a note |
| `instruction_files` | `true` | add the team's instruction files to reviewer prompts: `AGENTS.md` and `CLAUDE.md` in the root and in every directory above a changed file, `REVIEW.md`, `.github/copilot-instructions.md`, and `.github/instructions/*.instructions.md` whose `applyTo` globs match a changed file (files with `excludeAgent: code-review` are skipped). They are read from the **base** revision, so a pull request cannot change the guidance its own review follows; capped at 16 KB |

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

Absent: disabled — repository search is lexical-only and no `vera` binary,
key or index is needed. Present: enabled unless `enabled: false`.

| key | default | notes |
|---|---|---|
| `enabled` | `true` when the block is present | |
| `executable` | `vera` | binary name or path |
| `version` | — | expected Vera version; mismatch is reported by `doctor` |
| `backend` | `local` | `local` (on-device embeddings) or `api` |
| `embedding` | — | `api` backend: `{base_url, model, api_key_env}` |
| `reranker` | — | optional `{base_url, model, api_key_env}` |
| `exclude` | `[]` | glob patterns excluded from indexing and lexical tools |

```yaml
vera:
  backend: api
  embedding:
    base_url: https://openrouter.ai/api/v1
    model: qwen/qwen3-embedding-8b
    api_key_env: OPENROUTER_API_KEY
  reranker:
    base_url: https://openrouter.ai/api/v1
    model: qwen/qwen3-reranker-8b
    api_key_env: OPENROUTER_API_KEY
```

`revera cache-key` prints a hash of the index-shaping settings (backend,
embedding model, excludes, Vera version) or `disabled`; the Action uses it
to cache `.vera` between runs.

## `github`

| key | default | notes |
|---|---|---|
| `token_env` | `GITHUB_TOKEN` | needs `pull-requests: write` for `publish: comment` |
| `summary_marker` | `<!-- revera-summary -->` | marker identifying the managed summary comment |
| `bot_login` | `github-actions[bot]` | author treated as Revera when the token cannot name itself |
| `allow_forks` | `false` | publish on fork PRs (secrets are normally unavailable there; the run is reported `partial`) |
| `resolve_threads` | `true` | resolve Revera's own review thread when the recheck validator marks its finding resolved. A thread is left open when a human replied, when its root comment is not authored by Revera's identity or lacks the finding's marker, or when not all of its comments could be read; at most 50 per run. Failures are recorded in the report, never fatal |

## `delegated` and `panel`

| key | default | notes |
|---|---|---|
| `delegated.max_questions` | `4` | questions the lead may plan |
| `delegated.worker_max_tool_calls` | `12` | |
| `delegated.worker_max_seconds` | `120` | |
| `panel.focuses` | `[general, cross-file]` | focus labels assigned to scouts without an explicit `focus` |
| `panel.scout_max_tool_calls` | `15` | |
| `panel.lens_router` | off | optional TypeSafe lens selection, below |

### `panel.lens_router` (optional, TypeSafe)

Before the scouts start, one request to a [TypeSafe](https://docs.typesafe.ai)
System One model asks, for every lane, how likely the diff holds changes that
lane's focus covers (a `noul` question per lane, batched in one call). Lanes
below `min_probability` are not run; the most relevant lane always runs. The
router only chooses scouts: every candidate still goes through fresh-context
validation. Changes touching a security-sensitive path (see `triage`) bypass
the router and run every lane.

The router fails open: a missing key, HTTP error, timeout, malformed or
missing answer runs every lane, and the summary note says why. Skipped lanes
and their probabilities are listed in the summary note.

**Data boundary:** when enabled, up to `max_state_bytes` of the reviewed diff
is sent to `base_url`. Leave it unset for repositories whose code must not
reach that service.

| key | default | notes |
|---|---|---|
| `api_key_env` | required | *name* of the environment variable holding the TypeSafe key; required for `panel` runs, checked by `revera doctor` |
| `base_url` | `https://api.typesafe.ai/v1` | requests go to `{base_url}/systemone` |
| `model` | `jev-latest` | TypeSafe model id |
| `min_probability` | `0.2` | lanes below this relevance probability are skipped |
| `timeout_seconds` | `15` | per request, also bounded by the run deadline |
| `max_state_bytes` | `60000` | cap on the diff text sent |

## `triage`

Deterministic diff triage runs before any model call. Files it drops are
removed from the diff that reviewers, diff tools and static rules see (repository
tools can still read them on request), and each one is listed in the
summary under "not checked" with its reason. Triage settings are part of the
review identity, so changing them re-reviews an unchanged PR.

| key | default | notes |
|---|---|---|
| `filter_noise` | `true` | drop dependency lockfiles, minified bundles (`.min.js`, `.bundle.js`), JS/CSS source maps, files under `vendor/`, `node_modules/` or `third_party/`, and files whose first five lines carry a generated marker (`@generated`, `DO NOT EDIT`, `Code generated by`); paths containing `migration` are never treated as generated |
| `ignore_paths` | `[]` | extra globs never reviewed, applied even with `filter_noise: false` |
| `risk_tiers` | `false` | size the `panel` and `delegated` swarms by the reviewed change (below) |
| `sensitive_paths` | `[]` | extra globs that force the `full` tier |
| `trivial_max_lines` | `10` | added plus deleted reviewed lines at or under this are `trivial` |
| `lite_max_lines` | `100` | at or under this (and above `trivial_max_lines`) are `lite` |
| `lite_max_lanes` | `2` | panel lanes kept for a `lite` change, in configured order |

Credential files (`.npmrc`, `.netrc`, `.pypirc`, `.dockercfg`,
`.git-credentials`, SSH private keys and anything under `.ssh/`,
`.aws/credentials`, `.docker/config.json`, `.env*` except
`.example`/`.sample`/`.template`/`.dist`, and `*.pem`, `*.key`, `*.p12`,
`*.pfx`, `*.jks`, `*.keystore`) are always withheld from every model and from
the repository tools, whatever `filter_noise` says. The static rules still scan
them locally; any match is listed in the summary by line number, never by
content.

Files that do not fit in `review.max_diff_bytes` are listed in the summary as
well: their diff is not in the reviewers' prompt and they are reachable only
through the repository tools.

With `risk_tiers: true`, a change is `full` when it touches more than 20
reviewed files or any security-sensitive path (a path containing `auth`,
`crypto`, `security`, `secret`, `credential`, `password`, `permission`,
`oauth`, `jwt`, `session` or `sandbox`, anything under `.github/workflows/`, or
a `sensitive_paths` glob). A `trivial` change runs one investigator instead of
the panel or delegated swarm; a `lite` change runs at most `lite_max_lanes`
panel lanes. Fresh-context validation is unchanged in every tier. An explicit
`--strategy` on the command line (or the Action's `strategy` input) opts the
run out of tier downgrades. The summary note and `stats.risk_tier` in the run
report record the tier applied.

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
revera review --preview --base <rev> [--config <file>] [--strategy <s>]
revera doctor [--config <file>] [--profile <name>] [--strategy <s>] [--publish dry-run|comment]
revera cache-key [--config <file>] [--profile <name>]
revera cache-info [--repo <path>]
```

`--force` re-reviews even when the previous review of the same head, base
and effective configuration was `complete`.

`--preview` prints what a review would do without running it: the files it
would review, the files triage skips and why, files that would not fit in
`review.max_diff_bytes`, the strategy, risk tier and reviewer lanes. It makes
no model, Vera or GitHub call, needs no credentials and writes no state. It
works on local diffs only (not with `--event`).

## Action inputs

| input | default | notes |
|---|---|---|
| `config` | `revera.yaml` | |
| `profile` | — | |
| `strategy` | — | |
| `publish` | `comment` | `dry-run` to only print the plan |
| `fail-on` | `failed` | `failed` · `partial` · `never` |
| `revera-version` | pinned to the Action release | Revera binary to install |
| `vera-version` | `1.4.1` | Vera binary to install (skipped when Vera is disabled) |
| `github-token` | `${{ github.token }}` | |
| `cache` | `true` | cache the `.vera` index between runs |

Outputs: `status` (`complete` / `partial` / `failed`), `findings` (accepted
count), `report-path`.
