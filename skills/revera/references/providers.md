# Provider routes

```yaml
models:
  investigator:
    protocol: openai-chat        # openai-chat | openai-responses | anthropic | gemini
    base_url: https://api.example.com/v1
    api_key_env: REVIEW_API_KEY
    model: your-model-id
  # validator: omitted → same route, fresh context
```

- `api_key_env` must be a valid variable name and must not be a GitHub token
  variable (`GITHUB_TOKEN`, `ACTIONS_RUNTIME_TOKEN`, or `github.token_env`).
- A separate `validator:` route is optional; set it to validate with a
  different model.
- `reasoning` defaults to `medium` and degrades automatically when a provider
  rejects it.
- `revera doctor` checks only that the variable is set. To check a key works,
  run a live review (after the user agrees to the cost).
