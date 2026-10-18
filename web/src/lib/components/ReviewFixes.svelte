<script lang="ts">
  import { untrack } from 'svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import type { JobSummary, FixSummary } from '#lib/types';
  import FixStatus from './FixStatus.svelte';
  import Notice from './Notice.svelte';
  let { job, accepted }: { job: JobSummary; accepted: number } = $props();
  let fixes = $state<FixSummary[]>([]);
  let error = $state('');
  let generation = 0;
  async function load(id: string) {
    const current = ++generation;
    try {
      const result = await api.fixes({ job: id });
      if (current === generation) {
        fixes = result.fixes.filter((f) => f.review_job === id);
        error = '';
      }
    } catch (cause) {
      if (current === generation) error = errorMessage(cause);
    }
  }
  $effect(() => {
    const id = job.id;
    $live.fix;
    untrack(() => void load(id));
  });
</script>

{#if (job.kind === 'review' && job.status === 'complete' && accepted > 0) || fixes.length || error}
  <section class="section" aria-label="Review fix requests">
    <h3>Fix requests</h3>
    {#if job.kind === 'review' && job.status === 'complete' && accepted > 0}<a
        class="button"
        href={`/fixes/new?job=${encodeURIComponent(job.id)}`}>Fix with Revera</a
      >
      <p class="helper">
        Preview first. Generating a fix never grants permission to publish it.
      </p>{/if}
    {#if error}<Notice message={error} retry={() => load(job.id)} />{/if}
    {#if fixes.length}<ul>
        {#each fixes as fix}<li>
            <a href={`/fixes/${fix.id}`} class="text-link">{fix.id}</a><FixStatus
              state={fix.state}
            />
          </li>{/each}
      </ul>{/if}
  </section>
{/if}

<style>
  ul {
    list-style: none;
    padding: 0;
    margin: 16px 0;
  }
  li {
    display: grid;
    gap: 6px;
    padding: 10px 0;
    font-size: 12px;
    overflow-wrap: anywhere;
    border-bottom: 1px solid var(--border);
  }
  .helper {
    margin-top: 8px;
  }
</style>
