# Self-hosted GitHub App

`revera serve` runs a private GitHub App with a browser dashboard on your server. Install it once
to review PRs across your repositories, including new repositories, without
workflow files or repository secrets. The GitHub Actions path remains available
if you prefer reviews to run in Actions rather than on your own machine.

## Requirements

- A server with a public HTTPS URL that GitHub can reach.
- GitHub permission to create and install an app on your account or organization.
- A review API endpoint, model ID and key, configured as for the CLI.
- Persistent storage for `/data`, with enough room for repository checkouts.

For a native installation, install `revera`, `git` and `vera`, and make them
available on `PATH`. The container includes these tools.

## Run it

Create a data directory owned by container user 10001, then start the server.
You do not need a YAML file: the dashboard writes it when you configure models.

```sh
mkdir -p data
sudo chown -R 10001:10001 data
podman run -d --name revera --restart unless-stopped \
  -p 127.0.0.1:8080:8080 -v "$PWD/data:/data:Z" \
  -e REVERA_PUBLIC_URL=https://review.example.com ghcr.io/veratools/revera:0
podman logs revera
```

Put it behind HTTPS as described below, then open the setup link from the logs.
The wizard creates the app and lets you configure models and keys. Until a
configuration is saved, jobs are skipped with “models are not configured yet”
and no PR check is created. Saving the first configuration triggers reconciliation
so skipped PRs can be picked up.

For Docker, use `docker` instead of `podman`; omit `:Z` on hosts without SELinux.
A Compose example:

```yaml
services:
  revera:
    image: ghcr.io/veratools/revera:0
    restart: unless-stopped
    ports:
      - "127.0.0.1:8080:8080"
    volumes:
      - ./data:/data
    environment:
      REVERA_PUBLIC_URL: https://review.example.com
      REVERA_CONCURRENCY: "2"
      REVERA_RECONCILE_MINUTES: "10"
```

Run `docker compose up -d`, then read `docker compose logs revera` for the setup
link. Keep keys out of the Compose file and repository.

As a file-based alternative, save this minimal configuration as
`data/revera.yaml` before starting:

```yaml
models:
  investigator:
    protocol: openai-chat
    base_url: https://api.example.com/v1
    api_key_env: REVIEW_API_KEY
    model: your-model-id
```

Enter the key in the dashboard, or export `REVIEW_API_KEY` and add
`-e REVIEW_API_KEY` to the container command (for Compose, add
`REVIEW_API_KEY: ${REVIEW_API_KEY}` under `environment`). A present but invalid
configuration is still a startup error; an explicit `--config` file must exist.
Validation is on by default. Vera retrieval is off unless configured;
see [configuration.md](configuration.md).

Other settings, each also available as a flag:

| Environment variable | Default | Flag |
| --- | --- | --- |
| `REVERA_PUBLIC_URL` | required HTTPS URL | `--public-url` |
| `REVERA_LISTEN` | `0.0.0.0:8080` | `--listen` |
| `REVERA_DATA_DIR` | `/data` | `--data-dir` |
| `REVERA_CONFIG` | `/data/revera.yaml` | `--config` |
| `REVERA_CONCURRENCY` | `2` | `--concurrency` |
| `REVERA_RECONCILE_MINUTES` | `10` | `--reconcile-minutes` |

`REVERA_ALLOWED_OWNERS` adds comma-separated account or organization logins to
the app owner's allowed installations, merged with the dashboard's allowed
owners. Credentials use the environment names in the configuration. `${VAR}`
substitutions in endpoint URLs work as they do in Actions.

## Put it behind HTTPS

Keep port 8080 private and terminate HTTPS at your reverse proxy. With existing
certificates, nginx can forward requests as follows:

```nginx
server {
    listen 443 ssl;
    server_name review.example.com;
    ssl_certificate /etc/ssl/review/fullchain.pem;
    ssl_certificate_key /etc/ssl/review/privkey.pem;
    client_max_body_size 25m;
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
}
```

Caddy can obtain and renew certificates automatically:

