# How Revera works

Revera is a single Rust binary. Deterministic code owns everything that
touches the pull request (diffing, review identity, state, anchoring and
publication), and the configured models do the reasoning. This document
describes the pipeline, its boundaries and the state it keeps.

## Pipeline

```text
1. diff       base…head (merge-base; fetch more history if needed)
2. retrieve   optional `vera update` on the exact head; lexical tools always
3. investigate  agent loop with read-only tools → candidate findings
4. validate   one fresh-context agent per candidate → accepted | rejected | uncertain
5. reconcile  anchor to diff lines, dedupe, resolve/reopen against prior state
6. publish    inline comments + one managed summary comment (or dry-run)
```

**Diff.** The checked-out tree must be exactly `head`; dirty or mismatched
trees are refused rather than reviewed approximately. In GitHub Actions this
means checking out `pull_request.head.sha`, not the merge ref. If no merge base
is found, Revera fetches more history (`--deepen=200`, then `--unshallow`).
It fails if there is still no merge base; it never falls back to a two-dot diff.

**Retrieval.** Every run has lexical tools over the tracked files of the
head tree (`grep_repo`, `find_files`, `read_file`, `diff_context`,
`list_changed_files`; `.git`, `.revera` and Vera's `.vera*` directories
excluded). When a `vera:` block is configured, Revera runs `vera update .`
once before any model call and adds `vera_search`, `vera_references`,
`vera_grep` and `vera_overview`. Models cannot rebuild the index. If Vera is missing, fails or times out, the
run continues lexical-only and is marked `partial` with "semantic retrieval
unavailable".

**Investigate.** The investigator reads the diff, explores with tools inside
a tool-call and wall-clock budget, and ends by calling `submit_findings`
with structured candidates. A malformed submission gets exactly one repair
round; still-malformed output makes the run `partial` while valid findings
from a mixed list are kept. Prompts are capability-neutral: Revera appends a
note to each system prompt listing the tools actually available in that run.
If Vera is disabled or index setup fails, its tools are hidden and the note
describes lexical-only retrieval.

