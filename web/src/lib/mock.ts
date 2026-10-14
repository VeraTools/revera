import { parseDocument } from 'yaml';
import type { JobSummary, JobDetail, Repository, Settings, Session, SetupStage, Role } from './types';

const ago = (minutes: number) => new Date(Date.now() - minutes * 60000).toISOString();
const titles = ['Preserve session expiry when refreshing tokens', 'Add cursor pagination to the activity feed', 'Handle empty results in repository search', 'Reduce duplicate database queries', 'Update retry policy for background tasks', 'Keep user preferences during migration', 'Add request tracing to the API'];
let jobs: JobSummary[] = titles.map((title, i) => ({
  id: `review-${107 - i}`, kind: 'review', repo: i % 3 === 0 ? 'northstar/api' : i % 3 === 1 ? 'northstar/web' : 'northstar/worker',
  pr: 248 - i * 7, title, author: ['alex', 'sam', 'morgan'][i % 3], head_sha: '8c74a1e9bf062d4a18f60c32',
  trigger: 'webhook', status: (['running', 'queued', 'complete', 'complete', 'partial', 'failed', 'complete'] as const)[i],
  findings: { total: i === 2 ? 2 : i === 3 ? 1 : 0, high: i === 2 ? 1 : 0, medium: i === 2 || i === 3 ? 1 : 0, low: 0 },
  queued_at: ago([3, 1, 22, 50, 101, 144, 213][i]), started_at: i !== 1 ? ago([2, 1, 21 + 2 / 60, 48.6, 100, 142.8, 212 + 7 / 60][i]) : undefined,
  finished_at: i > 1 ? ago([2, 1, 18, 46, 95, 142, 210][i]) : undefined,
  duration_ms: i > 1 ? [0, 0, 182000, 156000, 300000, 48000, 127000][i] : undefined,
  tokens: i > 1 ? { prompt: 18420 + i * 1400, completion: 2120, cached: 8400 } : undefined,
  reason: i === 4 ? 'The investigator reached its time limit. Validated findings were preserved.' : i === 5 ? 'The model endpoint did not respond. Check the endpoint in Settings.' : undefined,
  pr_url: `https://github.com/northstar/${i % 3 === 0 ? 'api' : i % 3 === 1 ? 'web' : 'worker'}/pull/${248 - i * 7}`,
  check_run_url: 'https://github.com/northstar/api/checks'
}));
let repos: Repository[] = [
  { full_name: 'northstar/api', private: true, default_branch: 'main', enabled: true, has_override: false, index: { state: 'ready', updated_at: ago(34) }, last_job: jobs[0], html_url: 'https://github.com/northstar/api' },
  { full_name: 'northstar/web', private: false, default_branch: 'main', enabled: true, has_override: true, index: { state: 'ready', updated_at: ago(68) }, last_job: jobs[3], html_url: 'https://github.com/northstar/web' },
  { full_name: 'northstar/worker', private: true, default_branch: 'main', enabled: true, has_override: false, index: { state: 'building' }, last_job: jobs[2], html_url: 'https://github.com/northstar/worker' },
  { full_name: 'northstar/docs', private: false, default_branch: 'main', enabled: false, has_override: false, index: { state: 'none' }, html_url: 'https://github.com/northstar/docs' },
  { full_name: 'northstar/internal-tools', private: true, default_branch: 'main', enabled: true, has_override: false, index: { state: 'none' }, html_url: 'https://github.com/northstar/internal-tools' }
];
const initialYaml = `# Default review configuration\n# Repositories can supply their own revera.yaml on the base branch.\nmodels:\n  # Reads the diff and proposes findings.\n  investigator:\n    protocol: openai-chat\n    base_url: https://api.example.com/v1\n    api_key_env: REVERA_MODEL_API_KEY\n    model: model-name\n    max_output_tokens: 4000\n    temperature: 0.2\n    reasoning: medium\n  # Re-checks each finding in a fresh context.\n  validator:\n    protocol: openai-chat\n    base_url: https://api.example.com/v1\n    api_key_env: REVERA_MODEL_API_KEY\n    model: model-name\n    max_output_tokens: 4000\n    temperature: 0.2\n    reasoning: medium\n\nreview:\n  strategy: baseline\n  min_severity: low\n  max_findings: 10\n  publish: comment\n\nbudget:\n  agent_max_tool_calls: 25\n  agent_max_seconds: 300\n  run_max_requests: 120\n  run_max_seconds: 900\n  retries: 3\n\nvera:\n  enabled: true\n  backend: local\n`;
let settings: Settings = {
  yaml: import.meta.env.VITE_MOCK_EMPTY_YAML === '1' ? '' : initialYaml,
  server: { admins: ['alex'], allowed_owners: ['northstar'], default_enabled: true, repos: {}, review_drafts: false, skip_authors: ['dependabot[bot]', 'renovate[bot]'] },
  secrets: import.meta.env.VITE_MOCK_EMPTY_YAML === '1' ? [] : [{ name: 'REVERA_MODEL_API_KEY', source: 'environment' }], referenced_env: []
};
let stage: SetupStage = 'done';
let signedIn = true;
const listeners = new Set<(event: 'job' | 'repos', job?: JobSummary) => void>();
export function subscribeMock(listener: (event: 'job' | 'repos', job?: JobSummary) => void) { listeners.add(listener); return () => { listeners.delete(listener); }; }
const emit = (event: 'job' | 'repos', job?: JobSummary) => listeners.forEach((listener) => listener(event, job));
export function setMockStage(next: SetupStage) { stage = next; signedIn = next !== 'create_app' && next !== 'sign_in'; }

