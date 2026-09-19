# Review strategies

`review.strategy` (or `--strategy` / the Action's `strategy` input) selects
how candidate findings are produced. Every strategy feeds the same
fresh-context validator, anchoring, state and publication; they differ only
in the investigate step.

## `baseline` (default)

One investigator agent explores the change with tools and submits
candidates; one validator agent per candidate re-derives it from scratch.
Two model calls' worth of context per finding, the lowest cost and latency,
and — in every evaluation so far — the fewest false positives. With a
single configured route the validator reuses that route in a fresh
conversation; configure `models.validator` to validate with a different
model.

```yaml
models:
  investigator: { protocol: openai-chat, base_url: https://api.example.com/v1, api_key_env: REVIEW_API_KEY, model: fast-model }
  validator:    { protocol: anthropic, api_key_env: ANTHROPIC_API_KEY, model: claude-sonnet-4-6 }
```

## `delegated` (advanced)

A lead model plans up to `delegated.max_questions` bounded questions about
the diff (one call, no tools). Workers answer each question concurrently
with tools under their own budgets (`delegated.worker_max_tool_calls`,
`delegated.worker_max_seconds`). The lead synthesises worker reports into
candidates (one call), which go to the validator. A failed worker becomes a
coverage gap and marks the run `partial`.

```yaml
review:
  strategy: delegated
models:
  investigator: { ... }              # used as lead and worker when the routes below are omitted
  lead:    { protocol: openai-chat, base_url: ..., api_key_env: ..., model: strong-model }
  workers:
    - { protocol: openai-chat, base_url: ..., api_key_env: ..., model: cheap-model }
```

## `panel` (advanced)

Each configured scout runs the investigator prompt independently with a
focus label (`panel.focuses` supplies labels for scouts without an explicit
`focus`). Candidates are unioned and collapsed (same file, overlapping
lines, same `defect_key`); there is no voting, so a finding from one scout
survives until the validator rejects it.

```yaml
review:
  strategy: panel
models:
  investigator: { ... }              # validator inherits this route unless set
  scouts:
    - { name: general,    protocol: openai-chat, base_url: ..., api_key_env: ..., model: model-a }
    - { name: cross-file, protocol: gemini,      api_key_env: GEMINI_API_KEY,  model: gemini-2.5-pro }
```

## Choosing

| | cost / latency vs baseline | recall (measured) | false positives (measured) |
|---|---|---|---|
| `baseline` | 1× | tied with the others on every corpus | 0 with the same model on both routes |
| `delegated` | ~1.6–2× cost, ~2.4× wall time | no gain | 1 on the hard corpus |
| `panel` | 1.5–8× depending on lanes | no gain | 2–35, rising with lane count |

Multi-lane strategies produce more raw candidates, and the validator did not
absorb the extra noise in our runs. Use them only where you have measured a
benefit on your own repositories; a single stronger validator route on
`baseline` is the cheaper first experiment. Details and caveats:
[evaluation.md](evaluation.md).
