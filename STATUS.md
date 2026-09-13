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
- `openai-chat` provider (tool calling, retries, `max_completion_tokens`
  fallback, content-array replies, run request budget) and `scripted`
  provider for offline fixtures.
- Fixture suite: `fixtures/run-fixture.sh` exercises break → fix → clean.
- `action.yml` composite action (vera+revera install with sha256 verify,
  .vera cache restore/save, fail-on), release workflow (tag `v*` → release +
  major tag move), self-review dogfood workflow.

## Current blocker

None for M1+M2+M3 baseline. `delegated`/`panel` strategies remain future work.

## Next three tasks

1. `delegated` strategy (lead plan → workers → lead synthesize).
2. `panel` strategy (scout lanes, union + collapse).
3. Cut a `v0.1.x`/`v1.x` tag to exercise the release workflow.

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
