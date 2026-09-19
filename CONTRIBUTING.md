# Contributing

## Build and test

Rust toolchain per `rust-toolchain.toml`. Everything below runs offline.

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test
scripts/test-install-revera.sh && scripts/test-install-vera.sh && scripts/test-action-outcome.sh
scripts/check-versions.sh          # Cargo.toml and action.yml agree
scripts/check-docs-hygiene.sh      # public docs name providers, not routing services
eval/test-run-all.sh               # eval runner plan (no API calls)
```

Live checks, run when the relevant key is available:

- `OPENROUTER_API_KEY=… VERA_HOME=$HOME/.vera-revera fixtures/run-fixture.sh`
  — scripted models against a real Vera index (break → fix → clean →
  delegated → panel).
- `revera review --repo <path> --base <rev> --config <cfg>` against a real
  provider for a manual end-to-end run; `revera doctor` first.

CI runs the offline set on every push and PR (`ci.yml`), the composite
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
3. `release.yml` builds the static `x86_64-unknown-linux-musl` binary,
   publishes it with a SHA-256 checksum and generated notes, and moves the
   `vX` major tag. Pre-release or malformed tags do not move the major tag.
4. Open a small follow-up PR so `dogfood-released.yml` exercises the
   published `@v0` Action.
