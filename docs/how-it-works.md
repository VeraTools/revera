# How Revera works

Revera is a single Rust binary. Deterministic code owns everything that
touches the pull request — diffing, review identity, state, anchoring,
publication — and configurable models own the reasoning. This document
describes the pipeline, its boundaries and the state it keeps.

## Pipeline

```text
1. diff       base…head (merge-base; two-dot fallback on shallow clones)
2. retrieve   optional `vera update` on the exact head; lexical tools always
3. investigate  agent loop with read-only tools → candidate findings
4. validate   one fresh-context agent per candidate → accepted | rejected | uncertain
5. reconcile  anchor to diff lines, dedupe, resolve/reopen against prior state
6. publish    inline comments + one managed summary comment (or dry-run)
```

**Diff.** The checked-out tree must be exactly `head`; dirty or mismatched
trees are refused rather than reviewed approximately. In GitHub Actions this
means checking out `pull_request.head.sha`, not the merge ref.

**Retrieval.** Every run has lexical tools over the tracked files of the
head tree (`grep_repo`, `find_files`, `read_file`, `diff_context`,
`list_changed_files`; `.git`, `.vera`, `.revera` excluded). When a `vera:`
block is configured, Revera runs `vera update .` once before any model call
and adds `vera_search`, `vera_references`, `vera_grep` and `vera_overview`.
Models cannot rebuild the index. If Vera is missing, fails or times out, the
run continues lexical-only and is marked `partial` with "semantic retrieval
unavailable".

**Investigate.** The investigator reads the diff, explores with tools inside
a tool-call and wall-clock budget, and ends by calling `submit_findings`
with structured candidates. A malformed submission gets exactly one repair
round; still-malformed output makes the run `partial` while valid findings
from a mixed list are kept.

**Validate.** Candidates are first collapsed (same file, overlapping lines,
same `defect_key`), then each one is given to a validator agent that starts
from an empty conversation: it sees the claim and the diff, has the same
tools, and must re-establish the defect from the code before returning
`accepted`. By default the validator uses the investigator's route, so a
single configured model still gets an independent second look; configure
`models.validator` to use a different model. Only `accepted` findings are
published. `uncertain` findings stay in the report and are listed in the
summary only when `review.publish_uncertain` is on.

**Reconcile and publish.** Inline comments require the finding to sit on a
head-side line of a diff hunk; other accepted findings go into the summary
under "Findings outside the diff". Inline publication and the summary
comment are reconciled separately, and the publisher reads existing
`<!-- revera-id:… -->` markers on the PR before posting, so a summary
failure never duplicates inline comments on retry. The publisher re-fetches
the PR head SHA and refuses to publish if it moved.

## Boundaries

1. Models return findings and verdicts through tools (`submit_findings`,
   `submit_verdict`). They never post comments, run shell commands or write
   files.
2. All model tools are read-only and scoped to the exact head tree.
3. Providers, models and credential *names* come from trusted configuration;
   credential values are read from the environment and never logged or
   fingerprinted.
4. Everything provider- or subprocess-facing is bounded by
   `budget.run_max_seconds`, with a reserve kept for validation and the
   final report. A budget breach yields `partial` with the reason in the
   summary — never a silently truncated "clean" review.

## Outcomes

| status | meaning |
|---|---|
| `complete` | every stage finished; zero findings means "reviewed, nothing found" |
| `partial` | something was not checked: budget, provider error, retrieval unavailable, malformed model output, skipped fork |
| `failed` | configuration/setup error or crash; no trustworthy report |

The CLI exits 0 / 2 / other respectively; the Action normalises a missing or
unparseable report to `failed` and applies `fail-on` to the result. The run
report (`--out`, default `.revera/last-report.json`) carries `status`,
`reason`, `findings`, `coverage_gaps`, per-phase `timing`, a per-route
`ledger` (requests, tokens, requested vs effective reasoning) and `stats`
(candidates before/after validation, retrieval mode, tool calls, reuse).

## Findings

```text
id                    12 hex chars = sha256(file + "\0" + defect_key)[..12]
defect_key            model-supplied snake_case identity of the defect
severity              high | medium | low
file, start_line, end_line?
title, claim, trigger, impact, suggested_fix?
introduced_by_change  bool
supporting_evidence[] { path, start_line, end_line, note }
counterevidence_checked[]   filled by the validator
validation_status     accepted | rejected | uncertain
source                investigator | scout:<name> | worker:<n> | prior
```

Identity is `file + defect_key`, not wording, so a rephrased finding on the
next push is the same finding.

## State and re-review

State lives in `.revera/state.json` locally and, on GitHub, inside the
managed summary comment as `<!-- revera-state:<base64 json> -->`. That
comment is selected by marker, decodable state *and* ownership (recorded
comment id, or the authenticated viewer / configured bot login) — never by
marker text alone. State is bounded to 200 findings (resolved and rejected
pruned first); corrupt state is quarantined to `state.json.corrupt`.

Each run computes `review_key = sha256(base sha, exact head tree id, patch
id, config fingerprint)`. The fingerprint covers the *effective*
configuration: strategy, thresholds, budgets, concurrency, retries,
delegated/panel settings, the model routes actually used (protocol, base
URL, model, reasoning, extra-header names — no key names or values), prompt
hashes, engine version and Vera index identity. A prior review is reused
only when it ran under the same key, finished `complete` and left no
unposted findings; partial and failed runs are always redone.

On a new push, prior open findings are re-checked by the validator against
the new head: a finding that no longer holds is `resolved`; a resolved
finding that reappears is `reopened` and published again; still-valid,
already-posted findings are left alone. New candidates are deduplicated
against prior ids.

## Strategies

`baseline` is the pipeline above. `delegated` and `panel` replace the
investigate step with multiple lanes and share everything else (tools,
schema, validator, anchoring, publication). See [strategies.md](strategies.md).

## Vera integration

`vera:` configuration becomes the subprocess environment (`backend: api`
sets `VERA_BACKEND=api` plus `EMBEDDING_*`/`RERANKER_*` from the configured
env names). `.revera/vera-cache.json` records Vera version, backend,
embedding model and vector dimension and is rewritten only after a
successful, health-checked index or update; a restored cache that does not
match is discarded. `revera cache-key` hashes just the index-shaping
configuration (backend, embedding model, excludes, Vera version) so the
Action's cache survives investigator/validator changes; it prints
`disabled` when Vera is off, and the Action then skips the Vera download
entirely.

## Code map

```text
src/
  cli.rs        review | doctor | cache-key | cache-info
  config.rs     revera.yaml schema, env expansion, profiles, effective validation
  git/          base/head/merge-base, unified diff → hunks → reviewable lines
  vera.rs       external `vera` executable: version, update, search, references
  provider/     ModelClient: openai-chat, openai-responses, anthropic, gemini, scripted
  tools.rs      read-only tool set exposed to models
  agent.rs      bounded tool-calling loop
  pipeline/     baseline | delegated | panel → candidates → validation → anchoring
  findings.rs   schema, ids, state
  github/       event parsing, API, publication, state marker
  report.rs     run report + summary rendering
prompts/        investigator, validator, lead, worker, scout prompts
```
