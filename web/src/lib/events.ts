import { writable } from 'svelte/store';
import { mockMode } from './api';
import type { JobSummary, FixSummary } from './types';

export const live = writable<{
  job?: JobSummary;
  fix?: FixSummary;
  repos: number;
  connected: boolean;
}>({
  repos: 0,
  connected: false,
});

export function connectEvents() {
  if (mockMode) {
    let clean: (() => void) | undefined;
    let closed = false;
    void import('./mock').then(({ subscribeMock }) => {
      if (closed) return;
      live.update((state) => ({ ...state, connected: true }));
      clean = subscribeMock((event, payload) =>
        live.update((state) => ({
          ...state,
          job: event === 'job' ? (payload as JobSummary) : state.job,
          fix: event === 'fix' ? (payload as FixSummary) : state.fix,
          repos: state.repos + (event === 'repos' ? 1 : 0),
        })),
      );
    });
    return () => {
      closed = true;
      clean?.();
      live.update((state) => ({ ...state, connected: false }));
    };
  }
  const source = new EventSource('/api/events');
  source.onopen = () => live.update((state) => ({ ...state, connected: true }));
  source.onerror = () => live.update((state) => ({ ...state, connected: false }));
  source.addEventListener('job', (event) => {
    try {
      const job = JSON.parse((event as MessageEvent).data) as JobSummary;
      live.update((state) => ({ ...state, job }));
    } catch {
      /* Ignore malformed events; the next fetch remains authoritative. */
    }
  });
  source.addEventListener('repos', () =>
    live.update((state) => ({ ...state, repos: state.repos + 1 })),
  );
  source.addEventListener('fix', (event) => {
    try {
      const fix = JSON.parse((event as MessageEvent).data) as FixSummary;
      live.update((state) => ({ ...state, fix }));
    } catch {
      /* The next fetch remains authoritative. */
    }
  });
  return () => source.close();
}
