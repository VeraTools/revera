Adds `vera index --full-text-only`, which builds a keyword (BM25) index without creating or loading an embedding provider. Grep, structural queries, references, and overview work as usual; search ranks by keyword match only. Based on the full-text-only part of #302 by @ple1n, reworked to go through the existing pipelines.

## What changes

- The full-index and update pipelines take `Option<&P>` for the provider. With no provider the embed stage is skipped, so parsing, staging, and publication are the same code path as an embedded build. No `vectors.db`, checkpoint, or model metadata is written; the index records `index_mode = full_text_only`.
- The index's mode decides what updates do: a keyword index is never embedded (a provider passed in is ignored, and vector-coverage repair is skipped), and an embedded index updated without a provider fails with a clear error.
- `SearchContext::search` returns keyword results with no fallback warning for keyword indexes, read from the cached index metadata.
- `vera update`, `vera search`, the MCP watcher, and MCP `search_code` skip provider creation for keyword indexes, so no model is downloaded or loaded.
- `vera index` takes an options struct instead of positional flags. When the provider can't be created, the error points at `--full-text-only`.
- `vera stats` and the index summary show the mode. JSON output is unchanged.
- Running `vera index .` without the flag rebuilds with embeddings. There are no confirmation prompts.

## Tests

- Core: building a keyword index and converting both ways; keyword updates with and without a provider (asserting no embed calls and the mode is kept); an embedded index updated without a provider errors; search on a keyword index with a working provider, a failing provider, and no provider.
- CLI: `crates/vera-cli/tests/full_text_only.rs` runs index, search, grep, structural, references, overview, update, and stats with an embedding config that cannot work.

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, and `cargo test --workspace` pass locally. `cargo deny` needs #303.
