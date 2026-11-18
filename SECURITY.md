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
  provider, optional Vera and opt-in external web-search (`web_search`, off by
  default; queries go to Exa and require public event metadata or explicit
  `allow_private: true`) requests, accepts structured findings, and decides
  what is published.
- Opt-in GitHub App fixes (`docs/server.md#fixes-opt-in`, off by default) edit a
  disposable head snapshot inside a separate Linux bubblewrap sandbox without a
  GitHub token; configured test commands run in their own sandbox without
  network or credentials, and Rust checks every patch before human-approved or
  explicitly configured draft-PR publication.
- In the Action, the GitHub token needs `contents: read` and
  `pull-requests: write` to read pull request data and publish comments, and is
  used for nothing else; fork PRs are skipped by default because secrets are
  not available to them. In the self-hosted GitHub App, jobs receive
  short-lived per-repository installation tokens scoped to each job's required
  permissions.
- Release binaries are published with SHA-256 checksums;
  `scripts/install-revera.sh` verifies them before install.
