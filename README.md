# Revera

Provider-independent PR reviewer. Deterministic Rust owns review state and
publication; configurable OpenAI-compatible models do the reasoning;
[Vera](https://github.com/VeraTools/Vera) supplies repository-aware retrieval.

## CLI

```sh
revera review --repo <path> --base <rev> [--head <rev>] --config revera.yaml [--profile deep] [--force]
revera doctor [--config revera.yaml]
revera cache-info [--repo <path>]
```

`review` prints a summary to stdout and writes a full JSON report to
`--out` (default `.revera/last-report.json`). Exit codes: 0 complete,
2 partial, 1 failed/config error.

## Configuration

See `revera.example.yaml` and `docs/DESIGN.md`. Model roles
(`investigator`, `validator`, optional `lead`/`workers`/`scouts`) each take
a `protocol` (`openai-chat` or `scripted`), endpoint, and model; Vera API
mode reads keys from the env names given in config.

## Status

M1+M2 core: baseline strategy (investigate → collapse → validate → anchor),
state/patch-id short-circuit, dry-run report. `delegated`/`panel` strategies
and GitHub publication (`--publish comment`) are not yet implemented.
