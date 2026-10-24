<script lang="ts">
  import type { PullDetail, JobSummary } from '../types';
  import { canFix } from '../pulls';
  import { relative } from '../time';
  import { triggerLabel } from '../job-labels';
  import PullStatus from './PullStatus.svelte';
  import Status from './Status.svelte';
  import FixStatus from './FixStatus.svelte';
  let { detail }: { detail: PullDetail } = $props();
  const latest = $derived(detail.review.latest);
</script>

{#snippet jobRow(job: JobSummary)}
  <li>
    <a href={`/reviews/${encodeURIComponent(job.id)}`} class="history-link"
      ><Status status={job.status} /><span class="history-meta"
        >{job.kind === 'review' ? `${job.mode} review` : 'Index'} · {triggerLabel(job.trigger)} · {relative(
          job.finished_at || job.queued_at,
        )}</span
      ></a
    >
  </li>
{/snippet}
<aside class="revera-card" aria-labelledby="revera-heading">
  <h2 id="revera-heading">Revera</h2>
  <div class="state-card">
    <PullStatus review={detail.review} />
    {#if latest}
      <p class="helper reviewed">
        {latest.finished_at ? 'Reviewed' : 'Queued'}
        <code title={latest.head_sha || ''}>{latest.head_sha?.slice(0, 7) || '—'}</code>
        · {relative(latest.finished_at || latest.queued_at)}
      </p>
      {#if latest.findings.total}<div class="severity-counts">
          {#each ['high', 'medium', 'low'] as severity}{@const count =
              latest.findings[severity as 'high' | 'medium' | 'low']}{#if count}<span
                class={severity}>{count} {severity}</span
              >{/if}{/each}
        </div>{/if}
      {#if latest.findings.carried}<p class="helper reviewed">
          {latest.findings.carried} still open from earlier reviews
        </p>{/if}
      {#if detail.review.state === 'outdated'}<p class="helper stale">
          New commits since this review
        </p>{/if}
      <a class="text-link open-review" href={`/reviews/${encodeURIComponent(latest.id)}`}
        >Open review</a
      >
    {:else}<p class="helper reviewed">No Revera review yet.</p>{/if}
    {#if canFix(detail)}<a
        class="button small fix-action"
        href={`/fixes/new?job=${encodeURIComponent(latest!.id)}`}>Fix findings</a
      >{/if}
  </div>
  <section class="history">
    <h3>Job history</h3>
    <ul>
      {#each detail.jobs.slice(0, 5) as job (job.id)}{@render jobRow(job)}{:else}<li class="helper">
          No jobs for this pull request.
        </li>{/each}
    </ul>
    {#if detail.jobs.length > 5}<details>
        <summary>Show {detail.jobs.length - 5} older jobs</summary>
        <ul>
          {#each detail.jobs.slice(5) as job (job.id)}{@render jobRow(job)}{/each}
        </ul>
      </details>{/if}
  </section>
  {#if detail.fixes.length}<section class="history">
      <h3>Fix requests</h3>
      <ul>
        {#each detail.fixes as fix (fix.id)}<li>
            <a class="history-link" href={`/fixes/${encodeURIComponent(fix.id)}`}
              ><FixStatus state={fix.state} /><span class="history-meta"
                >{fix.findings_count}
                {fix.findings_count === 1 ? 'finding' : 'findings'} · {relative(
                  fix.updated_at,
                )}</span
              ></a
            >
          </li>{/each}
      </ul>
    </section>{/if}
  <p class="helper policy">
    Trigger: <span>{detail.trigger}</span> · Mode: <span>{detail.mode}</span><br /><a
      class="text-link"
      href={`/repositories?q=${encodeURIComponent(detail.repo)}`}>Repository settings</a
    >
  </p>
</aside>

<style>
  .revera-card {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 20px;
    align-self: start;
    min-width: 0;
  }
  h2 {
    font-size: 14px;
    margin-bottom: 16px;
  }
  h3 {
    font-size: 12px;
    margin-bottom: 4px;
  }
  .reviewed {
    margin-top: 10px;
  }
  .severity-counts {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 11px;
    margin-top: 10px;
  }
  .high {
    color: var(--red);
  }
  .medium {
    color: var(--amber);
  }
  .low {
    color: var(--muted);
  }
  .stale {
    color: var(--amber);
    margin-top: 10px;
  }
  .open-review {
    display: inline-block;
    font-size: 12px;
    margin-top: 12px;
  }
  .fix-action {
    display: flex;
    margin-top: 16px;
  }
  .history {
    border-top: 1px solid var(--border);
    margin-top: 20px;
    padding-top: 16px;
  }
  ul {
    padding: 0;
    margin: 0;
    list-style: none;
  }
  li {
    padding: 10px 0;
  }
  .history-link {
    display: grid;
    gap: 4px;
  }
  .history-link:hover {
    color: var(--accent);
  }
  .history-meta {
    font-size: 11px;
    color: var(--muted);
    line-height: 1.6;
  }
  .history-meta::first-letter {
    text-transform: uppercase;
  }
  summary {
    color: var(--muted);
    font-size: 11px;
    cursor: pointer;
    padding: 8px 0;
  }
  .policy {
    border-top: 1px solid var(--border);
    padding-top: 16px;
    margin-top: 16px;
    line-height: 2;
  }
  .policy span {
    text-transform: capitalize;
  }
</style>
