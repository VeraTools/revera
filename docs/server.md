# Self-hosted GitHub App

Run Revera on your server, install your GitHub App and manage reviews in a
browser. Choose all repositories to include new ones without adding workflows
or repository secrets. For reviews hosted by GitHub instead, use the [Action](action.md).

## Quickstart

1. Start the container on a server with a public HTTPS address and persistent
   storage. The data directory must be writable by container user 10001:

   ```sh
   mkdir -p data
   podman unshare chown -R 10001:10001 data
   podman run -d --name revera --restart unless-stopped \
     -p 127.0.0.1:8080:8080 -v "$PWD/data:/data:Z" \
     -e REVERA_PUBLIC_URL=https://review.example.com ghcr.io/veratools/revera:0
   ```

   This uses rootless Podman (without `sudo`). For Docker or rootful Podman,
   use `sudo chown -R 10001:10001 data` instead. Replace the example URL with
   yours. For Docker, use `docker` in place of `podman`; omit `:Z` on hosts
   without SELinux. No YAML file is needed yet.
2. Put an HTTPS reverse proxy in front of `127.0.0.1:8080`. Keep that port
   private. [Caddy and nginx examples](#https-reverse-proxy) are below. GitHub
   must be able to reach `/webhook` without an interactive login.
3. Run `podman logs revera` and open the setup link. Treat it as a secret.
   Keep using that browser until setup finishes.
4. Create and install the GitHub App. Change its name if taken, and enter your
   organization if needed. Click **Create GitHub App**, confirm on GitHub,
   then sign in with GitHub in the same browser. That browser becomes the
   first administrator. Follow the installation redirect and choose **All
   repositories** or selected repositories. You need permission to create
   and install an app on that account.
5. Return to the dashboard and connect a model endpoint, model ID and key.
   Test the connection and finish setup. One model route is enough: validation
   reuses it in a fresh conversation. Retrieval is optional; if enabled,
   configure and test its embedding endpoint. You can also configure a reranker.

Comment `@<app-slug> review` on a PR to start a review. The default trigger
is **mention**. Choose **auto** in the dashboard if you want reviews on PR
events instead. Existing open PRs are not reviewed in bulk after installation.

The server saves app credentials in `/data/app.json`; app creation and its
callback then close. The `/setup` page remains available for configuration
and recovery. Until model configuration is saved, jobs are skipped without
creating PR checks. The dashboard writes `/data/revera.yaml` when you save models.

## Triggers, review modes and mentions

Set defaults in **Settings → Review** and overrides in **Repositories**.
Trigger and mode are independent; a repository can inherit either default.

| Trigger | Behavior |
|---|---|
| `mention` (default) | Review on command or manual request; keep the index warm on default-branch pushes |
| `auto` | Also review opened, reopened, updated and ready-for-review PRs, and recover missed head changes |
| `off` | Stop new reviews and indexing, including mentions and manual requests |

Changing a setting does not cancel work already queued or running. Removal
and cancellation still work when a repository is off.

| Mode | Behavior |
|---|---|
| `standard` (default) | Configured review without a profile |
| `quick` | Apply the `quick` profile for a smaller budget |
| `deep` | Apply the `deep` profile for a larger budget |

Built-in quick and deep modes scale from your configured budget, keeping
the configured strategy and validation.
Custom profiles with those names replace the built-ins. See
[profile defaults](configuration.md#profiles).

Use the app's slug in a new PR conversation comment:

```text
@<app-slug> review
@<app-slug> quick
@<app-slug> deep
@<app-slug> review quick
@<app-slug> review standard
@<app-slug> review deep
@<app-slug> help
```

`review` uses the repository's effective mode. The slug is case-insensitive.
Only OWNER, MEMBER and COLLABORATOR commenters can run commands. Bots,
comment edits, ordinary issues, quoted lines and code are ignored. Unknown
commands receive help. A valid command gets an eyes reaction and forces a
review of the current head, including drafts and otherwise skipped authors.
A closed PR gets a reply instead. Allowed-owner and off settings still apply.

When upgrading settings without a trigger field, the default becomes
**mention**, not auto. Select auto to retain automatic reviews. Legacy
`enabled: false` repository settings become off; `enabled: true` inherits.
Legacy `default_enabled: false` becomes a global off default.

### Enable mentions on an existing GitHub App

New apps request the permission and event during setup. Existing apps need
this change on GitHub before mentions work:

1. Open **GitHub Settings → Developer settings → GitHub Apps → Edit** for the
   app. For an organization app, start from **Organization Settings**.
2. In **Permissions & events → Repository permissions**, set **Issues** to
   **Read-only**. Keep the existing Pull requests, Contents and Checks
   permissions. Under **Subscribe to events**, select **Issue comment** and save.
3. Each installation owner must approve the added permission. Open **Settings →
   Applications → Installed GitHub Apps**, then the app's **Review request →
   Accept new permissions**. Organization owners use **Organization Settings →
   Third-party Access → GitHub Apps**. The link in GitHub's permission-change
   email also works.

[Issue comment delivery](https://docs.github.com/en/webhooks/webhook-events-and-payloads#issue_comment)
requires Issues read. Replies and the eyes reaction on a pull request use the
existing Pull requests write permission.

## Dashboard

**Overview** shows connection health, jobs and recent results. The notice
“Mentions need one change on your GitHub App” links to the permission change
for existing apps. **Repositories** has independent Trigger and Mode controls;
choose Default in either to inherit the server setting.

![Repository settings](images/repositories.png)

**Reviews** shows mode, trigger, findings, timing, token use and redacted logs.
You can re-run or cancel a review.

![Review detail](images/review.png)

In **Settings**, configure model routes and fallbacks, review defaults,
budgets, retrieval and access. **Settings → Review** also lists mention
commands; **Settings → Retrieval** includes optional web-search settings.
The form and raw YAML edit the same configuration. Invalid YAML leaves the
previous file unchanged.

Keys use the configuration's `api_key_env` names. Dashboard values live in
private `secrets.json`; the API returns names and sources, never values.
A saved value overrides the server environment for probes, expansion and
jobs. Deleting it falls back to the environment or reports the key as missing.
GitHub token names are reserved and cannot be model keys.

Configuration or referenced-key changes clear the affected health results.
The server tests unchecked roles at startup and each reconciliation tick;
**Test** runs a probe immediately. Setup waits for required model checks and
active retrieval keys. Results survive restarts; unchanged saves and unrelated
keys do not clear them.

## Review lifecycle

Auto reviews skip drafts, `dependabot[bot]` and `renovate[bot]` by default;
edit those preferences in Settings. Fork PRs use the CLI's fork guard and
are skipped by default. Closing a PR cancels its work. A newer head replaces
pending work and cancels an older running review.

Default-branch pushes warm the Vera index in mention and auto modes. Work
runs one job at a time per repository, up to the global concurrency limit.
Each job runs the CLI in a separate process with a 55-minute hard limit.

The **Revera** check is successful for a complete review with no findings,
neutral for accepted findings or partial work, and failed for execution
failure. Cancelled work gets a cancelled check. Only validator-accepted
findings count toward check and dashboard totals. Re-running the App's check
forces a fresh review even when the patch is unchanged.

Reconciliation runs at startup and every ten minutes by default. The first
sighting records existing PR heads without reviewing them. Later head changes
are recovered in auto mode if not already queued or running.

Failed or partial indexes keep the previous push checkpoint. Automatic
retries wait 10 minutes, then double up to 24 hours after the latest failure.
A complete index resets the wait. Push webhooks and **Index now** bypass it.

## Deployment reference

The container includes `revera`, `git` and `vera`. For a native installation,
put those binaries on `PATH` and use a writable data directory and a private
listener behind your HTTPS proxy:

```sh
revera serve --public-url https://review.example.com \
  --data-dir "$HOME/.local/share/revera" --listen 127.0.0.1:8080
```

Release images include the dashboard. A native build needs `npm ci && npm run
build` in `web/` before building Rust to include it.

### Docker Compose

Run `mkdir -p data && sudo chown -R 10001:10001 data`, then save:

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
```

Run `docker compose up -d`, then read `docker compose logs revera` for the
setup link. Keep keys out of the Compose file and repository.

### Podman Quadlet

Quadlet lets systemd start Revera at boot and restart it if it stops. For a
rootful setup, prepare the data directory:

```sh
sudo mkdir -p /srv/revera/data /etc/containers/systemd
sudo chown -R 10001:10001 /srv/revera/data
```

Create the env file with private permissions, then add
`REVERA_PUBLIC_URL=https://review.example.com` and any model key variables:

```sh
sudo install -m 600 /dev/null /srv/revera/revera.env
```

Use this file for keys when managing the server through configuration files.
[Dashboard-saved keys](#dashboard) override environment values of the same name.

Save this as `/etc/containers/systemd/revera.container`:

```ini
[Unit]
Description=Revera PR review server
Wants=network-online.target
After=network-online.target

[Container]
Image=ghcr.io/veratools/revera:0
ContainerName=revera
PublishPort=127.0.0.1:8080:8080
Volume=/srv/revera/data:/data:Z
EnvironmentFile=/srv/revera/revera.env
NoNewPrivileges=true
AutoUpdate=registry

[Service]
Restart=always
TimeoutStartSec=300

[Install]
WantedBy=multi-user.target
```

Run `sudo systemctl daemon-reload && sudo systemctl start revera`.

For rootless Podman, put the unit in `~/.config/containers/systemd/`, use
absolute paths under your home directory for the data and env files, and
prepare the data directory with `podman unshare chown` as in the quickstart.
Change `WantedBy` to `default.target`, then run
`systemctl --user daemon-reload && systemctl --user start revera`.
Run `loginctl enable-linger "$USER"` so it starts at boot without a login.

`AutoUpdate=registry` lets `sudo podman auto-update` pull new `:0` images and
restart the service. For scheduled updates, run
`sudo systemctl enable --now podman-auto-update.timer`. For rootless Podman,
omit `sudo` and use `systemctl --user` for the timer.

### Server options

| Environment variable | Default | Flag |
|---|---|---|
| `REVERA_PUBLIC_URL` | required HTTPS URL | `--public-url` |
| `REVERA_LISTEN` | `0.0.0.0:8080` | `--listen` |
| `REVERA_DATA_DIR` | `/data` | `--data-dir` |
| `REVERA_CONFIG` | `/data/revera.yaml` | `--config` |
| `REVERA_CONCURRENCY` | `2` | `--concurrency` |
| `REVERA_RECONCILE_MINUTES` | `10` | `--reconcile-minutes` |

`REVERA_ALLOWED_OWNERS` adds comma-separated account or organization logins
to the app owner's allowed installations, merged with dashboard preferences.

For file-based setup, write a [model configuration](configuration.md) to
`data/revera.yaml`. Enter its keys in the dashboard, or export the named
variables and pass them to the container with `-e REVIEW_API_KEY` (Compose:
`REVIEW_API_KEY: ${REVIEW_API_KEY}` under `environment`). `${VAR}` expansion
works as it does in the CLI. A present but invalid configuration prevents
startup; a file named explicitly with `--config` must exist.

### HTTPS reverse proxy

Caddy can obtain and renew certificates:

```caddyfile
review.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

With existing certificates, nginx can forward requests:

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

A tunnel also works if it provides a stable HTTPS URL and forwards webhook
bodies and signature headers unchanged.

### Per-repository configuration

A `revera.yaml` on the repository's default branch replaces the server
configuration in full, rather than merging. For a PR targeting another branch,
configuration comes from that PR's base commit. A PR cannot change its own
review configuration. Without that file, the server default is used.

Web search is separately opt-in through trusted [`web_search`](configuration.md#web_search)
settings. Queries go to Exa; private or unknown repositories also need
`allow_private: true`. Free hosted Exa needs no key. An optional `EXA_API_KEY`
selects keyed REST. Store keys in the dashboard or server environment, never YAML.

### Data directory

```text
/data/revera.yaml             default review configuration
/data/app.json                app credentials and OAuth client secret (0600)
/data/settings.json           admins, allowed owners and repo preferences
/data/secrets.json            dashboard-managed environment values (0600)
/data/health.json             redacted model probe results
/data/inventory.json          last successful installation/repository inventory
/data/session-key             cookie signing key (0600)
/data/bootstrap-admin         first-admin bootstrap marker (0600)
/data/setup-token             setup token (0600; deleted after setup)
/data/seen.json               reconciliation checkpoints
/data/mentions.json           recent @mention dedupe records (up to 4096)
/data/repos/<owner>/<repo>/   reusable checkouts and warm Vera index
/data/cache/<owner>/<repo>/   repository-isolated Vera home and cache
/data/jobs/<id>/meta.json     status, timestamps, findings and token counts
/data/jobs/<id>/report.json   redacted CLI report, when available
/data/jobs/<id>/log.txt       redacted child output
```

Removing an installation or repository deletes its workspace and cancels its
active jobs. Shutdown and restart mark unfinished jobs cancelled. Stored
check identity lets startup cancel abandoned checks with fresh scoped tokens.
The newest 500 jobs are retained; active work is not pruned.

Event payloads (`jobs/<id>/event.json`) are removed when a run ends; a crash
can leave one behind. Review state is seeded from the managed PR summary,
not a previous checkout. Back up the whole data directory and treat it as private.
To restore, put back the whole directory, including `bootstrap-admin` and
`mentions.json`, preserving ownership (container UID 10001), before starting
the server.

## Upgrading

Back up the data directory, then update with the same volume:

- Compose: `docker compose pull && docker compose up -d`.
- Quadlet: `sudo podman auto-update`, or `sudo podman pull ghcr.io/veratools/revera:0`
  followed by `sudo systemctl restart revera`. For rootless Podman, omit `sudo`
  and use `systemctl --user`.
- Plain `podman run`: pull with `podman pull ghcr.io/veratools/revera:0`, run
  `podman rm -f revera`, then repeat the original run command with the same volume.
  Restarting the old container alone does not use the new image.

Check the [trigger migration](#triggers-review-modes-and-mentions) and
[existing-app permission step](#enable-mentions-on-an-existing-github-app).

Configuration can name an expected Vera version, but the CLI does not enforce
it during review. Keep that setting aligned with the image's Vera release.

Older servers without a dashboard may have no `client_secret` in `app.json`.
Add `https://<your-host>/auth/callback` in the App's authorization callback
URLs and generate an OAuth client secret. Stop the server and add the
`client_secret` field without changing other credentials or file permissions.
If absent, create `settings.json` with `{"admins":["your-github-login"]}`.
Restart and sign in. Do not delete a bootstrap marker to reopen enrollment.
Old flat job logs are not imported; archive them separately if needed.

## Security model

The App is private. Installations are accepted only for its owner and allowed
owners; dashboard access is limited to administrators. Webhook signatures are
checked before parsing. Setup is token-gated, checks callback state and closes
when the App is configured.

Sessions use signed, HttpOnly, Secure, SameSite=Lax cookies and last 14 days.
Dashboard writes also need a CSRF token. Sign out from the avatar menu.
`/healthz` returns `ok`; repository and review data require an administrator.

The App private key, webhook secret and OAuth client secret stay in
`app.json`, never child arguments or environment. Jobs get short-lived
installation tokens scoped to one repository and its required permissions.
Git authentication uses environment configuration, not command arguments or
`.git/config`. Model keys come from the dashboard or server environment after
configuration trust checks.

Base-branch configuration is trusted administrative input. Review changes
to it carefully. Jobs have process isolation, not a sandbox for arbitrary
untrusted executables. Use the same precautions as in Actions.

### If a secret leaks

- **GitHub App credentials:** replace the affected credential in the App's
  GitHub settings and revoke the leaked private key or client secret. Stop
  Revera and update the matching field in the data directory's `app.json`:
  `pem` (private key), `webhook_secret` or `client_secret`. Keep valid JSON
  and mode `600`, then restart.
- **Session signing key:** stop Revera, delete `session-key` from the data
  directory and restart. A new key is generated, signing everyone out.
- **Model API keys:** rotate them at the provider, then update the env file
  and restart, or update the dashboard key. Remember that
  [dashboard-saved keys](#dashboard) override matching environment values.

## Server API

Dashboard API writes require an administrator session and CSRF token.

- `GET /api/settings/server` returns preferences, also under `server` in
  `GET /api/settings`. `PUT /api/settings/server` replaces
  `{admins, allowed_owners, default_trigger, default_mode, repos, review_drafts,
  skip_authors}`. Triggers are `mention|auto|off`; modes are `quick|standard|deep`.
  `repos` maps full repository names to `{trigger: value|null, mode: value|null}`.
- `GET /api/repos` includes effective `trigger` and `mode`, nullable
  `trigger_override` and `mode_override`, and index/job metadata. A waiting
  index retry has `index.retry_at`. `pushed_at` is the last push as an RFC 3339
  UTC timestamp, omitted when unknown or invalid. `open_prs` is the open PR count
  from the last reconciliation (normally every 10 minutes), omitted for older
  inventories that have no count. `review_count` counts review jobs in retained
  history; `finding_count` sums their findings, excluding index jobs.
  `last_job` is the newest job of any kind; `last_review` is the newest review job.
- `PATCH /api/repos/:owner/:repo` accepts `{trigger?: value|null, mode?: value|null}`.
  Omitted fields are unchanged; null inherits. Legacy `{enabled: false}` maps
  to off and `{enabled: true}` clears the trigger override for one release.
- `PATCH /api/repos` applies the same trigger/mode changes to a batch:
  `{"repos": ["owner/a", "owner/b"], "mode": "deep"}`. Include 1–1000 known
  repository names and at least one of `trigger` or `mode`. Omitted fields stay
  unchanged; null clears the override. An unknown repository returns HTTP 404;
  an empty/oversized batch or missing settings returns HTTP 422. Invalid batches
  change nothing. Success saves preferences once, sends one `repos` live event
  and returns `{"ok": true, "updated": 2}` for this example.
- `POST /api/repos/:owner/:repo/pulls/:number/review` and
  `POST /api/jobs/:id/rerun` accept optional `{mode?: value}`. Absent or null
  mode uses the repository mode. Success returns `{id}`. Off repositories
  return HTTP 409, including for manual indexing.
- `GET /api/overview` includes `app.issue_comment_subscribed` (boolean, or
  null when registration metadata is unavailable) and `app.settings_url`.
  Review summaries include `mode` and trigger `mention|auto|manual|rerun`.
  `last_24h` and `last_7d` group jobs by their queue time. Each has `reviews`,
  `failed` (failed reviews only), `findings`, `severity: {high, medium, low}`,
  `tokens` (prompt + completion), `prompt_tokens`, `cached_tokens` and
  `median_duration_ms`. Findings and severity count review jobs only; tokens
  count all jobs. The duration median uses complete, partial and failed reviews
  with recorded durations, averaging the two middle values when needed; it is
  null when none exist. `history_since` is the oldest retained job's queue time,
  or null for no jobs. History keeps the last 500 jobs, so a window may be only
  partly covered.
- `GET /api/jobs` accepts `q` for a case-insensitive substring search of title,
  author or repository. Digits, optionally prefixed with `#`, also match the PR
  number. Empty `q` does not filter. Search combines with `repo`, `status` and
  `kind`; `limit`, `before` and response cursor `next` paginate matching jobs.
- `GET /api/jobs/:id` includes the report's `stats` object and `strategy` string
  when present, alongside the existing `timing` and `ledger` objects. Older or
  missing reports omit these fields. `stats` includes candidate/validation
  counts, retrieval and tool use; `timing` contains phase durations; `ledger`
  contains requests, token use and per-route totals. All API output is redacted.

## Troubleshooting

- If the server will not start, check the data directory's ownership, HTTPS
  public URL and any existing YAML file. A missing default config is allowed
  for first setup; an invalid one is not.
- If first-time setup returns 401, use the exact logged link.
  A rejected sign-in usually means the login is not an administrator or first
  setup finished in a different browser.
- If a connection failure returns you to setup, check the individual
  investigator, validator and embedding results. Signed-in administrators can
  use **Open settings** or visit `/settings` to repair the saved configuration.
  Setup completion still requires successful tests for the configured connections.
- For an embedding HTTP 400, check the endpoint, key and supported model ID.
  Embedding models produce search vectors; rerankers reorder search results and
  cannot replace an embedding model. To continue without semantic search, turn
  off **Use Vera retrieval** under **Repository search**, test connections again,
  then finish setup. Retrieval is optional, but its embedding test must pass
  while API retrieval is enabled.
- If mentions do nothing, check the App's Issues permission, Issue comment
  subscription and installation approval. Confirm the repository is not off
  and the commenter is an owner, member or collaborator.
- If auto reviews do not start, check the trigger, allowed owners, installation
  access, drafts and skipped authors. Installing does not backfill open PRs.
- For partial or failed reviews, open the redacted job log in Reviews and
  check key names, endpoints and the report's stop reason. Fork skips are partial.
- For Vera failures, confirm the binary is on `PATH` and embedding keys are
  present. Do not share an explicit `vera.home` or `REVERA_VERA_HOME` between
  concurrent repositories.
- For webhook 401 responses, check signatures, proxy body handling and that
  `/data/app.json` belongs to this App.
