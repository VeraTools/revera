# Status

`v0.2.0` is released: static `x86_64-unknown-linux-musl` asset, `v0` tag,
`dogfood-released.yml` exercises the published Action on ubuntu-latest and
ubuntu-22.04. This document tracks what the code on `main` does, what is
known to be missing, and what to do next.

## What works

### Review flow
- `revera review --base <rev> [--head <rev>]` (local) and `--event <path>`
  (GitHub `pull_request`/`pull_request_target` payload): merge-base diff
  (two-dot fallback on shallow clones), exact-head enforcement (refuses on
  HEAD mismatch or dirty tracked files; `--event` checks out
  `pull_request.head.sha` detached first).
- Investigator → fresh-context validator per candidate → deterministic diff
  anchoring → state reconciliation → publication. `baseline` is the default;
  `delegated` (lead/workers) and `panel` (scout lanes) are advanced and
  experimental.
- Truthful terminal handling: the investigator's `submit_findings` call is
  schema-checked. `findings: []` is a valid clean result; `{}`, a
  non-array `findings`, or invalid entries get exactly one repair round
  (when time remains); still-invalid output makes the run `partial` and any
  valid findings from a mixed list are kept and validated.
- Outcomes: exit `0 complete`, `2 partial`, `1` failed/config error; the
  JSON report, stdout summary, Action outputs and GitHub summary all derive
  from the same `RunStatus`. A partial run with zero findings says so
  explicitly ("absence of findings is not evidence the change is clean").

### State, identity, reuse
- `.revera/state.json` is versioned (`STATE_VERSION = 2`), bounded (200
  findings, pruned oldest-resolved first) and records the outcome of the
  last run per review key.
- Review identity = base sha + exact head tree id + patch id (informational)
  + fingerprint of review-affecting config (strategy, thresholds, limits,
  budgets, model routes without secrets, prompt content hash, engine version,
  Vera index identity). Reuse requires a prior `complete` run under the same
  key with no unposted open findings; partial/failed runs are never reused.
- Corrupt state → quarantined to `state.json.corrupt`, fresh review. Legacy
  (pre-v2) state → fresh review, publication ids retained (no re-posting).
- Finding lifecycle: open → resolved when it stops being reproduced,
  reopened (and re-published) when it comes back; canonical finding detail
  (file/line/title/severity/claim/trigger/impact/evidence refs) is retained.
- In event mode the state blob lives in the managed summary comment; that
  comment is selected by marker + decodable state + ownership (recorded id,
  authenticated viewer or known bot), never by marker text alone.
  Inline and summary publication are reconciled separately.

### Runtime and retrieval
- `budget.run_max_seconds` bounds every provider request, retry, tool batch
  and Vera subprocess by the remaining deadline, with a reserve for
  validation and report generation. Timed-out Vera children are killed and
  reaped.
- `vera.enabled: false` runs lexical-only with no Vera binary, key or index.
  Vera failures (missing binary, missing key, index/API error, timeout)
  degrade to lexical-only, mark the run partial with "semantic retrieval
  unavailable", and are visible in `stats.retrieval` and the summary.
- Lexical tools (`grep_repo`, `find_files`, `read_file`) operate on tracked
  files of the exact head, honour excludes, skip `.git`/`.vera`/`.revera`,
  return numbered lines, and are bounded in output and count. No shell
  execution, no second semantic index.
- Vera cache: compatibility (backend + embedding model) and health are
  checked before reuse; cache metadata is rewritten only after a successful
  index/update; the Action saves the cache only when that happened.
  `revera cache-key` exposes the index identity (independent of
  investigator/validator settings) or `disabled`.

### Providers
- Shared `HttpTransport` (ledger reservation per attempt, 429/5xx + transient
  retry, Retry-After incl. HTTP-dates, capped backoff, deadline) with
  adapters for `openai-chat`, `openai-responses`, `anthropic`, `gemini`, and
  offline `scripted`. Per-route `reasoning` with 400-driven step-down.

