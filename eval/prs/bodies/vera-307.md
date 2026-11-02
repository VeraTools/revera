Lets you query a second index without custom index locations. Each tree keeps its own `<root>/.vera`. The use case comes from @ple1n (#302): searching a project plus an index of the local cargo crate cache.

**CLI**
- `vera -C <DIR>` (`--directory`) changes directory before running the command, like `git -C`. It is accepted only before the subcommand:
  ```bash
  vera index ~/.cargo/registry/src --full-text-only
  vera -C ~/.cargo/registry/src search "retry backoff"
  ```
- `vera grep` gets `-C` as the short form of `--context`, matching grep and rg habits. `vera grep foo -C 3` used to be a parse error. Because the top-level `-C` is not global, the two never collide: `vera -C dir grep foo -C 1` works.

**MCP**
- Every tool accepts an optional `repo` argument. The existing search context is keyed by configuration, not repository, so one long-running server already serves any number of indexes with a single loaded model.
- A `repo` other than the server's working directory must already be indexed. It is never auto-indexed or watched, so a wrong path such as `/` cannot start a filesystem-wide index or a huge inotify watch.
- Omitting `repo` keeps today's behavior.

This replaces the `--save`/`--index-dir` plus `load_index`/`list_indexes` design from #302. Vera derives the repository root from the index directory in many places, so moving indexes would mean threading an explicit root through about 25 files. The use case works without that.

Tests:
- CLI: `-C` with absolute and relative directories, `--directory`, and grep `-C N`.
- MCP: all seven tools against a secondary keyword index, refusal of an unindexed `repo` without creating anything, and root resolution.
