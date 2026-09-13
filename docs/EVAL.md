# Revera model evaluation log

## Model selection (2026-09-13, openrouter.ai/api/v1/models)

Cheap candidates (tools supported, prompt <= $0.30/M, completion <= $1.0/M):

| id | prompt $/M | completion $/M |
|---|---|---|
| deepseek/deepseek-v4-flash | 0.049 | 0.099 |
| qwen/qwen3.5-flash-02-23 | 0.065 | 0.260 |
| mistralai/mistral-small-3.2-24b-instruct | 0.075 | 0.200 |
| google/gemini-2.5-flash-lite | 0.100 | 0.400 |
| meta/muse-spark-1.3-contributor | 0.10 | 0.20 |

Strong candidates (in brief preference order): anthropic/claude-sonnet-4.6
($3.00/$15.00), openai/gpt-5.4 ($2.50/$15.00), google/gemini-2.5-pro
($1.25/$10.00).

**Selected (user directive): `meta/muse-spark-1.3-contributor` for ALL routes**
(investigator, validator, and later scouts/workers). Verified: id present in
/api/v1/models, `"tools"` in supported_parameters, $0.10/M prompt, $0.20/M
completion.

## Live probe — crossfile fixture (dry-run)

Config: `fixtures/configs/live-openrouter.yaml` (muse-spark all routes, vera api
via qwen/qwen3-embedding-8b, OpenRouter headers HTTP-Referer + X-Title).

### muse-spark runs (authoritative)

| run | summary | investigator tools used | caller bug found | validator verdict | requests | prompt tok | compl tok | wall | est. cost |
|---|---|---|---|---|---|---|---|---|---|
| break | 1 finding: **[high]** src/checkout.rs:3 discount unit change breaks final_price (negative totals) | vera_references, vera_grep, read_file | yes | accepted | 8 | 20758 | 2858 | 24.1s | ~$0.0026 |
| fix | 0 findings | vera_references, read_file, search | n/a (not present) | n/a (no candidates) | 9 | 30459 | 3353 | 32.7s | ~$0.0037 |
| clean | 0 findings (docs-only) | read_file, references | n/a | n/a | 4 | 9240 | 820 | 8.7s | ~$0.0011 |

Notes:
- muse-spark emits well-formed OpenAI tool_calls; no adapter issues observed.
- On `break`, the finding targets `src/checkout.rs` which is NOT in the diff, so
  it correctly lands in "Findings outside the diff" (summary placement).

### Earlier runs against other models (superseded by the muse-only directive; kept for reference)

- qwen/qwen3.5-flash-02-23 (investigator) + claude-sonnet-4.6 (validator):
  initially found the bug but it was silently dropped (see adapter bugs below);
  after the fix, found + validated (8 requests, ~24s).
- deepseek/deepseek-v4-flash: correct tool calls, 19 requests over 3m19s, missed
  the caller bug (reasoned checkout was already correct — but see worktree
  caveat).
- google/gemini-2.5-flash-lite: first run returned content-part arrays which the
  parser read as `null` → NoTerminalCall (1 request); worked after the
  content-array fix.
- mistralai/mistral-small-3.2-24b-instruct: clean tool use, missed the bug.
- claude-sonnet-4.6 as investigator: also reported no findings.

IMPORTANT CAVEAT on the earlier "missed" verdicts: those runs were executed with
the fixture worktree checked out at `fix` while --head pointed at `break`, so
Vera's index and read_file reflected the FIXED source — the models literally
could not see the bug via tools. After `git checkout break` (worktree == head),
muse-spark found it on the first try. Lesson recorded: review runs must be done
with the PR head checked out (event mode does this naturally).

## Adapter bugs found and fixed during the probe

1. `openai_chat.rs` `parse_message`: `content` returned as a parts array
   (`[{"type":"text","text":...}]`, seen from Gemini) was read as null → agent
   loop saw empty non-tool reply → NoTerminalCall. Fixed: join text parts.
2. `submit_findings` schema declared findings items as opaque objects, so live
   models invented field names (`path: "file.rs:6"`, `description`) that failed
   `Finding` deserialization and were silently dropped — zero findings despite a
   correct terminal call. Fixed: full JSON schema for finding items
   (required: defect_key,severity,file,start_line,title,claim) plus tolerant
   parsing (`path:"file:line"` → file+start_line, `description` → `claim`) and a
   warn log for dropped findings.

## M4 strategy comparison — crossfile fixture `break` (live, muse-spark all routes)

| strategy | found bug | false findings | requests | prompt tok | completion tok | wall s | est. cost |
|---|---|---|---|---|---|---|---|
| baseline | yes (checkout.rs:3, outside-diff) | 0 | 8 | 20,758 | 2,858 | 24.1 | ~$0.0026 |
| delegated | yes (checkout.rs:4, outside-diff) | 0 | 10 | 31,100 | 5,710 | 66.5 | ~$0.0043 |
| panel | yes (checkout.rs:4, outside-diff) | 0 | 13 | 40,084 | 5,324 | 46.2 | ~$0.0051 |

Notes:
- All three strategies found the real defect with zero false findings; both new
  strategies anchored at the caller (checkout.rs, not in the diff) → "Findings
  outside the diff", same as baseline's muse run.
- Delegated: lead planned 2 questions, both workers answered, synthesis
  re-derived the finding (one worker candidate was dropped as malformed —
  missing `severity` — but the lead still produced the correct finding).
- Panel: `2 scouts, 2 raw candidates → 1 unique` (both scouts converged on the
  same defect; collapse merged them).
- Cost estimate: $0.10/M prompt + $0.20/M completion (muse-spark pricing).
