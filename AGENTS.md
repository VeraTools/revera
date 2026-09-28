# Agent notes for this repository

Revera is a Rust PR reviewer: an investigator model proposes candidates, a
fresh-context validator re-derives each one, and deterministic Rust owns the
diff, review identity, state and publication. Keep that split.

## Invariants (do not weaken)

- Only a fresh validator verdict from the current run (or a recheck verdict)
  makes a finding publishable. `review.validate: false` is evaluation-only and
  is rejected together with `publish: comment`.
- In event mode, configuration inside the checkout is read from the base
  commit, never the PR head; trust checks run before any credential is read.
- Secrets never reach models, logs, reports, comments or state: resolve keys
  through `redact::secret_env`, send text through `redact::text`/`redact::json`,
  write files through `fsutil` (symlink-safe, atomic). `config::SENSITIVE_GLOBS`
  is the single content policy for tools and diffs.
- The Vera index identity (`vera::index_key`) covers only what changes
  indexed content. Reranker, model routes, prompts, budgets and guidance
  belong in the review fingerprint so review-only changes keep a warm cache.
- Guidance (`review.guidance`) stays `off` by default until an evaluation
  shows it helps.

## Checks

Everything offline; run before pushing:

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
REVERA_TEST_VERA=$(command -v vera) cargo test
./scripts/check-versions.sh && ./scripts/check-docs-hygiene.sh
bash scripts/test-install-revera.sh && bash scripts/test-install-vera.sh
bash scripts/test-action-outcome.sh
bash eval/test-run-all.sh && bash eval/test-frozen.sh
```

Public files (README, docs, prompts, skills, AGENTS.md) name model providers,
not routing intermediaries (`check-docs-hygiene.sh`). Workflow `uses:` pins
are full commit SHAs with a version comment. Live-provider checks are listed
in CONTRIBUTING.md; report them as run, skipped or blocked, never as passed
when they did not run.
