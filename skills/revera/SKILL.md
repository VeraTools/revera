---
name: revera
description: Set up, configure, run or diagnose Revera, the AI pull-request reviewer (revera.yaml, the Revera GitHub Action, the revera CLI, Vera retrieval for Revera, or a Revera run that came back partial/failed). Not for general code review requests.
---

# Using Revera

Revera reviews a PR diff with an investigator model, re-checks every
candidate with a fresh-context validator, and publishes only validated
findings. Use this skill only for Revera itself.

## Workflow

1. **Find the config.** `revera.yaml` at the repository root, or the path in
   the Action's `config` input. Start from `revera.example.yaml`; every key is
   in `docs/configuration.md`.
2. **Configure routes** — see `references/providers.md`. Keys are referenced
   by environment-variable *name* (`api_key_env`). Never write a key value into
   YAML, a workflow, a commit or a chat message; ask the user to add it as an
   Actions secret or export it in their own shell.
3. **Optional retrieval** — see `references/retrieval.md`.
4. **Static check:** `revera doctor --config revera.yaml`. It validates the
   config, the routes and that the named key variables are set. It makes no
   provider calls, so a passing doctor does not prove a key or model works.
5. **Live run** (spends provider tokens; confirm with the user first):
   `revera review --repo . --base origin/main --head HEAD` prints a dry-run
   plan. Only `publish: comment` (the Action's default) posts to a PR.
6. **Action setup** — see `references/setup.md`.
7. **Diagnose** a `partial`/`failed` run — see `references/troubleshooting.md`.

## Rules

- Do not set `review.validate: false` for real reviews; it is for
  evaluation and cannot be combined with `publish: comment`.
- Keep `review.guidance` at `off` unless the user asks for it.
- Report what was actually checked: say "doctor passed" or "live review ran",
  not "working", when only one of them happened.
