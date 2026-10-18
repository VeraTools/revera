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
  if (response.headers.get('content-type')?.includes('text/plain'))
    return (await response.text()) as T;
  return response.json();
}

const repoPath = (repo: string) => repo.split('/').map(encodeURIComponent).join('/');
export const api = {
  async session() {
    const session = await request<Session>('/api/session');
    csrf = session.csrf ?? '';
    return session;
  },
  logout: () => request<void>('/api/logout', 'POST'),
  manifest: (token: string, name: string, org: string) =>
    request<{ post_url: string; manifest: unknown }>(
      `/api/setup/manifest?${new URLSearchParams({ token, name, org })}`,
    ),
  overview: () => request<Overview>('/api/overview'),
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
  bulkRepoSettings: (
    repos: string[],
    settings: { trigger?: Trigger | null; mode?: ReviewMode | null },
  ) => request<{ ok: boolean; updated: number }>('/api/repos', 'PATCH', { repos, ...settings }),
  repoSettings: (repo: string, settings: { trigger?: Trigger | null; mode?: ReviewMode | null }) =>
    request<void>(`/api/repos/${repoPath(repo)}`, 'PATCH', settings),
  index: (repo: string) => request<void>(`/api/repos/${repoPath(repo)}/index`, 'POST'),
  pulls: (repo: string) => request<PullRequest[]>(`/api/repos/${repoPath(repo)}/pulls`),
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
};

export function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : 'Something went wrong. Please try again.';
}
