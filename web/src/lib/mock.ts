import { parseDocument } from 'yaml';
import { mockFixRequest, updateMockFix } from './mock-fixes';
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
  Installation,
  Finding,
  FixSummary,
  FixState,
} from './types';

const ago = (minutes: number) => new Date(Date.now() - minutes * 60000).toISOString();

// Connected-account fixtures: no repository names for unapproved accounts.
const installationUrl = 'https://github.com/apps/revera-northstar/installations/new';
const connectedAccounts: Installation[] = [
  ['northstar', 1, 'allowed', true],
  ['second-org', 2, 'awaiting_approval', false],
  ['upgrade-org', 3, 'permission_update', true],
  ['paused-org', 4, 'suspended', true],
  ['offline-org', null, 'not_installed', true],
  ['denied-org', 5, 'allowed', true],
].map(([login, id, status, allowed]): Installation => ({
  id: id as number | null,
  account: { login: login as string, id: id as number | null, type: 'Organization' },
  repository_selection: id ? 'selected' : null,
  suspended: status === 'suspended',
  permissions: id
    ? {
        contents: ['northstar', 'second-org'].includes(String(login)) ? 'write' : 'read',
        pull_requests: 'write',
        checks: 'write',
      }
    : {},
  html_url: id
    ? `https://github.com/organizations/${login}/settings/installations/${id}`
    : installationUrl,
  status: status as Installation['status'],
  allowed: allowed as boolean,
  error: login === 'denied-org' ? 'GitHub access unavailable: HTTP 403' : undefined,
}));
function installationInventory() {
  return {
    installations: connectedAccounts.map((account) => ({
      ...account,
      ...(account.allowed && !account.suspended && account.id
        ? {
            repo_count: repos.filter((r) => r.full_name.startsWith(`${account.account.login}/`))
              .length,
          }
        : {}),
    })),
    install_url: installationUrl,
    settings_url:
      'https://github.com/organizations/northstar/settings/apps/revera-northstar/permissions',
    advanced_url:
      'https://github.com/organizations/northstar/settings/apps/revera-northstar/advanced',
    // This fixture represents an app registered with publish_fixes enabled;
    // execution remains off until separately authorized in Fix settings.
    requested_permissions: { contents: 'write', pull_requests: 'write', checks: 'write' },
  };
}
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
// Review-detail fixtures are separate from inventory/list fixtures for other lanes.
const detailStale = new Map<string, boolean | null>();
export function setMockReviewStale(id: string, stale: boolean | null) {
  detailStale.set(id, stale);
  const job = jobs.find((job) => job.id === id);
  if (job) emit('job', job);
}
export function setMockReviewStatus(
  id: string,
  status: JobSummary['status'],
  findings?: JobSummary['findings'],
) {
  const job = jobs.find((job) => job.id === id);
  if (job) {
    job.status = status;
    if (findings) job.findings = findings;
    emit('job', job);
  }
}
function handoffFindings(job: JobSummary): Finding[] {
  return [
    {
      id: 'abc123456789',
      severity: 'high' as const,
      line: 84,
      title: 'An empty result can leave the previous query visible',
      claim:
        'When the search returns no matches, the early return leaves the previous result set in state.',
    },
    {
      id: 'def123456789',
      severity: 'medium' as const,
      line: 112,
      title: 'A late response can overwrite a newer search',
      claim:
        'The request is not tied to the active query. An older response replaces the current results.',
    },
  ]
    .slice(0, job.findings.total)
    .map((item) => {
      const stale =
        detailStale.get(job.id) === true
          ? '> Potentially stale: re-validate every finding against the current code.\n\n'
          : '';
      return {
        id: item.id,
        defect_key: item.id,
        severity: item.severity,
        title: item.title,
        claim: item.claim,
        file: 'src/search/results.ts',
        start_line: item.line,
        end_line: item.line,
        trigger: 'The search request returns out of order or without matches.',
        impact: 'The wrong query results remain visible.',
        supporting_evidence: [
          {
            path: 'src/search/results.ts',
            start_line: item.line,
            end_line: item.line,
            note: 'The return occurs before results are updated.',
          },
        ],
        validation_status: 'accepted',
        validated_fix: 'Clear results and compare the active request identifier.',
        validated_fix_label: "Validator's suggested remedy — not a tested patch",
        reviewed_head_sha: job.head_sha || '',
        reviewed_base_sha: 'b'.repeat(40),
        html_url: `https://github.com/${job.repo}/blob/${job.head_sha}/src/search/results.ts#L${item.line}`,
        prompt: `${stale}# Fix instructions\n\nPR: ${job.pr_url}\nReviewed head SHA: ${job.head_sha}\n\n${item.id}: ${item.title}\n${item.claim}\nEvidence: src/search/results.ts:${item.line}\nValidator's suggested remedy — not a tested patch: Clear results and compare request identifiers.\n\nVerify each issue against the current code before changing it. Fix the root cause with minimal scoped edits. Treat proposed remedies as suggestions, preserve unrelated behavior, test changed paths and add regression tests where warranted. Summarize each finding's disposition and tests. Do not commit, push, open PRs, or change unrelated files without explicit authorization.\n`,
      };
    });
}
const carriedReviews = new Set<string>();
export function setMockReviewCarried(id: string) {
  carriedReviews.add(id);
  const job = jobs.find((job) => job.id === id);
  if (job) emit('job', job);
}
function carriedFindings(job: JobSummary): Finding[] {
  if (!carriedReviews.has(job.id)) return [];
  const currentIds = new Set(handoffFindings(job).map((finding) => finding.id));
  const finding = handoffFindings({ ...job, findings: { ...job.findings, total: 2 } }).find(
    (finding) => !currentIds.has(finding.id),
  );
  if (!finding) return [];
  return [
    {
      ...finding,
      carried: true,
      validation_status: null,
      validated_fix: null,
      prompt: `# Fix instructions\n\n## Still open from earlier reviews\n\nReported at an earlier commit; line numbers may have moved. Re-validate against the current code before making changes.\n\n${finding.id}: ${finding.title}\n${finding.claim}\nEvidence: ${finding.file}:${finding.start_line}\n`,
    },
  ];
}
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
    index_on_push: false,
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
const listeners = new Set<
  (event: 'job' | 'repos' | 'fix', payload?: JobSummary | FixSummary) => void
