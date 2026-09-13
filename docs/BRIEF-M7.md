# Brief: M7 — per-route reasoning levels

Branch: `devin/$(date +%s)-reasoning` off `main`. PR against main.

## Config (src/config.rs)
```yaml
models:
  validator:
    protocol: anthropic
    model: claude-sonnet-4-5
    reasoning: high              # none | minimal | low | medium | high | xhigh   (default: medium)
  investigator:
    protocol: openai-chat
    base_url: https://openrouter.ai/api/v1
    model: meta/muse-spark-1.3-contributor
    reasoning:                   # long form
      effort: medium
      budget_tokens: 4096        # optional; overrides the effort→budget mapping where budgets are the native knob
      field: auto                # auto | openai | openrouter   (openai-chat only; see below)
```
`ModelRoute.reasoning: Reasoning` with `#[serde(default)]` = `{effort: Medium, budget_tokens: None, field: Auto}`;
accept the bare string form via an untagged enum. `effort: none` disables and sends no reasoning
fields at all. Reasoning is ON by default for every route (user requirement: "thinking should almost
always be used"); routes whose provider rejects the field degrade automatically (below).

Effort → budget mapping (used where the provider takes a token budget):
minimal 1024, low 2048, medium 8192, high 16384, xhigh 32768. `budget_tokens` wins when set.

## Wire mapping
- **openai-chat**: `field: openai` → `"reasoning_effort": "<effort>"` (xhigh→high unless model starts
  with `gpt-5`). `field: openrouter` → `"reasoning": {"effort": …}` or `{"max_tokens": budget}` when
  budget_tokens is set. `auto` = openrouter when base_url host contains `openrouter.ai`, else openai.
  On 400 whose body mentions `reasoning` (either spelling) and reasoning not yet dropped →
  `RetrySameSlot` with `attempt.drop_reasoning = true` and a ledger error note
  `"400: retrying without reasoning"` (mirrors the existing max_completion_tokens fallback). Same for
  `temperature` 400 (reasoning models reject it) — move the existing `drop_temperature` handling from
  openai-responses into a shared helper both adapters use.
  Ignore `reasoning_content`/`reasoning` fields in the response message except for usage.
- **openai-responses**: `"reasoning": {"effort": "<effort>"}`, `"include": ["reasoning.encrypted_content"]`.
  Because `store:false`, reasoning items from `output[]` MUST be echoed back on the next turn:
  parse keeps every `{"type":"reasoning", …}` item verbatim in the new opaque field
  `ChatMessage.provider_state: Option<Value>` (see below) and `build` re-emits them as input items
  immediately before that assistant turn's `function_call` items. Drop-on-400 fallback as above.
- **anthropic**: `"thinking": {"type":"enabled","budget_tokens": B}` where B = budget (mapped or
  explicit) clamped to `max_output_tokens - 1024`; if `max_output_tokens <= B + 1024` raise
  `max_tokens` in the body to `B + max_output_tokens` (so visible output isn't starved). Anthropic
  requires `temperature` omitted (or 1) with thinking on — omit it. Parse keeps `thinking` and
  `redacted_thinking` blocks verbatim in `provider_state`; `build` re-emits them as the leading
  content blocks of that assistant message (required for tool-use continuation; otherwise the API
  400s). On 400 mentioning `thinking` → drop and retry (once).
- **gemini**: `generationConfig.thinkingConfig`. Models whose id starts with `gemini-3` →
  `{"thinkingLevel": "low"|"high"}` (minimal/low→low, else high); otherwise
  `{"thinkingBudget": B, "includeThoughts": false}` (none → budget 0 for 2.5-flash; for 2.5-pro,
  which cannot disable thinking, send nothing). Parse keeps each part's `thoughtSignature` and
  re-emits it on the echoed `functionCall`/text part (required for multi-turn function calling).
  On 400 mentioning `thinking` → drop and retry.
- **scripted**: ignore.

## Types
- `ChatMessage.provider_state: Option<serde_json::Value>` (`#[serde(default, skip_serializing_if = Option::is_none)]`):
  opaque, adapter-owned; only the adapter that produced it interprets it. Adapters must tolerate
  a foreign shape (ignore). Agent loop copies the assistant message as-is (it already does).
- `AttemptState`: add `drop_reasoning: bool`.
- `Usage.reasoning_tokens: u64` + `LedgerEntry.reasoning_tokens`: openai-chat
  `usage.completion_tokens_details.reasoning_tokens`, responses `usage.output_tokens_details.reasoning_tokens`,
  gemini `usageMetadata.thoughtsTokenCount`, anthropic: not reported separately → 0. Surface the
  total in the run report/summary line where prompt/completion totals appear (`RunLedger::totals`
  returns a 4-tuple; update callers) and in eval/score.py `reasoning_tokens` (0 when absent).
- `revera doctor` prints the effective reasoning setting per route.

## Tests (tests/provider_wiremock.rs)
Per adapter: body contains the expected reasoning field for `effort: high`; `effort: none` emits
none; 400 mentioning reasoning → second request without it (two ledger entries, one with the retry
note). Responses + anthropic + gemini: round-trip test — a mocked response containing a
reasoning item / thinking block / thoughtSignature followed by a tool call; the next `build` echoes
it back in the right position. Config test: bare-string and long forms parse; unknown effort fails.

## Live smoke (cheap only)
muse-spark via OpenRouter openai-chat with `reasoning: medium` — record whether OpenRouter accepts
or the fallback fires (either is fine; report which). No other live calls.

## Docs
revera.example.yaml (both forms), README Providers table gets a "reasoning" column with the wire
field per protocol, STATUS.md bullet. Commit this brief.

Verification: cargo test, clippy -D warnings, fmt --check, fixtures/run-fixture.sh. Commit + push;
report at the push with the diff summary and live smoke result.
