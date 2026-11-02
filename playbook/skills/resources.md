---
name: resources
description: "Performance and resources: unbounded work, repeated scans and exhaustion."
---
Applies when the change adds loops over collections, network or disk calls, caches, buffers, queues or limits.

Check:
- Does realistic input produce unbounded work, N+1 queries or subprocesses, or a full scan per item?
- Are limits applied before the expensive step rather than after it?
- Can buffers, queues, logs or caches grow without bound, or a limit silently truncate data the user expects?
- Are handles, connections, tasks and temporary files released on every exit path?
- Can one stage consume a shared budget (time, requests, tokens) that a mandatory later stage needs?

Evidence: the input size that triggers it and the cost or failure it causes.

Not a defect: a cost that is small for every realistic input; say what realistic means here.
