# Changelog

## v0.8.1

- Setup shows a separate connection result for the investigator, the validator
  and, when API retrieval is on, the embedding model. **Finish setup** needs
  every tested connection to pass, and a failed embedding test explains how to
  fix it or turn retrieval off. Reloading `/setup` after the App is created now
  works, and signed-in administrators can open Settings from setup to repair a
  saved configuration. Thanks to @cam-cc.
- Fix requests: replies to `@app fix` read "awaiting confirmation" instead of
  internal state names. Draft PRs and commits are titled after the finding
  (`Fix: <title>`), and the PR body lists findings with their severity. The
  fix history records the requester's permission check separately from the
  confirmation.
- Fix sandbox on SELinux hosts: when the probe fails because bubblewrap cannot
  make its root read-only, the reason now points to the fix. For Podman, run the
  container with the `container_engine_t` label; see
  [Sandbox runtime and test tools](docs/server.md#sandbox-runtime-and-test-tools).

## v0.8.0

Existing setups keep working without changes. Fix execution and external PR
tracking are off by default, and no new GitHub permission is needed for
reviews. See [Upgrading from 0.7.0](docs/server.md#from-070-to-080).

- Connected accounts: the Repositories page lists every installation of your
  GitHub App with its status (allowed, awaiting approval, permission update
  required, suspended, not installed). Administrators approve or revoke owners
  there; revoking cancels that owner's work and removes its repositories.
  Approvals follow GitHub account IDs, so renames keep working. Installation
  changes refresh through webhooks, and new installations return to this page.
- Setup asks who can install the App (only your account, or any account) and
  whether to request Contents write for publishing approved fixes. Both
  default to the previous behavior. Unapproved owners never get jobs, and their
  repository names are not shown.
- External reviews: paste an open public PR URL under **Reviews → Review PR
  URL** to review it privately, without installing the App there. Nothing is
  posted to GitHub, only the server's configuration is used, and no GitHub
  token reaches the review. You can also track your own open public PRs
  (off by default) so each new push is reviewed.
- Review details show only accepted findings, with evidence, links to the
  reviewed lines and a "may be outdated" label when the PR has changed.
  Rejected and uncertain candidates move behind **Show diagnostics**.
- Copy or download instructions for your coding agent, per finding or for the
  whole review. Complete GitHub summaries with findings include a collapsed
  **Fix with your coding agent** section. The summary's count of accepted
  findings matches the dashboard.
- Small, exact replacements checked by the validator appear as GitHub
  suggested changes. Revera only suggests them when the lines match the
  reviewed commit exactly and sit inside the PR's changes.
- Opt-in fixes for the GitHub App. A fixer model edits a snapshot of the PR in
  a bubblewrap sandbox without a GitHub token; Rust checks the patch, an
  optional test command runs without network, and a separate verifier model
  checks each finding. Request fixes from the dashboard, the summary link, the
  **Fix with Revera** check button or `@<app> fix`. Results are previews you
  can download; publishing as a draft PR or a commit to the PR branch needs
  Contents write and, by default, an administrator's approval. Revera never
  force-pushes, re-reviews what it published and reports which findings were
  resolved. Settings → Fixes and per-repository overrides control all of it.

## v0.7.0

- Quick and Deep now scale from your budget instead of using fixed limits.
  Quick halves the tool calls and time limits. Deep doubles them, up to 50
  minutes per review, and doubles the request limit. Before, a raised budget
  could make Deep shorter than a normal review. With the default budget the
  numbers barely change; Quick's per-review time goes from 420 to 450
  seconds, and Deep's request limit goes from 120 to 240. Custom `quick` and
  `deep` profiles still replace the built-ins.
- Repositories page: sort by recent activity, name, last review, open pull
  requests or review count; filter by trigger and by recent activity; select
  several repositories and change their trigger or mode, or index them, in
  one step. Rows show open PRs, the last push and a `revera.yaml` badge. The
  view is kept in the URL.
- Overview: switch between the last 24 hours and the last 7 days. It shows
  findings by severity, the median review time, the share of cached tokens,
  and each model with its own status.
- Reviews page: search by title, author, repository or PR number. Filters
  are kept in the URL, and each repository's menu links to its reviews.
- Review details: the summary is formatted instead of shown as raw Markdown.
  Finding locations link to the file at the reviewed commit. Readable timing,
  review counts and per-model usage replace the raw JSON, which is still
  available under "Raw report data".
- Settings → Budgets shows the Quick and Deep limits your current budget
  produces.
- Fixed: the Reviews page showed no jobs unless a repository, a status and a
  kind were all chosen, because the server treated unset filters as filters
  for empty values. Also fixed: the Repositories page showed an index job as
  the last review.
- Success messages disappear after a few seconds, and the sidebar remembers
  whether it is collapsed.

## v0.6.2

- The rootless Podman quickstart now prepares the data directory with
  `podman unshare chown`. The previous `sudo chown` left the container unable
  to write to it.
- New server docs: a Podman Quadlet setup, upgrade steps for each install
  method, restoring a backup, and what to do if a secret leaks. Several
  statements in the docs were corrected to match the code.
- The dashboard shows when a key saved in the dashboard overrides the same
  variable from the environment, and dashboard keys can be removed from where
  they are shown.
- Settings adds controls for reviewing PRs from forks, the largest diff size
  and using a separate validator model. The Budgets tab says which limits Quick
  and Deep change.
- Removed the Publish mode setting, which had no effect on the server.
- Web search detects `EXA_API_KEY` when it is set in the environment but not
  named in the configuration.

## v0.6.1

- The dashboard has a theme menu in the top bar: System, Light or Dark. The
  choice is saved in the browser; System follows the operating system as before.
- Settings → Models points to the Retrieval tab, where the embedding and
  reranker models are configured.

## v0.6.0

- The GitHub App accepts PR conversation commands: `@<app-slug> review`,
  `quick`, `deep`, `review standard` and `help`. Owners, members and collaborators
  can request a fresh review of the current head, including drafts.
- Repository triggers are now `mention`, `auto` and `off`, with independent
  `quick`, `standard` and `deep` review modes. The default trigger is **mention**,
  including upgrades without a trigger field. Select auto to keep automatic
  PR reviews. Legacy disabled repositories stay off; enabled ones inherit the
  default. Built-in quick and deep profiles also work in the CLI and Action,
  keep validation on and leave the configured strategy unchanged.
- The investigator now sees findings that are still open on the pull request,
  so a repeat review of the same head no longer posts a reworded duplicate.
- Existing GitHub Apps need Issues read-only access, the Issue comment event and
  installation-owner approval before mentions work. New apps request them
  during setup. See the [upgrade steps](docs/server.md#enable-mentions-on-an-existing-github-app).
- Model routes support ordered fallback models across providers and protocols.
  Eligible provider failures try the next available route; the last successful
  route stays selected for that role during the run. Reports record switches
  and per-route usage without changing the Vera index identity.
- Optional `web_search` and `web_read` tools can check external technical
  facts through Exa. Web search stays off by default. Private or unknown
  repositories require a separate trusted opt-in; queries leave the server.
  The [paired evaluation](docs/evaluation.md#opt-in-web-search) reports the
  small sample, completion gaps and source-verification limitation.
- Setup includes an optional reranker. Dashboard settings expose model
  fallbacks, default triggers and modes, a mention-command reference and web
  search. Repositories have independent Trigger and Mode controls; reviews
  show both. Overview points existing App owners to the mention-permission change.
- Release notes now come from the matching section of `CHANGELOG.md`.
  The Action has a shorter Marketplace description. Setup documentation is
  split into [GitHub App](docs/server.md), [Action](docs/action.md) and
  [CLI](docs/cli.md) guides.

## v0.5.1

- Failed or partial server indexes now wait before automatic retries: 10 minutes,
  doubling after each failure up to 24 hours. A completed index resets the wait;
  push webhooks and manual indexing still run immediately. The repository API
  reports the next retry time as `index.retry_at` while waiting.
- Server model health checks now also run on each reconciliation tick, so editing
  the configuration file by hand re-tests the changed models without a restart.

## v0.5.0

- New `revera serve` runs Revera as a self-hosted GitHub App. Install it
  once on an account or organization and it reviews pull requests in every
  selected repository, including repositories created later, with no
  workflow files or repository secrets. Each review runs the same `revera
  review` code path as the Action and shows up on the PR as a "Revera"
  check plus the usual summary and inline comments. Pushes to the default
  branch refresh the Vera index, so new PRs start from a warm index.
- The server includes a dashboard: a guided setup (create the app, sign in
  with GitHub, install, connect models), review history with findings and
  logs, per-repository on/off switches, and settings for models, keys,
  budgets and retrieval. Settings and the raw YAML edit the same file.
- A container image is published as `ghcr.io/veratools/revera` for
  `linux/amd64` and `linux/arm64`. Release binaries now include
  `aarch64-unknown-linux-musl`, and `install-revera.sh` picks the right
  one for the machine.
- `revera review --bot-login <login>` names the account that owns Revera's
  comments when the token cannot look itself up (GitHub App installation
  tokens).
- The Action installs Vera 2.1.4 by default (was 2.1.3). The first run
  after upgrading builds a fresh index.
- The Action is unchanged otherwise; see [docs/server.md](docs/server.md)
  for choosing between the two.

## v0.4.7

- The Action's display name is now "Revera PR Review", so it can be listed
  on the GitHub Marketplace ("Revera" is taken by a GitHub account). Usage is
  unchanged: `uses: VeraTools/revera@v0`.

## v0.4.6

- New PRs can start from a warm Vera index. On `push`, `schedule` and
  `workflow_dispatch` events the Action no longer fails with "unsupported
  event": it runs the new `revera index` command, which builds or refreshes
  the index without reviewing, and caches it under the branch. Add
  `push: branches: [main]` to the workflow and each new PR into `main`
  restores that index instead of building one from scratch. Pull request
  events review as before; other events fail with a clear message.
- `revera index [--config] [--profile]` builds or refreshes the Vera index
  for local use too.
- `pull_request_target` reviews no longer save the index cache. They run in
  the base branch's cache scope, so a PR-head index would have seeded the
  base branch and every other PR; they still restore the base branch's.

## v0.4.5

- The Action installs Vera 2.1.3 by default (was 2.0.1). Its `vera update`
  re-embeds files whose vectors are missing, so a restored index that lost
  its vectors recovers instead of searching by keyword only. The Action's
  index cache key includes the Vera version, so the first run after
  upgrading builds a fresh index.
- `stats.retrieval` counts searches where Vera fell back to keyword-only
  results (for example after an embedding key expired), as
  `vera+rerank (keyword-only on 2 searches)`. These used to be reported as
  plain `vera+rerank`. Vera 2.1.2 and later warn on every such fallback;
  older versions miss the case of a missing vector store.
- Release verification probes reranking with one direct
  `vera search --rerank-status` after the review, so the check no longer
  depends on the investigator choosing to call `vera_search`.
- Breaking: `models.scouts[].focus` is removed. It was accepted but never
  used; scouts always took their focus from `panel.focuses` in order.
  Configs that set it now fail with an unknown-key error.
- The skill's Action example checks out the PR head, matching the README
  quickstart.
- Dependencies updated, including serde-saphyr 2 for config parsing.

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
- Full documentation rewrite (`docs/configuration.md`, review strategies,
  `docs/evaluation.md`, `docs/how-it-works.md`), CONTRIBUTING and SECURITY
  pages, and release hardening (tag/commit validation, verified artifact
  packaging, composite Action pinned at 0.3.0).
