import type {
  Session,
  Overview,
  JobsPage,
  JobDetail,
  Repository,
  PullRequest,
  Settings,
  ServerSettings,
  Role,
  TestResult,
  Trigger,
  ReviewMode,
  ExternalPolicy,
  ExternalPR,
  TrackedPR,
  Installations,
  FixSummary,
  FixRecord,
  FixPlan,
  FixDetail,
  FixAction,
  FixSettings,
  FixPolicy,
  FixOverride,
  FixDelivery,
  PullInbox,
  PullDetail,
} from './types';

export const mockMode = import.meta.env.VITE_MOCK === '1';
let csrf = '';

export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
  ) {
    super(message);
  }
}

async function request<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  if (mockMode) {
    const { mockRequest } = await import('./mock');
    const payload = body === undefined ? undefined : JSON.parse(JSON.stringify(body));
    return mockRequest(path, method, payload) as Promise<T>;
  }
  const response = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: {
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...(method === 'GET' ? {} : { 'X-Revera-CSRF': csrf }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    const data = await response.json().catch(() => ({ error: response.statusText }));
    throw new ApiError(data.error || `Request failed (${response.status})`, response.status);
  }
  if (response.status === 204) return undefined as T;
  if (response.headers.get('content-type')?.startsWith('text/'))
    return (await response.text()) as T;
  return response.json();
}

