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
    request<JobsPage>(`/api/jobs?${new URLSearchParams({ limit: '25', ...filters })}`),
  job: (id: string) => request<JobDetail>(`/api/jobs/${encodeURIComponent(id)}`),
  log: (id: string) => request<string>(`/api/jobs/${encodeURIComponent(id)}/log`),
  rerun: (id: string) => request<void>(`/api/jobs/${encodeURIComponent(id)}/rerun`, 'POST'),
  cancel: (id: string) => request<void>(`/api/jobs/${encodeURIComponent(id)}/cancel`, 'POST'),
  repos: () => request<Repository[]>('/api/repos'),
  enable: (repo: string, enabled: boolean) =>
    request<void>(`/api/repos/${repoPath(repo)}`, 'PATCH', { enabled }),
  index: (repo: string) => request<void>(`/api/repos/${repoPath(repo)}/index`, 'POST'),
  pulls: (repo: string) => request<PullRequest[]>(`/api/repos/${repoPath(repo)}/pulls`),
  review: (repo: string, number: number) =>
    request<void>(`/api/repos/${repoPath(repo)}/pulls/${number}/review`, 'POST'),
  settings: () => request<Settings>('/api/settings'),
  saveConfig: (yaml: string) => request<void>('/api/settings/config', 'PUT', { yaml }),
  saveServer: (server: ServerSettings) => request<void>('/api/settings/server', 'PUT', server),
  setSecret: (name: string, value: string) =>
    request<void>(`/api/secrets/${encodeURIComponent(name)}`, 'PUT', { value }),
  deleteSecret: (name: string) =>
    request<void>(`/api/secrets/${encodeURIComponent(name)}`, 'DELETE'),
  test: (role: Role) => request<TestResult>('/api/settings/test', 'POST', { role }),
  models: (role: Role) => request<{ models: string[] }>('/api/settings/models', 'POST', { role }),
};

export function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : 'Something went wrong. Please try again.';
}
