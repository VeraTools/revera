<script lang="ts">
  import { relative, duration, severitySplit } from '#lib/time';
  import type { JobSummary } from '#lib/types';
  import { triggerLabel } from '#lib/job-labels';
  import Status from './Status.svelte';
  import ExternalBadge from './ExternalBadge.svelte';
  let { jobs }: { jobs: JobSummary[] } = $props();
</script>

<div class="table-scroll">
  <table class="jobs-table">
    <thead
      ><tr
        ><th>Review</th><th>Status</th><th>Findings</th><th>Duration</th><th class="text-right"
          >Updated</th
        ></tr
      ></thead
    >
    <tbody>
      {#each jobs as job (job.id)}
        <tr>
          <td
            ><a class="row-title" href={`/reviews/${job.id}`}>{job.title || 'Repository index'}</a>
            <div class="row-meta">
              <ExternalBadge source={job.source} />
              <span>{job.repo}</span><span class="meta-separator">·</span><span
                >{job.kind === 'index' ? 'Index' : `#${job.pr}`}</span
              >{#if job.author}<span class="meta-separator">·</span><span>{job.author}</span>{/if}
              {#if job.kind === 'review'}<span>·</span><span class="review-kind"
                  >{job.mode} · {triggerLabel(job.trigger)}</span
                >{:else}<span>·</span><span>{triggerLabel(job.trigger)}</span>{/if}
              {#if job.source === 'external' && job.head_sha}<code title="Reviewed head"
                  >{job.head_sha.slice(0, 7)}</code
                >{/if}
            </div></td
          >
          <td><Status status={job.status} /></td>
          <td
            >{#if job.findings.total}<span class="finding-count">{job.findings.total}</span>
              <div class="row-meta nowrap">{severitySplit(job.findings)}</div>{:else}<span
                class="muted">{['queued', 'running'].includes(job.status) ? '—' : '0'}</span
              >{/if}</td
          >
          <td class="muted tabular">{duration(job.duration_ms)}</td>
          <td class="text-right muted nowrap"
            ><time
              datetime={job.finished_at || job.started_at || job.queued_at}
              title={new Date(job.finished_at || job.started_at || job.queued_at).toLocaleString()}
              >{relative(job.finished_at || job.started_at || job.queued_at)}</time
            ></td
          >
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .review-kind {
    text-transform: capitalize;
  }
</style>
