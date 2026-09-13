# Status

## What works

- `revera review --base <rev> [--head <rev>]` baseline strategy end to end:
  merge-base diff collection, Vera indexing, investigator agent loop,
  candidate collapse, per-candidate fresh-context validation, diff anchoring,
  summary markdown + JSON report, `.revera/state.json` with open/resolved
  tracking and patch-id short-circuit (`--force` bypasses).
- `revera doctor`, `revera cache-info`.
- `openai-chat` provider (tool calling, retries, `max_completion_tokens`
  fallback, run request budget) and `scripted` provider for offline fixtures.
- Fixture suite: `fixtures/run-fixture.sh` exercises break → fix → clean.

## Current blocker

None for M1+M2. GitHub publication and delegated/panel strategies are M3+.

## Next three tasks

1. `--publish comment` GitHub publisher (head-SHA recheck, managed summary
   comment with `<!-- revera-state:... -->` marker).
2. `delegated` strategy (lead plan → workers → lead synthesize).
3. `panel` strategy (scout lanes, union + collapse).

## Exact test-demo command

```sh
OPENROUTER_API_KEY=... VERA_HOME=$HOME/.vera-revera fixtures/run-fixture.sh
```

## Expensive-to-reverse decisions

- Vera invoked as an external pinned executable, not linked (`vera-core`/
  ONNX/tree-sitter builds stay out of Revera).
- Review state keyed on `finding_id = sha256(file + defect_key)`; defect_key
  wording is part of the durable state format.
- Model output contract is the `submit_findings`/`submit_verdict` tool-call
  schema; prompt files in `prompts/` are the interface contract.
