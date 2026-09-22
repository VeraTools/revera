# Focus addenda appended to the investigator prompt in panel mode.
# Section headers are the `focus` values; the body under each is appended verbatim.

## general
Cover the whole changed surface with no special emphasis.

## cross-file
Concentrate on effects outside the diff: every changed public symbol's callers, implementors of changed traits/interfaces, tests and fixtures encoding the old behavior, and config/serialization consumers. Spend most tool calls on `vera_references`, `vera_grep`, and reading the callers you find. Do not spend effort on defects fully visible inside a single hunk.

## security
Concentrate on trust boundaries the PR touches: input validation removed or weakened, authorization checks, path/URL/shell/SQL construction from untrusted data, secret handling, deserialization of external data, and error paths that leak or fail open. Report only reachable issues with a concrete attacker-controlled input.

## concurrency
Concentrate on ordering, locking, shared mutable state, async cancellation, retries, idempotency, and races introduced by the change. Trace who calls the changed code concurrently.

## data
Concentrate on data integrity: units and scales, off-by-one, null/empty handling, numeric overflow/precision, schema and migration compatibility, ordering assumptions, and silent truncation. Verify against callers that produce or consume the data.

## architecture
Concentrate on structural design and system boundaries: circular dependencies, layering violations, leaky abstractions, public API contract drift, and coupling introduced between modules that should remain decoupled. Check whether the change respects existing architectural layers and separation of concerns.

## regression
Concentrate on backward compatibility and behavior preservation: breaking changes to public function signatures, traits, or serialized formats, altered error variants, unintended side effects in existing code paths, and regressions against tests or fixtures encoding legacy behavior.

