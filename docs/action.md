# GitHub Action

Run Revera in one repository without hosting a server. The Action installs
the binaries, reviews pull requests and posts accepted findings using the
workflow's GitHub token. It supports Linux x86_64 runners.

For a dashboard and mention commands, use the [self-hosted GitHub App](server.md).

## Quickstart

1. Create `revera.yaml` in the repository root. Replace the endpoint and model
   ID with your provider's values:

   ```yaml
   models:
     investigator:
       protocol: openai-chat
       base_url: https://api.example.com/v1
       api_key_env: REVIEW_API_KEY
       model: your-model-id
   ```

   This enables baseline review and fresh-context validation using the same
   route. Vera retrieval is off. Other protocols and optional settings are in
   the [configuration reference](configuration.md).

2. Open **Settings → Secrets and variables → Actions → New repository secret**.
   Add `REVIEW_API_KEY` with your model API key as its value. Never put the
   value in YAML.
3. Save this as `.github/workflows/revera.yml`:

   ```yaml
   name: revera
   on: pull_request
   permissions:
     contents: read
     pull-requests: write
   jobs:
     review:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
           with:
             persist-credentials: false
             fetch-depth: 0
             ref: ${{ github.event.pull_request.head.sha }}
         - uses: VeraTools/revera@v0
           env:
             REVIEW_API_KEY: ${{ secrets.REVIEW_API_KEY }}
   ```

4. Merge both files into the default branch, then open a PR from a branch
   in the repository. Review configuration comes from the PR's base commit,
   so a PR cannot change the endpoints or credential names its own review uses.

Keep the explicit head checkout: GitHub's default merge ref is not the tree
Revera reviews. Findings are posted inline when they sit on diff lines;
other accepted findings go in the managed summary. Later pushes update the
same review state without duplicating still-open findings.

A complete review with accepted or carried findings also has a collapsed **Fix with
your coding agent** section in its summary. Small, exact replacements may
appear as GitHub suggested changes; neither is a tested patch. See
[instructions and suggestions](how-it-works.md#instructions-for-a-coding-agent).
Carried findings remain available after later pushes or identical-content
reruns, without being counted as newly accepted or posted inline again.

## Inputs

Set inputs under `with:` on the `VeraTools/revera` step. Pass provider keys
through `env:` using the names in your configuration.

| Input | Default | Meaning |
|---|---|---|
| `config` | `revera.yaml` | Configuration path |
| `profile` | none | Named profile, including built-in `quick` and `deep` |
| `strategy` | from configuration | Override `baseline`, `delegated` or `panel` |
| `publish` | `comment` | `dry-run` prints the plan without posting |
| `fail-on` | `failed` | `failed`, `partial` or `never` |
| `revera-version` | pinned to the Action release | Revera binary to install |
| `vera-version` | `2.1.4` | Vera binary to install; skipped when retrieval is off |
| `github-token` | `${{ github.token }}` | Token for PR data and comments |
| `cache` | `true` | Cache the Vera index between runs |

For example, to use a larger review budget and fail the step on incomplete work:

```yaml
with:
  profile: deep
  fail-on: partial
```

Built-in Quick and Deep scale from your configured budget without changing
validation or strategy. A custom profile named `quick` or `deep` replaces that
built-in; other custom names leave both built-ins available. See
[profile settings](configuration.md#profiles) and
[strategy behavior](how-it-works.md#strategies).

## Outputs and exit codes

| Revera exit | `status` output | Meaning |
|---|---|---|
| `0` | `complete` | Every stage finished; zero findings means nothing was found |
| `2` | `partial` | Some work was not checked; zero findings is not a clean verdict |
| other | `failed` | Configuration/setup error or crash; no trustworthy report |

`findings` is the accepted finding count, or zero when no trustworthy report
exists. `report-path` points to the JSON report and is empty for index-only
runs or failures before a report was written. A missing or invalid review
report is treated as `failed`.

`fail-on: failed` fails the step only on failure. `partial` also fails it on
incomplete work. `never` leaves the step successful for these review outcomes;
it does not suppress errors in earlier setup steps. Finding a defect does
not itself fail the Action.

## Keep the index warm

This matters only with [Vera enabled](configuration.md#vera). A new PR can
restore its base branch's cache, but not another PR's. Add a push trigger:

```yaml
on:
  pull_request:
  push:
    branches: [main]
```

On push, the Action refreshes the index without reviewing or publishing.
The checkout above also works on push: the empty PR head expression falls
back to the pushed commit. Later PRs into `main` restore that index.

`schedule` and `workflow_dispatch` also index the checked-out commit. These
runs need embedding credentials and any variables referenced by the config.
They report `complete` or `failed`, with no findings or report. With Vera
disabled, indexing does nothing. Other events are rejected.

Only a healthy refreshed index is saved. Changes to models, prompts, budgets
or reranking keep the index cache; changes to indexed content settings
invalidate it. See [cache identity](configuration.md#vera).

## Fork pull requests

When publishing comments (the Action default), fork PRs are skipped by default
and reported as `partial`, not clean. Dry-run reviews do not apply this guard.
Fork workflows normally have no provider secrets, and their GitHub tokens
cannot post review comments.

`github.allow_forks: true` removes Revera's fork guard; it does not supply
credentials or grant permissions. Treat enabling it as a trust decision.
Do not switch to `pull_request_target` only to expose secrets to fork code.
If you use that event, review the workflow's trust boundary and keep the
explicit PR-head checkout. Revera reads configuration from the base commit,
does not execute repository code, and restores but never saves the index in
that event's base-branch cache scope.

## Version pinning

`VeraTools/revera@v0` follows the promoted release in that major line. Use an
exact `@vX.Y.Z` tag to hold a version, or a full commit SHA with a version
comment for an immutable Action pin. The checkout example is SHA-pinned too.

The Action downloads a checksummed release binary. For reproducible runs,
keep `revera-version` aligned with the Action release; pinning the Action
alone does not pin a manually overridden binary version. Upgrade the
`vera-version` input deliberately if you set it yourself.
