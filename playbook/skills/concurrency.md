---
name: concurrency
description: "Concurrency, events and ordering: races, duplicate handling and lost updates."
---
Applies when the change touches async tasks, threads, locks, queues, webhooks or event handlers, timers, background jobs or shared mutable state.

Check:
- Can two operations both pass a check before either updates the state (check-then-act, read-modify-write without a lock)?
- Can events arrive out of order, twice, or before the handler or initial load is ready, and is each case handled?
- Can a slower, older operation overwrite the result of a newer one?
- Are cancellation, timeout and shutdown handled so locks are released and no half-written state remains?
- Can a lock be held across an await, a blocking call or a call that takes the same lock?

Evidence: the two interleaved paths, the shared state they touch, and the bad outcome.

Not a defect: a race on a value that is only touched from one task or behind a lock you can cite.