### Action, CI, release
- `action.yml`: installs Vera and Revera into `$RUNNER_TEMP/bin` (checksum
  verified, absolute-path invocation, directory created first), computes the
  Vera cache key, restores/saves the index cache only when healthy, removes
  stale reports, captures the exit code and normalises it through
  `scripts/action-outcome.sh` (`fail-on`, truthful `status`/`report-path`/
  `findings` outputs, job summary).
- `ci.yml` offline job: version check, installer tests, outcome test, fmt,
  clippy, tests. `action-smoke.yml`: the composite Action itself on
  ubuntu-22.04 and ubuntu-24.04 with scripted models and Vera disabled,
  asserting complete / partial / failed and `fail-on`. Live jobs
  (`live-fixtures`, `self-review`, `dogfood-released`) are separate, run
  only when secrets exist, and annotate a skip when they don't — a skip is
  never shown as a pass. Publishing workflows use per-PR `concurrency`.
- Release: `scripts/check-versions.sh` keeps `action.yml` and Cargo in step;
  `scripts/install-revera.sh` is shared by the Action and the release verify
  step; tags `vX.Y.Z` move `vX`.

### Diagnostics
- `revera doctor` checks config, each enabled route (protocol, base URL,
  key env set and non-empty, script path for scripted routes), Vera only
  when enabled (binary, version, key env), the `.vera` index, the GitHub
  token when `publish: comment`, and the git repo. Never prints secret
  values; every failure has a `next:` line.
- Reports carry `stats`: candidates, accepted/rejected/uncertain,
  resolved/reopened, reuse, retrieval mode, files read, per-tool
  calls/errors/latency, malformed findings, repair used. No full transcripts
  or source dumps.

## Known limitations

- Eval evidence (docs/EVAL.md) is thin: 1–2 reps per cell, small synthetic
  repos, one model family on most routes; no validator-model comparison on
  frozen candidates yet. Treat model recommendations as provisional.
- Cold `vera index` of a 50k-LOC repo via the OpenRouter embedding backend
  took 22 min once (idle stall on one connection); warm `vera update` is
  ~5 s, so the Action cache path matters. Runs that hit `run_max_seconds`
  during indexing degrade to lexical-only and are reported partial.
- `fixtures/run-fixture.sh` needs live OpenRouter embeddings (real Vera
  index); the offline equivalent is `action-smoke.yml` + `cargo test`.
- Fork PRs are skipped in `comment` mode (no secrets); there is no
  `pull_request_target` recipe yet because it would run untrusted code with
  secrets.
- Linux x86_64 only.

## Next

1. Validator comparison on frozen candidates/evidence (docs/EVAL.md plan),
   and Vera-on vs lexical-only under equal budgets on the hard corpus.
2. Presentation fixtures rendered from real runs for every outcome (see
   `tests/finish_plan_tests.rs::summary_presentation_*` for the asserted
   text today).
3. Tighten panel collapse/validation before recommending any multi-lane
   strategy.

## Exact test-demo commands

```sh
# offline
cargo test && scripts/test-install-revera.sh && scripts/test-install-vera.sh && scripts/test-action-outcome.sh
# live fixtures (real Vera index; scripted models)
OPENROUTER_API_KEY=... VERA_HOME=$HOME/.vera-revera fixtures/run-fixture.sh
# live review
OPENROUTER_API_KEY=... RELAY_FAST_API_KEY=... OPENCODE_GO_API_KEY=... \
  revera review --repo <repo> --base <base> --head <head> --config revera.yaml --publish dry-run
```

## Expensive-to-reverse decisions

- Vera is an external pinned executable, not linked.
- `finding_id = sha256(file + defect_key)`; the state format (v2) and the
  `<!-- revera-state:base64 -->` blob in the summary comment are durable.
- Model output contract is the `submit_findings`/`submit_verdict` tool-call
  schema; `prompts/` is the interface contract and its hash is part of the
  review identity.
- `posted` is set only after a successful surface, never before.
- Default routes: Muse Spark 1.3 Contributor via OpenCode Go for the
  investigator, relay.fast for other models, OpenRouter for Vera
  embeddings/reranker (see `revera.yaml`).
