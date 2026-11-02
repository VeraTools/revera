---
name: logic
description: Local logic, boundaries and data handling inside the changed code.
---
Applies to any changed condition, loop, arithmetic, parsing, sorting or data transformation.

Check:
- What happens for zero, one and many items, empty strings, missing optional values and the largest realistic input?
- Did a condition invert, a comparison change from strict to inclusive, or an early return or `continue` move so a later step is skipped?
- Are units, scales, signs and ordering preserved (bytes vs characters, ms vs s, absolute value vs signed delta, ascending vs descending)?
- Can slicing, indexing or integer conversion go out of range or overflow on short or malformed input?
- Does the code act on the intended object (the right variable, key, index or loop item)?

Evidence: the changed line, a concrete input value, and the result it produces versus the intended one.

Not a defect: an edge case that callers provably never produce; cite the guard if you find one. A guard or check that the PR removes is different: if the function accepted that input before, losing it is a regression even when today's callers happen not to pass it.