Optional `web_search`/`web_read` supply untrusted public technical evidence only
when enabled in trusted config; private/unknown repositories need a separate
opt-in. Investigation and fresh validation share bounded requests with validator
capacity reserved; planning and synthesis have no web access. See
[configuration](configuration.md#web_search) for the third-party data boundary.

**Validate.** Candidates are first collapsed (same file, overlapping lines,
same `defect_key`), then each one is given to a validator agent that starts
from an empty conversation: it sees the claim and the diff, has the same
tools, and must re-establish the defect from the code before returning
`accepted`. By default the validator uses the investigator's route, so a
single configured model still gets an independent second look; configure
`models.validator` to use a different model. Only `accepted` findings are
published. `uncertain` findings stay in the report and are listed in the
summary only when `review.publish_uncertain` is on. Optional per-role
[fallback models](configuration.md#fallback-models) inherit with the route;
switching providers replays the full conversation and keeps the last
successful alternative for that role for the remainder of the run.

**Reconcile and publish.** Inline comments require the finding to sit on a
head-side line of a diff hunk; other accepted findings go into the summary
under "Findings outside the diff". Inline publication and the summary
comment are reconciled separately, and the publisher reads existing
`<!-- revera-id:… -->` markers on the PR before posting, so a summary
failure never duplicates inline comments on retry. The publisher re-fetches
the PR head SHA and refuses to publish if it moved.

Installed App reviews and the Action can publish to GitHub. The App also
supports [external public PR reviews](server.md#external-public-pr-reviews):
these use the same review engine but keep results private, with no GitHub
comments or checks.

## Boundaries

1. Review models return findings and verdicts through tools (`submit_findings`,
   `submit_verdict`). They never post comments, run shell commands or write
   files.
2. Repository tools are read-only and scoped to the exact head tree; opt-in web
   evidence is a separate, bounded external data source, never instructions.
3. Providers, models and credential *names* come from trusted configuration;
   credential values are read from the environment and never logged or
   fingerprinted.
4. Everything provider- or subprocess-facing is bounded by
   `budget.run_max_seconds`, with a reserve kept for validation and the
   final report. A budget breach yields `partial` with the reason in the
   summary, never a silently truncated "clean" review.
5. [Opt-in App fixes](server.md#fixes-opt-in) have a separate boundary and
   limits. A fixer can edit only a disposable head snapshot in a bubblewrap
   sandbox; its fresh-context verifier is read-only. Both receive selected
   model credentials and network access for model calls, never a GitHub token.
   Only an administrator-approved test command executes repository code, in a
   separate sandbox without network or credentials. Rust checks the patch;
   a dashboard administrator must separately approve code publication, which
   rechecks the PR head, policy and installation grants. Model output cannot
   authorize a push.

## Outcomes

| status | meaning |
|---|---|
| `complete` | every stage finished; zero findings means "reviewed, nothing found" |
| `partial` | something was not checked: budget, provider error, retrieval unavailable, malformed model output, skipped fork |
| `failed` | configuration/setup error or crash; no trustworthy report |

The dashboard also labels jobs `queued`, `running`, `cancelled` or `skipped`.
Skipped or cancelled work is not a completed review, even when it has no findings.

The CLI exits 0 / 2 / other respectively; the Action normalises a missing or
unparseable report to `failed` and applies `fail-on` to the result. The run
report (`--out`, default `.revera/last-report.json`) carries `status`,
`reason`, `findings`, `coverage_gaps`, per-phase `timing`, a per-route
`ledger` (requests, tokens, requested vs effective reasoning) and `stats`
(candidates before/after validation, retrieval mode, tool calls, reuse).
When a completed review is reused for identical content and effective
configuration, `stats.reused` is `true` and `stats.validation` is `"reused"`.

## Findings

```text
id                    12 hex chars = sha256(file + "\0" + defect_key)[..12]
defect_key            model-supplied snake_case identity of the defect
severity              high | medium | low
file, start_line, end_line?
title, claim, trigger, impact
suggested_fix?        investigator's proposal; never published
validated_fix?        remedy supplied or approved by the validator
suggestion?           validator's exact {start_line, end_line, original, replacement}
introduced_by_change  bool
supporting_evidence[] { path, start_line, end_line, note }
counterevidence_checked[]   filled by the validator
validation_status     accepted | rejected | uncertain
source                investigator | scout:<name> | worker:<n> | prior
```

Identity is `file + defect_key`, not wording, so a rephrased finding on the
next push is the same finding. The investigator cannot set a publishable
remedy: its `suggested_fix` is only a proposal, and any investigator-supplied
`validated_fix` is discarded. The validator's `Verdict.fix` becomes
`validated_fix`. The dashboard, agent instructions and comments use that
validator-owned remedy, not the investigator's proposal. It is advice, not
a tested patch.

The review page shows accepted findings with their IDs, trigger, impact,
evidence and links to the file at the reviewed commit. GitHub, the dashboard
and the review API count only accepted findings. Rejected, uncertain and
unvalidated candidates are hidden under **Show diagnostics**, labelled
**Not validated — not reported as defects**; they are not included in agent
instructions. The full JSON report can still contain these candidates.

### Instructions for a coding agent

For a complete review with accepted findings, the dashboard offers **Copy fix
instructions** and **Download .md** for all findings. Each accepted finding
also has **Copy prompt**. Instructions include the PR URL, repository, base
and head commit IDs (SHAs), findings, evidence and the validator's suggested
remedies. They are built without another model call and ask the agent to
verify each issue, make scoped edits, test them, and get permission before
committing, pushing or opening a PR. Long instructions are capped and link
back to the review.

**Potentially stale** means the PR head has changed since review; copied and
downloaded instructions then require re-validation. **Freshness unknown**
means the current head could not be checked, not that the result is current.
Re-run the review to check the latest head.

On GitHub, a complete review with accepted findings adds a collapsed
**Fix with your coding agent** section to the existing summary. App reviews
link to the dashboard; instructions are also included in the comment when
they fit within 3,000 characters. Partial or failed reviews do not get this
handoff section.

### GitHub suggested changes

An inline finding can include GitHub's suggested-change block only
when the validator supplies an exact replacement: the original text must
match the reviewed head byte for byte, the finding must cover those same
lines, and every line must be added or modified in one head-side diff hunk.
The range is at most 10 lines; the replacement is at most 20 lines and 2,000
bytes. Sensitive files, redacted text, unsafe comment markers and invalid
characters are refused.

If any check fails, Revera keeps the explanatory finding without a suggestion
block. Unchanged lines, larger edits and prose remedies alone do not
produce a block. A suggestion still needs a person to review and apply it;
passing these checks does not mean it has been tested.

## State and re-review

State lives in `.revera/state.json` locally and, on GitHub, inside the
managed summary comment as `<!-- revera-state:<base64 json> -->`. That
comment is selected by marker, decodable state *and* ownership (recorded
comment id, or the authenticated viewer / configured bot login), never by
marker text alone. State is bounded to 200 findings (resolved and rejected
pruned first); corrupt state is quarantined to `state.json.corrupt`.

Each run computes `review_key = sha256(base sha, exact head tree id, patch
id, config fingerprint)`. The fingerprint covers the *effective*
configuration: strategy, thresholds, budgets, concurrency, retries,
delegated/panel settings, the model routes actually used (protocol, base
URL, model, reasoning, extra-header names; no key names or values), prompt
hashes, engine version and Vera index identity. A prior review is reused
only when it ran under the same key, finished `complete` and left no
unposted findings; partial and failed runs are always redone.

On a new push, prior open findings are re-checked by the validator against
the new head: a finding that no longer holds is `resolved`; a resolved
finding that reappears is `reopened` and published again; still-valid,
already-posted findings are left alone. New candidates are deduplicated
against prior ids.

## Strategies

Select `review.strategy`, `--strategy` in the CLI or the Action's `strategy`
input. Every strategy uses the same fresh validator, anchoring, state and
publication. They change how candidates are produced, not what can post.

| Strategy | Candidate production |
|---|---|
| `baseline` (default) | One investigator explores the change and submits candidates |
| `delegated` | A lead plans bounded questions; workers investigate in parallel; the lead synthesises candidates |
| `panel` | Independent scouts investigate with focus labels; their candidates are combined and collapsed |

Baseline is the recommended starting point. The validator can use the same
model in a new conversation or a separate `models.validator` route.

Delegated review uses `models.lead` and `models.workers`, both inheriting the
investigator when omitted. Planning has read-only repository tools, no web
access and a six-call tool budget; synthesis has `read_file`.
A failed worker creates a coverage gap and makes the run partial. Panel uses
named `models.scouts` with labels from `panel.focuses`, in order; a single
scout runs once per label. There is no voting: a claim from one scout still
goes to validation.

In the [strategy evaluation](evaluation.md#strategy-and-retrieval-comparison-meta-muse-spark-on-every-route),
multiple lanes did not improve recall and cost more. Panels also produced
more false positives. Measure a benefit on your own repositories before
choosing them. Route and budget keys are in the
[configuration reference](configuration.md#delegated-and-panel).

## Vera integration

`vera:` configuration becomes the subprocess environment (`backend: api`
sets `VERA_BACKEND=api` plus `EMBEDDING_*`/`RERANKER_*` from the configured
env names). `.revera/vera-cache.json` records Vera version, backend,
embedding model and vector dimension and is rewritten only after a
successful, health-checked index or update; a restored cache that does not
match is discarded. `revera cache-key` hashes only the index-shaping
configuration (backend, embedding origin and model, excludes, Vera
version) so the Action's cache survives investigator, validator, reranker,
prompt, budget and credential-name changes; it prints
`disabled` when Vera is off, and the Action then skips the Vera download
entirely.

## Code map

```text
src/
  cli.rs        serve | review | index | doctor | cache-key | cache-info
  config.rs     revera.yaml schema, env expansion, profiles, effective validation
  git.rs        git subprocesses: revisions, merge-base, diff, patch and tree ids, grep
  diff.rs       unified diff → hunks → reviewable lines
  vera.rs       external `vera` executable: version, update, search, references
  provider/     ModelClient: openai-chat, openai-responses, anthropic, gemini, scripted
  tools.rs      read-only tool set exposed to models
  agent.rs      bounded tool-calling loop
  pipeline/     baseline | delegated | panel → candidates → validation → anchoring
  findings.rs   finding and verdict schema, ids, candidate collapsing
  handoff.rs    accepted-finding instructions shared by dashboard and GitHub
  state.rs      review state and review key
  github/       event parsing, API, publication, state marker
  report.rs     run report + summary rendering
  server/       GitHub App, dashboard API, review queue and external PR tracking
prompts/        investigator, validator, lead, worker, scout prompts
```
