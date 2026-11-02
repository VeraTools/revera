---
name: config
description: "Configuration, build and release: defaults, flags, versions and packaging."
---
Applies when the change touches configuration parsing, defaults, environment variables, feature flags, build scripts, CI workflows, packaging, versions or release steps.

Check:
- Do defaults agree across code, config files, environment variables, CLI help and documentation?
- Are new options validated, and do invalid or conflicting combinations fail clearly instead of being ignored?
- Does a config change alter a trust, cache or identity boundary, or get read from a source an untrusted party controls?
- Do CI, install and release steps build, test and ship the artifact that users actually get, at the version claimed?
- Are files that must stay in sync (lockfiles, version strings, generated code, embedded assets) updated together?

Evidence: the two places that disagree, or the combination that misbehaves.

Not a defect: an undocumented internal default that no user can set.
