# Security

## Reporting

Report vulnerabilities privately through
[GitHub security advisories](https://github.com/VeraTools/revera/security/advisories/new)
rather than public issues. Expect an acknowledgement within a week.

## Model of trust

- Configuration (`revera.yaml`) is trusted: it chooses providers, models and
  the *names* of credential environment variables. Credential values are
  read from the environment and are never logged, written to reports or
  included in review fingerprints.
- Pull request content is untrusted. Models see it, but they can only call
  read-only tools over the checked-out head tree (no shell, no writes, no
  network) and can only return structured findings; Revera decides what is
  published.
- The GitHub token needs `pull-requests: write` for publishing and is used
  for nothing else. Fork PRs are skipped by default because secrets are not
  available to them.
- Release binaries are published with SHA-256 checksums;
  `scripts/install-revera.sh` verifies them before install.
