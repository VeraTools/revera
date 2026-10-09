# Action setup

```yaml
name: revera
on:
  pull_request:
permissions:
  contents: read
  pull-requests: write
jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<full-sha> # vX.Y.Z
        with:
          fetch-depth: 0
          ref: ${{ github.event.pull_request.head.sha }}
      - uses: VeraTools/revera@<full-sha> # vX.Y.Z
        env:
          REVIEW_API_KEY: ${{ secrets.REVIEW_API_KEY }}
```

- Pin both actions to a full commit SHA with a version comment.
- Check out the PR head (`ref:` above), not the default merge commit, so the
  workspace is the tree Revera reviews.
- The secret name must match `api_key_env` in `revera.yaml`; the user adds the
  value under Settings → Secrets and variables → Actions.
- On `pull_request` events Revera reads `revera.yaml` from the **base**
  commit, so config changes take effect after they merge. Event-mode
  endpoints must be `https://`.
- Fork PRs get no secrets; with `publish: comment` Revera skips them
  (status `partial`, reason in the report) unless `github.allow_forks: true`.
- Inputs: `config`, `profile`, `strategy`, `publish`, `fail-on`
  (`failed` | `partial` | `never`), `revera-version`, `vera-version`, `github-token`,
  `cache`.
  Outputs: `status`, `report-path`, `findings`.
