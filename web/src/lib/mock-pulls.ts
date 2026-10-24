import { ApiError } from './api';
import type {
  JobSummary,
  PullSummary,
  PullReview,
  PullDetail,
  PullComment,
  PullAuthor,
  PullThread,
  Repository,
  ReviewMode,
  FixSummary,
} from './types';

const ago = (minutes: number) => new Date(Date.now() - minutes * 60000).toISOString();
const head = '8c74a1e9bf062d4a18f60c32';
const bot: PullAuthor = { login: 'revera-northstar', bot: true, revera: true };
const human = (login: string): PullAuthor => ({ login, bot: false, revera: false });
const zero = () => ({ total: 0, high: 0, medium: 0, low: 0 });
const definitions = [
  ['northstar/api', 251, 'Handle connection timeouts', 'sam', 'not_reviewed'],
  ['northstar/api', 227, 'Reduce duplicate database queries', 'alex', 'outdated'],
  ['northstar/worker', 234, 'Handle empty results in repository search', 'morgan', 'findings'],
  ['northstar/api', 248, 'Preserve session expiry when refreshing tokens', 'alex', 'reviewing'],
  ['northstar/web', 241, 'Add cursor pagination to the activity feed', 'sam', 'reviewing'],
  ['northstar/api', 206, 'Add request tracing to the API', 'morgan', 'clean'],
  ['northstar/web', 220, 'Update retry policy for background tasks', 'sam', 'partial'],
  ['northstar/worker', 213, 'Keep user preferences during migration', 'morgan', 'failed'],
  ['northstar/docs', 12, 'Document the new authentication flow', 'alex', 'skipped'],
  ['northstar/web', 252, 'Update testing dependencies', 'dependabot[bot]', 'not_reviewed'],
  ['northstar/worker', 260, 'Explore bounded background queues', 'sam', 'not_reviewed'],
  ['northstar/web', 253, 'Keep the keyboard focus after saving', 'alex', 'findings'],
  ['northstar/api', 254, 'Clarify retry diagnostics', 'sam', 'findings'],
] as const;
const ids: Record<string, number> = {
  'northstar/api': 1,
  'northstar/web': 2,
  'northstar/worker': 3,
  'northstar/docs': 4,
};
const fixtures: PullSummary[] = definitions.map(([repo, number, title, author, state], i) => ({
  repo,
  repo_id: ids[repo],
  number,
  title,
  author,
  author_is_bot: author.endsWith('[bot]'),
  draft: number === 260,
  fork: number === 234 || number === 260,
  head_sha: number === 227 ? 'f'.repeat(40) : head,
  head_ref: number === 234 ? 'fix/search-state' : `topic/${number}`,
  base_ref: 'main',
  created_at: ago(3 * 1440 + i * 60),
  updated_at: ago(i * 14 + 3),
  url: `https://github.com/${repo}/pull/${number}`,
  labels:
    number === 234
      ? ['bug', 'search', 'needs discussion', 'frontend']
      : number === 252
        ? ['dependencies']
        : i % 3 === 0
          ? ['enhancement']
          : [],
  trigger: repo === 'northstar/docs' ? 'off' : 'mention',
  mode: repo === 'northstar/worker' ? 'deep' : 'standard',
  review: { latest: null, state, active_job: null },
}));

