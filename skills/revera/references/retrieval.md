# Vera retrieval

Lexical search over the PR head is always on. Add Vera for semantic search
and references on larger repositories:

```yaml
vera:
  backend: api
  embedding:
    base_url: https://api.example.com/v1
    model: your-embedding-model
    api_key_env: EMBEDDING_API_KEY
  reranker:                      # optional
    base_url: https://api.example.com/v1
    model: your-reranker-model
    api_key_env: RERANK_API_KEY
    protocol: generic            # generic | voyage
```

- Revera runs Vera with its own isolated home and ignores ambient `VERA_*` /
  embedding variables, so local Vera settings do not leak in.
- The index cache key (`revera cache-key`) covers only the embedding and
  indexing settings. Changing models, prompts, budgets, guidance or the
  reranker reuses the warm index.
- The run report's `stats.retrieval` says what happened: `lexical-only`,
  `vera`, `vera+rerank`, `vera (rerank degraded)` (reranker failures fall
  back to unreranked results) or `unavailable: <reason>`.
