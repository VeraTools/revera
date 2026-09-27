# Changelog

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
