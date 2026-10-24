<script lang="ts">
  import { DropdownMenu } from 'bits-ui';
  import { goto } from '$app/navigation';
  import { MoreHorizontal, ArrowUpRight, GitPullRequest, Square } from '@lucide/svelte';
  import { api, errorMessage } from '../api';
  import { canFix, isOpen, needsReview, type PullTarget } from '../pulls';
  import type { ReviewMode } from '../types';
  let {
    pull,
    closed = false,
    onqueued,
    onrefresh,
    onsuccess,
    onerror,
  }: {
    pull: PullTarget;
    closed?: boolean;
    onqueued: (id: string) => void;
    onrefresh: () => Promise<void>;
    onsuccess: (message: string) => void;
    onerror: (message: string) => void;
  } = $props();
  let busy = $state(false);
  const off = $derived(pull.trigger === 'off');
  const inactive = $derived(closed || !isOpen(pull));
  const hint = $derived(
    inactive
      ? `This pull request is ${pull.merged ? 'merged' : 'closed'}`
      : off
        ? 'Reviews are off for this repository'
        : '',
  );
  async function queue(mode: ReviewMode | null = null) {
    if (off || inactive) return;
    busy = true;
    try {
      const { id } = await api.review(pull.repo, pull.number, mode);
      onqueued(id);
      onsuccess(`Review queued for ${pull.repo} #${pull.number}.`);
      await onrefresh();
    } catch (cause) {
      onerror(errorMessage(cause));
    } finally {
      busy = false;
    }
  }
  async function cancel() {
    if (!pull.review.active_job) return;
    busy = true;
    try {
      await api.cancel(pull.review.active_job);
      onsuccess('The review has been cancelled.');
      await onrefresh();
    } catch (cause) {
      onerror(errorMessage(cause));
    } finally {
      busy = false;
    }
  }
</script>

<div class="pull-actions">
  {#if pull.review.state === 'reviewing'}
    <button class="button small danger" disabled={busy} onclick={() => void cancel()}
      ><Square size={12} />Cancel</button
    >
  {:else if inactive}
    <span class="helper">{pull.merged ? 'Merged' : 'Closed'}</span>
  {:else if canFix(pull)}
    <a class="button small" href={`/fixes/new?job=${encodeURIComponent(pull.review.latest!.id)}`}
      >Fix findings</a
    >
  {:else if needsReview.has(pull.review.state)}
    <span title={hint}
      ><button class="button small" disabled={busy || off} onclick={() => void queue()}
        >Review</button
      ></span
    >
  {/if}
  <DropdownMenu.Root>
    <DropdownMenu.Trigger
      class="button ghost icon small"
      aria-label={`Actions for ${pull.repo} #${pull.number}`}
      ><MoreHorizontal size={17} /></DropdownMenu.Trigger
    >
    <DropdownMenu.Portal
      ><DropdownMenu.Content class="menu pull-menu" sideOffset={4} align="end">
        {#if !inactive}<div class="menu-heading helper">Review with mode</div>
          {#each ['quick', 'standard', 'deep'] as mode}
            <DropdownMenu.Item
              class="menu-item"
              disabled={busy || off}
              onSelect={() => void queue(mode as ReviewMode)}
              ><GitPullRequest size={14} />{mode[0].toUpperCase() +
                mode.slice(1)}</DropdownMenu.Item
            >
          {/each}{/if}
        {#if hint}<p class="menu-hint helper">{hint}</p>{/if}
        {#if pull.review.latest}<DropdownMenu.Item
            class="menu-item"
            onSelect={() => void goto(`/reviews/${encodeURIComponent(pull.review.latest!.id)}`)}
            >View latest review</DropdownMenu.Item
          >{/if}
        {#if off}<DropdownMenu.Item class="menu-item" onSelect={() => void goto('/repositories')}
            >Repositories</DropdownMenu.Item
          >{/if}
        <DropdownMenu.Item
          class="menu-item"
          onSelect={() => window.open(pull.url, '_blank', 'noopener,noreferrer')}
          >Open on GitHub<ArrowUpRight size={13} /></DropdownMenu.Item
        >
      </DropdownMenu.Content></DropdownMenu.Portal
    >
  </DropdownMenu.Root>
</div>

<style>
  .pull-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 4px;
  }
  .menu-heading {
    padding: 8px 10px 4px;
  }
  .menu-hint {
    max-width: 210px;
    padding: 8px 10px;
  }
  :global(.pull-menu [data-disabled]) {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