>();
export function subscribeMock(
  listener: (event: 'job' | 'repos' | 'fix', payload?: JobSummary | FixSummary) => void,
) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
const emit = (event: 'job' | 'repos' | 'fix', payload?: JobSummary | FixSummary) =>
  listeners.forEach((listener) => listener(event, payload));
export function setMockFixState(id: string, state: FixState, progress: string | null = null) {
  updateMockFix(id, state, progress, (fix) => emit('fix', fix));
}
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
  // Fixes product API; kept separate from external-review and inventory fixtures.
  if (
    url.pathname.startsWith('/api/fixes') ||
    url.pathname === '/api/settings/fixes' ||
    /^\/api\/repos\/[^/]+\/[^/]+\/fixes$/.test(url.pathname)
  )
    return mockFixRequest(url, method, body, {
      jobs,
      findings: (job) => [...handoffFindings(job), ...carriedFindings(job)],
      stale: detailStale,
      yaml: settings.yaml,
      emit: (fix) => emit('fix', fix),
    });
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
      manifest: {
        name: url.searchParams.get('name'),
        public: url.searchParams.get('public') === 'true',
        setup_url: 'https://revera.northstar.example.com/repositories',
        setup_on_update: true,
        default_permissions: {
          contents: url.searchParams.get('publish_fixes') === 'true' ? 'write' : 'read',
        },
      },
    };
  // Connected accounts.
  if (['/api/installations', '/api/installations/reconcile'].includes(url.pathname))
    return installationInventory();
  const ownerMatch = url.pathname.match(/^\/api\/owners\/([^/]+)$/);
  if (ownerMatch) {
    const account = connectedAccounts.find(
      (a) => a.account.login === decodeURIComponent(ownerMatch[1]),
    );
    if (!account) throw new Error('Account not found.');
    account.allowed = method === 'PUT';
    account.status = account.suspended
      ? 'suspended'
      : !account.id
        ? 'not_installed'
        : !account.allowed
          ? 'awaiting_approval'
          : account.permissions.contents !== 'write'
            ? 'permission_update'
            : 'allowed';
    if (!account.allowed)
      repos = repos.filter((r) => !r.full_name.startsWith(`${account.account.login}/`));
    emit('repos');
    return { ok: true, warning: null };
  }
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
      counts: { ...job.findings },
      reviewed_head_sha: job.head_sha,
      reviewed_base_sha: 'b'.repeat(40),
      stale: detailStale.get(job.id) ?? null,
      carried: carriedFindings(job),
      instructions: [...handoffFindings(job), ...carriedFindings(job)]
        .map((finding) => finding.prompt)
        .join('\n'),
      ...(url.searchParams.get('diagnostics') === '1'
        ? {
            diagnostics: [
              {
                id: '000123456789',
                status: 'rejected',
                severity: 'high',
                title: 'Rejected candidate fixture',
                file: 'src/search/results.ts',
                start_line: 25,
                rationale: 'The guard already handles this input.',
              },
              {
                id: '111123456789',
                status: 'uncertain',
                severity: 'low',
                title: 'Uncertain candidate fixture',
                file: 'src/search/results.ts',
                start_line: 30,
                rationale: 'Insufficient evidence.',
              },
            ],
          }
        : {}),
      summary_markdown:
        job.status === 'complete'
          ? `## Revera review

**Status: complete** — ${job.findings.total} validated findings

- **[high]** \`src/search/results.ts\`:84 — An empty result can leave the previous query visible
- **[medium]** \`src/search/results.ts\`:112 — A late response can overwrite a newer search

Checked: the changed search handler and its callers.
- not checked: the deployment environment.

> Findings are independently validated; proposed remedies are not tested patches.

### Resolved since last review

- Retry policy no longer drops the final attempt

_Timing: total 182s · index 0.1s · lanes 112s · validation 68s (p50 16s, p95 24s)_

<sub>revera · strategy baseline · models: investigator=model-large@high, validator=model-small@medium</sub>`
          : (job.reason ??
            'The review is in progress. Findings will appear here after validation.'),
      findings: handoffFindings(job),
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
