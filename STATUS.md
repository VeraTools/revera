# Status

## What works

- `revera review --base <rev> [--head <rev>]` baseline strategy end to end:
  merge-base diff collection (two-dot fallback on shallow clones), Vera
  indexing, investigator agent loop, candidate collapse, per-candidate
  fresh-context validation, diff anchoring, summary markdown + JSON report,
  `.revera/state.json` with open/uncertain/resolved tracking and patch-id
  short-circuit (`--force` bypasses).
- `revera review --event <path>` GitHub event mode: base/head/title from the
  payload, base-sha fetch on shallow clones, state seeded from the managed
  summary comment's `<!-- revera-state:... -->` blob.
- `revera review --event ... --publish comment`: head-SHA recheck (refuses on
  head moved), inline COMMENT review for unposted accepted findings, managed
  summary comment upsert, `posted` marking after successful posts, fork guard
  (`github.allow_forks`, default false).
- `revera doctor`, `revera cache-info`.
- Providers via a shared `HttpTransport` (ledger reservation per attempt,
  429/5xx + transient retry, Retry-After, backoff+jitter) + per-protocol
  `ProtocolAdapter`: `openai-chat`, `openai-responses`, `anthropic`,
  `gemini` (no call ids — synthesized `name-idx`), and offline `scripted`.
- `delegated` strategy: lead plans bounded questions (`submit_plan`), workers
  answer them concurrently on a restricted toolbox (`submit_worker_result`),
  lead synthesizes final candidates (`submit_findings`); blocked/gaps surface
  as coverage_gaps under "Not checked"; 0-question plans degrade to baseline.
- `panel` strategy: concurrent scout lanes (investigator prompt + focus
  addendum parsed from prompts/scout_focus.md `## <focus>` sections), union +
  collapse, "panel: N scouts, M raw candidates → K unique" in the summary.
- Run-wide controls: wall-clock deadline (run_max_seconds) caps every agent
  call and marks unvalidated candidates uncertain; request reservations count
  every HTTP attempt (incl. retries); lanes past the request budget are
  skipped with a partial_reason.
- Fixture suite: `fixtures/run-fixture.sh` exercises break → fix → clean →
  delegated → panel (all scripted).
- Eval harness in `eval/`: 6 synthetic corpus repos, 6 configs (baseline,
  no-vera, no-reranker, candidate-only, panel, delegated) x 2 reps; scoring
  + markdown summary in `eval/results.jsonl` / docs/EVAL.md.
- Per-route `reasoning` levels (default `medium`) across all four HTTP
  adapters; `provider_state` echoes reasoning/thinking items verbatim;
  400s mentioning the reasoning field drop it and retry on the same slot.
- `action.yml` composite action (vera+revera install with sha256 verify,
  .vera cache restore/save, fail-on), release workflow (tag `v*` → release +
  major tag move), self-review dogfood workflow.

## Current blocker

None for M1–M5. Live delegated + panel runs on the crossfile `break` fixture
both found the bug (muse-spark all routes); see docs/EVAL.md.

## Next three tasks

1. Address review feedback / CI on the PR.
2. Cut a `v0.1.x`/`v1.x` tag to exercise the release workflow.
3. Tune delegated/panel model routing (lead vs worker/scout quality).

## Exact test-demo command

```sh
OPENROUTER_API_KEY=... VERA_HOME=$HOME/.vera-revera fixtures/run-fixture.sh
# live probe:
VERA_HOME=$HOME/.vera-revera revera review --repo <repo> --base <base> \
  --head <head> --config fixtures/configs/live-openrouter.yaml
```

## Expensive-to-reverse decisions

- Vera invoked as an external pinned executable, not linked (`vera-core`/
  ONNX/tree-sitter builds stay out of Revera).
- Review state keyed on `finding_id = sha256(file + defect_key)`; defect_key
  wording is part of the durable state format. In event mode the state blob
  lives inside the managed summary comment (`<!-- revera-state:base64 -->`).
- Model output contract is the `submit_findings`/`submit_verdict` tool-call
  schema; prompt files in `prompts/` are the interface contract.
- `posted` is set only by the GitHub publisher after a successful post —
  dry-run never marks findings posted.
- All model routes default to `meta/muse-spark-1.3-contributor` (user
  directive); see docs/EVAL.md.
