import type { PullDetail, PullReview, PullSummary } from './types';

export type PullTarget = Pick<
  PullSummary,
  'repo' | 'number' | 'url' | 'head_sha' | 'trigger' | 'mode' | 'review'
> &
  Partial<Pick<PullDetail, 'state' | 'merged'>>;
export const isOpen = (pull: PullTarget) => (!pull.state || pull.state === 'open') && !pull.merged;
export const pullPath = (pull: { repo: string; number: number }) =>
  `/pulls/${pull.repo.split('/').map(encodeURIComponent).join('/')}/${pull.number}`;
export const canFix = (pull: PullTarget) =>
  isOpen(pull) &&
  pull.review.state === 'findings' &&
  pull.review.latest?.head_sha === pull.head_sha;
export function reviewing(review: PullReview, id: string): PullReview {
  return { ...review, state: 'reviewing', active_job: id };
}
export const needsReview = new Set(['not_reviewed', 'outdated', 'failed', 'partial', 'skipped']);
export const reviewDecision = (state: string | null) =>
  ({
    APPROVED: 'Approved',
    CHANGES_REQUESTED: 'Changes requested',
    REVIEW_REQUIRED: 'Review required',
    COMMENTED: 'Commented',
    DISMISSED: 'Dismissed',
    PENDING: 'Pending',
  })[state || ''] ||
  state ||
  '';

// GitHub label colors are untrusted strings. Only six-digit RGB reaches CSS;
// black/white is selected using WCAG luminance, not the label's apparent hue.
export function labelStyle(color: string) {
  if (!/^[0-9a-f]{6}$/i.test(color)) return '';
  const rgb = [0, 2, 4]
    .map((i) => parseInt(color.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  const luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
  return `background:#${color};color:${luminance > 0.179 ? '#000' : '#fff'};border-color:transparent`;
}

export type TimelineItem =
  | { kind: 'comment'; at: string; comment: PullDetail['comments'][number] }
  | {
      kind: 'review';
      at: string;
      review: PullDetail['reviews'][number];
      threads: PullDetail['threads'];
    }
  | { kind: 'thread'; at: string; thread: PullDetail['threads'][number] };

export function conversation(
  detail: PullDetail,
  unresolved: boolean,
  hideBots: boolean,
): TimelineItem[] {
  const threads = detail.threads.filter(
    (thread) =>
      (!unresolved || !thread.is_resolved) &&
      (!hideBots || thread.comments.some((comment) => !comment.author.bot)),
  );
  const items: TimelineItem[] = [];
  if (!unresolved)
    for (const comment of detail.comments) {
      if (!hideBots || !comment.author.bot)
        items.push({ kind: 'comment', at: comment.created_at, comment });
    }
  for (const review of detail.reviews) {
    const nested = threads.filter((thread) => thread.review_id === review.id);
    if ((unresolved || (hideBots && review.author.bot)) && !nested.length) continue;
    items.push({
      kind: 'review',
      at: review.submitted_at || nested[0]?.comments[0]?.created_at || detail.created_at,
      review,
      threads: nested,
    });
  }
  const reviewIds = new Set(detail.reviews.map((review) => review.id));
  for (const thread of threads)
    if (!thread.review_id || !reviewIds.has(thread.review_id)) {
      items.push({
        kind: 'thread',
        at: thread.comments[0]?.created_at || detail.created_at,
        thread,
      });
    }
  return items.sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
}