export function annotatePullJob(job: JobSummary) {
  const pull = fixtures.find(
    (pull) => job.source === 'installed' && pull.repo === job.repo && pull.number === job.pr,
  );
  if (!pull) return;
  Object.assign(job, {
    repo_id: pull.repo_id,
    title: pull.title,
    author: pull.author,
    head_sha: pull.head_sha,
    pr_url: pull.url,
  });
}
function reviewFor(pull: PullSummary, jobs: JobSummary[]): PullReview {
  const reviews = jobs
    .filter(
      (job) =>
        job.source === 'installed' &&
        job.repo === pull.repo &&
        job.pr === pull.number &&
        job.kind === 'review',
    )
    .sort((a, b) => b.queued_at.localeCompare(a.queued_at));
  const job = reviews[0],
    active = reviews.find((job) => ['queued', 'running'].includes(job.status));
  return {
    latest: job
      ? {
          id: job.id,
          status: job.status,
          mode: job.mode,
          head_sha: job.head_sha || null,
          findings: { ...job.findings },
          finished_at: job.finished_at || null,
          queued_at: job.queued_at,
        }
      : null,
    active_job: active?.id || null,
    state: active
      ? 'reviewing'
      : !job
        ? 'not_reviewed'
        : job.head_sha !== pull.head_sha
          ? 'outdated'
          : ['failed', 'cancelled'].includes(job.status)
            ? 'failed'
            : job.status === 'complete'
              ? job.findings.total
                ? 'findings'
                : 'clean'
              : job.status === 'partial'
                ? 'partial'
                : 'skipped',
  };
}
function seed(jobs: JobSummary[]) {
  for (const job of jobs) {
    if (!job.repo_id) job.repo_id = ids[job.repo];
  }
  for (const pull of fixtures) {
    if (
      pull.review.state === 'not_reviewed' ||
      jobs.some(
        (job) => job.source === 'installed' && job.repo === pull.repo && job.pr === pull.number,
      )
    )
      continue;
    const status = pull.review.state === 'skipped' ? 'skipped' : 'complete';
    const findings =
      pull.number === 253
        ? { total: 1, high: 0, medium: 1, low: 0 }
        : pull.number === 254
          ? { total: 1, high: 0, medium: 0, low: 1 }
          : zero();
    jobs.push({
      source: 'installed',
      publication: 'github',
      id: `pull-review-${pull.number}`,
      kind: 'review',
      repo_id: pull.repo_id,
      repo: pull.repo,
      pr: pull.number,
      title: pull.title,
      author: pull.author,
      head_sha: pull.head_sha,
      trigger: 'manual',
      mode: pull.mode,
      status,
      findings,
      queued_at: ago(90),
      finished_at: ago(88),
      pr_url: pull.url,
    });
  }
  const searchReview = jobs.find((job) => job.id === 'review-105');
  if (searchReview && !jobs.some((job) => job.id === 'search-history-0')) {
    for (let i = 0; i < 6; i++)
      jobs.push({
        ...searchReview,
        id: `search-history-${i}`,
        status: i === 0 ? 'failed' : 'complete',
        findings: zero(),
        queued_at: ago(400 + i * 60),
        finished_at: ago(397 + i * 60),
      });
  }
}
const comment = (id: string, author: PullAuthor, body: string, minutes: number): PullComment => ({
  id,
  author,
  body,
  created_at: ago(minutes),
  url: `https://github.com/northstar/worker/pull/234#${id}`,
});
const description = `## Keep search results consistent

An empty response currently leaves the previous query visible. This change clears the result set and ignores late responses.

### Implementation

- Track the active request before updating state.
- Preserve the loading indicator until the current request finishes.
- Keep the existing cursor pagination contract.


\`\`\`typescript
const request = ++activeRequest;
const response = await client.search(query);
if (request !== activeRequest) return;
results = response.items;
loading = false;
\`\`\`

### Checklist

- [x] Regression test for empty results
- [x] Regression test for out-of-order responses
- [ ] Check keyboard navigation with the new state

> This only changes the client state. The API response stays the same.

### Test plan

1. Search for a query with matches.
2. Search again for a query without matches.
3. Confirm the old results disappear.
4. Throttle the network and submit two queries.
5. Confirm that the newest query wins.

<details><summary>Compatibility notes</summary>

Existing callers still receive the same result shape. See https://github.com/northstar/web for the follow-up.

</details>

---

No migration is required. **Review the request guard** before merging.
`;
function discussion(pull: PullSummary): Pick<PullDetail, 'comments' | 'reviews' | 'threads'> {
  const threads: PullThread[] = [
    {
      id: 'thread-resolved',
      review_id: 'review-human',
      is_resolved: true,
      is_outdated: false,
      path: 'src/search/results.ts',
      line: 84,
      original_line: 84,
      diff_hunk:
        '@@ -80,7 +80,8 @@ async function search(query) {\n   const request = ++activeRequest;\n   loading = true;\n   const response = await client.search(query);\n-  if (!response.items.length) return;\n+  if (request !== activeRequest) return;\n+  results = response.items;\n   loading = false;\n }',
      comments: [
        comment(
          'resolved-1',
          human('alex'),
          'Could we compare the request ID **before** assigning results?',
          130,
        ),
        comment(
          'resolved-2',
          human('morgan'),
          'Done in the latest commit. The regression test covers this now.',
          120,
        ),
      ],
      truncated: false,
    },
    {
      id: 'thread-open',
      review_id: 'review-human',
      is_resolved: false,
      is_outdated: false,
      path: 'src/search/results.test.ts',
      line: 112,
      original_line: 112,
      diff_hunk:
        '@@ -110,3 +110,5 @@\n+it("ignores late responses", async () => {\n+  await search("latest");\n+  expect(results).toEqual(latest);\n+});',
      comments: [
        comment(
          'open-1',
          human('alex'),
          'Please also assert that the loading state is cleared for the newest request.',
          125,
        ),
      ],
      truncated: false,
    },
    {
      id: 'thread-outdated',
      review_id: 'missing-review',
      is_resolved: false,
      is_outdated: true,
      path: 'src/search/results.ts',
      line: null,
      original_line: 42,
      diff_hunk: '-  results = previousResults;\n+  results = response.items;',
      comments: [
        comment(
          'outdated-1',
          human('sam'),
          'This location moved, but is the empty response case covered?',
          100,
        ),
        comment('outdated-2', human('morgan'), 'Yes — see the new regression test above.', 95),
      ],
      truncated: false,
    },
    {
      id: 'thread-revera',
      review_id: 'review-revera',
      is_resolved: false,
      is_outdated: false,
      path: 'src/search/results.ts',
      line: 112,
      original_line: 112,
      diff_hunk:
        '-  results = response.items;\n+  if (request === activeRequest) results = response.items;',
      comments: [
        comment(
          'revera-inline',
          bot,
          'A late response can overwrite a newer search. Compare the active request identifier before updating state.',
          80,
        ),
      ],
      truncated: false,
    },
  ];
  return {
    comments: [
      comment(
        'comment-1',
        human('morgan'),
        'Ready for another look. The [regression tests](https://github.com/northstar/worker) cover both cases.',
        180,
      ),
      comment(
        'comment-revera',
        bot,
        '## Revera review\n\n**2 validated findings**. See the review sidebar for details.',
        78,
      ),
      comment(
        'comment-xss',
        human('sam'),
        'Security regression sample (render as text):\n\n<img id="injected-image" src=x onerror="window.__pullXss=true">\n<script>window.__pullXss=true</script>\n[unsafe](javascript:window.__pullXss=true)\n\n![screenshot](https://example.com/no-load.png)',
        65,
      ),
      comment(
        'comment-bot',
        { login: 'ci-helper[bot]', bot: true, revera: false },
        'All 128 checks passed.',
        60,
      ),
    ],
    reviews: [
      {
        id: 'review-human',
        author: human('alex'),
        state: 'CHANGES_REQUESTED',
        body: 'The request guard looks good. Two small testing questions below.',
        submitted_at: ago(135),
        url: pull.url,
      },
      {
        id: 'review-approved',
        author: human('sam'),
        state: 'APPROVED',
        body: '',
        submitted_at: ago(90),
        url: pull.url,
      },
      {
        id: 'review-revera',
        author: bot,
        state: 'COMMENTED',
        body: '## Revera review\n\nTwo findings were independently validated at this head.',
        submitted_at: ago(82),
        url: pull.url,
      },
    ],
    threads,
  };
}
interface Context {
  jobs: JobSummary[];
  repos: Repository[];
  fixes: (repo: string, number: number) => Promise<FixSummary[]>;
  enqueue: (repo: string, pr?: number, mode?: ReviewMode | null) => JobSummary;
}
let refreshedAt = ago(4);
export async function mockPullRequest(
  url: URL,
  method: string,
  body: Record<string, unknown> | undefined,
  context: Context,
): Promise<{ handled: boolean; value?: unknown }> {
  const match = url.pathname.match(/^\/api\/repos\/([^/]+)\/([^/]+)\/pulls\/(\d+)(\/review)?$/);
  if (url.pathname === '/api/installations/reconcile') {
    refreshedAt = ago(0);
    return { handled: false };
  }
  if (url.pathname !== '/api/pulls' && !match) return { handled: false };
  seed(context.jobs);
  const inventory = fixtures
    .filter((pull) => context.repos.some((repo) => repo.full_name === pull.repo))
    .map((pull) => {
      const repo = context.repos.find((repo) => repo.full_name === pull.repo)!;
      return {
        ...pull,
        trigger: repo.trigger,
        mode: repo.mode,
        review: reviewFor(pull, context.jobs),
      };
    });
  if (url.pathname === '/api/pulls')
    return {
      handled: true,
      value: structuredClone({ refreshed_at: refreshedAt, pulls: inventory }),
    };
  const pull = inventory.find(
    (pull) =>
      pull.repo === `${decodeURIComponent(match![1])}/${decodeURIComponent(match![2])}` &&
      pull.number === Number(match![3]),
  );
  if (!pull) {
    // Leave the existing repository chooser's additional PR fixtures working.
    if (match![4]) return { handled: false };
    throw new ApiError('not found', 404);
  }
  if (match![4] && method === 'POST')
    return {
      handled: true,
      value: { id: context.enqueue(pull.repo, pull.number, body?.mode as ReviewMode | null).id },
    };
  const history = context.jobs.filter(
    (job) => job.source === 'installed' && job.repo === pull.repo && job.pr === pull.number,
  );
  const value: PullDetail = {
    ...pull,
    author: pull.author_is_bot
      ? { login: pull.author, bot: true, revera: false }
      : human(pull.author),
    body: description,
    state: 'open',
    merged: false,
    additions: 42,
    deletions: 18,
    changed_files: 3,
    review_decision: 'CHANGES_REQUESTED',
    labels: pull.labels.map((name, i) => ({
      name,
      color: ['d73a4a', 'd4c5f9', 'fbca04', '0366d6'][i % 4],
    })),
    ...discussion(pull),
    truncated: { labels: false, comments: pull.number === 234, reviews: false, threads: false },
    jobs: [...history].sort((a, b) => b.queued_at.localeCompare(a.queued_at)),
    fixes: await context.fixes(pull.repo, pull.number),
  };
  return { handled: true, value: structuredClone(value) };
}
