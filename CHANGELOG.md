# Changelog

## Unreleased

- Release verification probes reranking with one direct
  `vera search --rerank-status` after the review, so the check no longer
  depends on the investigator choosing to call `vera_search`.
- The skill's Action example checks out the PR head; without it every run
  was refused because the checked-out tree was the merge commit.

## v0.4.4

- The Action installs Vera 2.0.1 by default (was 1.4.1). The Action's index
  cache key includes the Vera version, so its first run after upgrading
  builds a fresh index. Vera 2 keeps finished embeddings when an API index
  fails, retries with backoff, and reads indexes built by 1.x.
- Embedding defaults for `vera.backend: api` are now Vera 2's own (8 / 256 /
  120 s: two 128-input requests at a time). The 0.4.3 values allowed only
  one request in flight on Vera 2; a cold index of this repository took
  60–64 s with them and 26–30 s with the new ones. Revera still sets them
  explicitly so the persistent Vera home does not keep older values.
- The index or update summary (files, chunks, embedding requests, retries,
  timeouts, elapsed time) is logged after each successful refresh.
- Vera 2's `.vera.build/`, `.vera.old/` and `.vera.resume/` directories are
  covered by the content policy like `.vera/`: excluded from the index, the
  diff and every file tool.
- Agents are told when a fifth of their tool budget remains, and once the
  budget is spent only the submission tool is offered, with one reminder
  (while at least 15 s remain) if the model still tries to research. In
  this repository's self-review, 4 of 5 runs without the early notice ended
  `partial` with nothing submitted; with it, 2 of 2 completed.
- `check-versions.sh` also checks that every Vera pin matches the Action's
  `vera-version` default.
- Dependencies: `yoke-derive` 0.8.4 (0.8.3 was yanked), `uuid` 1.27.0,
  `taiki-e/install-action` 2.87.23.

## v0.4.3

- Fixed: reports show `retrieval: vera+rerank` when the configured reranker
  is active; earlier releases reported `vera` even though searches were
  reranked.
- With `vera.backend: api`, Revera sets Vera's embedding concurrency,
  in-flight batch size and request timeout (defaults 2 / 128 / 120 s instead
  of Vera's 8 / 16 / 60 s, which timed out on hosted endpoints and left no
  index). Override them with `vera.embedding.max_concurrent_requests`,
  `max_in_flight_inputs` and `timeout_secs`; they do not change the index
  identity.
- Release verification can require a reranked, error-free `vera_search` in
  the first run (`require_rerank`, on by default).
- The frozen-evaluation test waits up to 30 s for its mock server and fails
  clearly if it does not start.

## v0.4.2

- Public review footers now show only `role=model@effort` (and an effective
  effort when it differs); reused reviews no longer print an empty models list.
- Release verification evidence and uploaded artifacts omit endpoints.
- Workflows read model and embedding endpoints and keys from the
  `REVERA_MODEL_*` and `REVERA_EMBEDDING_*` secrets.
- The release workflow serializes runs per tag.

## v0.4.1

- Security: query values in model, embedding and reranker `base_url`s
  (for example `?key=...`) no longer appear in reports, PR comment footers,
  `revera doctor` output, provider errors or logs. Route labels keep query
  names only, request errors drop their URL, and provider and Vera error
  text drops URL queries. Earlier releases printed the full URL; keep
  keys in `api_key_env` and rotate any key that was placed in a URL.
- `release-verify.yml` installs the Vera version from `action.yml` instead of
  the Revera version.

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
