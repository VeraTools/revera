---
name: filesystem
description: "Filesystem and destructive operations: deletion, overwrite, atomicity and paths."
---
Applies when the change creates, deletes, moves, overwrites or cleans up files or directories, or resolves user-supplied paths.

Check:
- Can a flag, default or cleanup delete or overwrite data the tool did not create (an existing directory, a user file, a path the user mistyped)?
- What remains after a write, rename or swap fails halfway, and is the previous good copy still available?
- Can symlinks, `..`, relative paths or case differences escape the intended directory or make two paths compare unequal?
- Do generated artifacts (indexes, backups, temp files) end up inside a tree that is later indexed, committed or published?
- Does resolving or validating a path create directories or files as a side effect?

Evidence: the operation, the path value that triggers it, and what is lost or left behind.

Not a defect: deleting a path the tool provably created and owns.
