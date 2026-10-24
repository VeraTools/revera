<script lang="ts">
  import { LoaderCircle } from '@lucide/svelte';
  import type { PullReview } from '../types';
  import Status from './Status.svelte';
  let { review }: { review: PullReview } = $props();
  const labels = {
    not_reviewed: 'Not reviewed',
    outdated: 'New commits',
    reviewing: 'Reviewing',
    failed: 'Failed',
    partial: 'Partial',
    findings: 'Findings',
    clean: 'Clean',
    skipped: 'Skipped',
  };
  const counts = $derived(review.latest?.findings);
  const status = $derived(
    review.state === 'findings'
      ? counts?.high
        ? 'high'
        : counts?.medium
          ? 'medium'
          : 'low'
      : review.state === 'clean'
        ? 'complete'
        : review.state === 'outdated'
          ? 'partial'
          : review.state,
  );
</script>

{#if review.state === 'reviewing'}
  <span class="reviewing"><LoaderCircle size={13} />Reviewing</span>
{:else}
  <Status
    {status}
    label={review.state === 'findings'
      ? `${counts?.total || 0} ${counts?.total === 1 ? 'finding' : 'findings'}`
      : labels[review.state]}
  />
{/if}

<style>
  .reviewing {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
    white-space: nowrap;
  }
  .reviewing :global(svg) {
    color: var(--blue);
    animation: spin 1.4s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .reviewing :global(svg) {
      animation: none;
    }
  }
</style>
