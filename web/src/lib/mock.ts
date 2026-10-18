import { parseDocument } from 'yaml';
import type {
  JobSummary,
  JobDetail,
  Repository,
  Settings,
  Session,
  SetupStage,
  Role,
  ReviewMode,
  Trigger,
  ExternalPolicy,
  ExternalPR,
  TrackedPR,
} from './types';

const ago = (minutes: number) => new Date(Date.now() - minutes * 60000).toISOString();
const repoNames = ['api', 'web', 'worker'];
const titles = [
  'Preserve session expiry when refreshing tokens',
  'Add cursor pagination to the activity feed',
  'Handle empty results in repository search',
  'Reduce duplicate database queries',
  'Update retry policy for background tasks',
  'Keep user preferences during migration',
  'Add request tracing to the API',
];
let jobs: JobSummary[] = titles.map((title, i) => ({
  source: 'installed',
  publication: 'github',
  id: `review-${107 - i}`,
  kind: 'review',
  repo: `northstar/${repoNames[i % 3]}`,
  pr: 248 - i * 7,
  title,
  author: ['alex', 'sam', 'morgan'][i % 3],
  head_sha: '8c74a1e9bf062d4a18f60c32',
  trigger: (['mention', 'auto', 'mention', 'manual', 'auto', 'rerun', 'auto'] as const)[i],
  mode: (['standard', 'quick', 'deep', 'standard', 'deep', 'standard', 'quick'] as const)[i],
  status: (['running', 'queued', 'complete', 'complete', 'partial', 'failed', 'complete'] as const)[
    i
  ],
  findings: {
    total: i === 2 ? 2 : i === 3 ? 1 : 0,
    high: i === 2 ? 1 : 0,
    medium: i === 2 || i === 3 ? 1 : 0,
    low: 0,
  },
  queued_at: ago([3, 1, 22, 50, 101, 144, 213][i]),
  started_at: i !== 1 ? ago([2, 1, 21 + 2 / 60, 48.6, 100, 142.8, 212 + 7 / 60][i]) : undefined,
  finished_at: i > 1 ? ago([2, 1, 18, 46, 95, 142, 210][i]) : undefined,
  duration_ms: i > 1 ? [0, 0, 182000, 156000, 300000, 48000, 127000][i] : undefined,
  tokens: i > 1 ? { prompt: 18420 + i * 1400, completion: 2120, cached: 8400 } : undefined,
  reason:
    i === 4
      ? 'The investigator reached its time limit. Validated findings were preserved.'
      : i === 5
        ? 'The model endpoint did not respond. Check the endpoint in Settings.'
        : undefined,
  pr_url: `https://github.com/northstar/${repoNames[i % 3]}/pull/${248 - i * 7}`,
  check_run_url: 'https://github.com/northstar/api/checks',
}));
let repos: Repository[] = [
  {
    full_name: 'northstar/api',
    private: true,
    default_branch: 'main',
    trigger: 'mention',
    mode: 'standard',
    trigger_override: null,
    mode_override: null,
    has_override: false,
    index: { state: 'ready', updated_at: ago(34) },
    last_job: jobs[0],
    pushed_at: ago(8),
    open_prs: 8,
    review_count: 18,
    finding_count: 7,
    html_url: 'https://github.com/northstar/api',
  },
  {
    full_name: 'northstar/web',
    private: false,
    default_branch: 'main',
    trigger: 'auto',
    mode: 'quick',
    trigger_override: 'auto',
    mode_override: 'quick',
    has_override: true,
    index: { state: 'ready', updated_at: ago(68) },
    last_job: jobs[3],
    pushed_at: ago(240),
    open_prs: 3,
    review_count: 12,
    finding_count: 5,
    html_url: 'https://github.com/northstar/web',
  },
  {
    full_name: 'northstar/worker',
    private: true,
    default_branch: 'main',
    trigger: 'mention',
    mode: 'deep',
    trigger_override: null,
    mode_override: 'deep',
    has_override: false,
    index: { state: 'building' },
    last_job: jobs[2],
    pushed_at: ago(65),
    open_prs: 2,
    review_count: 6,
    finding_count: 2,
    html_url: 'https://github.com/northstar/worker',
  },
  {
    full_name: 'northstar/docs',
    private: false,
    default_branch: 'main',
    trigger: 'off',
    mode: 'standard',
    trigger_override: 'off',
    mode_override: null,
    has_override: false,
    index: { state: 'none' },
    pushed_at: ago(45 * 1440),
    open_prs: 0,
    review_count: 0,
    finding_count: 0,
    html_url: 'https://github.com/northstar/docs',
  },
  {
    full_name: 'northstar/internal-tools',
    private: true,
    default_branch: 'main',
    trigger: 'auto',
    mode: 'standard',
    trigger_override: 'auto',
    mode_override: null,
    has_override: false,
    index: { state: 'none', retry_at: new Date(Date.now() + 5 * 60000).toISOString() },
    pushed_at: ago(10 * 1440),
    open_prs: 1,
    review_count: 3,
    finding_count: 1,
    html_url: 'https://github.com/northstar/internal-tools',
  },
  ...['billing', 'cli', 'design-system', 'gateway', 'mobile', 'notifications', 'sdk'].map(
    (name, i): Repository => ({
      full_name: `northstar/${name}`,
      private: i % 2 === 0,
      default_branch: i === 2 ? 'develop' : 'main',
      trigger: i === 6 ? 'off' : i === 3 ? 'auto' : 'mention',
      mode: i === 0 ? 'deep' : 'standard',
      trigger_override: i === 6 ? 'off' : i === 3 ? 'auto' : null,
      mode_override: i === 0 ? 'deep' : null,
      has_override: i === 0,
      index: {
        state: i === 6 ? 'none' : 'ready',
        ...(i === 6 ? {} : { updated_at: ago((i + 1) * 1440) }),
      },
      ...(i === 5
        ? {}
        : {
            pushed_at: ago([180, 2 * 1440, 6 * 1440, 30, 20 * 1440, 0, 60 * 1440][i]),
            open_prs: [4, 1, 0, 6, 2, 0, 0][i],
          }),
      review_count: [15, 2, 0, 9, 4, 0, 1][i],
      finding_count: [3, 1, 0, 2, 1, 0, 0][i],
      html_url: `https://github.com/northstar/${name}`,
    }),
  ),
];
const initialYaml = `# Default review configuration
# Repositories can supply their own revera.yaml on the base branch.
models:
  # Reads the diff and proposes findings.
  investigator:
    protocol: openai-responses
    base_url: https://api.example.com/v1
    api_key_env: OPENAI_API_KEY
    model: model-large
    max_output_tokens: 16000
    reasoning: medium
    fallback:
      - protocol: anthropic
        base_url: https://backup.example.com
        api_key_env: ANTHROPIC_API_KEY
        model: model-small
  # Re-checks each finding in a fresh context.
  validator:
    protocol: anthropic
    base_url: https://backup.example.com
    api_key_env: ANTHROPIC_API_KEY
    model: model-small
    max_output_tokens: 8000
    reasoning: medium

review:
  strategy: baseline
  min_severity: low
  max_findings: 10
  publish: comment

budget:
  agent_max_tool_calls: 25
  agent_max_seconds: 300
  run_max_requests: 120
  run_max_seconds: 900
  retries: 3

vera:
  enabled: true
  backend: local

web_search:
  enabled: true
  allow_private: false
  max_searches_per_review: 4
  max_reads_per_review: 4
`;
let settings: Settings = {
  yaml: import.meta.env.VITE_MOCK_EMPTY_YAML === '1' ? '' : initialYaml,
  server: {
    external: {
      tracking_enabled: false,
      poll_minutes: 60,
      max_tracked: 25,
      max_repo_size_kb: 1048576,
      reviews_per_hour: 20,
      concurrency: 1,
      retention_days: 7,
      max_checkouts: 50,
    },
    admins: ['alex'],
    allowed_owners: ['northstar'],
    default_trigger: 'mention',
    default_mode: 'standard',
    repos: Object.fromEntries(
      repos
        .filter((repo) => repo.trigger_override || repo.mode_override)
        .map((repo) => [
          repo.full_name,
          { trigger: repo.trigger_override, mode: repo.mode_override },
        ]),
    ),
    review_drafts: false,
    skip_authors: ['dependabot[bot]', 'renovate[bot]'],
  },
  secrets:
    import.meta.env.VITE_MOCK_EMPTY_YAML === '1'
      ? []
      : [
          { name: 'OPENAI_API_KEY', source: 'dashboard', environment: true },
          { name: 'ANTHROPIC_API_KEY', source: 'environment', environment: true },
        ],
  referenced_env: [],
};
let stage: SetupStage = 'done';
let setupAuthorized = true;
let signedIn = true;
const listeners = new Set<(event: 'job' | 'repos', job?: JobSummary) => void>();
export function subscribeMock(listener: (event: 'job' | 'repos', job?: JobSummary) => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
const emit = (event: 'job' | 'repos', job?: JobSummary) =>
  listeners.forEach((listener) => listener(event, job));
export function setMockStage(next: SetupStage, authorized = true) {
  stage = next;
  setupAuthorized = authorized;
  signedIn = next !== 'create_app' && next !== 'sign_in';
}

function references() {
  const found = new Set<string>();
  const walk = (value: unknown, key = '') => {
    if (typeof value === 'string') {
      if (key === 'api_key_env') found.add(value);
      for (const match of value.matchAll(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g)) found.add(match[1]);
    } else if (Array.isArray(value)) value.forEach((item) => walk(item));
    else if (value && typeof value === 'object')
      Object.entries(value).forEach(([name, item]) => walk(item, name));
  };
  const config = parseDocument(settings.yaml).toJS();
  walk(config);
  if (config?.web_search?.enabled) found.add(config.web_search.api_key_env ?? 'EXA_API_KEY');
  settings.referenced_env = [...found];
  for (const name of found)
    if (!settings.secrets.some((secret) => secret.name === name))
      settings.secrets.push({ name, source: 'missing', environment: false });
}
let jobSequence = 0;
function enqueue(
  repo: string,
  pr?: number,
  mode?: ReviewMode | null,
  trigger: JobSummary['trigger'] = 'manual',
) {
  const repository = repos.find((item) => item.full_name === repo);
  if (repository?.trigger === 'off') throw new Error('Reviews are off for this repository.');
  const job: JobSummary = {
    source: 'installed',
    publication: 'github',
    id: `job-${Date.now()}-${++jobSequence}`,
    repo,
    pr,
    title: pr ? 'Manual review' : 'Repository index',
    kind: pr ? 'review' : 'index',
    trigger,
    mode: mode || repository?.mode || settings.server.default_mode,
    status: 'queued',
    findings: { total: 0, high: 0, medium: 0, low: 0 },
    queued_at: ago(0),
  };
  jobs = [job, ...jobs];
  emit('job', job);
  return job;
}

// Lane B: public discovery/selection and private jobs, isolated from installed mocks.
const externalChoices: ExternalPR[] = [1, 2, 3].map((number) => ({
  url: `https://github.com/community/small-project/pull/${number}`,
  repo: 'community/small-project',
  number,
  title: `Public contribution ${number}`,
}));
let externalTracked: TrackedPR[] = [];
const externalIgnored = new Set<string>();
function externalTarget(url: string) {
  const match =
    /^https:\/\/github\.com\/([A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+)\/pull\/([1-9][0-9]*)$/.exec(url);
  if (!match || match[1].split('/').some((part) => part === '.' || part === '..'))
    throw new Error('Use https://github.com/OWNER/REPO/pull/NUMBER');
  return { repo: match[1], number: Number(match[2]) };
}
function externalEnqueue(url: string, mode: ReviewMode, trigger: JobSummary['trigger'] = 'manual') {
  const target = externalTarget(url);
  const job: JobSummary = {
    id: `external-${++jobSequence}`,
    source: 'external',
    publication: 'private',
    repo_id: 700,
    ...target,
    pr: target.number,
    title: `Private review of ${target.repo} #${target.number}`,
    kind: 'review',
    trigger,
    mode,
    status: 'queued',
    pr_url: url,
    head_sha: 'a'.repeat(40),
    base_sha: 'b'.repeat(40),
    findings: { total: 0, high: 0, medium: 0, low: 0 },
    queued_at: ago(0),
  };
  jobs = [job, ...jobs];
  emit('job', job);
  return job;
}
async function externalRequest(
  path: string,
  method: string,
  data?: Record<string, unknown>,
): Promise<{ handled: boolean; value?: unknown }> {
  let value: unknown;
  if (path === '/api/settings/external') {
    if (method === 'PUT')
      settings.server.external = structuredClone(data) as unknown as ExternalPolicy;
    value = structuredClone(settings.server.external);
  } else if (path === '/api/external/discover') {
    value = {
      prs: externalChoices.filter((pr) => !externalIgnored.has(pr.url)),
      incomplete: false,
    };
  } else if (path === '/api/external/reviews') {
    value = { id: externalEnqueue(String(data?.url), data?.mode as ReviewMode).id };
  } else if (path === '/api/external/tracking') {
    if (method === 'POST') {
      if (!settings.server.external.tracking_enabled)
        throw new Error('Enable Track my external PRs first.');
      const url = String(data?.url);
      const target = externalTarget(url);
      externalTracked = externalTracked.filter((pr) => pr.url !== url);
      externalTracked.push({
        url,
        ...target,
        repo_id: 700,
        actor: 'alex',
        title: `Public contribution ${target.number}`,
        mode: data?.mode as ReviewMode,
        status: 'tracking',
        reason: null,
        head_sha: '',
        next_poll: 0,
      });
    }
    value = { tracked: structuredClone(externalTracked) };
  } else if (path.startsWith('/api/external/tracking/')) {
    const [, number] = path.slice('/api/external/tracking/'.length).split('/');
    externalTracked = externalTracked.map((pr) =>
      pr.number === Number(number)
        ? { ...pr, status: 'stopped', reason: 'untracked manually' }
        : pr,
    );
    value = { ok: true };
  } else if (path === '/api/external/ignore') {
    externalTarget(String(data?.url));
    externalIgnored.add(String(data?.url));
    value = { ok: true };
  } else return { handled: false };
  return { handled: true, value };
}

export async function mockRequest(path: string, method: string, body?: unknown): Promise<unknown> {
  const url = new URL(path, 'https://revera.example.com');
  const data = body as Record<string, unknown> | undefined;
  const external = await externalRequest(url.pathname, method, data);
  if (external.handled) return external.value;
  if (url.pathname === '/api/session')
    return {
      authenticated: signedIn,
      user: signedIn ? { login: 'alex', avatar_url: '/avatar.svg' } : undefined,
      csrf: 'mock-csrf',
      setup: {
        stage,
        public_url: 'https://revera.northstar.example.com',
        authorized: setupAuthorized,
      },
    } satisfies Session;
  if (url.pathname === '/api/logout') {
    signedIn = false;
    return;
  }
  if (url.pathname === '/api/setup/manifest')
    return {
      post_url: 'https://github.com/settings/apps/new',
      manifest: { name: url.searchParams.get('name'), public: false },
    };
  if (url.pathname === '/api/overview')
    return {
      app: {
        name: 'Revera',
        slug: 'revera-northstar',
        html_url: 'https://github.com/apps/revera-northstar',
        install_url: 'https://github.com/apps/revera-northstar/installations/new',
        settings_url:
          'https://github.com/organizations/northstar/settings/apps/revera-northstar/permissions',
        issue_comment_subscribed: false,
        owner: 'northstar',
      },
      health: {
        github: { ok: true },
        models: ['investigator', 'validator'].map((role) => ({
          role,
          model: role === 'investigator' ? 'model-large' : 'model-small',
          ok: true,
          checked_at: ago(12),
        })),
        vera: { enabled: true, version: '2.1.4' },
      },
      queue: {
        running: jobs.filter((job) => job.status === 'running'),
        queued: jobs.filter((job) => job.status === 'queued'),
      },
      last_24h: {
        reviews: 24,
        findings: 8,
        failed: 1,
        tokens: 382400,
        severity: { high: 2, medium: 4, low: 2 },
        prompt_tokens: 340000,
        cached_tokens: 129200,
        median_duration_ms: 156000,
      },
      last_7d: {
        reviews: 127,
        findings: 35,
        failed: 3,
        tokens: 2148000,
        severity: { high: 7, medium: 19, low: 9 },
        prompt_tokens: 1900000,
        cached_tokens: 893000,
        median_duration_ms: 182000,
      },
      history_since: ago(5 * 1440),
      recent: jobs.slice(0, 10),
    };
  if (url.pathname === '/api/jobs') {
    const q = (url.searchParams.get('q') || '').toLowerCase();
    const selected = jobs.filter(
      (job) =>
        ['repo', 'status', 'kind'].every(
          (field) =>
            !url.searchParams.get(field) ||
            job[field as 'repo' | 'status' | 'kind'] === url.searchParams.get(field),
        ) &&
        (!q ||
          [job.title, job.author, job.repo].some((value) => value?.toLowerCase().includes(q)) ||
          (/^#?\d+$/.test(q) && job.pr === Number(q.replace(/^#/, '')))),
    );
    const start = url.searchParams.get('before')
      ? selected.findIndex((job) => job.id === url.searchParams.get('before')) + 1
      : 0;
    const limit = Number(url.searchParams.get('limit') || 25);
    return {
      jobs: selected.slice(start, start + limit),
      next: selected.length > start + limit ? selected[start + limit - 1].id : undefined,
    };
  }
  const jobMatch = url.pathname.match(/^\/api\/jobs\/([^/]+)(?:\/(log|rerun|cancel))?$/);
  if (jobMatch) {
    const job = jobs.find((item) => item.id === jobMatch[1]);
    if (!job) throw new Error('Review not found.');
    if (jobMatch[2] === 'log')
      return [
        `[${job.queued_at}] Review queued: ${job.repo} #${job.pr ?? 'index'}`,
        `[${job.started_at ?? job.queued_at}] Investigator started.`,
        'Reading the diff and repository context.',
        '2 candidates proposed. Starting fresh-context validation.',
        '2 findings validated. Review complete.',
        '',
      ].join('\n');
    if (jobMatch[2] === 'rerun') {
      if (job.source === 'external')
        return {
          id: externalEnqueue(job.pr_url!, (data?.mode as ReviewMode) || job.mode, 'rerun').id,
        };
      return { id: enqueue(job.repo, job.pr, data?.mode as ReviewMode | null, 'rerun').id };
    }
    if (jobMatch[2] === 'cancel') {
      job.status = 'cancelled';
      job.finished_at = ago(0);
      emit('job', job);
      return;
    }
    return {
      job: { ...job },
      summary_markdown:
        job.status === 'complete'
          ? `## Revera review

**Status: complete** — ${job.findings.total} validated findings

- **[high]** \`src/search/results.ts\`:84 — An empty result can leave the previous query visible
- **[medium]** \`src/search/results.ts\`:112 — A late response can overwrite a newer search

Checked: the changed search handler and its callers.
- not checked: the deployment environment.

> Review incomplete: investigator reported 1 unchecked area(s).

### Resolved since last review

- Retry policy no longer drops the final attempt

_Timing: total 182s · index 0.1s · lanes 112s · validation 68s (p50 16s, p95 24s)_

<sub>revera · strategy baseline · models: investigator=model-large@high, validator=model-small@medium</sub>`
          : (job.reason ??
            'The review is in progress. Findings will appear here after validation.'),
      findings: job.findings.total
        ? [
            {
              severity: 'high' as const,
              file: 'src/search/results.ts',
              line: 84,
              title: 'An empty result can leave the previous query visible',
              body:
                'When the search returns no matches, the early return leaves the previous ' +
                'result set in state. Clear the stored results before returning so a new query ' +
                'cannot display stale entries.',
              status: 'validated',
            },
            {
              severity: 'medium' as const,
              file: 'src/search/results.ts',
              line: 112,
              title: 'A late response can overwrite a newer search',
              body:
                'The request is not tied to the active query. If responses arrive out of ' +
                'order, an older query replaces the current results. Compare a request ' +
                'identifier before updating state.',
              status: 'validated',
            },
          ].slice(0, job.findings.total)
        : [],
      strategy: 'baseline',
      stats: {
        candidates: 4,
        accepted: job.findings.total,
        rejected: 2,
        files_read: 12,
        retrieval: 'vera',
        validation: true,
      },
      timing: {
        total_ms: 182000,
        vera_index_ms: 100,
        lanes_ms: 112000,
        validate_ms: 68000,
        validate_p50_ms: 16000,
        validate_p95_ms: 24000,
        first_candidate_ms: 21000,
        first_validated_ms: 128000,
        incomplete_phases: [],
        phases: [],
        skipped_phases: [],
      },
      ledger: {
        requests: 14,
        prompt_tokens: job.tokens?.prompt ?? 0,
        completion_tokens: job.tokens?.completion ?? 0,
        cached_prompt_tokens: job.tokens?.cached ?? 0,
        reasoning_tokens: 1400,
        cache_write_tokens: 0,
        wall_ms: 180000,
        by_route: [
          {
            role: 'investigator',
            model: 'model-large',
            route: 'investigator',
            requests: 9,
            prompt_tokens: 14220,
            completion_tokens: 1420,
            cached_prompt_tokens: 6000,
            reasoning_tokens: 1000,
            requested_reasoning: 'high',
            effective_reasoning: 'high',
            cache_write_tokens: 0,
          },
          {
            role: 'validator',
            model: 'model-small',
            route: 'validator',
            requests: 5,
            prompt_tokens: 7000,
            completion_tokens: 700,
            cached_prompt_tokens: 2400,
            reasoning_tokens: 400,
            requested_reasoning: 'medium',
            effective_reasoning: 'medium',
            cache_write_tokens: 0,
          },
        ],
        ...(job.id === 'review-105'
          ? {
              fallback_events: [
                {
                  role: 'investigator',
                  from_model: 'model-large',
                  to_model: 'model-small',
                  reason: 'rate_limit_or_server',
                },
              ],
              unavailable_routes: [
                { role: 'validator', model: 'model-alternative', reason: 'missing_key' },
              ],
            }
          : {}),
      },
    } satisfies JobDetail;
  }
  if (url.pathname === '/api/repos') {
    if (method !== 'PATCH')
      return structuredClone(
        repos.map((repo) => ({
          ...repo,
          last_review: jobs.find((job) => job.repo === repo.full_name && job.kind === 'review'),
        })),
      );
    const names = data?.repos;
    if (
      !Array.isArray(names) ||
      !names.length ||
      names.length > 1000 ||
      !data ||
      !('trigger' in data || 'mode' in data)
    )
      throw new Error('Choose 1–1000 repositories and at least one setting.');
    if (names.some((name) => !repos.some((repo) => repo.full_name === name)))
      throw new Error('Repository not found.');
    if (
      ('trigger' in data &&
        data.trigger !== null &&
        !['mention', 'auto', 'off'].includes(String(data.trigger))) ||
      ('mode' in data &&
        data.mode !== null &&
        !['quick', 'standard', 'deep'].includes(String(data.mode)))
    )
      throw new Error('Invalid repository setting.');
    for (const name of new Set(names as string[])) {
      const override = { ...settings.server.repos[name] };
      if ('trigger' in data) override.trigger = data.trigger as Trigger | null;
      if ('mode' in data) override.mode = data.mode as ReviewMode | null;
      settings.server.repos[name] = override;
    }
    syncRepos();
    emit('repos');
    return { ok: true, updated: new Set(names).size };
  }
  const repoMatch = url.pathname.match(
    /^\/api\/repos\/([^/]+)\/([^/]+)(?:\/(index|pulls)(?:\/(\d+)\/review)?)?$/,
  );
  if (repoMatch) {
    const name = `${decodeURIComponent(repoMatch[1])}/${decodeURIComponent(repoMatch[2])}`;
    const repo = repos.find((item) => item.full_name === name);
    if (!repo) throw new Error('Repository not found.');
    if (method === 'PATCH') {
      const override = { ...settings.server.repos[name] };
      if (data && 'trigger' in data) override.trigger = data.trigger as Trigger | null;
      if (data && 'mode' in data) override.mode = data.mode as ReviewMode | null;
      settings.server.repos[name] = override;
      syncRepos();
      emit('repos');
      return;
    }
    if (repoMatch[3] === 'index') {
      repo.index.state = 'building';
      enqueue(name);
      emit('repos');
      return;
    }
    if (repoMatch[4]) {
      return { id: enqueue(name, Number(repoMatch[4]), data?.mode as ReviewMode | null).id };
    }
    if (repoMatch[3] === 'pulls')
      return [
        {
          number: 251,
          title: 'Handle connection timeouts',
          author: 'sam',
          draft: false,
          head_sha: '9a32b88',
          updated_at: ago(23),
          last_job: { ...jobs[2], repo: name, pr: 251 },
        },
        {
          number: 249,
          title: 'Simplify configuration loading',
          author: 'alex',
          draft: true,
          head_sha: 'b4cf092',
          updated_at: ago(60),
        },
      ];
  }
  if (url.pathname === '/api/settings') {
    references();
    return structuredClone(settings);
  }
  if (url.pathname === '/api/settings/config') {
    const doc = parseDocument(String(data?.yaml));
    if (doc.errors.length) throw new Error(doc.errors[0].message);
    settings.yaml = String(data?.yaml);
    references();
    return;
  }
  if (url.pathname === '/api/settings/server') {
    if (method === 'GET') return structuredClone(settings.server);
    settings.server = structuredClone(body) as Settings['server'];
    syncRepos();
    emit('repos');
    return;
  }
  if (url.pathname.startsWith('/api/secrets/')) {
    const name = decodeURIComponent(url.pathname.split('/').at(-1)!);
    const environment =
      settings.secrets.find((secret) => secret.name === name)?.environment || false;
    settings.secrets = settings.secrets.filter((secret) => secret.name !== name);
    const status = {
      name,
      source: method === 'DELETE' ? (environment ? 'environment' : 'missing') : 'dashboard',
      environment,
    } as const;
    settings.secrets.push(status);
    return status;
  }
  if (url.pathname === '/api/settings/test') {
    if (data?.fallback !== undefined) {
      const doc = parseDocument(settings.yaml);
      const role =
        data.role === 'validator' && !doc.hasIn(['models', 'validator'])
          ? 'investigator'
          : String(data.role);
      if (!doc.hasIn(['models', role, 'fallback', Number(data.fallback)]))
        throw new Error('Fallback model index is out of range.');
    } else if (data?.role === 'investigator' || data?.role === 'validator') stage = 'done';
    return { ok: true, latency_ms: 342 };
  }
  if (url.pathname === '/api/settings/models')
    return { models: ['model-large', 'model-small', 'model-alternative'] };
  throw new Error(`Mock endpoint not found: ${method} ${path}`);
}

function syncRepos() {
  for (const repo of repos) {
    const override = settings.server.repos[repo.full_name];
    repo.trigger_override = override?.trigger ?? null;
    repo.mode_override = override?.mode ?? null;
    repo.trigger = repo.trigger_override ?? settings.server.default_trigger;
    repo.mode = repo.mode_override ?? settings.server.default_mode;
  }
}
