# Revera dashboard

Requires Node 24 or later. The SPA is served by `revera serve` on the same
origin as its API; it does not store credentials or session tokens locally.

```sh
cd web
npm ci
npm run dev
```

`npm ci` runs `svelte-kit sync` through the prepare lifecycle to generate
Kit 3's `$app/tsconfig` and route types. Dev startup also syncs them, so
removing `.svelte-kit` does not require a separate manual command. Complete
dependency installation before starting Vite; reinstalling dependencies
while Vite is running can remove the generated config beneath it.

Without a running server, use the stateful in-browser fixtures:

```sh
VITE_MOCK=1 npm run dev
```

Mock mode starts with a signed-in demo workspace. Visit
`/setup?stage=create_app&token=demo` or `/setup?stage=configure` to preview the
wizard. These stage controls only exist in mock mode. Mock changes last until
the page is refreshed; mutations emit the same job/repository events used by
the live dashboard. Mock requests never contact a model endpoint or GitHub.

Visit `/setup?stage=create_app&authorized=0` to preview the missing-setup-link
panel. `authorized=1` simulates a browser that already holds the setup cookie.
These authorization controls also only exist in mock mode.

```sh
npm run check
npm run build
```

`npm run format` formats the dashboard with Prettier. `npm run lint` checks
formatting, and `npm run check` runs that check before Svelte diagnostics.

The static SPA is written to `build/` with `index.html` as its fallback.
Build without `VITE_MOCK` for release. `build/.gitkeep` is kept so Rust can
build before the dashboard has been compiled.

With the mock dev server running, `npm run check:browser` exercises the
wizard, settings, YAML comment preservation, secrets, and repository actions.
It also captures desktop screenshots in both themes and a mobile overview in
`$HOME/.local/tmp/revera-server/shots/`. Set `CHROMIUM_PATH` to a local Chromium
executable if it differs from the script's workstation default. Use
`-- --screenshots-only` to capture without repeating the interaction checks.
Remove generated `build/` files (keeping `.gitkeep`) after local builds;
release assets are produced by CI, never from mock mode.

Settings forms and the YAML editor share a comment-preserving YAML Document.
“Validate YAML” checks syntax locally; Save performs the API's complete trust
and strategy validation. Tests and model lists use saved configuration.
