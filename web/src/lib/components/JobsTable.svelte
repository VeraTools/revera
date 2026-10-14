<script lang="ts">
  import { relative, duration } from '#lib/time';
  import type { JobSummary } from '#lib/types';
  import Status from './Status.svelte';
  let { jobs }: { jobs: JobSummary[] } = $props();
</script>

<div class="table-scroll">
  <table class="jobs-table">
    <thead><tr><th>Review</th><th>Status</th><th>Findings</th><th>Duration</th><th class="text-right">Updated</th></tr></thead>
    <tbody>
      {#each jobs as job (job.id)}
        <tr>
          <td><a class="row-title" href={`/reviews/${job.id}`}>{job.title || 'Repository index'}</a><div class="row-meta"><span>{job.repo}</span><span class="meta-separator">·</span><span>{job.kind === 'index' ? 'Index' : `#${job.pr}`}</span>{#if job.author}<span class="meta-separator">·</span><span>{job.author}</span>{/if}</div></td>
          <td><Status status={job.status} /></td>
          <td>{#if job.findings.total}<span class="finding-count">{job.findings.total}</span>{:else}<span class="muted">{['queued', 'running'].includes(job.status) ? '—' : '0'}</span>{/if}</td>
          <td class="muted tabular">{duration(job.duration_ms)}</td>
          <td class="text-right muted nowrap"><time datetime={job.finished_at || job.started_at || job.queued_at} title={new Date(job.finished_at || job.started_at || job.queued_at).toLocaleString()}>{relative(job.finished_at || job.started_at || job.queued_at)}</time></td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>
