# Contributing

## Build and test

Rust toolchain per `rust-toolchain.toml`. Everything below runs offline.

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test
scripts/test-install-revera.sh && scripts/test-install-vera.sh && scripts/test-action-outcome.sh
scripts/test-verify-release-report.sh  # release-verify report checks (mock GitHub)
scripts/check-versions.sh          # Cargo.toml and action.yml agree
scripts/check-docs-hygiene.sh      # tracked text has no routing intermediary names
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

Live checks, run when the relevant endpoint and key are available:

- `REVERA_EMBEDDING_BASE_URL=… REVERA_EMBEDDING_API_KEY=… VERA_HOME=$HOME/.vera-revera fixtures/run-fixture.sh`
  — scripted models against a real Vera index (break → fix → clean →
  delegated → panel).
- `REVERA_MODEL_BASE_URL=… REVERA_MODEL_API_KEY=… revera review --repo <path> --base <rev> --config <cfg>`
  against a real model endpoint for a manual end-to-end run; `revera doctor`
  first. Configurations using Vera also need
  `REVERA_EMBEDDING_BASE_URL=… REVERA_EMBEDDING_API_KEY=…`.

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
3. `release.yml` builds static `x86_64` and `aarch64` musl binaries and
   publishes them with SHA-256 checksums and generated notes. It also pushes
   the `ghcr.io/veratools/revera:X.Y.Z` image (amd64 and arm64). It does not
   mark the release Latest, move the `vX` major tag or move the `:0` image.
4. Prove the published binary on a fixture PR with a known defect:
   dispatch `release-verify.yml` with the exact tag, the PR number,
   `expect_file` and `expect_terms`. The fixture head must not have been
   reviewed before: the first run has to be a fresh review, and the
   second run proves identical-head reuse. Put `[skip ci]` in the fixture
   commit message so `self-review.yml` and `dogfood-released.yml` do not
   review the head first. `require_rerank` (default on) also requires the
   first run to report `retrieval: vera+rerank`, and reranked results from
   either an error-free `vera_search` call or a direct
   `vera search --rerank-status` probe the workflow runs after the review.
   The proof's "Binary SHA-256" is the extracted `revera` binary; the
   release's `.sha256` file is the tarball's checksum. If a run fails because
   a model call errored rather than because the defect was missed, amend the
   fixture commit (keeping `[skip ci]`), force-push and dispatch again.
5. For releases that change the App server, run the
   [App server smoke test](#app-server-smoke-test) on the `X.Y.Z` image.
6. Dispatch `release-promote.yml` with the tag. It moves `vX` and the `:0`
   and `:latest` images, marks the release Latest and reads them back.
7. Open a small follow-up PR so `dogfood-released.yml` exercises the
   promoted `@v0` Action.

### App server smoke test

This manual check covers the GitHub App journeys that automated tests mock.
Run it on a test server with the release image (`ghcr.io/veratools/revera:X.Y.Z`),
a GitHub App that has **Contents: write**, and a small public test repository.
Use a test repository only: this flow pushes a branch and opens a draft PR.
Record each step as passed, failed or skipped.

1. **Connect another account.** The App must be public. Install it on a second
   account or organization (`https://github.com/apps/<app-slug>/installations/new`).
   On **Repositories → Connected accounts**, the owner shows
   **Awaiting Revera approval** and its repositories are not reviewed. Approve it and
   check that its repositories appear.
2. **Check the sandbox.** Open **Settings → Fixes**. **Sandbox available** must
   show (`proc: false` is fine). If not, read the reason and see
   [Sandbox runtime and test tools](docs/server.md#sandbox-runtime-and-test-tools).
3. **Prepare the test repository.** Give it a test command that needs only
   `/usr` tools, for example `sh tests/run.sh`. Open a PR that adds a small,
   clear bug which its tests do not catch. In **Settings → Fixes**, set the test
   command. On the repository's fix settings, set execution to **Enabled** and
   delivery to **Draft pull request**, leaving the global execution switch off.
4. **Review.** Comment `@<app-slug> review`. The summary must list the finding
   and include the **Fix with your coding agent** instructions and the
   **Fix with Revera** offer. A small, exact remedy should appear as a GitHub
   suggested change.
5. **Copy the agent prompt.** Open the review from the summary link and use
   **Copy fix instructions** or **Download .md**. They name the reviewed base and head SHAs and the
   finding.
6. **Request a fix.** Comment `@<app-slug> fix`. Revera replies that the
   request is awaiting confirmation. Open it, check the plan and confirm.
7. **Check the preview.** When it is **ready**, the patch is limited to the
   finding, the verifier marks it resolved, and the tests passed.
8. **Approve.** Approve with **Draft pull request** delivery. A draft PR by the App opens
   against the PR's branch.
9. **Follow-up review.** The fix moves to **verified**, with the finding
   resolved and no new findings.
10. **External review.** On **Reviews**, start an external review of a small
    public PR in a repository where the App is not installed. It completes, its
    instructions download, and nothing is posted on that PR.

Afterwards, turn repository execution back off, clear the test command if it
was only for this check, and close the test PRs.