```caddyfile
review.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

A tunnel also works if it provides a stable HTTPS URL and forwards webhook bodies
and signature headers unchanged. Do not put an interactive login in front of
`/webhook`.

## Browser setup

1. Read `podman logs revera` or `docker compose logs revera`.
2. Open the setup link printed in the log. Treat that link as a secret.
3. Edit the app name if it is already taken. Optionally enter your organization.
4. Click **Create GitHub App**, then **Create GitHub App** on GitHub's page.
5. Sign in with GitHub in that same browser. The browser holding the setup
   cookie becomes the first administrator; later sign-ins must be listed in
   **Settings → Access**.
6. Follow the installation redirect and choose **All repositories**, or select
   individual repositories.
7. Return to the dashboard, configure your models and keys, then click **Test**.
   If you enable remote retrieval, configure its keys and test the embedding
   endpoint too. Finish once the checks pass.

The server saves the app credentials in `/data/app.json`. The setup-token
endpoints then disappear. `/` serves the dashboard; repository and review data
require a signed-in administrator. `/healthz` returns `ok`.

Sessions last 14 days and use a signed, HttpOnly, Secure, SameSite=Lax cookie.
Sign out from the avatar menu. Dashboard writes also require a CSRF token;
signing in alone is not enough for an API mutation.

## Dashboard settings and keys

- **Overview** shows connection health, active jobs and recent results.
- **Reviews** shows findings, timing, token use and redacted logs, with
  **Re-run** and **Cancel** actions.
- **Repositories** controls automatic reviews and indexing per repository.
  Manual **Index now** and **Review a PR** actions remain available when
  automatic work is disabled.
- **Settings** edits the default review YAML, models, budgets, retrieval and
  access. Saving YAML runs the same parse, trust and credential checks as a
  review. Invalid input leaves the previous file unchanged.

Set or replace keys in the dashboard using the config's `api_key_env` names.
Values are stored privately in `secrets.json`; the API returns only each name
and its source, never the value. A dashboard value overrides the server process
environment for probes, `${VAR}` expansion and child jobs. Deleting that value
falls back to the environment, or reports the key as missing. GitHub token names
are reserved and cannot be used for model keys.

Config or referenced-key changes clear cached probe results. The server tests
roles without a current result at startup and on every reconciliation tick;
you can also run **Test** immediately. Setup
stays at configuration until the required model checks pass and active retrieval
keys are present. Probe results and installation presence survive restarts, so
completed setup stays complete without waiting for the first inventory. Health is
keyed to the saved YAML and its referenced environment values; secret values are
never stored in the health file. Unrelated keys and unchanged saves do not clear it.

## What happens on PRs, pushes and re-runs

Opened, reopened, updated and ready-for-review PRs enqueue a review. Drafts and
PRs by `dependabot[bot]` or `renovate[bot]` are skipped by default; draft and
author preferences are editable in Settings. Closing a PR cancels its
pending and running work. A newer head replaces pending work and cancels an
older running review. Fork PRs use the same fork guard as Actions; they are
skipped by default.

Pushes to the default branch warm the Vera index. Work runs one job at a time
per repository, up to the global concurrency limit. Each job has a 55-minute
hard limit. Reviews use the existing CLI in a separate process.

The **Revera** check is successful when a complete review finds nothing, neutral
when it finds issues or finishes partially, and failed when execution fails.
Only validator-accepted findings count toward the check and dashboard totals;
rejected or unconfirmed candidates do not.
Cancelled work gets a cancelled check. Re-running the app's check forces a fresh
review, even if the patch is unchanged.
Failed jobs show the final error line from the redacted log, or their exit status
when no error line is available. A failed index keeps its previous push checkpoint
so reconciliation can retry it.
After consecutive failed or partial indexes, automatic retries wait 10 minutes,
then 20, 40, and so on, capped at 24 hours after the latest failure finished.
A complete index resets the count. Push webhooks and manual **Index now** requests
bypass this wait. `GET /api/repos` includes `index.retry_at` while a retry is waiting.

Reconciliation runs at startup and every ten minutes by default to recover
missed events. The first sighting records existing PR heads without reviewing
them: installing the app does not backfill every open PR. Later head changes
are reviewed if they are not already queued or running.

## Per-repository override

A `revera.yaml` on the repository's default branch replaces the server default
in full; it is not merged. For PRs targeting another branch, configuration comes
from that PR's base commit. A PR cannot change its own review configuration.
Without that file, the operator-trusted server config is used. Keep API keys in
the dashboard or server environment, not in configuration files.

External web evidence is separately opt-in through `web_search` in that same
trusted YAML (see [configuration](configuration.md#web_search)). Queries go to
Exa; private or unknown repositories additionally require `allow_private: true`.
No key is required for free hosted Exa; an optional `EXA_API_KEY` selects keyed
REST. The dashboard web-search controls are not yet implemented; use YAML and
the server environment for this feature. Never put key values in the YAML.

## Data directory layout

```text
/data/revera.yaml             operator's default configuration
/data/app.json                app credentials, including OAuth client secret (0600)
/data/settings.json           administrators, allowed owners and repo preferences
/data/secrets.json            dashboard-managed environment values (0600)
/data/health.json             keyed, redacted model probe results
/data/inventory.json          last successful installation/repository inventory
/data/session-key             32-byte cookie signing key (0600)
/data/bootstrap-admin         first-admin bootstrap marker (0600)
/data/setup-token             bootstrap token (0600; deleted after setup)
/data/seen.json               reconciliation checkpoints
/data/repos/<owner>/<repo>/   reusable checkouts, including the warm .vera index
/data/cache/<owner>/<repo>/   repository-isolated Vera configuration and cache
/data/jobs/<id>/meta.json     job status, timestamps, findings and token counts
/data/jobs/<id>/report.json   redacted CLI report, when available
/data/jobs/<id>/log.txt       redacted child stdout/stderr
```

Removing an installation or its repositories deletes the corresponding
workspaces and marks their active jobs cancelled. Shutdown and restart also mark
unfinished jobs cancelled. The newest 500 jobs are retained; active work is not
pruned. Check IDs and repository/installation identity are stored in job metadata
so startup can cancel abandoned checks using fresh repository-scoped tokens.
HTTP draining is limited to ten seconds after jobs and event streams are stopped.
Event payloads exist only while a review runs. Review state is re-seeded
from the managed summary comment, not trusted from a previous checkout. Back up
the credentials, settings, keys and default config; treat the entire data
directory as private.

## Upgrading

Back up the credentials and config, pull the new image, then restart with the
same `/data` volume. For Compose: `docker compose pull && docker compose up -d`.
Reconciliation recovers changed PR heads after restart. Configuration can pin a
Vera version in its cache identity, but the CLI does not enforce that installed
version; keep your configuration and image's Vera release aligned.

When upgrading from the server without a dashboard, older `app.json` files may
lack `client_secret`. In the GitHub App's settings, add
`https://<your-host>/auth/callback` as an authorization callback URL and generate
an OAuth client secret. Stop the server, add that `client_secret` field to
`app.json` without changing its other credentials or 0600 permissions, and
create `settings.json` containing `{"admins":["your-github-login"]}` if it does
not yet exist. Restart and sign in. Do not delete an existing bootstrap marker
to reopen administrator enrollment. Old flat job logs are not imported into the
dashboard; archive them separately if needed.

