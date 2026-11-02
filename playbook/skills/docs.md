---
name: docs
description: Documentation, CLI and user-facing text that misstate behavior.
---
Applies when the change edits docs, READMEs, changelogs, help text, error messages, hints, UI strings or comments that state a contract, or changes behavior those describe.

Check:
- Does every new or changed claim match the code: flags, defaults, conditions, guarantees ("always", "never", "automatically"), and supported modes?
- Is behavior documented as unconditional when the code makes it best effort, conditional or mode-specific?
- Do other docs, still unchanged, now contradict the new behavior? Search for the old wording.
- Are changelog and release notes placed under the right version, without rewriting history?
- Do user-facing messages report the real outcome, with correct counts, plurals and next steps, and do printed commands target the right path?

Evidence: the doc or message line and the code line that contradicts it.

Not a defect: wording preferences, typos that do not change meaning, or missing docs.
