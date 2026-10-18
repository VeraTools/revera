# Focused reviewer tool audit

The current read-only tool set is in `src/tools.rs`; planning excludes web,
delegated workers and panel scouts investigate, synthesis gets only `read_file`,
and fresh validators share the web limits with validation capacity reserved.
These are implementation/scope judgments, not measured quality claims.
Existing [evaluations](evaluation.md) compare reviewer configurations, not each
tool in isolation; they do not establish a benefit for additional local tools.

| candidate | judgment |
|---|---|
| `read_file` | Keep: immutable head code with numbered lines proves local evidence; exclusions and bounded output limit disclosure. |
| `list_changed_files` | Keep: cheap scope inventory; not proof of coverage by itself. |
| `diff_context` | Keep: changed hunks show introduction versus prior behavior; cannot prove untouched base callers. |
| `grep_repo` | Keep: tracked-head exact matches discover callers without an index; regex noise is bounded by existing limits. |
| `find_files` | Keep: tracked-head glob lookup resolves paths cheaply; no duplicate file-discovery tool needed. |
| `vera_search` | Keep optional: indexed semantic retrieval finds cross-file context; stale/untracked hits are filtered, lexical search remains available. |
| `vera_references` | Keep optional: caller/callee lookup checks changed contracts; graph misses still need lexical checks. |
| `vera_grep` | Keep existing optional tool: indexed regex is useful in retrieved context; do not add another regex name. |
| `vera_overview` | Keep optional: architecture orientation; not necessary for every review or sufficient evidence of a defect. |
| `web_search` | Add opt-in: fills the external exact-version contract gap; outbound query disclosure requires screening and a private/unknown-repo consent boundary. |
| `web_read` | Add opt-in: selected-source text can establish the contract beyond snippets; Rust-owned IDs, URL checks and untrusted-data framing bound access. |
| `read_file(revision: base)` | Defer: a real untouched-base-context gap is plausible, but no observed case here demonstrates benefit over existing diff minus-lines. |
| Generic shell / mutation | Reject this pass: breaks the deterministic read-only reviewer boundary rather than filling an observed evidence gap. |
| Browser / arbitrary HTTP / crawl | Reject: broader egress and injection surface than selected Exa results, with no demonstrated additional benefit. |
| Universal MCP / plugins | Reject: hosted Exa is a fixed HTTP contract, not a model-controlled tool registry or generic plugin mechanism. |
| Duplicate regex / semantic tools | Reject: existing local search families already cover this role. |
| GitHub issue / PR search | Defer: no measured reviewer deficiency here justifies permissions, noise and outbound context exposure. |
| Blame / history | Defer: introduction is checked with the diff; no realistic failing case here requires historical browsing. |
| Dependency scanner | Defer: external advisory evidence fits narrow web queries; no new execution or scanner tool without an observed gap. |
| CI logs | Defer: may contain secrets and unrelated failures; no demonstrated evidence gap outweighs that exposure in this pass. |
