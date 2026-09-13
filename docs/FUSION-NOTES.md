# Devin CLI artifact inspection (bounded)

Static, strings-level inspection of the official Devin CLI bundle, done to check
whether locally shipped Fusion/delegation mechanics suggest anything concrete for
Revera's `delegated` / `panel` strategies. No deobfuscation, no runtime tracing.

## Artifact

| item | value |
|---|---|
| installer | `https://cli.devin.ai/install.sh` -> `https://static.devin.ai/cli/current/manifest.json` |
| version | `3000.10.21` |
| bundle | `https://static.devin.ai/cli/3000.10.21/devin-3000.10.21-x86_64-unknown-linux.tar.gz` |
| SHA-256 (manifest and measured) | `7cac6f5739ba3a3e5542f3b7fa07ed902d6dfb96ca22e4c63ae84c03bb7db47c` |
| contents | `bin/devin` (single native ELF, ~174 MB extracted), `share/devin/docs`, man page |
| runtime | native Rust (`Cargo [[bin]]`, `hyper`/`h2`, bundled SQLite, `chisel`/`chisel_ui` crates) |

The orchestration is compiled Rust, not bundled JS; the string table is readable
but nothing beyond names, prompts, log lines and SQL is recoverable statically.

## What is local vs remote

- **Model routing is remote.** Strings `Resolving model router '' via AssignModel RPC`,
  `/exa.api_server_pb.ApiServerService/AssignModel`, `Fusion lead router ''
  assigned sidekick ''`, `ModelAssignment { model_uid, assignment_jwt, harness_uids }`.
  The lead/sidekick model pairing is decided server-side and returned as a signed
  assignment; the binary only carries pricing/usage bookkeeping
  (`FusionUsage`, `leadModelUid`, `leadPricesUsdPerMillion`, `sidekickPricesUsdPerMillion`).
- **Inference is remote** (`GetChatMessage` stream over Connect/protobuf).
- **Scheduling, persistence and handoff representation are local.**

## Local Fusion mechanics visible in the binary

1. **One persistent sidekick, addressed by a deterministic id.** SQL comment:
   `-- Chain heads for persistent subagents (e.g. the Local Fusion sidekick), keyed
   by deterministic agent id. A subagent's chain is its own tree in the forest,
   unreachable from sessions.main_chain_id`. The sidekick's conversation is a
   separate message chain in the same SQLite store; on load, `persistent subagent:
   stored chain head missing from forest; starting fresh`.
2. **Handoffs are conversation turns, not task records.** Prompt fragments:
   `Here is the full handoff conversation you had with the lead`, `<lead_handoff>`,
   `Summarize: truncated handoff history from N messages (M dropped)`, and
   `[handoff] truncate_diff: truncating large diff ... (diff truncated, too large
   to include in full)`. The lead's brief is plain text; the sidekick's reply is
   a report; history is summarized when it grows.
3. **Mid-run brief injection.** `Brief injected into the running sidekick`,
   `Local Fusion: sidekick finished during injection; brief not delivered`,
   plus foreground/background moves. There is no queue of tasks — a new brief is
   appended to the running agent's input.
4. **Lead-side policy is prompt text.** The lead's rules (review the sidekick's
   diff before it lands, batch rework into one handoff, keep correctness-critical
   authoring with the lead, "the anti-duplication rule runs both ways") ship as
   prompt strings, not code paths.
5. **Subagent profiles** (`core/subagent_profiles`, `run_subagent`) are chosen by
   write-access needs; depth (`depth`, `isBackground`) is tracked per subagent
   event.

Nothing about retries/escalation between lead and sidekick is encoded as a state
machine; escalation is again prompt-level ("Escalate after several attempts have
failed ... or when the blocker needs something only the lead or user can provide").

## Comparison with Revera

| mechanic | Devin CLI (Local Fusion) | Revera today | takeaway |
|---|---|---|---|
| worker identity | one persistent sidekick with its own chain | `delegated`: N stateless worker agents, one per lead question; `panel`: independent scouts | Revera's workers are cheaper and parallel; persistence is unnecessary for a single PR review. No change. |
| handoff payload | free-text brief + summarized history, diff truncated | structured question + diff/context tools; worker answers are structured findings | Revera's structured contract is stricter and already what the validator needs. Keep. |
| large-diff handling | truncate the diff in the handoff | `diff_context` / `read_file` tools with UTF-8-safe truncation, request/tool budgets | Equivalent intent; Revera lets the worker page instead of truncating once. Keep. |
| model routing | remote assignment of lead/sidekick pair | static config (`models.*` routes, optional `workers`/`scouts` lists) | Revera must stay provider-independent and offline-configurable; no remote router. |
| failure handling | prompt-level escalation | code-level: worker failure -> coverage gap + `partial_reasons`; provider errors -> retry/fallback ledger | Revera's is more testable; this session added explicit partial reasons for worker failures. |
| synthesis | lead reads sidekick report and decides | lead consolidates worker findings -> validator | Same shape. No change. |

Conclusion: the useful ideas (lead/worker split, lead reviews worker output,
batch rework) are already in `delegated`/`panel`. The only concrete gap worth
recording was worker-failure visibility, which is fixed in this branch. No
further design changes are taken from the artifact.
