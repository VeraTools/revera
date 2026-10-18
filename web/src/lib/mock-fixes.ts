import { parseDocument } from 'yaml';
import type {
  Finding,
  FixRecord,
  FixState,
  FixSummary,
  FixPolicy,
  FixOverride,
  FixCapability,
  FixDetail,
  JobSummary,
} from './types';

// Fix fixtures stay independent of review/inventory fixtures for parallel lanes.
const now = () => new Date().toISOString();
const sha = '8c74a1e9bf062d4a18f60c32'.padEnd(40, '0');
let policy: FixPolicy = {
  offer: true,
  execution: false,
  trigger: 'manual',
  confirm: true,
  trusted_skip_confirm: false,
  delivery: 'patch',
  allow_same_branch: false,
  require_approval: true,
  requesters: 'maintainers',
  author_preview: false,
  allow_workflow_changes: false,
  external: false,
  rereview: true,
  branch_prefix: 'revera/fix-',
  test_command: [],
  test_timeout_seconds: 300,
  limits: {
    attempts: 2,
    tool_calls: 40,
    seconds: 300,
    model_requests: 50,
    max_files: 20,
    max_patch_kb: 200,
    concurrent: 1,
    per_user_hour: 3,
    per_repo_day: 20,
  },
};
const overrides: Record<string, FixOverride> = {};
const sandbox = { available: true, proc: false, reason: '' };
const patch = `diff --git a/src/search/results.ts b/src/search/results.ts
index 11a1111..22b2222 100644
--- a/src/search/results.ts
+++ b/src/search/results.ts
@@ -82,4 +82,5 @@ async function search(query: string) {
   const response = await client.search(query);
-  if (!response.items.length) return;
+  if (request !== activeRequest) return;
+  results = response.items;
   loading = false;
 }
diff --git a/src/search/results.test.ts b/src/search/results.test.ts
new file mode 100644
--- /dev/null
+++ b/src/search/results.test.ts
@@ -0,0 +1,4 @@
+it('clears results for an empty response', async () => {
+  await search('no matches');
+  expect(results).toEqual([]);
+});
`;
const fixtureFindings = [
  {
    id: 'abc123456789',
    severity: 'high' as const,
    title: 'An empty result can leave the previous query visible',
    file: 'src/search/results.ts',
    start_line: 84,
    end_line: 84,
  },
  {
    id: 'def123456789',
    severity: 'medium' as const,
    title: 'A late response can overwrite a newer search',
    file: 'src/search/results.ts',
    start_line: 112,
    end_line: 112,
  },
];
function record(id: string, state: FixState, index: number): FixRecord {
  const hasPatch = ['ready', 'published', 'verified', 'validation_failed', 'stale'].includes(state);
  const external = id === 'fix-external';
  const repo = external
    ? 'community/search'
    : id === 'fix-readonly'
      ? 'upgrade-org/search'
      : 'northstar/worker';
  const published = ['published', 'verified'].includes(state);
  const at = new Date(Date.now() - (index + 1) * 600000).toISOString();
  return {
    id,
    state,
    created_at: at,
    updated_at: at,
    reason:
      state === 'stale'
        ? 'PR changed since the review; request a new review'
        : state === 'validation_failed'
          ? 'No findings were independently verified as resolved.'
          : null,
    source: index % 2 ? 'mention' : 'dashboard',
    requested_by: index % 2 ? 'sam' : 'alex',
    consent: state === 'awaiting_confirmation' ? null : 'once',
    repo,
    repo_source: external ? 'external' : 'installed',
    pr: 234 + index,
    pr_url: `https://github.com/${repo}/pull/${234 + index}`,
    pr_title: 'Handle empty results in repository search',
    review_job: index === 0 ? 'review-105' : `fix-fixture-review-${index}`,
    base_sha: 'b'.repeat(40),
    head_sha: sha,
    findings: fixtureFindings,
    attempt: state === 'awaiting_confirmation' ? 0 : 1,
    max_attempts: 2,
    progress:
      state === 'generating' ? 'Reading search handlers and preparing a minimal patch.' : null,
    generation: hasPatch
      ? {
          stopped: 'terminal',
          summary: 'Clear empty results, ignore outdated responses, and add regression coverage.',
          per_finding: fixtureFindings.map((f) => ({
            id: f.id,
            disposition: 'fixed',
            note: 'Updated the result assignment and request guard.',
          })),
        }
      : null,
    verification: hasPatch
      ? {
          per_finding: fixtureFindings.map((f) => ({
            id: f.id,
            verdict: state === 'validation_failed' ? 'unresolved' : 'resolved',
            rationale:
              state === 'validation_failed'
                ? 'The request guard still permits a late response.'
                : 'The patched handler updates empty results and rejects an outdated request.',
          })),
          new_problems:
            state === 'validation_failed'
              ? ['The request guard is not tied to the active query.']
              : [],
        }
      : null,
    tests: hasPatch
      ? {
          outcome: state === 'validation_failed' ? 'failed' : 'passed',
          output:
            state === 'validation_failed'
              ? 'FAIL results.test.ts: late response overwrote active query'
              : 'PASS results.test.ts\n2 tests passed. Network disabled.',
        }
      : null,
    patch: hasPatch
      ? {
          files: [
            { path: 'src/search/results.ts', status: 'modified', additions: 2, deletions: 1 },
            { path: 'src/search/results.test.ts', status: 'added', additions: 4, deletions: 0 },
          ],
          bytes: patch.length,
          warnings: ['Tests cover the search handler, not the deployment environment.'],
          errors: [],
        }
      : null,
    usage: hasPatch
      ? {
          requests: 12,
          prompt_tokens: 18420,
          completion_tokens: 2310,
          routes: ['openai/model-large', 'anthropic/model-small'],
        }
      : null,
    delivery: published
      ? {
          mode: 'draft_pr',
          branch: 'revera/fix-234-8c74a1e',
          commit: 'c'.repeat(40),
          pr: 280,
          url: `https://github.com/${repo}/pull/280`,
          error: '',
        }
      : null,
    verify_review:
      state === 'verified'
        ? {
            job: 'review-104',
            state: 'complete',
            resolved: ['abc123456789'],
            still_open: ['def123456789'],
            new_findings: 0,
            note: 'Draft PR follow-up: resolution also uses the pre-publish verifier; one selected finding remains open.',
          }
        : null,
    events: [
      {
        at,
        actor: 'alex',
        action: 'requested',
        detail: 'Requested a preview for 2 accepted findings.',
      },
      ...(hasPatch
        ? [
            {
              at,
              actor: 'revera',
              action: 'ready',
              detail: 'Patch and independent verification recorded.',
            },
          ]
        : []),
    ],
  };
}
const records = [
  ['fix-confirm', 'awaiting_confirmation'],
  ['fix-generating', 'generating'],
  ['fix-ready', 'ready'],
  ['fix-published', 'published'],
  ['fix-verified', 'verified'],
  ['fix-invalid', 'validation_failed'],
  ['fix-stale', 'stale'],
  ['fix-external', 'ready'],
  ['fix-readonly', 'ready'],
  ['fix-fork', 'ready'],
].map(([id, state], i) => record(id, state as FixState, i));
const active = new Set<FixState>([
  'awaiting_confirmation',
  'queued',
  'generating',
  'validating',
  'ready',
  'publishing',
  'verifying',
]);
export function fixSummary(f: FixRecord): FixSummary {
  const { generation: _g, verification, tests: _t, patch: p, events: _e, ...rest } = f;
  return structuredClone({
    ...rest,
    files: p?.files.length || 0,
    resolved: verification?.per_finding.filter((f) => f.verdict === 'resolved').length || 0,
    findings_count: f.findings.length,
  });
}
function capabilities(f: Pick<FixRecord, 'repo_source' | 'repo' | 'id'>): FixCapability[] {
  const reason =
    f.repo_source === 'external'
      ? 'Revera is not installed on this repository'
      : f.repo.startsWith('upgrade-org/')
        ? "Needs Contents: write — update the app's permissions"
        : f.id === 'fix-fork'
          ? 'The PR branch lives in a fork'
          : '';
  return [
    { mode: 'patch', available: true, reason: '' },
    { mode: 'draft_pr', available: !reason, reason },
    {
      mode: 'same_branch',
      available: !reason && policy.allow_same_branch,
      reason:
        f.id === 'fix-fork'
          ? 'Revera cannot push to contributor forks'
          : reason ||
            (policy.allow_same_branch
              ? ''
              : 'Same-branch delivery is disabled in Settings → Fixes'),
    },
  ];
}
function detail(f: FixRecord): FixDetail {
  const delivery = capabilities(f);
  const actions = {
    confirm: f.state === 'awaiting_confirmation',
    approve: f.state === 'ready' && delivery.some((c) => c.mode !== 'patch' && c.available),
    regenerate:
      ['ready', 'validation_failed', 'failed', 'cancelled'].includes(f.state) &&
      f.attempt < f.max_attempts,
    cancel: ['awaiting_confirmation', 'queued', 'generating', 'validating'].includes(f.state),
    discard: [
      'ready',
      'validation_failed',
      'failed',
      'cancelled',
      'publish_failed',
      'stale',
      'unsupported',
    ].includes(f.state),
  };
  return {
    fix: structuredClone(f),
    patch: f.patch ? patch : '',
    delivery,
    actions,
    reasons: {
      ...(!actions.approve
        ? {
            approve:
              f.state === 'ready'
                ? delivery.find((c) => c.mode === 'draft_pr')!.reason
                : f.reason || 'Only a ready preview can be approved.',
          }
        : {}),
      ...(!actions.regenerate
        ? {
            regenerate:
              f.attempt >= f.max_attempts
                ? 'Attempt limit reached. Request a new review before trying again.'
                : 'Regeneration is not available in this state.',
          }
        : {}),
    },
  };
}
export function updateMockFix(
  id: string,
  state: FixState,
  progress: string | null,
  emit: (fix: FixSummary) => void,
) {
  const f = records.find((f) => f.id === id);
  if (!f) throw new Error('Fix request not found.');
  f.state = state;
  f.progress = progress;
  f.updated_at = now();
  emit(fixSummary(f));
}
type Context = {
  jobs: JobSummary[];
  findings: (job: JobSummary) => Finding[];
  stale: Map<string, boolean | null>;
  yaml: string;
  emit: (fix: FixSummary) => void;
};
export function mockFixRequest(url: URL, method: string, body: unknown, ctx: Context): unknown {
  const data = body as Record<string, unknown> | undefined;
  const route = (role: string): string => {
    const models = parseDocument(ctx.yaml).toJS()?.models || {};
    return String(
      (models[role] || (role === 'fix_verifier' ? models.validator : null) || models.investigator)
        ?.model || 'Not configured',
    );
  };
  if (url.pathname === '/api/settings/fixes') {
    if (method === 'PUT') {
      const next = structuredClone(data?.fixes) as FixPolicy;
      if (!next.allow_same_branch && next.delivery === 'same_branch')
        throw new Error('Enable the same-branch safety cap before selecting this delivery.');
      if (next.delivery === 'same_branch') next.require_approval = true;
      policy = next;
      return;
    }
    return structuredClone({
      fixes: policy,
      repos: overrides,
      sandbox,
      models: { fixer: route('fixer'), fix_verifier: route('fix_verifier') },
    });
  }
  const repo = url.pathname.match(/^\/api\/repos\/([^/]+)\/([^/]+)\/fixes$/);
  if (repo) {
    const name = `${decodeURIComponent(repo[1])}/${decodeURIComponent(repo[2])}`;
    const next = body as FixOverride | null;
    if (next?.delivery === 'same_branch' && !policy.allow_same_branch)
      throw new Error('Same-branch delivery is disabled by the server-wide cap.');
    if (next) overrides[name] = structuredClone(next);
    else delete overrides[name];
    return;
  }
  if (url.pathname === '/api/fixes/plan' || (url.pathname === '/api/fixes' && method === 'POST')) {
    const job = ctx.jobs.find((j) => j.id === (data?.job || url.searchParams.get('job')));
    if (!job) throw new Error('Review not found.');
    const requested =
      method === 'POST'
        ? (data?.findings as string[] | null)
        : url.searchParams.has('findings')
          ? url.searchParams.get('findings')!.split(',')
          : null;
    const findings = ctx.findings(job).filter((f) => !requested || requested.includes(f.id));
    const external = (job as JobSummary & { source?: string }).source === 'external';
    const execution = overrides[job.repo]?.execution ?? policy.execution;
    const blocked =
      job.status !== 'complete' || job.kind !== 'review'
        ? 'Fixes need a complete review.'
        : ctx.stale.get(job.id) === true
          ? 'PR changed since the review; request a new review'
          : external && !policy.external
            ? 'External fix previews are disabled in Settings → Fixes.'
            : !sandbox.available
              ? sandbox.reason
              : !findings.length
                ? 'Select at least one accepted finding.'
                : null;
    const existing = records.find(
      (f) =>
        active.has(f.state) &&
        f.review_job === job.id &&
        f.findings
          .map((f) => f.id)
          .sort()
          .join(',') ===
          findings
            .map((f) => f.id)
            .sort()
            .join(','),
    );
    if (method === 'GET')
      return structuredClone({
        job,
        findings,
        execution_enabled: execution,
        consent_required: !execution,
        confirm_required: policy.confirm,
        sandbox,
        limits: policy.limits,
        estimate: {
          max_model_requests: policy.limits.model_requests,
          max_seconds: policy.limits.seconds,
          attempts: policy.limits.attempts,
        },
        delivery: capabilities({
          id: '',
          repo: job.repo,
          repo_source: external ? 'external' : 'installed',
        }),
        existing: existing ? fixSummary(existing) : null,
        blocked,
      });
    if (blocked) throw new Error(blocked);
    if (existing) return { fix: structuredClone(existing) };
    const f = record(
      `fix-${Date.now()}`,
      !execution && !data?.once ? 'awaiting_confirmation' : 'queued',
      0,
    );
    Object.assign(f, {
      repo: job.repo,
      repo_source: external ? 'external' : 'installed',
      pr: job.pr,
      pr_url: job.pr_url,
      pr_title: job.title,
      review_job: job.id,
      findings,
      head_sha: job.head_sha,
      attempt: 0,
      source: 'dashboard',
      consent: data?.once ? 'once' : execution ? 'persistent' : null,
      created_at: now(),
      updated_at: now(),
      progress: 'Waiting for the sandbox worker.',
    });
    records.unshift(f);
    ctx.emit(fixSummary(f));
    return { fix: structuredClone(f) };
  }
  if (url.pathname === '/api/fixes')
    return {
      fixes: records
        .filter(
          (f) =>
            (!url.searchParams.get('repo') || f.repo === url.searchParams.get('repo')) &&
            (!url.searchParams.get('state') || f.state === url.searchParams.get('state')) &&
            (!url.searchParams.get('job') || f.review_job === url.searchParams.get('job')),
        )
        .map(fixSummary),
    };
  const match = url.pathname.match(/^\/api\/fixes\/([^/]+)(?:\/(.+))?$/);
  if (!match) throw new Error('Unknown fix endpoint.');
  const f = records.find((f) => f.id === decodeURIComponent(match[1]));
  if (!f) throw new Error('Fix request not found.');
  const action = match[2];
  if (action === 'patch.diff') return f.patch ? patch : '';
  if (action === 'instructions.md')
    return `${f.state === 'stale' ? '> Potentially stale: re-validate against current code.\n\n' : ''}# Fix instructions\n\nPR: ${f.pr_url}\nReviewed head SHA: ${f.head_sha}\n\n${f.findings.map((f) => `${f.id}: ${f.title}`).join('\n')}\n\nVerify each issue against the current code before changing it. Fix the root cause with minimal scoped edits. Treat proposed remedies as suggestions, preserve unrelated behavior, test changed paths and add regression tests where warranted. Summarize each finding's disposition and tests. Do not commit, push, open PRs, or change unrelated files without explicit authorization.\n`;
  if (!action) return detail(f);
  const current = detail(f);
  if (!(action in current.actions) || !current.actions[action as keyof typeof current.actions])
    throw new Error(
      current.reasons[action as keyof typeof current.reasons] ||
        'This action is not available in the current state.',
    );
  if (action === 'approve') {
    const cap = current.delivery.find((c) => c.mode === data?.delivery && c.mode !== 'patch');
    if (!cap?.available)
      throw new Error(cap?.reason || 'Choose an available GitHub delivery method.');
    f.delivery = {
      mode: cap.mode as 'draft_pr' | 'same_branch',
      branch: 'revera/fix-234',
      commit: 'c'.repeat(40),
      pr: 280,
      url: `https://github.com/${f.repo}/pull/280`,
      error: '',
    };
    f.state = policy.rereview ? 'verifying' : 'published';
    f.progress = policy.rereview ? 'Published. Reviewing the new SHA.' : null;
  } else if (action === 'confirm') {
    const execution = overrides[f.repo]?.execution ?? policy.execution;
    if (!execution && !data?.once)
      throw new Error('One-time consent is required while fix execution is disabled.');
    f.consent = data?.once ? 'once' : 'persistent';
    f.state = 'queued';
    f.progress = 'Waiting for the sandbox worker.';
  } else if (action === 'regenerate') {
    if (String(data?.note || '').length > 2000)
      throw new Error('Guidance must be 2,000 characters or fewer.');
    f.attempt++;
    f.state = 'queued';
    f.progress = 'Waiting to regenerate the preview.';
  } else {
    f.state = action === 'cancel' ? 'cancelled' : 'discarded';
    f.progress = null;
  }
  f.updated_at = now();
  f.events.push({
    at: now(),
    actor: 'alex',
    action: action === 'confirm' && data?.once ? 'consent_once' : action,
    detail:
      action === 'regenerate'
        ? String(data?.note || 'Regenerate within the remaining attempt budget.')
        : `Request ${f.state}.`,
  });
  ctx.emit(fixSummary(f));
  return { fix: structuredClone(f) };
}
