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
- Eval harness in `eval/`: 6 synthetic corpus repos + 7 hard repos
  (`build-corpus-hard.sh`: historical Revera regressions, multi-hop trait
  contract, two clean controls with FP traps), 7 configs; scoring counts TP by
  severity, validator rejections and clean-PR noise. Results in
  `eval/results.jsonl` / docs/EVAL.md.
- Event mode reviews the exact PR head: `prepare` refuses when HEAD != the
  requested head or tracked files are dirty; `--event` checks out
  `pull_request.head.sha` (detached) before indexing. Empty `models.workers`
  falls back to the investigator. Summary-only and local/dry-run findings are
  marked posted after the summary/report is written, so identical reruns
  short-circuit. One run deadline spans rechecks, agents and validators.
  GitHub 422 on the inline review degrades to summary-only. Retry-After
  accepts HTTP-dates; backoff is capped at 60s. Empty length-truncated
  provider replies are errors, not silent no-ops.
- Release path: `scripts/check-versions.sh` (CI) keeps `action.yml`'s
  `revera-version` equal to Cargo's version; `scripts/install-revera.sh` is
  shared by the Action and the release workflow's verify step (`sha256sum -c`,
  `revera --version`); tags `vX.Y.Z` move `vX`.
- Per-route `reasoning` levels (default `medium`) across all four HTTP
  adapters; `provider_state` echoes reasoning/thinking items verbatim;
  400s mentioning the reasoning field drop it and retry on the same slot.
- `action.yml` composite action (vera+revera install with sha256 verify,
  .vera cache restore/save, fail-on), release workflow (tag `v*` → verify
  packaged artifact → release → move `vX`), self-review dogfood workflow
  (checks out `pull_request.head.sha`).

## Current blocker / known limitations

- `v0.1.0` is released and `uses: VeraTools/revera@v0` resolves (`v0` and
  `v0.1.0` both point at the release commit). Known issue: the 0.1.0
  `x86_64-unknown-linux-gnu` binary needs glibc >= 2.39, so it fails on
  Ubuntu 22.04 hosts; fixed on main by shipping a static
  `x86_64-unknown-linux-musl` asset in the next release.
- Eval evidence is muse-spark-only, 2 reps per cell, small repos; Vera's
  recall effect is still unmeasured (docs/EVAL.md). Strong-model comparisons
  were not run (user directive).
- The fixture suite needs live OpenRouter embeddings; it passed on this
  branch after one earlier run timed out at the embeddings endpoint.

## Next three tasks

1. Tag the next release to ship the musl asset, then dogfood the Action on
   the next PR.
2. Add one large-repo, multi-hop eval case to measure Vera's recall effect.

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
- `posted` is set only after a successful surface: the GitHub publisher
  after inline post/summary upsert, and the local/dry-run path after the
  report is written — never before.
- All model routes default to `meta/muse-spark-1.3-contributor` (user
  directive); see docs/EVAL.md.
