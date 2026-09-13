# Brief: review fixes + M3 GitHub integration + live model probe

## A. Fixes from review of M1/M2

1. `agent.rs`: after the "Budget exhausted" notice, if the next completion
   returns non-terminal tool calls the loop currently runs them and spins
   forever (the `else if` requires `extra_completion_after_budget`, which was
   already cleared). Fix: once `budget_notice_sent` is true and the completion
   is not terminal, return `StopReason::ToolBudget/TimeBudget` immediately
   (do not execute more tools). Add a stub-client unit test for this path.
2. `baseline.rs`: when `vera.ensure_index()` fails, push
   `"retrieval unavailable: <err>"` into `partial_reasons` (status partial) and
   make the vera tools return `{"error":"retrieval unavailable"}` fast instead
   of re-invoking vera each call (a `ToolBox::disable_vera(reason)` flag).
3. Short-circuit path: set `state.reviewed_head = head_sha` before `state.save`.
4. `posted` flag: the pipeline must NOT set `posted=true`. Introduce
   `ReviewState::mark_posted(ids)` called only by the publisher after a
   successful GitHub review post. Dry-run leaves posted=false. Findings that
   are `Open` but not posted are re-validated (recheck) on the next run and,
   if still accepted, are eligible for posting then.
5. `ValidationStatus::Uncertain` -> `FindingState::Uncertain` (new variant),
   not Open; uncertain findings are rechecked next run like open ones but are
   never posted inline.

## B. GitHub mode (src/github/{mod.rs, event.rs, api.rs, publish.rs})

- `revera review --event <path>` (also honors `GITHUB_EVENT_PATH` when
  `--event` is given without a value... keep simple: `--event <path>`
  required). Parse `pull_request` / `pull_request_target` payloads:
  `repository.full_name`, `number`, `pull_request.{title, body, head.sha,
  head.ref, head.repo.full_name, base.sha, base.ref}`. Unsupported event ->
  error. `--repo` defaults to cwd (the checked-out head).
- Ensure `base.sha` is present locally; if `git cat-file -e` fails run
  `git fetch --no-tags --depth=1 origin <base.sha>` (then merge-base may be
  missing; fall back to `git diff base.sha head.sha` two-dot when
  `merge-base` fails, and log it).
- `GitHubApi { base: https://api.github.com (env GITHUB_API_URL override), token }`
  via reqwest with `Accept: application/vnd.github+json`, UA `revera`.
  Methods: `get_pull(owner, repo, n) -> head_sha`, `list_issue_comments` (paginate
  per_page=100), `create_issue_comment`, `update_issue_comment`,
  `create_review(commit_id, event: "COMMENT", body, comments[])` with
  `comments[i] = {path, line, side:"RIGHT", body}` (+ `start_line`, `start_side`
  when end_line > start_line). Error bodies excerpted (<=300 chars).
- State on GitHub: find the managed summary comment by
  `github.summary_marker`; the state blob is in the same comment as
  `<!-- revera-state:<base64(json)> -->`. In event mode, load state from
  there (fallback empty) instead of `.revera/state.json`, and save to the
  comment on every run (also in dry-run? NO — dry-run must not write to
  GitHub; dry-run in event mode writes the plan to `--out` and to
  `.revera/state.json`).
- Publisher (`publish: comment`): (1) re-fetch PR head; if != analyzed head
  -> do not post anything, report status `partial`, reason "head moved
  <old> -> <new>", exit 2. (2) create review with inline comments for
  accepted+Inline findings whose ids are not `posted` (cap
  `review.max_findings`); if none, skip the review call. (3) upsert summary
  comment (marker + summary markdown + state blob), where the summary lists
  all currently open accepted findings including previously posted ones, and a
  "Resolved since last review" line when rechecks resolved anything. (4)
  mark posted ids. Never include the state blob in the review body.
- Fork guard: if `head.repo.full_name != repository.full_name` and
  `publish == comment` and `github.allow_forks` (new config key, default
  false) is false -> downgrade to dry-run with a logged reason and summary
  note "fork PR: publication skipped".
- `--publish comment` outside event mode -> error "requires --event".
- Report: `RunReport.publication: { mode: dry-run|comment, review_id?, summary_comment_id?, skipped_reason? }`.
- Tests: event parsing fixture (`tests/fixtures/pr_event.json`), state blob
  encode/decode round-trip, publisher against wiremock: head-moved refuses;
  happy path posts review + creates summary; second run updates the existing
  summary comment and posts only unposted ids; fork downgrade.

## C. Action + release

