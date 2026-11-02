---
name: contracts
description: "Cross-file and API contracts: callers, implementations and consumers of changed behavior."
---
Applies when a function signature, return value, error type, default, public type, trait, schema, route or serialized field changes.

Check:
- Which unchanged callers depend on the old behavior, and do they still work? Search for every use of the changed symbol, string or key.
- Were all implementations of a changed trait or interface, and all match arms over a changed enum, updated?
- Do serialized keys, JSON fields, file formats, API responses and persisted data stay compatible with existing readers and stored data?
- Does a changed error or empty result mean something different to callers (`None` vs error, empty list vs failure)?
- When the PR adds a new mode or flag, does every code path that branches on the old modes handle it?

Evidence: the changed definition plus at least one consumer `path:line` that breaks.

Not a defect: a consumer you hypothesize but cannot find; drop it, or mention the uncertainty in `coverage`. Reserve `not_checked` for changed code you did not read.
