## Problem
The App re-indexed constantly with no PRs open:
- every default-branch push queued an index, and
- every 10 minutes reconciliation queued an index whenever GitHub's repo `pushed_at` changed. GitHub bumps that for pushes to **any** branch or tag, so an unchanged `main` was re-indexed over and over (production: four runs in 40 minutes at the same commit).

Reviews already update their own index (`src/pipeline/common.rs` `ensure_index`), so pre-indexing only buys first-review latency, at the cost of embedding requests for every push.

## Change
- New server setting `index_on_push` (**off by default**), shown in Settings → Review as *Index default-branch pushes*.
- Mention mode: a reviewable PR that opens, reopens or becomes ready, and targets the default branch, warms the default-branch index unless that commit is already indexed. Auto mode needs nothing extra, because its review indexes.
- `SeenRepo.indexed_sha` records the commit a successful index fetched. A failed index clears it, since the old index may have been discarded.
- With push indexing on: push webhooks skip an already-indexed `after` commit, and reconciliation looks up the default-branch head and only indexes when it moved. If the lookup fails, it queues the index as before. Retries and `index.retry_at` only apply when the setting is on.
- Jobs remove stale `.git/index.lock` and `HEAD.lock` left by an interrupted checkout. One left behind on Oct 19 has failed every `VeraTools/revera` job since. Other locks are left alone, because the fix controller shares the git dir (it only runs rev-parse, cat-file and fetch-by-sha, which never take these two).
- Dashboard shows a "Pull request" trigger label; docs and changelog updated.

## Checks
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`: pass
- `cargo test` (with `REVERA_TEST_VERA`): 517 passed, 0 failed
- `check-versions`, `check-docs-hygiene`, install/action/release-report/eval script tests: pass
- `web`: `npm run check` and `npm run build`: pass
- Live-provider checks and the App server smoke test: not run
