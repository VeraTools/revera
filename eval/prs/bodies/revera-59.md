- **Pull requests page**: open PRs across installed repositories with their Revera review state, filters, and review / fix / cancel actions. A PR page shows the description and the GitHub conversation (threads nested under reviews, resolved threads and Revera's own comments collapsed) next to Revera's reviews, jobs and fixes. Findings still open from earlier reviews count as findings.
- **Repository guidance**: `agents` falls back to `AGENTS.md`, then `CLAUDE.md`; new `skills` level lists `.agents/.claude/.opencode` skills for `read_skill`. All from the base commit; validators get the same guidance. `review.notes`, `--notes-file` (outside the checkout in event mode) and `--guidance`. Per-repository level and notes with a preview under Repositories → Review guidance. Default stays `off`.
- **Fixes from the server audit**: tini plus non-detached Git maintenance (zombie git processes), empty diffs finish without model calls (evaluation runs leave state alone), outside-diff findings listed once, no empty Trigger/Impact lines.
- Agent notes and the Revera skill corrected.
- Version 0.10.0.

Checks run locally: fmt, clippy, cargo test (539 passed), web check/build, browser check (mock), versions, docs hygiene, install/action/release-verify/eval script tests. Live GitHub GraphQL detail and guidance preview are covered by wiremock only.