## Security model

The app is private. Installations are accepted only for its owner and the merged
allowed-owner list. Dashboard access is limited to the administrator list.
Webhook signatures are checked before payloads are parsed. Bootstrap setup is
token-gated and its callback checks state; both endpoints return 404 once the
app is configured.

The app private key, webhook secret and OAuth client secret stay in `app.json`, never in child
arguments or environment. Each job receives a short-lived installation token
scoped to its repository and permissions. Git authentication uses environment
configuration, not command arguments or `.git/config`. Model keys come from the
dashboard or server environment, subject to the CLI's existing configuration trust
checks. Base-branch configuration is trusted administrative input: review
changes to it carefully. This is process isolation, not a sandbox for arbitrary
untrusted executables; use the same configuration precautions as in Actions.

## Troubleshooting

- **Server will not start:** ensure the default config exists and parses, the
  data volume is writable by uid 10001, and the public URL uses HTTPS.
- **Setup returns 401:** use the exact logged token link. A callback with a
  different state is rejected. After setup, a 404 is expected.
- **Sign-in is rejected:** confirm your login is an administrator. First-time
  setup must finish in the browser that opened the setup link. For an older
  app without OAuth credentials, follow the upgrade steps above.
- **Dashboard not built:** native `cargo build` works without Node and serves
  this placeholder. To include the dashboard, run `npm ci && npm run build` in
  `web/` before building the Rust binary. Release images include it.
- **Webhook returns 401:** check the delivery's signature and that the proxy
  leaves the body unchanged. Ensure `/data/app.json` belongs to this app.
- **No reviews after installation:** existing PRs are deliberately not
  backfilled. Update a PR or re-run its Revera check. Check allowed owners,
  installation repository access, draft status and bot authorship.
- **Partial or failed check:** open the job log in Reviews, check API key
  names and endpoint configuration, and inspect the report. A fork
  skip is partial rather than a failed review.
- **Vera problems:** confirm `vera` is on `PATH`, embedding credentials are set
  and the image version matches your intended cache configuration. Explicit
  `vera.home` or `REVERA_VERA_HOME` overrides the normal repository-isolated cache
  location; do not point concurrent repositories at the same Vera home.
