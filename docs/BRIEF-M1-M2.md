# Implementation brief: M1 + M2 (Rust core)

Read docs/DESIGN.md, revera.example.yaml, prompts/*.md first. Those are settled;
implement exactly this. Repo: /home/ubuntu/repos/revera (empty git repo, branch main,
no commits). Rust 1.98 toolchain available. Vera 1.4.1 is installed at
~/.local/bin/vera (verified working in API mode with the env below).

## Crate

Single crate `revera` (lib + bin), edition 2021, MSRV note 1.85. Dependencies
(all widely used, pick current stable versions from crates.io): clap (derive),
serde, serde_json, serde_yaml, tokio (rt-multi-thread, macros, process, time,
sync), reqwest (json, rustls-tls, no default-features), anyhow, thiserror,
tracing + tracing-subscriber (env-filter), sha2, hex, base64, chrono (serde),
regex, globset, similar (NOT needed — we parse `git diff` output ourselves),
tempfile (dev), wiremock (dev). Add `unidiff`-style parsing by hand: parse
`git diff --no-color --unified=3 <base>...<head>` (merge-base form) into
`Vec<FileDiff { old_path, new_path, status, hunks: Vec<Hunk{ old_start, old_len, new_start, new_len, lines: Vec<DiffLine{kind: Add|Del|Ctx, old_no, new_no, text}> }> }>`.
Also expose `head_side_lines(file) -> BTreeSet<u32>` (Add + Ctx new_no) for anchoring.

Module layout: src/{main.rs, lib.rs, cli.rs, config.rs, git.rs, diff.rs, vera.rs,
provider/{mod.rs, openai_chat.rs, scripted.rs}, tools.rs, agent.rs,
pipeline/{mod.rs, baseline.rs, validate.rs, anchor.rs}, findings.rs, state.rs,
report.rs, prompts.rs (include_str! of prompts/*.md)}.
Delegated/panel strategies come in a later brief; leave `Strategy` enum with
all three variants and `todo`-style clean error ("strategy not implemented yet")
for the two.

## Config (config.rs)

Mirror revera.example.yaml exactly with serde structs; unknown fields are an
error (`deny_unknown_fields`). `${VAR}` expansion in all string values, error
if unset (name the var). Loading order: `--config <path>` else `./revera.yaml`
else error. Roles: `investigator` (required), `validator` (required), `lead`
(optional), `workers` (optional list), `scouts` (optional list). `ModelRoute`
= {protocol, base_url?, api_key_env?, model, max_output_tokens (default 4000),
temperature (default 0.2), extra_headers: map (default empty), script? (for
scripted)}. Validate at load: openai-chat requires base_url + api_key_env and
the env var must be set (error naming the var, never fall back); scripted
requires `script` path. Also add `profiles:` map (optional) of partial
overrides `{review?, budget?}` selectable via `--profile <name>`; ship
`fast` (defaults) and `deep` (agent_max_tool_calls 60, run_max_requests 400,
run_max_seconds 2400, strategy unchanged) semantics by simply applying the
override map; the example yaml should show a `profiles.deep` block.

## Provider (provider/)

```rust
pub struct ChatMessage { role: Role, content: Option<String>, tool_calls: Vec<ToolCall>, tool_call_id: Option<String>, name: Option<String> }
pub struct ToolCall { id: String, name: String, arguments: serde_json::Value }
pub struct ToolSpec { name: String, description: String, parameters: serde_json::Value }
pub struct Completion { message: ChatMessage, usage: Usage{prompt_tokens,completion_tokens}, model: String, latency_ms: u64 }
#[async_trait] pub trait ModelClient: Send+Sync { async fn complete(&self, messages:&[ChatMessage], tools:&[ToolSpec]) -> Result<Completion, ProviderError>; fn route_label(&self)->String; }
```
`openai_chat.rs`: POST `{base_url}/chat/completions` with `Authorization: Bearer`
+ extra_headers, body `{model, messages, tools, tool_choice:"auto", temperature, max_tokens}`
(`max_tokens`; if the server returns a 400 mentioning `max_completion_tokens`,
retry once with that field). Parse `choices[0].message` incl. `tool_calls[].function.{name,arguments}`
(arguments is a JSON string; parse it; on invalid JSON keep raw as `{"_raw": "..."}`).
Retries: on 429/5xx/connect/timeouts, up to `budget.retries` with backoff
0.5s*2^n (+/-jitter), honor `Retry-After`. 4xx other than 429 is fatal with the
body excerpt. Never change the model id. Timeout per request 120s.
Also record every request into a shared `RunLedger` (Arc<Mutex>): route label,
model, tokens, latency, retries, error — used by report and by the
`run_max_requests` budget (exceeding it -> ProviderError::BudgetExhausted).

`scripted.rs`: script JSON file:
```json
{ "roles": { "investigator": [ [turn, turn, ...], [ ... ] ], "validator": [ [...] ] } }
```
turn = `{"content": "..."}` or `{"tool_calls": [{"name": "...", "arguments": {...}}]}`.
Each new agent session for a role pops the next conversation (a role name is
passed via `ScriptedClient::new(path, role)`); turns are consumed in order; if
exhausted, return a turn that calls the terminal tool with empty payload and
log a warning. Also support an optional `"expect_tool_result_contains": "..."`
on a turn: after the previous tool result comes back, if the last tool message
does not contain the substring, return an error `ScriptExpectationFailed`
(this is how the fixture proves Vera actually found the caller).

## Tools (tools.rs)

Implement as a `ToolBox { repo_root, diff: Arc<DiffSet>, vera: Arc<VeraClient>, max_output_bytes }`
with `specs() -> Vec<ToolSpec>` and `async fn call(&self, name, args) -> String`
(errors are returned as tool results `{"error": "..."}` so the model can
recover). Tools and exact JSON params:
- `read_file {path, start_line?, end_line?}` -> numbered lines `N| text`, max 400 lines per call, path must resolve under repo_root (reject `..`, absolute, symlink escape via canonicalize) and must not be under `.git`/`.vera`/`.revera`.
- `list_changed_files {}` -> `path status +adds -dels` lines.
- `diff_context {path, line}` -> the hunk containing head line `line` with `+/-/space` prefixes and both line numbers.
- `vera_search {query, intent?, path_glob?, lang?, limit? (default 8, max 20)}` -> `vera search <query> [--intent] [--path] [--lang] -n N --json`; render as `path:start-end [symbol_type symbol_name]\n<content>` blocks.
- `vera_references {symbol, direction? ("callers"|"callees", default callers), limit? (default 20)}` -> `vera references <symbol> [--callees] -n N --json`.
- `vera_grep {pattern, path_glob?, limit? (default 30)}` -> `vera grep <pattern> [--path] -n N --json` (check `vera grep --help` for exact flags).
- `vera_overview {}` -> `vera overview --json` rendered compactly.
- terminal tools (schema only, handled by the agent loop, not ToolBox): `submit_findings {findings: [Finding], coverage: string}`, `submit_verdict {validation_status, counterevidence_checked: [string], severity?, start_line?, end_line?, rationale}`.
Truncate every tool result to `max_output_bytes` with a trailing `...[truncated, N bytes total]`.

## Vera wrapper (vera.rs)

`VeraClient { exe: PathBuf, repo_root, env: Vec<(String,String)> }`. Build env from
`vera:` config: backend api -> `VERA_BACKEND=api`, `EMBEDDING_MODEL_BASE_URL`,
`EMBEDDING_MODEL_ID`, `EMBEDDING_MODEL_API_KEY` (value read from `api_key_env`),
same for `RERANKER_*` when configured, plus `VERA_NO_UPDATE_CHECK=1`. Never log
key values. Methods: `version()` (parse `vera --version`), `ensure_index()`
(if `.vera/` missing -> `vera index . --json` else `vera update . --json`,
return parsed summary), `search/references/grep/overview` returning
`serde_json::Value`. Non-zero exit -> error with stderr tail. After indexing,
write `.revera/vera-cache.json` `{vera_version, backend, embedding_model, dim (from .vera/vectors.manifest "dim"), updated_at}`.
`revera cache-info` prints it. `revera doctor` checks: config loads, each
openai-chat route's key env set, vera executable found + version vs
`vera.version`, git repo detected, `.vera` present or not.

## Agent loop (agent.rs)

```rust
pub struct AgentRun { pub final_call: Option<ToolCall>, pub transcript_len: usize, pub tool_calls: u32, pub stopped: StopReason }
pub async fn run_agent(client:&dyn ModelClient, system:&str, user:&str, toolbox:&ToolBox, terminal_tool:&str, budget:&AgentBudget) -> Result<AgentRun>
```
Loop: complete -> for each tool call: if name==terminal -> stop, return it;
else execute via toolbox concurrently (join_all) and append tool messages
(role tool, tool_call_id). If the assistant returns no tool calls but content:
try to parse the content as JSON matching the terminal tool's arguments
(models sometimes answer in text); if that fails, append a user message
"Call `<terminal>` to finish." once, then give up with StopReason::NoTerminalCall.
Stop reasons: Terminal, ToolBudget, TimeBudget, NoTerminalCall, ProviderError.
On ToolBudget/TimeBudget append one user message "Budget exhausted; call
<terminal> now with what you have" and allow exactly one more completion.

## Findings, validation, anchoring, state

findings.rs: the schema from DESIGN.md as serde structs; `Finding::id()` =
hex(sha256(file + "\0" + defect_key))[..12]; `Severity` ordering; validator
`Verdict` struct. `collapse(candidates) -> Vec<Finding>`: merge when same
file and (same defect_key OR line ranges overlap and normalized titles share
>= 60% of tokens); keep the highest severity, union evidence, record all
`source`s.
pipeline/validate.rs: for each collapsed candidate spawn a fresh validator
agent (system=validator.md, user = diff excerpt for the file + the candidate
JSON), concurrency = review.concurrency, apply the verdict (status, severity
override, anchor override, counterevidence, rationale). A validator failure
(provider error/budget) marks the candidate `uncertain` with rationale
"validator unavailable: <reason>" and sets run status partial.
pipeline/anchor.rs: inline if file in diff and start_line in head_side_lines;
else `placement: Summary`. Cap inline at review.max_findings ordered by
severity then file/line.
state.rs: `ReviewState { reviewed_head, reviewed_base, findings: Vec<StateFinding{id,status: open|resolved|rejected, file, start_line, title, posted: bool}> }`,
load/save `.revera/state.json` (GitHub marker comes in M3). On a run with
prior state: prior `open` findings become recheck candidates (system=recheck.md)
before new investigation; those rejected -> resolved; accepted -> kept, not
reposted (posted=true carried over). New findings whose id matches a prior
open/posted id are not reposted.
pipeline/baseline.rs: orchestrates: collect diff -> vera.ensure_index ->
rechecks -> investigator agent (user message = PR title/body + changed files +
diff, truncated per review.max_diff_bytes) -> collapse -> min_severity filter
-> validate -> anchor -> plan.
report.rs: `RunReport { status: complete|partial|failed, reason?, base, head,
strategy, findings (all, with status), plan: PublicationPlan{ inline: [{file,line,end_line?,body}], summary_markdown, state }, ledger: {requests, prompt_tokens, completion_tokens, by_route: [...], wall_ms} }`.
Summary markdown: header "Revera review", one line per accepted finding
(severity, file:line, title), "Findings outside the diff" section, "Not checked"
line from coverage/unresolved, run status line, and a compact footer with
strategy + models used (never keys). Body of an inline comment: `**[severity] title**\n\nclaim\n\nTrigger: ...\nImpact: ...\nEvidence: path:line, ...\n\n<suggested fix if any>` + `<!-- revera-id:<id> -->`.

## CLI (cli.rs / main.rs)

```
revera review --repo <path=.> --base <rev> [--head <rev=HEAD>] [--config p] [--profile name] [--strategy s] [--publish dry-run|comment] [--out report.json] [--title ..] [--body-file ..]
revera doctor [--config p]
revera cache-info [--repo p]
```
`--publish comment` without GitHub context -> clear error (publisher is M3).
Exit codes: 0 complete, 2 partial, 1 failed/config error. Print the summary
markdown to stdout and the full report JSON to `--out` (default
`.revera/last-report.json`). Log via tracing to stderr; `RUST_LOG` honored,
default info; log per-request ledger lines at debug.

## Fixtures (fixtures/)

`fixtures/make-fixtures.sh` creates under a target dir (arg, default
/tmp/revera-fixtures) two git repos:
- `crossfile/`: commits `base` (src/lib.rs, src/pricing.rs `discount_for_tier -> f64` fraction 0.2/0.1, src/checkout.rs `final_price = base*(1.0-d)`, tests/pricing_test.rs asserting fraction), `break` (pricing.rs returns percent 0..100: 20.0/10.0, doc comment updated; test updated; checkout.rs UNCHANGED), `fix` (checkout.rs divides by 100). Tag the commits `base`, `break`, `fix`.
- `clean/`: commits `base` (same as above) and `docs` (adds a doc comment + a `log::debug!`-free harmless comment in pricing.rs).
Scripts under `fixtures/scripts/`: `crossfile-break.json` (investigator: vera_references discount_for_tier with expect_tool_result_contains "checkout.rs"; read_file src/checkout.rs; submit_findings with one finding defect_key `final_price_treats_percent_as_fraction` at src/pricing.rs line of the `"gold" => 20.0` change, severity high, evidence checkout.rs lines; validator: read_file then submit_verdict accepted), `crossfile-fix.json` (recheck validator: read_file, submit_verdict rejected; investigator: submit_findings []), `clean.json` (investigator: submit_findings [] coverage "...").
Configs under `fixtures/configs/`: `scripted-crossfile-break.yaml`, etc. with
`vera.backend: api` using OPENROUTER_API_KEY env names (as in example) — OR, if
OPENROUTER_API_KEY is unset, the integration test is skipped with a message.
`fixtures/run-fixture.sh`: builds fixtures, checks out `break`, runs `revera review --base base --head break --config ...`, asserts report has 1 accepted finding anchored inline in src/pricing.rs, then checks out `fix`, reruns with `--base base --head fix` and asserts 0 open findings and the prior finding is `resolved`, then runs clean control and asserts 0 findings and status complete. Exit non-zero on any assertion failure.

## Tests

Unit tests: diff parsing (incl. renames, new/deleted files, `\ No newline`), head_side_lines, `${VAR}` expansion + missing-var error, config rejects openai-chat without key env, finding id stability, collapse rules, anchoring in/out of hunk, state transitions (open->resolved, no repost), scripted provider ordering + expectation failure, summary rendering. Provider contract tests with wiremock: success with tool_calls, 429 then success (retry), 500x4 -> error, `max_completion_tokens` retry, 400 fatal with body excerpt, model id in request body equals config. Agent-loop tests with a stub ModelClient: terminal on first call, tool then terminal, budget exhaustion path, text-JSON fallback.

Verification (mandatory before reporting): `cargo build`, `cargo test`,
`cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, then
`fixtures/run-fixture.sh` with these env vars for Vera API mode:
`OPENROUTER_API_KEY` (already in env), and `VERA_HOME=$HOME/.vera-revera`.
Also run `revera doctor` on the fixture. Report the report JSON path for the
break run and paste its summary markdown.

## Housekeeping

Add: `.gitignore` (target, .revera/, .vera/, *.log), `LICENSE` (MIT, holder
"VeraTools"), `README.md` (short: what it is, CLI usage, config pointer,
status), `STATUS.md` (sections: What works / Current blocker / Next three
tasks / Exact test-demo command / Expensive-to-reverse decisions), `rust-toolchain.toml` (stable),
`.github/workflows/ci.yml` (fmt, clippy -D warnings, test on ubuntu-latest;
fixture script runs only when secret OPENROUTER_API_KEY is present).
Commit on `main` in a few logical commits (conventional-commit style subjects),
do NOT push. Do not add code comments explaining the diff; keep comments minimal.
