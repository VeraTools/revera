---
name: subprocess
description: "Subprocesses and environment: inherited variables, arguments, exit status and quoting."
---
Applies when the change runs child processes (git, shells, package managers, the tool itself) or reads environment variables.

Check:
- Can inherited environment variables (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, proxy or locale settings, tool-specific config) redirect the child to another repository or setting, including in tests and test helpers?
- Is the working directory explicit, and are arguments passed without shell interpretation? Do paths with spaces, quotes, `$` or leading dashes survive?
- Are exit status, stderr, timeouts and signals checked, and is output parsed robustly (encoding, empty output, trailing newline)?
- Can a long-running or detached child outlive its parent, become a zombie, or keep a lock?
- Does a printed command for the user (hints, docs) run correctly when copied, including quoting and the right target?

Evidence: the spawn site, the inherited or crafted value, and the wrong effect.

Not a defect: variables the program deliberately forwards and documents.
