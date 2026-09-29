# Changelog

## v0.4.0

- Prompt caching is enabled by default on supported model routes, with
  provider-compatible hints, retry without rejected hints and cached-token
  usage in the ledger. Validator-owned remedies are the only fixes rendered
  in reports and comments; prompts now describe the tools actually available,
  including lexical-only fallback when Vera retrieval is unavailable.
  Identical completed reviews can be reused and are reported with
  `stats.validation: "reused"`.
- Added base-commit review guidance (`off` by default); evaluation found no
  benefit so far ([details](docs/evaluation.md)). Vera index identity now
  excludes review-only settings, so reranker/model/prompt changes preserve
  the warm index; reranking fallback and retrieval coverage are reported.
- Event-mode configuration now comes from the base commit and trust checks
  precede credential resolution. Publication and state ownership fail closed;
  repository-sensitive content is excluded from model tools and diffs.
- Vera tool results are limited to tracked files at the reviewed head, like
  every other tool, so untracked working-tree files never reach a model.
  Endpoint query values (which may carry credentials) no longer enter review
  identity or prompt-cache keys. Written reports drop the investigator's
  `suggested_fix` and any fix without an accepted verdict unless validation
  is off; an evaluation-only run (`validate: false`) now reports candidates
  already tracked in state instead of dropping them.
- Replaced `serde_yaml` with `serde-saphyr`, refreshed the HTTP/TLS stack and
  added bundled Mozilla trust roots so HTTPS works on hosts without a system
  CA bundle. Minimum supported Rust was lowered to 1.89.
- CI adds pinned static analysis (`cargo-deny`, `cargo-machete`, shellcheck,
  Ruff, actionlint and zizmor) and an MSRV check. Frozen-candidate evaluation
  now pins validators and records truth scores and provenance; a first
  validator comparison kept GLM `glm-5.3-flash` as the recommended validator.
- Exact-version releases publish checksummed assets without moving the
  `vX` major tag or marking the release Latest. After a live proof is
  verified by `scripts/verify-release-report.py`, the manual promotion
  workflow (gated by a required-reviewer environment) moves `vX` through the
  Git refs API and marks the release Latest;
  see [the release sequence](CONTRIBUTING.md#releasing).

## v0.3.0

- Configuration is now minimal by default: a `models.investigator` route is
  the only required key; `review:` may be omitted (baseline strategy,
  `validate: true`, `publish: dry-run`).
- `models.validator` is optional and inherits the investigator route (fresh
  context); an explicit validator identical to the investigator produces the
  same review identity.
- Vera retrieval is opt-in: an absent `vera:` section means disabled, a
  present one means enabled unless `enabled: false`.
- Credential checks are strategy-aware: `Config::load` checks route shape
  only; `validate_for` requires keys only for the routes the effective
  strategy uses, and `doctor` reports missing keys per route with `next:`
  hints instead of failing the whole config.
- `doctor` gained `--profile`, `--strategy` and `--publish` and validates
  panel scout/focus cardinality; the fork guard now runs before credential
  checks so forked PRs get a partial "review skipped" report.
- Full documentation rewrite (`docs/configuration.md`, `docs/strategies.md`,
  `docs/evaluation.md`, `docs/how-it-works.md`), CONTRIBUTING and SECURITY
  pages, and release hardening (tag/commit validation, verified artifact
  packaging, composite Action pinned at 0.3.0).
