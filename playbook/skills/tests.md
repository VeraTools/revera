---
name: tests
description: "Tests and verification: tests that cannot fail, and expectations that encode bugs."
---
Applies when the change adds or edits tests, fixtures, mocks, assertions, CI checks or verification scripts.

Check:
- Would this test still pass if the behavior it claims to cover were removed or broken? Look for assertions that are always true, assert on the wrong value, or never run.
- Does it observe the result (output, state, error) rather than only executing the code?
- Do waits, timers and async checks prove what they claim, or do they return before the event happens?
- Did a changed expectation preserve the regression it guarded, or weaken it to match new buggy behavior?
- Do mocks and fixtures still match the real interface they stand in for?

Evidence: the assertion line and the concrete broken implementation that would still pass it.

Not a defect: a missing test for unchanged behavior, or a request to add more tests in general.
