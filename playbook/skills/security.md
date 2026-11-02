---
name: security
description: "Trust boundaries: untrusted input, secrets, authorization and injection."
---
Applies when the change handles external input (requests, webhooks, PR content, files, environment, CLI arguments), credentials, permissions, or builds paths, URLs, shell commands, queries or HTML from data.

Check:
- Can attacker-controlled data reach a path, shell command, query, URL, template or HTML sink without validation or escaping?
- Was an authentication, authorization, signature or trust check removed, weakened, reordered after the use it protects, or made to fail open?
- Can a secret reach logs, error messages, reports, caches, models or responses, including through truncation that splits a redacted value?
- Can content the user does not control (PR text, fetched pages, repository files) gain instruction or configuration authority?
- Does an exclusion or deny list get applied in one code path but not in a parallel one?

Evidence: the source of the untrusted data, the sink, and the missing check, with `path:line` for each.

Not a defect: input that only a trusted operator controls, unless the PR crosses that boundary.
