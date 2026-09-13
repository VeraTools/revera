# Brief: M6 — multi-protocol providers

Branch: `devin/$(date +%s)-multi-protocol` off `main`. PR against main.

## Goal
`ModelRoute.protocol` gains `openai-responses`, `anthropic`, `gemini` next to
`openai-chat` and `scripted`. Each route stays independent (protocol, base_url,
api_key_env, model, extra_headers), so a config can mix e.g. an Anthropic
validator with an OpenRouter investigator. No behavior change for existing
`openai-chat` configs.

## Structure (src/provider/)
- `http.rs`: extract the whole attempt loop from openai_chat.rs (ledger
  reservation per attempt, 429/5xx + transient retry w/ Retry-After + backoff
  + jitter, error excerpting, LedgerEntry recording, latency) into
  `HttpTransport { http, ledger, max_requests, retries }` with
  `async fn send<A: ProtocolAdapter>(&self, adapter, messages, tools) -> Result<Completion>`.
  Adapter trait (sync, no I/O):
  ```rust
  pub trait ProtocolAdapter: Send + Sync {
      fn label(&self) -> String;                       // route_label
      fn model(&self) -> &str;
      fn build(&self, messages, tools, attempt: &mut AttemptState) -> Result<HttpRequestSpec, ProviderError>;
      fn parse(&self, status: u16, body: &str, attempt: &mut AttemptState) -> Parse; // Ok(ChatMessage, Usage) | RetrySameSlot(reason) | Err
  }
  pub struct HttpRequestSpec { url, headers: Vec<(String,String)>, body: Value }
  ```
  `RetrySameSlot` is the generalisation of today's max_tokens→max_completion_tokens
  400 fallback (openai-chat keeps that in `AttemptState`). Every attempt still
  takes a ledger reservation.
- `openai_chat.rs`: becomes an adapter; identical wire format to today.
- `openai_responses.rs`: POST `{base_url}/responses`. Body: `model`,
  `input` (system → `instructions`; user/assistant text → `{role, content:[{type:"input_text"|"output_text", text}]}`;
  assistant tool calls → `{type:"function_call", call_id, name, arguments: string}`;
  tool results → `{type:"function_call_output", call_id, output}`), `tools:
  [{type:"function", name, description, parameters, strict:false}]`,
  `tool_choice:"auto"`, `max_output_tokens`, `temperature`, `store:false`.
  Parse `output[]`: `message` items → concat `content[].text` where type
  `output_text`; `function_call` items → ToolCall{id: call_id, name, arguments
  parsed from string}. Usage from `usage.input_tokens`/`output_tokens`.
  Unsupported-temperature 400 (reasoning models) → RetrySameSlot dropping
  temperature.
- `anthropic.rs`: POST `{base_url}/v1/messages` (base_url default
  `https://api.anthropic.com`; strip a trailing `/v1` if the user gives it).
  Headers: `x-api-key`, `anthropic-version: 2023-06-01`. System messages →
  top-level `system` (joined). Assistant with tool_calls → content blocks
  `[{type:"text"}?, {type:"tool_use", id, name, input}]`. Tool results →
  a `user` message with `[{type:"tool_result", tool_use_id, content}]`;
  consecutive tool results MUST be merged into one user message. Tools →
  `{name, description, input_schema}`. `max_tokens` required. Parse `content[]`
  text + tool_use blocks; usage `input_tokens`/`output_tokens`.
  Anthropic requires alternating roles: merge consecutive same-role messages.
- `gemini.rs`: POST `{base_url}/v1beta/models/{model}:generateContent` with
  header `x-goog-api-key` (base_url default `https://generativelanguage.googleapis.com`).
  System → `systemInstruction.parts[{text}]`. Roles: user→`user`,
  assistant→`model`. Tool calls → parts `{functionCall:{name,args}}`; tool
  results → `user` parts `{functionResponse:{name, response:{content: <parsed json or {"text": s}>}}}`
  (merge consecutive). Gemini has no call ids: synthesize `id = format!("{name}-{idx}")`
  on parse and match tool results by position/name. Tools →
  `tools:[{functionDeclarations:[{name,description,parameters}]}]` — strip
  JSON-schema keys Gemini rejects (`additionalProperties`, `$schema`,
  `default`, `examples`) recursively. `generationConfig:{temperature,
  maxOutputTokens}`. Parse `candidates[0].content.parts[]`; usage
  `usageMetadata.promptTokenCount`/`candidatesTokenCount`.
- config.rs: `Protocol` enum + `validate_route` (base_url optional for
  anthropic/gemini with defaults; api_key_env required for all HTTP protocols);
  `pipeline/mod.rs` factory; `cli.rs` doctor check uses `protocol.is_http()`
  instead of `== OpenaiChat`.

## Tests (tests/provider_wiremock.rs — extend)
For each new adapter: (1) request-shape test asserting the exact JSON body and
headers for a 3-message conversation (system, user, assistant tool_call, tool
result) with one tool spec; (2) response parse test with a tool call + text;
(3) retry test (429 then 200, two ledger entries). Anthropic: consecutive
tool_result merge. Gemini: schema-key stripping and synthesized ids round-trip
(a tool result for `read_file-0` maps back to functionResponse name
`read_file`). Keep the existing openai-chat tests green unchanged.

## Live smoke (optional, cheap only)
Only if the key works: muse-spark via OpenRouter `openai-responses`
(`https://openrouter.ai/api/v1`) one-shot with a tool call. Skip anthropic/gemini
live (no keys) — say so in the report.

## Docs
`revera.example.yaml` protocol comment + one example route per protocol
(commented out); README "Providers" section; STATUS.md bullet.

Verification: cargo test, clippy -D warnings, fmt --check, fixtures/run-fixture.sh.
Commit + push; report at push with the diff summary.