- `action.yml` (composite) at repo root. Inputs: `config` (default
  `revera.yaml`), `profile` (""), `strategy` (""), `publish` (default
  `comment`), `fail-on` (`failed` | `partial` | `never`, default `failed`),
  `revera-version` (default `0.1.0`; the release workflow bumps this),
  `vera-version` (default `1.4.1`), `github-token` (default `${{ github.token }}`),
  `cache` (default `true`). Outputs: `status`, `report-path`, `findings`.
  Steps: (1) `actions/cache/restore@v4` for `.vera` keyed
  `revera-vera-${{ runner.os }}-${{ inputs.vera-version }}-${{ hashFiles(inputs.config) }}-${{ github.event.pull_request.base.ref }}-${{ github.sha }}`
  with restore-keys dropping the sha then the ref; (2) install vera: download
  `https://github.com/VeraTools/Vera/releases/download/v<ver>/vera-x86_64-unknown-linux-gnu.tar.gz`
  + `release-manifest.json`, verify sha256 from the manifest, put on PATH;
  (3) install revera: download `https://github.com/VeraTools/revera/releases/download/v<ver>/revera-x86_64-unknown-linux-gnu.tar.gz`
  + `.sha256`, verify, put on PATH; (4) run `revera review --event "$GITHUB_EVENT_PATH" --config ... --publish ... --out .revera/report.json`
  with `GITHUB_TOKEN` from the input, capturing exit code, set outputs, then
  apply `fail-on`; (5) `actions/cache/save@v4` (always, `if: always()`) with
  the full key. Linux x86_64 only; error clearly on other runners.
- `.github/workflows/release.yml`: on tag `v*`: build release binary on
  ubuntu-latest, package `revera-x86_64-unknown-linux-gnu.tar.gz` + `.sha256`,
  create GitHub release with `softprops/action-gh-release@v2`, then force-move
  the major tag (`v1`) to the release commit (`git tag -f v1 && git push -f origin v1`)
  when the tag is `v1.*`.
- `.github/workflows/self-review.yml`: on `pull_request` in this repo, runs
  `uses: ./` with `publish: comment` only when `secrets.OPENROUTER_API_KEY` is
  set (dogfood). Use a `revera.yaml` at repo root: investigator = the cheap
  model, validator = the strong model chosen in part D, vera api via
  OPENROUTER_API_KEY, strategy baseline, publish comment.
- README: usage block
  ```yaml
  - uses: actions/checkout@v4
    with: { fetch-depth: 0 }
  - uses: VeraTools/revera@v0
    with: { config: revera.yaml }
    env: { REVIEW_API_KEY: ${{ secrets.REVIEW_API_KEY }}, OPENROUTER_API_KEY: ${{ secrets.OPENROUTER_API_KEY }} }
  ```
  plus permissions `pull-requests: write, contents: read`.

## D. Live model probe (do this early; it de-risks the agent loop)

Query `https://openrouter.ai/api/v1/models` (no auth needed) and pick:
- CHEAP: 2 models with `"tools"` in `supported_parameters`, prompt price
  <= $0.30/M and completion <= $1.0/M, from different vendors (prefer
  qwen/deepseek/google-flash-lite/mistral/xai-mini style; avoid `:free`).
- STRONG: 1 model with tools, prefer in order: `anthropic/claude-sonnet-4*`,
  `openai/gpt-5*` (non-nano), `google/gemini-2.5-pro`/`gemini-3*-pro`.
Record ids + prices in `docs/EVAL.md` (create; this file will grow in M5).
Then write `fixtures/configs/live-openrouter.yaml` (investigator=CHEAP#1,
validator=STRONG, base_url https://openrouter.ai/api/v1, api_key_env
OPENROUTER_API_KEY, extra_headers `HTTP-Referer: https://github.com/VeraTools/revera`, `X-Title: Revera`)
and run the crossfile fixture `break` and `clean` and `fix` sequence with it
(dry-run). Record in docs/EVAL.md: did the investigator call
`vera_references`/`vera_search`, was the caller bug found, validator
verdict, tokens per route, wall time, cost estimate. If a model ignores
tools or returns malformed calls, note it and try CHEAP#2; fix genuine
adapter bugs you find (e.g. content-array messages, `reasoning` fields,
`tool_calls` with `index`, empty `content: null` handling).

## Verification (mandatory)

`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
`fixtures/run-fixture.sh`, the live probe (report the numbers), and
`act`-free static check of action.yml via `python3 -c 'import yaml,sys;yaml.safe_load(open("action.yml"))'`.
Update STATUS.md. Commit on main in logical commits, do not push. Report the
live probe results verbatim (summary markdown + ledger) and any deviations.
