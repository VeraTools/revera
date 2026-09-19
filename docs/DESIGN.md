# Revera design

Revera is a provider-independent GitHub PR reviewer. Deterministic Rust owns
review state and GitHub publication; configurable models do the reasoning;
[Vera](https://github.com/VeraTools/Vera) supplies repository-aware retrieval.

## Shape

```text
GitHub Action (composite) or local CLI
        |
        v
revera (single Rust crate)
  cli/        clap surface: review, doctor, cache-info
  config/     revera.yaml + env interpolation, model roles
  git/        base/head/merge-base, unified diff -> hunks -> reviewable lines
  vera/       external `vera` executable: version, index/update, search, references, grep, overview
  provider/   ModelClient trait; `openai-chat` (tool calling) and `scripted` (offline fixtures)
  tools/      read-only tool set exposed to models
  agent/      bounded tool-calling loop with budgets
  pipeline/   strategies (baseline | delegated | panel) -> candidates -> fresh validation -> anchoring/dedup
  findings/   schema, stable ids, state
  github/     PR collection from event JSON, review/summary publication, state marker
  report/     run report + dry-run plan
```

## Architecture decision: standalone Rust core, Vera external

Chosen over forking a TypeScript reviewer (misospace/pr-reviewer-action,
jbot) because: the reasoning loop, anchoring and state logic are small; a
native binary avoids a Node toolchain in the Action; Vera is already a Rust
binary and is invoked as a pinned executable so Revera does not inherit its
build (ONNX, tree-sitter grammars). Linking `vera-core` is deferred until a
measured need appears.

## Boundaries

1. Models return structured findings through a `submit_findings` /
   `submit_verdict` tool. They never post comments.
2. Model tools are read-only: `read_file`, `grep_repo`, `find_files`,
   `diff_context`, `list_changed_files`, plus `vera_search`, `vera_references`,
   `vera_grep`, `vera_overview` when Vera is enabled and healthy. Lexical
   tools see only tracked files of the exact head (excludes honoured;
   `.git`/`.vera`/`.revera` skipped). No shell.
3. Provider/model/credentials come from trusted config only.
4. Indexing is controller-owned: Revera runs `vera update .` once before any
   model call; a model cannot rebuild the index. If Vera is disabled or the
   update fails/times out, the run continues lexical-only and is marked
   partial with "semantic retrieval unavailable".
5. Publisher re-fetches the PR head SHA and refuses to publish if it moved.
6. Everything provider- or subprocess-facing is bounded by the run deadline
   (`budget.run_max_seconds`) with a reserve for validation and reporting.

## Latency and strong-model economy

- Strong-model tokens are the scarce resource; cheap-model tokens are not.
- One `vera update` per run; all model tool calls hit the prepared index.
- Investigation lanes (scouts/workers) run concurrently (`tokio::JoinSet`),
  bounded by `review.concurrency`.
- Validation runs per candidate in fresh contexts, concurrently; candidates are
  first collapsed (same file + overlapping lines + same `defect_key`) so the
  strong validator sees each logical defect once.
- Hard budgets per agent (tool calls, output tokens, wall clock) and per run
  (total model requests, wall clock). A budget breach yields status `partial`
  with the reason surfaced in the summary — never a fake clean review.
- Tool outputs are truncated to a configurable byte cap so cheap models do not
  flood their own context.

## Finding schema

```text
id                    12 hex chars = sha256(file + "\0" + defect_key)[..12]
defect_key            model-supplied snake_case identity of the defect (not wording)
severity              high | medium | low
file, start_line, end_line?
title, claim, trigger, impact
introduced_by_change  bool
supporting_evidence[] { path, start_line, end_line, note }
counterevidence_checked[]   strings, filled by the validator
validation_status     accepted | rejected | uncertain
suggested_fix?
source                investigator | scout:<name> | worker:<n> | prior
```

Only `accepted` findings are published. `uncertain` findings are listed in the
summary as "unconfirmed" only when `review.publish_uncertain` is on (default
off) and are always kept in the run report.

## Re-review

State (`version: 2`) = `{ reviewed_head, reviewed_base, review_key, last_status,
findings: [{id, status, file, start_line, title, defect_key, severity, claim,
trigger, impact, evidence[], posted, ...}] }`, bounded to 200 findings
(resolved/rejected pruned first). Stored in `.revera/state.json` locally and,
on GitHub, embedded in the managed summary comment as
`<!-- revera-state:<base64 json> -->`. That comment is selected by marker +
decodable state + ownership (recorded comment id, authenticated viewer or
known bot login), never by marker text alone.

`review_key` = sha256(base sha, exact head tree id, patch id, config
fingerprint). The fingerprint covers strategy, thresholds, limits, budgets,
concurrency, retries, delegated/panel settings, model routes (protocol/base
URL/model/reasoning/extra-header *names* — no key names or values),
prompt content hash, engine version and Vera index identity. A run is reused
only when the prior run under the same key was `complete` and left no
unposted open findings; partial/failed runs are always redone. Corrupt state
is quarantined (`state.json.corrupt`); pre-v2 state triggers a fresh review
but keeps publication ids.

On a new push: prior unresolved findings are handed to the validator as
"recheck" candidates against the new head; findings that no longer hold are
marked `resolved`; a resolved finding that is reproduced again is `reopened`
and published again; findings still valid and already posted are not
reposted. New candidates are deduplicated against prior ids. Inline and
summary publication are tracked separately, and before posting the publisher
reads the PR's existing inline review comments and treats every
`<!-- revera-id:… -->` it finds there as posted — so a summary upsert that
fails after the inline review succeeded never duplicates inline comments on
the next run, even when the state blob never recorded them.

## Anchoring

Inline comments require `file` to be a changed file and `start_line` to be in
the head-side line set of some hunk in the PR diff (added or context lines).
Anything else goes into the summary under "Findings outside the diff".

## Strategies

- `baseline`: investigator (one agent, tools) -> candidates -> validator.
- `delegated`: lead produces N bounded questions (no tools, one call) ->
  workers answer each concurrently with tools -> lead synthesizes candidates
  from worker reports (one call) -> validator.
- `panel`: each configured scout runs the investigator prompt independently
  -> union + collapse -> validator. No majority voting; minority findings
  survive until the validator rejects them.

All strategies share tools, schema, validator, anchoring and publication.

## Timing

Every run records wall-clock `PhaseTiming` entries into the report's
`timing.phases`: `vera_index`, `recheck`, `lane` (baseline `investigator`,
`panel:<focus>`, delegated `worker:<qid>`), `plan`, `synthesis`,
`arbitration`, `validate` (label = candidate id), and `publish`. Outcomes are
`ok` (with `ok:candidates`/`ok:accepted` variants), `timeout`,
`tool_budget`, `error:<short>`, or `skipped`. Derived metrics in the report:
`total_ms`, `vera_index_ms`, `first_candidate_ms` (end of the first lane
that produced candidates), `first_validated_ms` (end of the first accepted
validate), `lanes_ms`/`validate_ms` (phase spans), `validate_p50_ms`/
`validate_p95_ms` (nearest-rank), `incomplete_phases` and `skipped_phases`.
Validate phases start when the semaphore permit is acquired and carry the
wait in `queue_ms`; `skipped` phases are excluded from spans and
percentiles. The `publish` phase measures inline-review publication only
(the summary comment cannot time its own upsert) and the footer's one-line
rendering (`_Timing: total … · lanes …_`) is refreshed before the comment
is posted so JSON, stdout and GitHub agree.

## Vera integration

Environment for `vera` subprocesses is built from `vera:` config
(`backend: api` sets `VERA_BACKEND=api` + `EMBEDDING_*`/`RERANKER_*` from the
configured env names). `.revera/vera-cache.json` records `vera_version`,
`backend`, `embedding_model`, `dim` (from `.vera/vectors.manifest`) and is
rewritten only after a successful, health-checked index/update; a restored
cache that does not match is discarded before `vera update`. `revera
cache-key` hashes the index-shaping config (backend, embedding model,
excludes, Vera version) — not investigator/validator settings — so the
Action cache survives model changes. `vera.enabled: false` needs no binary,
key or index. Vera subprocesses are deadline-bounded and killed on timeout.
