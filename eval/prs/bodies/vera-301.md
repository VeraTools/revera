Fixes found by auditing real agent sessions. The agents kept hitting missing indexes, shell timeouts on a first index, and leftover index files.

**Agent guidance** (managed AGENTS.md snippet and skill)
- The snippet and the skill description now say "in a repository with a Vera index" instead of making Vera the unconditional first search. Read-only agents with no shell were reporting BLOCKED because the old wording required Vera.
- A new paragraph covers missing indexes. Run `vera index .` in a code repository you'll work in. A first index can take minutes, longer than typical 120 s agent shell timeouts, so use a long timeout or run it in the background, and use rg until it finishes. Otherwise use your usual search tools.
- Existing snippets, marked or legacy, are still detected, and `vera agent install` / `sync` replaces them. Tests cover both.

**One no-index error**
- search, grep, structural, references, overview, update, and stats share one message that includes the fallback: "no index found in <dir> or any parent directory. Run `vera index .` from the repository root (a first index can take minutes), or search with rg until it exists." The resume hint is kept.
- overview, update, and stats resolve the nearest ancestor index the same way search does. `vera update` without an index now points to `vera index` instead of building one.

**Index files kept out of git**
- When an index is created or updated inside a git work tree, Vera adds `/.vera/`, `/.vera.build/`, `/.vera.resume/`, and `/.vera.lock` to the repository's local exclude file. The file is resolved with `git rev-parse --git-path info/exclude`, so linked worktrees and submodules work. Nested index roots get top-relative escaped paths.
- Only missing lines are added, under one comment, under a file lock. Each root is handled once per process, so watch mode doesn't spawn git on every update. Failures are silent apart from debug logs, and roots with line breaks are skipped.
- Docs no longer tell users to edit `.gitignore`. `.vera.lock` is still never deleted, because removing it would race with flock.

**`vera references --path`**
- Repeatable `--path` globs filter callers and callees with the same rules as search.

**fix(deps)** (second commit): clears the cargo-deny advisories that currently fail Validate on master. zip goes from 2 to 9 (RUSTSEC-2026-0367/0368/0369); entry names are now fallible, and a new test covers wheel extraction. webpki-roots and webpki-root-certs go to 1.0.10 (RUSTSEC-2026-0381/0383), and rustls-webpki to 0.103.16 (RUSTSEC-2026-0382).

Checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test` for vera-core, vera-cli, and vera-mcp pass locally. The workspace tests also passed on the first revision. Built-binary smoke test: in a fresh git repo, `vera search` prints the new message. `cargo deny check` passes after the dependency commit.