const repoPath = (repo: string) => repo.split('/').map(encodeURIComponent).join('/');
// The repository chooser retains its live per-repository listing.
function pulls(): Promise<PullInbox>;
function pulls(repo: string): Promise<PullRequest[]>;
function pulls(repo?: string): Promise<PullInbox | PullRequest[]> {
  return request(repo === undefined ? '/api/pulls' : `/api/repos/${repoPath(repo)}/pulls`);
}
export const api = {
  async session() {
    const session = await request<Session>('/api/session');
    csrf = session.csrf ?? '';
    return session;
  },
  logout: () => request<void>('/api/logout', 'POST'),
  manifest: (token: string, name: string, org: string, publicApp = false, publishFixes = false) =>
    request<{ post_url: string; manifest: unknown }>(
      `/api/setup/manifest?${new URLSearchParams({ token, name, org, public: String(publicApp), publish_fixes: String(publishFixes) })}`,
    ),
  overview: () => request<Overview>('/api/overview'),
  // Public metadata reads and private reviews never use an installed publication path.
  externalReview: (url: string, mode: ReviewMode) =>
    request<{ id: string }>('/api/external/reviews', 'POST', { url, mode }),
  externalDiscover: () =>
    request<{ prs: ExternalPR[]; incomplete: boolean }>('/api/external/discover'),
  externalTracking: () => request<{ tracked: TrackedPR[] }>('/api/external/tracking'),
  externalTrack: (url: string, mode: ReviewMode) =>
    request<void>('/api/external/tracking', 'POST', { url, mode }),
  externalUntrack: (repoId: number, number: number) =>
    request<void>(`/api/external/tracking/${repoId}/${number}`, 'DELETE'),
  externalIgnore: (url: string) => request<void>('/api/external/ignore', 'POST', { url }),
  externalPolicy: () => request<ExternalPolicy>('/api/settings/external'),
  saveExternalPolicy: (policy: ExternalPolicy) =>
    request<void>('/api/settings/external', 'PUT', policy),
  jobs: (filters: Record<string, string> = {}) =>
    request<JobsPage>(
      `/api/jobs?${new URLSearchParams({
        limit: '25',
        ...Object.fromEntries(Object.entries(filters).filter(([, value]) => value)),
      })}`,
    ),
  job: (id: string, diagnostics = false) =>
    request<JobDetail>(`/api/jobs/${encodeURIComponent(id)}${diagnostics ? '?diagnostics=1' : ''}`),
  log: (id: string) => request<string>(`/api/jobs/${encodeURIComponent(id)}/log`),
  rerun: (id: string, mode: ReviewMode | null = null) =>
    request<{ id: string }>(`/api/jobs/${encodeURIComponent(id)}/rerun`, 'POST', { mode }),
  cancel: (id: string) => request<void>(`/api/jobs/${encodeURIComponent(id)}/cancel`, 'POST'),
  repos: () => request<Repository[]>('/api/repos'),
  // Connected accounts.
  installations: () => request<Installations>('/api/installations'),
  reconcileInstallations: () => request<Installations>('/api/installations/reconcile', 'POST'),
  owner: (login: string, approve: boolean) =>
    request<{ ok: boolean; warning: string | null }>(
      `/api/owners/${encodeURIComponent(login)}`,
      approve ? 'PUT' : 'DELETE',
    ),
  bulkRepoSettings: (
    repos: string[],
    settings: { trigger?: Trigger | null; mode?: ReviewMode | null },
  ) => request<{ ok: boolean; updated: number }>('/api/repos', 'PATCH', { repos, ...settings }),
  repoSettings: (repo: string, settings: { trigger?: Trigger | null; mode?: ReviewMode | null }) =>
    request<void>(`/api/repos/${repoPath(repo)}`, 'PATCH', settings),
  index: (repo: string) => request<void>(`/api/repos/${repoPath(repo)}/index`, 'POST'),
  // Installed pull-request inbox and conversation.
  pulls,
  pull: (owner: string, repo: string, number: number) =>
    request<PullDetail>(`/api/repos/${repoPath(`${owner}/${repo}`)}/pulls/${number}`),
  review: (repo: string, number: number, mode: ReviewMode | null = null) =>
    request<{ id: string }>(`/api/repos/${repoPath(repo)}/pulls/${number}/review`, 'POST', {
      mode,
    }),
  settings: () => request<Settings>('/api/settings'),
  serverSettings: () => request<ServerSettings>('/api/settings/server'),
  saveConfig: (yaml: string) => request<void>('/api/settings/config', 'PUT', { yaml }),
  saveServer: (server: ServerSettings) => request<void>('/api/settings/server', 'PUT', server),
  setSecret: (name: string, value: string) =>
    request<void>(`/api/secrets/${encodeURIComponent(name)}`, 'PUT', { value }),
  deleteSecret: (name: string) =>
    request<void>(`/api/secrets/${encodeURIComponent(name)}`, 'DELETE'),
  test: (role: Role, fallback?: number) =>
    request<TestResult>('/api/settings/test', 'POST', { role, fallback }),
  models: (role: Role) => request<{ models: string[] }>('/api/settings/models', 'POST', { role }),
  // Fix requests and server-owned policy.
  fixes: (filters: Record<string, string> = {}) =>
    request<{ fixes: FixSummary[] }>(`/api/fixes?${new URLSearchParams(filters)}`),
  fixPlan: (job: string, findings?: string[]) =>
    request<FixPlan>(
      `/api/fixes/plan?${new URLSearchParams({ job, ...(findings ? { findings: findings.join(',') } : {}) })}`,
    ),
  requestFix: (job: string, findings: string[], once: boolean) =>
    request<{ fix: FixRecord }>('/api/fixes', 'POST', { job, findings, once }),
  fix: (id: string) => request<FixDetail>(`/api/fixes/${encodeURIComponent(id)}`),
  fixAction: (
    id: string,
    action: FixAction,
    body?: { once?: boolean; delivery?: FixDelivery; note?: string },
  ) => request<{ fix: FixRecord }>(`/api/fixes/${encodeURIComponent(id)}/${action}`, 'POST', body),
  fixArtifact: (id: string, artifact: 'patch.diff' | 'instructions.md') =>
    request<string>(`/api/fixes/${encodeURIComponent(id)}/${artifact}`),
  fixSettings: () => request<FixSettings>('/api/settings/fixes'),
  saveFixSettings: (fixes: FixPolicy) => request<void>('/api/settings/fixes', 'PUT', { fixes }),
  repoFixSettings: (repo: string, override: FixOverride | null) =>
    request<void>(`/api/repos/${repoPath(repo)}/fixes`, 'PUT', override),
};

export function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : 'Something went wrong. Please try again.';
}
