# Contributing

## Build and test

Rust toolchain per `rust-toolchain.toml`. Everything below runs offline.

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test
scripts/test-install-revera.sh && scripts/test-install-vera.sh && scripts/test-action-outcome.sh
scripts/test-verify-release-report.sh  # release-verify report checks (mock GitHub)
scripts/check-versions.sh          # Cargo.toml and action.yml agree
scripts/check-docs-hygiene.sh      # public docs name providers, not routing services
eval/test-run-all.sh               # eval runner plan (no API calls)
eval/test-frozen.sh                # frozen-candidate harness with scripted validators
```

Static analysis (the `lint` and `msrv` CI jobs pin the same versions):

```sh
cargo deny --locked check && cargo machete
git ls-files -z '*.sh' | xargs -0 shellcheck
ruff check . && ruff format --check .
actionlint && zizmor --offline --min-severity medium .github/workflows action.yml
cargo +1.89 check --all-targets --locked   # rust-version in Cargo.toml
```

Live checks, run when the relevant key is available:

- `REVERA_EMBEDDING_API_KEY=… VERA_HOME=$HOME/.vera-revera fixtures/run-fixture.sh`
  — scripted models against a real Vera index (break → fix → clean →
  delegated → panel).
- `revera review --repo <path> --base <rev> --config <cfg>` against a real
  provider for a manual end-to-end run; `revera doctor` first.

Report live checks as run, skipped or blocked; never count a check that did
not run as passed.

CI runs the offline set and static analysis on every push and PR (`ci.yml`), the composite
Action with scripted models on two Ubuntu runners (`action-smoke.yml`), and
the live fixture job when secrets are present (skipped, not passed,
otherwise).

## Changes

- Keep the pipeline simple: investigator → fresh validator → deterministic
  anchoring and publication. New abstractions need a measured reason.
- Behaviour that models can influence (findings, verdicts, summaries) needs
  a scripted fixture or unit test; `fixtures/scripts/*.json` show the
  format.
- Public documentation attributes models to their provider or family, uses
  generic OpenAI-compatible placeholders in examples, and never names
  routing intermediaries (`scripts/check-docs-hygiene.sh` enforces this).
- Update `docs/configuration.md` when a config key changes and
  `docs/evaluation.md` when you add measured evidence, with its caveats.

## Releasing

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`) and the
   `revera-version` default in `action.yml`; `scripts/check-versions.sh`
   must pass.
2. Merge to `main`, then tag the merge commit `vX.Y.Z` and push the tag.
3. `release.yml` builds the static `x86_64-unknown-linux-musl` binary and
   publishes it with a SHA-256 checksum and generated notes. It does not
   mark the release Latest or move the `vX` major tag.
4. Prove the published binary on a fixture PR with a known defect:
   dispatch `release-verify.yml` with the exact tag, the PR number,
   `expect_file` and `expect_terms`. The fixture head must not have been
   reviewed before: the first run has to be a fresh review, and the
   second run proves identical-head reuse.
5. Dispatch `release-promote.yml` with the tag. It moves `vX`, marks the
   release Latest and reads both back.
6. Open a small follow-up PR so `dogfood-released.yml` exercises the
   promoted `@v0` Action.
