# Configuration reference

Revera reads `revera.yaml` from the working directory, unless you pass
`--config <path>` or the Action's `config` input. The GitHub App dashboard
writes its server default to `/data/revera.yaml`. Unknown keys are errors.
`${VAR}` in string values expands from the environment; missing or empty
referenced variables are configuration errors.

Installed App reviews use the repository's base-commit `revera.yaml` when
present, otherwise the server default; they do not merge the two. External
public PR reviews always use the server file and disable repository guidance.
Owner approvals, [external review limits](server.md#external-settings-api) and
[fix execution policy](server.md#fix-settings-and-limits) are server preferences
in `settings.json`, not YAML review settings.

Setup guides: [GitHub App](server.md), [Action](action.md), [local CLI](cli.md).

## Reviewer playbook

Every review embeds `playbook/reviewer.md` and twelve defect-class skills:
`logic`, `contracts`, `state`, `errors`, `concurrency`, `security`, `filesystem`,
`subprocess`, `resources`, `config`, `tests` and `docs`. This playbook is separate
from repository guidance, which remains off by default.

Use `revera review --playbook-dir <DIR>` to load trusted operator overrides.
The directory may contain `reviewer.md` and `skills/<name>.md`. A built-in name
replaces that class; a new name adds one. In event mode the directory must be
outside the checkout after resolving both paths, including for `--private`
reviews. Operator-chosen symlinks in the root path are allowed. Private reviews
may use operator playbooks even though repository guidance is disabled.

Each skill has front matter followed by Markdown:

```markdown
---
name: logic
description: Local logic and boundary checks.
enabled: true
---
Check concrete inputs at each changed boundary.
```

`name` must equal the file stem and match `[a-z0-9][a-z0-9-]{0,39}`.
`description` is required, one line, at most 300 characters. `enabled` defaults
to `true`; `false` disables a class. Unknown front matter keys are errors.
Limits are 16 KiB for the reviewer, 8 KiB per skill, 64 KiB of effective
enabled content and 24 skills including disabled entries. Symlinks inside the directory,
non-regular files, invalid UTF-8 and unexpected entries are rejected; hidden
non-Markdown temporary files are ignored. Effective content changes invalidate
the review fingerprint but keep the Vera index warm.

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
| `fixer` | inherits `investigator` | App fixes only: generates edits in a sandbox |
| `fix_verifier` | inherits effective `validator` | App fixes only: independently checks the patched code, read-only |
| `lead` | `investigator` | `delegated` only: plans questions and synthesises worker reports |
| `workers` | `[investigator]` | `delegated` only: list of routes answering the lead's questions |
| `scouts` | — | `panel` only: list of named routes (`name` plus route fields) |

Omitting `validator` does not skip validation: the investigator's route is
called again with an empty conversation for every candidate. Set an
explicit validator to use a different model (a stronger one, or one from a
different provider family).

Fixes use the **server's** model configuration, never repository YAML.
`fix_verifier` inherits `validator`, or `investigator` when neither is set;
it does not inherit `fixer`. Inheritance includes the route's fallbacks;
an explicit route uses only its own. Both accept the same route fields below
and require HTTP protocols for sandbox execution. Configure them in
**Settings → Fixes → Fix model routes** or under `models` in the server YAML.
Naming a route does not enable execution or authorize publication; see
[Fixes (opt-in)](server.md#fixes-opt-in).

### Route fields

| key | default | notes |
|---|---|---|
| `protocol` | required | `openai-chat` · `openai-responses` · `anthropic` · `gemini` · `scripted` |
| `model` | required | provider model id |
| `base_url` | protocol default | required for `openai-chat` / `openai-responses`; `anthropic` and `gemini` default to the official endpoints |
| `api_key_env` | required for HTTP protocols | *name* of the environment variable holding the key; the value is never logged or fingerprinted |
| `max_output_tokens` | `16000` | |
| `timeout_secs` | `300` | per-request HTTP timeout in seconds; range `10`–`3600`; agent and run deadlines still apply |
| `temperature` | `0.2` | |
| `cache` | `true` | send explicit prompt-cache hints where the provider supports them |
| `reasoning` | `medium` | `none` · `minimal` · `low` · `medium` · `high` · `xhigh` · `max`, or the long form below |
| `extra_headers` | `{}` | header name → value (values may use `${VAR}`) |
| `session_header` | — | header that carries a stable per-run session id |
| `script` | — | `scripted` protocol only: path to a JSON transcript (tests, Action smoke) |
| `fallback` | `[]` | ordered alternative routes; entries cannot contain `fallback` |

Each fallback has its own `timeout_secs`; an inherited validator uses the
investigator's timeout along with the rest of its route. Providers or proxies
that cut silent non-streaming responses (for example after about 100–125 seconds)
need keepalives on the proxy side for longer request timeouts to help.

Reasoning long form:

```yaml
reasoning:
  effort: high
  budget_tokens: 16384      # optional; selects budget thinking on Anthropic, overrides the effort→budget mapping
  field: auto               # openai-chat only; detect the provider's reasoning format
```

Effort maps to a thinking budget where the provider takes one: minimal 1024,
low 2048, medium 8192, high 16384, xhigh 32768, max 65536. Anthropic uses
adaptive thinking by default, sending the effort directly (`minimal` maps to
`low`). An explicit `budget_tokens` selects budget thinking instead. If the
model answers HTTP 400 saying it does not support the selected thinking mode,
Revera switches once to the other mode and remembers it for later calls on
the same client; other 400s about thinking follow the fallback below. In
adaptive mode, `max_output_tokens` caps thinking plus the answer; Revera does
not raise it to accommodate a separate thinking budget.

If a provider rejects the reasoning field or effort with HTTP 400, the route
first steps `xhigh`/`max` down to `high`, then drops reasoning; the ledger
records requested vs effective effort per route.

Protocol wire formats:

| protocol | request | reasoning |
|---|---|---|
| `openai-chat` | `POST {base_url}/chat/completions` | `reasoning_effort` or `reasoning: {effort}` |
| `openai-responses` | `POST {base_url}/responses` | `reasoning.effort`; encrypted reasoning items echoed back |
| `anthropic` | `POST {base_url}/v1/messages` (default `https://api.anthropic.com`) | `thinking: {type: adaptive}` + `output_config.effort` by default; `thinking.budget_tokens` for explicit budgets or models without adaptive thinking |
| `gemini` | `POST {base_url}/v1beta/models/{model}:generateContent` | `thinkingConfig` |

All HTTP protocols share one transport: retries with `Retry-After`, the run
request budget, the run deadline and the ledger. Routes can mix protocols.

### Fallback models

Add `fallback` to a model route to try alternatives in order. Each entry
uses the same route fields and may use a different provider or protocol:

```yaml
models:
  investigator:
    protocol: openai-responses
    base_url: https://api.openai.com/v1
    api_key_env: OPENAI_API_KEY
    model: your-openai-model
    fallback:
      - protocol: anthropic
        api_key_env: ANTHROPIC_API_KEY
        model: your-anthropic-model
```

Revera tries the next available route after the current route exhausts its
retries and request adjustments. Fallback applies to HTTP 429, 5xx, 401, 403
and 404; timeouts, connection and transport errors; and empty or malformed
provider responses. Other HTTP errors, exhausted budgets or deadlines,
cancellation and agent submission failures do not trigger fallback.

A missing or empty key makes that route unavailable. The primary key may
be missing if an alternative has a key. `revera doctor` lists key presence
for each route; logs and the ledger record unavailable routes. `${VAR}`
expansion still requires every variable referenced in a string value.

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
| `strategy` | `baseline` | `baseline` · `delegated` · `panel` (see [strategies](how-it-works.md#strategies)) |
| `publish` | `dry-run` | `dry-run` prints the plan; `comment` posts to the PR (the Action passes `comment`) |
| `validate` | `true` | evaluation-only switch; `false` reports candidates as unvalidated, never publishes or stores them, and is rejected with `publish: comment` |
| `min_severity` | `low` | `low` · `medium` · `high`; findings below are dropped |
| `inline_min_severity` | `medium` | accepted findings below this are listed in the summary instead of posted inline; `low` lifts the severity bar, though findings outside the diff and beyond `max_findings` still go to the summary |
| `max_findings` | `10` | maximum inline comments per run; remaining accepted findings go in the summary |
| `publish_uncertain` | `false` | list `uncertain` verdicts in the summary as unconfirmed |
| `concurrency` | `4` | concurrent lanes / validations |
| `max_tool_output_bytes` | `12000` | truncation cap on each tool result |
| `max_diff_bytes` | `200000` | cumulative diff budget; files that do not fit are omitted with a note and mark the review `partial` |
| `guidance` | `off` | `off` · `review` · `agents` · `skills`; see below |
| `guidance_max_bytes` | `16384` | cap on repository guidance text |
| `notes` | `""` | operator review focus/instructions; at most 8000 characters |

`guidance` gives investigators and fresh validators repository conventions read from
the **base commit** (never the PR head): the root directory and each
ancestor directory of a changed file are searched; `review` reads
`REVIEW.md`; `agents` reads the first existing file in each directory:
`REVIEW.md`, `AGENTS.md`, then `CLAUDE.md`. `skills` adds an index of
`.agents/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md` and
`.opencode/skills/*/SKILL.md` (in that priority, deduplicated by name).
Matching skills can be read with `read_skill`, including relative reference
files, always from the base commit. The index is capped at 80 skills / 8 KiB;
individual skill reads are capped at 64 KiB and the tool output limit.
A guidance file that exists but cannot be used (over 256 KiB,
unreadable or not UTF-8) is recorded as `skipped` and its directory
contributes nothing; it never falls back to a lower-priority file. Sources, their
sha256 and truncation are recorded under
`stats.guidance` in the run report, along with listed/read skills and whether
notes were included; guidance and note text are not rendered into PR comments.
Operator `notes` apply independently of the guidance mode. CLI `--guidance`
overrides the mode; `--notes-file` appends notes (8000 characters total).
These settings affect the review fingerprint, not the Vera index identity.
It stays `off` by default; evaluation found no benefit so far
([evaluation](evaluation.md)). Guidance is text the models may
follow: in one evaluation run, a base `AGENTS.md` claiming a file was
externally audited and out of scope made it skip a real high-severity defect while the
review still reported `complete`. Enable it only for guidance you would
accept as review policy.

## `budget`

Hard limits. Breaching one ends the affected stage and marks the run
`partial` with the reason in the summary.

| key | default | notes |
|---|---|---|
| `agent_max_tool_calls` | `50` | per agent (investigator, validator, scout, worker) |
| `agent_max_seconds` | `900` | per agent wall clock |
| `run_max_requests` | `300` | model-provider HTTP attempts across the whole run, retries included; web evidence has separate shared limits |
| `run_max_seconds` | `1800` | run deadline; provider calls, tools and Vera subprocesses are bounded by the remaining time, with a reserve for validation and the report |
| `retries` | `3` | per request, honouring `Retry-After` |

The defaults allow longer investigations, including on larger repositories.
Earlier evaluations used smaller budgets; their timeouts do not describe the
current defaults. See the [large-repository results](evaluation.md#e3-current-engine-end-to-end)
before tuning a budget; more time does not guarantee completion.
In the App, **Settings → Budgets** previews the Quick and Deep limits produced
by these values. Custom profiles are labelled instead of showing built-in limits.

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
| `reranker` | — | optional `{base_url, model, api_key_env, protocol?, endpoint_path?, return_documents?, candidates?, max_doc_chars?}` |
| `exclude` | `[]` | glob patterns excluded from indexing and lexical tools |
| `home` | see below | Revera-owned Vera home (settings and local models) |

Reranker options: `protocol` is `generic` (`/rerank`, `top_n`) or `voyage`
(`top_k`); unset lets Vera infer it from the base URL. `endpoint_path` is
joined onto `base_url` (default `/rerank`). `return_documents` is `true` or
`false` (sent explicitly; default `false`) or `omit` (field left out for
providers that reject it). These options are rejected on `embedding`.
`candidates` (at least `1`, default `20`) and `max_doc_chars` (`0` is unlimited,
default `2000`) apply only to reranking; both defaults match Vera 2.3.1's, and
Revera sets them on every run (Vera 1.3.0 or later).

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

A new PR can restore its base branch's index but not another PR's. Add a
base-branch push trigger to refresh the shared starting index after merges.
Push, schedule and manual Action runs index only; they do not review or
publish. See the [workflow example](action.md#keep-the-index-warm).
The GitHub App updates the index during every review, and in mention mode warms
the default-branch index when a reviewable PR opens, reopens or becomes ready. Default-branch push
indexing is off by default; see [the server guide](server.md#review-lifecycle).

## `web_search`

External web search is **off by default** and independent of Vera. Enable
it when a correctness claim depends on an external API contract, an exact
dependency version, release notes or an advisory that is not in the checkout.
Repository tools remain the primary evidence source. The small
[paired evaluation](evaluation.md#opt-in-web-search) found external-fact bugs
with search enabled, but results are provisional and searches add time.

```yaml
web_search:
  enabled: false
  api_key_env: EXA_API_KEY       # optional; unset uses free hosted Exa
  free_first: false             # with a key, try free first then fail over to REST
  allow_private: false          # explicit trusted consent for private/unknown repos
  max_searches_per_review: 4
  max_reads_per_review: 4
```

Set `enabled: true` to expose the tools. `web_search(query)` returns up to
five untrusted excerpts with review-local IDs. `web_read(result_id)` reads
one selected result, up to 6000 characters; there is no arbitrary-URL tool.
Prompts require official sources, exact versions and independent verification
by the fresh validator. This is not an enforced read-before-accept gate:
one evaluation finding was accepted from search highlights without a page read.

Queries and selected URLs go to **Exa, a third party**. Public visibility must
come from event repository metadata; private or unknown repositories (including
local CLI reviews) expose no web tools unless trusted `allow_private: true` is
set. Query screening rejects recognizable secrets, source, URLs and encoded
blobs, but **cannot guarantee confidentiality**. Enable only when that outbound
data channel is acceptable. PR titles, bodies and pages cannot enable it.

With no key, HTTPS requests use Exa's free hosted endpoint. When the optional
key environment variable is set, requests use `api.exa.ai` with `x-api-key`.
`free_first: true` tries hosted Exa first and switches to REST for the rest of
the review on 429, 5xx, timeout or malformed responses; the free endpoint never
receives the key. Requests are capped at nine seconds and the remaining agent/
review deadline. Limits are 1–32 each; every outbound attempt, including failed
attempts and failover, consumes the shared search/read budget. With validation
enabled, investigators can consume at most half of each limit (rounded down).
Planning and synthesis cannot use web tools. Provider errors are explicit tool
errors, not empty success, and alone do not make an otherwise checked PR partial.

`revera doctor` reports the selected backend, optional-key readiness and privacy
boundary without making a network call. `stats.tools[].backends` records request
counts, errors and latency by `exa_free`/`exa_keyed`; reports store neither raw
queries nor retrieved pages. Nonsecret settings affect the review fingerprint,
never the Vera index. In the Action, merge the opt-in into the base config first;
an optional key can be passed as `EXA_API_KEY: ${{ secrets.EXA_API_KEY }}`.

## Event mode trust

With `--event` (the Action), a `--config` path inside the checkout is read
from the PR's base commit, never the PR head, so a pull request cannot
change the endpoints, executables or credential names its own review uses.
A path outside the checkout is read as-is (operator-provided). Before any
credential is resolved, event mode requires `https` endpoints (loopback
excepted), valid credential variable names that are not GitHub token
variables, and `review.validate: true` for `publish: comment`.

The server uses `--private` for external jobs. It requires `--event` and an
operator `--config` outside the checkout, forces dry-run/private publication,
disables repository guidance and makes no GitHub API calls in the review child.
It does not turn an arbitrary local checkout into a public-PR URL client; use
the App's [Review PR URL dialog](server.md#external-public-pr-reviews) for that.

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
| `delegated.worker_max_tool_calls` | `24` | |
| `delegated.worker_max_seconds` | `360` | |
| `panel.focuses` | `[general, cross-file]` | one focus label per scout, in order; a single scout runs once per label |
| `panel.scout_max_tool_calls` | `30` | |

A focus is `general` (the whole changed surface) or a comma-separated list
of enabled playbook skill names, for example `"logic,errors,state"`. Quote
comma-separated entries inside YAML flow lists. Custom classes are allowed;
unknown or disabled names in an explicit `panel.focuses` fail before any model
call and list valid names. Focuses Revera supplies itself (the default and
deep's lanes) skip disabled classes, and a lane left with none is dropped.
Legacy `cross-file` means `contracts`, and `data` means `logic`. Focused lanes
triage every changed file against their classes first, but may still report
other defects they notice. No scout routes means every lane uses the
investigator; a single route is repeated, and multiple routes must match the
number of focuses. Concurrency and the validation reserve still apply.

## `profiles`

Profiles override selected `review` and `budget` keys. Select one with
`--profile <name>` in the CLI or `profile` in the Action. The App's quick and
deep modes select the matching profile; standard mode applies no profile.
Profiles are applied before strategy resolution and credential checks.

`quick` and `deep` work without a `profiles` block. Their budgets scale from
your configured `budget` at the time the profile is applied. Validation stays
unchanged. Deep switches a baseline review to a panel; delegated and panel
strategies stay as configured. Unless `panel.focuses` is explicitly set, deep
uses five lanes: `general`, `logic,errors,state`,
`contracts,config,concurrency`, `filesystem,subprocess,security,resources`,
and `docs,tests`. Lanes without a scout route use the investigator route,
and at most `review.concurrency` lanes run at once.
When deep converts baseline to panel (including through `--strategy baseline`), each lane uses the effective
`budget.agent_max_tool_calls` unless `panel.scout_max_tool_calls` was explicitly
set. For example, a server budget of 50 tool calls gives each deep lane 50,
rather than the panel default of 30. Explicit panel limits always win.

| Setting | Built-in `quick` | Built-in `deep` |
|---|---|---|
| `budget.agent_max_tool_calls` | Half, rounded down; at least 1 | Unchanged |
| `budget.agent_max_seconds` | Half, rounded down; at least 30 | Unchanged |
| `budget.run_max_seconds` | Half, rounded down; at least 60 | Double, capped at 3000 seconds |
| `budget.run_max_requests` | Unchanged | Triple |
| `review.max_diff_bytes` | Unchanged | At least `400000`, keeping a higher configured value |
| `review.max_findings` | Unchanged | At least `20`, keeping a higher configured value |

With the default budget, quick allows 25 tool calls, 450 seconds per agent
and 900 seconds per run, with the request limit unchanged at 300. Deep allows
50 tool calls, 900 seconds per agent, 3000 seconds per run and 900 requests.
The 3000-second deep limit leaves room before server jobs stop at 55 minutes.
Retries and other review settings are unchanged.

A user profile named `quick` or `deep` replaces the built-in override, rather
than extending it. Keys it does not set keep the underlying configuration.
Effective profile settings are part of review identity. For a custom profile:

```yaml
profiles:
  custom-panel:
    review: { strategy: panel, max_findings: 20 }
    budget: { agent_max_tool_calls: 40, agent_max_seconds: 600, run_max_seconds: 1500 }
```

`--strategy` on the command line overrides both the file and the profile;
the built-in deep profile still turns an effective baseline into a panel.

## Command line

The [CLI guide](cli.md) covers installation and the first local review.

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

See the [Action guide](action.md) for workflow setup, outputs, fork handling
and version pinning.

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
