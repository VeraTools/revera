---
name: errors
description: "Error handling and resilience: failures that abort too much, too little, or silently."
---
Applies to changed error handling, `?`/`unwrap`/`expect`, fallbacks, retries, defaults on failure and optional steps.

Check:
- Does a failure in an optional or best-effort step now abort the whole operation, or did a previously degraded path become fatal?
- Does a fallback or ignored error hide that a required step failed, so the caller reports success?
- Are retries limited, safe to repeat, and limited to recoverable errors?
- Can the reported outcome (exit status, response, log, UI state) disagree with what was persisted?
- Is a panic or crash reachable from user input, a missing file or an unexpected external response?

Evidence: the failing call, how its error flows, and the observable result.

Not a defect: a crash on a condition the program already treats as fatal by design.
