---
name: state
description: "State, caches, indexes and lifecycle: stale data, skipped invalidation and identity keys."
---
Applies when the change reads or writes caches, indexes, stored state, sessions, temp or staging files, or records that something is done.

Check:
- Can an early return, empty input or error path skip the step that publishes, replaces, invalidates or cleans up state, leaving stale data live?
- Is something recorded as done, warm, indexed or migrated when the work did not actually happen (disabled feature, no-op, partial failure)?
- Does a cache or identity key include every input that changes the cached content, and only those?
- Can a retry, resume or restart reuse state that belongs to another commit, configuration, user or operation?
- Are backups, staging and lock artifacts removed or excluded on every exit path, including interruption?

Evidence: the sequence of steps that leaves the bad state, and where that state is later read.

Not a defect: stale state that is provably rebuilt before anyone reads it.
