# Diagnosing a run

Read the JSON report (`report-path` output or `--out`). Key fields:

| field | meaning |
|---|---|
| `status` | `complete`, `partial` (budget/provider/coverage problem) or `failed` |
| `stats.validation` | `fresh`, `reused` or `disabled` |
| `stats.accepted` / `rejected` / `uncertain` / `unvalidated` | verdict counts |
| `coverage_gaps` | files not reviewed (content policy, `max_diff_bytes`, budget) |
| `stats.retrieval` | `lexical-only`, `vera`, `vera+rerank`, `vera (rerank degraded)`, `unavailable: …` |
| `ledger.by_route` | requests and tokens per route |

Common causes:

- `config: FAIL` in doctor — fix the YAML; the message names the key.
- `key env … missing or empty` — the secret is not exported to the step.
- Untrusted config / non-HTTPS endpoint in event mode — the base-commit
  config is used; merge config fixes first.
- `partial` with budget notes — raise `budget.*` or narrow the diff.
- Uncertain verdicts with "malformed verdict" — the validator model is not
  following the tool schema; try a stronger validator route.
- `.env`, key and certificate files are never read or reviewed; they appear
  as coverage gaps, which is expected.