function references() {
  const found = new Set<string>();
  const walk = (value: unknown, key = '') => {
    if (typeof value === 'string') {
      if (key === 'api_key_env') found.add(value);
      for (const match of value.matchAll(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g)) found.add(match[1]);
    } else if (Array.isArray(value)) value.forEach((item) => walk(item));
    else if (value && typeof value === 'object') Object.entries(value).forEach(([name, item]) => walk(item, name));
  };
  walk(parseDocument(settings.yaml).toJS());
  settings.referenced_env = [...found];
  for (const name of found) if (!settings.secrets.some((secret) => secret.name === name)) settings.secrets.push({ name, source: 'missing' });
}
function enqueue(repo: string, pr?: number) {
  const job: JobSummary = { id: `job-${Date.now()}`, repo, pr, title: pr ? 'Manual review' : 'Repository index', kind: pr ? 'review' : 'index', trigger: 'manual', status: 'queued', findings: { total: 0, high: 0, medium: 0, low: 0 }, queued_at: ago(0) };
  jobs = [job, ...jobs]; emit('job', job); return job;
}

export async function mockRequest(path: string, method: string, body?: unknown): Promise<unknown> {
  const url = new URL(path, 'https://revera.example.com');
  const data = body as Record<string, unknown> | undefined;
  if (url.pathname === '/api/session') return { authenticated: signedIn, user: signedIn ? { login: 'alex', avatar_url: '/avatar.svg' } : undefined, csrf: 'mock-csrf', setup: { stage, public_url: 'https://revera.northstar.example.com' } } satisfies Session;
  if (url.pathname === '/api/logout') { signedIn = false; return; }
  if (url.pathname === '/api/setup/manifest') return { post_url: 'https://github.com/settings/apps/new', manifest: { name: url.searchParams.get('name'), public: false } };
  if (url.pathname === '/api/overview') return {
    app: { name: 'Revera', slug: 'revera-northstar', html_url: 'https://github.com/apps/revera-northstar', install_url: 'https://github.com/apps/revera-northstar/installations/new', owner: 'northstar' },
    health: { github: { ok: true }, models: ['investigator', 'validator'].map((role) => ({ role, model: 'model-name', ok: true, checked_at: ago(12) })), vera: { enabled: true, version: '2.1.3' } },
    queue: { running: jobs.filter((job) => job.status === 'running'), queued: jobs.filter((job) => job.status === 'queued') },
    last_24h: { reviews: 24, findings: 8, failed: 1, tokens: 382400 }, recent: jobs.slice(0, 10)
  };
  if (url.pathname === '/api/jobs') {
    const selected = jobs.filter((job) => ['repo', 'status', 'kind'].every((field) => !url.searchParams.get(field) || job[field as 'repo' | 'status' | 'kind'] === url.searchParams.get(field)));
    const start = url.searchParams.get('before') ? selected.findIndex((job) => job.id === url.searchParams.get('before')) + 1 : 0;
    const limit = Number(url.searchParams.get('limit') || 25);
    return { jobs: selected.slice(start, start + limit), next: selected.length > start + limit ? selected[start + limit - 1].id : undefined };
  }
  const jobMatch = url.pathname.match(/^\/api\/jobs\/([^/]+)(?:\/(log|rerun|cancel))?$/);
  if (jobMatch) {
    const job = jobs.find((item) => item.id === jobMatch[1]);
    if (!job) throw new Error('Review not found.');
    if (jobMatch[2] === 'log') return `[${job.queued_at}] Review queued: ${job.repo} #${job.pr ?? 'index'}\n[${job.started_at ?? job.queued_at}] Investigator started.\nReading the diff and repository context.\n2 candidates proposed. Starting fresh-context validation.\n2 findings validated. Review complete.\n`;
    if (jobMatch[2] === 'rerun') { enqueue(job.repo, job.pr); return; }
    if (jobMatch[2] === 'cancel') { job.status = 'cancelled'; job.finished_at = ago(0); emit('job', job); return; }
    return { job: { ...job }, summary_markdown: job.status === 'complete' ? 'Reviewed the changed code and its surrounding context. Two findings were independently re-checked in a fresh context before publication.' : job.reason ?? 'The review is in progress. Findings will appear here after validation.', findings: job.findings.total ? [
      { severity: 'high' as const, file: 'src/search/results.ts', line: 84, title: 'An empty result can leave the previous query visible', body: 'When the search returns no matches, the early return leaves the previous result set in state. Clear the stored results before returning so a new query cannot display stale entries.', status: 'validated' },
      { severity: 'medium' as const, file: 'src/search/results.ts', line: 112, title: 'A late response can overwrite a newer search', body: 'The request is not tied to the active query. If responses arrive out of order, an older query replaces the current results. Compare a request identifier before updating state.', status: 'validated' }
    ].slice(0, job.findings.total) : [], timing: { investigator_ms: 112000, validation_ms: 68000 }, ledger: { requests: 14, retries: 0 } } satisfies JobDetail;
  }
  if (url.pathname === '/api/repos') return structuredClone(repos);
  const repoMatch = url.pathname.match(/^\/api\/repos\/([^/]+)\/([^/]+)(?:\/(index|pulls)(?:\/(\d+)\/review)?)?$/);
  if (repoMatch) {
    const name = `${decodeURIComponent(repoMatch[1])}/${decodeURIComponent(repoMatch[2])}`;
    const repo = repos.find((item) => item.full_name === name);
    if (!repo) throw new Error('Repository not found.');
    if (method === 'PATCH') { repo.enabled = Boolean(data?.enabled); emit('repos'); return; }
    if (repoMatch[3] === 'index') { repo.index.state = 'building'; enqueue(name); emit('repos'); return; }
    if (repoMatch[4]) { enqueue(name, Number(repoMatch[4])); return; }
    if (repoMatch[3] === 'pulls') return [{ number: 251, title: 'Handle connection timeouts', author: 'sam', draft: false, head_sha: '9a32b88', updated_at: ago(23) }, { number: 249, title: 'Simplify configuration loading', author: 'alex', draft: true, head_sha: 'b4cf092', updated_at: ago(60) }];
  }
  if (url.pathname === '/api/settings') { references(); return structuredClone(settings); }
  if (url.pathname === '/api/settings/config') { const doc = parseDocument(String(data?.yaml)); if (doc.errors.length) throw new Error(doc.errors[0].message); settings.yaml = String(data?.yaml); references(); return; }
  if (url.pathname === '/api/settings/server') { settings.server = structuredClone(body) as Settings['server']; return; }
  if (url.pathname.startsWith('/api/secrets/')) {
    const name = decodeURIComponent(url.pathname.split('/').at(-1)!);
    settings.secrets = settings.secrets.filter((secret) => secret.name !== name);
    settings.secrets.push({ name, source: method === 'DELETE' ? 'missing' : 'dashboard' }); return;
  }
  if (url.pathname === '/api/settings/test') {
    if (data?.role === 'investigator' || data?.role === 'validator') stage = 'done';
    return { ok: true, latency_ms: 342 };
  }
  if (url.pathname === '/api/settings/models') return { models: ['model-name', 'model-name-large', 'model-name-small'] };
  throw new Error(`Mock endpoint not found: ${method} ${path}`);
}
