<script lang="ts">
  import type { PullDetail } from '../types';
  import { conversation, reviewDecision } from '../pulls';
  import { relative } from '../time';
  import PullComment from './PullComment.svelte';
  import PullThread from './PullThread.svelte';
  import GithubBody from './GithubBody.svelte';
  let { detail }: { detail: PullDetail } = $props();
  let unresolved = $state(false),
    hideBots = $state(false);
  const items = $derived(conversation(detail, unresolved, hideBots));
  const unresolvedCount = $derived(detail.threads.filter((thread) => !thread.is_resolved).length);
  const truncated = $derived(
    Object.values(detail.truncated).some(Boolean) ||
      detail.threads.some((thread) => thread.truncated),
  );
</script>

<section aria-labelledby="conversation-heading" class="conversation">
  <h2 id="conversation-heading">Conversation</h2>
  <p class="helper conversation-summary">
    {detail.comments.length} comments · {detail.reviews.length} reviews · {detail.threads.length} threads
    ({unresolvedCount} unresolved)
  </p>
  <div class="segmented conversation-filters" role="group" aria-label="Conversation filters">
    <button aria-pressed={!unresolved} onclick={() => (unresolved = false)}>All</button>
    <button aria-pressed={unresolved} onclick={() => (unresolved = true)}>Unresolved threads</button
    >
    <button aria-pressed={hideBots} onclick={() => (hideBots = !hideBots)}>Hide bots</button>
  </div>
  {#if truncated}<p class="truncation helper" role="note">
      Some conversation items are not shown. <a
        class="text-link"
        href={detail.url}
        target="_blank"
        rel="noreferrer">View on GitHub</a
      >
    </p>{/if}
  <div class="timeline">
    {#each items as item (item.kind === 'comment' ? item.comment.id : item.kind === 'review' ? item.review.id : item.thread.id)}
      <div class="timeline-item" class:standalone-thread={item.kind === 'thread'}>
        {#if item.kind === 'comment'}<PullComment comment={item.comment} />
        {:else if item.kind === 'thread'}<PullThread thread={item.thread} {hideBots} />
        {:else}
          {#snippet reviewBody()}
            <div class="review-heading">
              <strong
                >{hideBots && item.review.author.bot
                  ? 'Review discussion'
                  : item.review.author.login || 'Deleted user'}</strong
              >
              <span
                class:approved={item.review.state === 'APPROVED'}
                class:changes={item.review.state === 'CHANGES_REQUESTED'}
                >{reviewDecision(item.review.state)}</span
              >
              {#if item.review.submitted_at}<a
                  href={item.review.url}
                  class="muted"
                  target="_blank"
                  rel="noreferrer"
                  ><time datetime={item.review.submitted_at}
                    >{relative(item.review.submitted_at)}</time
                  ></a
                >{/if}
            </div>
            {#if item.review.body && (!hideBots || !item.review.author.bot)}<GithubBody
                body={item.review.body}
              />{/if}
            {#if item.threads.length}<div class="nested-threads">
                {#each item.threads as thread (thread.id)}<PullThread {thread} {hideBots} />{/each}
              </div>{/if}
          {/snippet}
          {#if item.review.author.revera && !hideBots}
            <details class="revera-row">
              <summary
                >Revera review · {item.review.submitted_at
                  ? relative(item.review.submitted_at)
                  : 'Pending'}</summary
              >
              <div class="expanded">{@render reviewBody()}</div>
            </details>
          {:else}{@render reviewBody()}{/if}
        {/if}
      </div>
    {:else}<p class="helper conversation-empty">
        {unresolved || hideBots
          ? 'No conversation items match these filters.'
          : 'No conversation yet.'}
      </p>{/each}
  </div>
</section>

<style>
  .conversation {
    margin-top: 28px;
  }
  .conversation-summary {
    margin: 6px 0 16px;
  }
  .conversation-filters {
    flex-wrap: wrap;
    margin-bottom: 20px;
  }
  .truncation {
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    margin-bottom: 16px;
  }
  .timeline-item {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 18px;
    margin-bottom: 16px;
    min-width: 0;
  }
  .standalone-thread {
    border: 0;
    padding: 0;
  }
  .review-heading {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 11px;
    color: var(--muted);
  }
  .review-heading strong {
    color: var(--text);
    font-size: 12px;
    font-weight: 500;
  }
  .review-heading:has(+ :global(.github-body)) {
    margin-bottom: 12px;
  }
  .approved {
    color: var(--green);
  }
  .changes {
    color: var(--amber);
  }
  .nested-threads {
    display: grid;
    gap: 12px;
    margin-top: 16px;
  }
  summary {
    cursor: pointer;
    color: var(--muted);
    font-size: 12px;
  }
  .expanded {
    margin-top: 16px;
  }
  .conversation-empty {
    padding: 20px 0;
  }
</style>
