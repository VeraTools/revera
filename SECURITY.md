# Security

## Reporting

Report vulnerabilities privately through
[GitHub security advisories](https://github.com/VeraTools/revera/security/advisories/new)
rather than public issues. Expect an acknowledgement within a week.

## Model of trust

- Configuration (`revera.yaml`) is trusted: in event mode, an in-checkout
  config is read from the PR base commit, never the PR head, and endpoint and
  credential-name trust checks run before any credential value is resolved.
  Configuration chooses providers, models and the *names* of credential
  environment variables. Credential values are read from the environment
  and are never logged, written to reports or included in review fingerprints.
- Pull request content is untrusted. Models see it, but can only request
  read-only tools scoped to the checked-out head tree; they have no shell or
  write tools and no direct arbitrary network access. Revera makes configured
  provider and optional Vera requests, accepts structured findings, and
  decides what is published.
- The GitHub token needs `pull-requests: write` for publishing and is used
  for nothing else. Fork PRs are skipped by default because secrets are not
  available to them.
- Release binaries are published with SHA-256 checksums;
  `scripts/install-revera.sh` verifies them before install.
