<script lang="ts">
  import type { PullThread } from '../types';
  import PullComment from './PullComment.svelte';
  let { thread, hideBots = false }: { thread: PullThread; hideBots?: boolean } = $props();
  const location = $derived(
    `${thread.path}${(thread.line ?? thread.original_line) ? `:${thread.line ?? thread.original_line}` : ''}`,
  );
  const comments = $derived(thread.comments.filter((comment) => !hideBots || !comment.author.bot));
  const lines = $derived(thread.diff_hunk.split('\n'));
</script>

{#snippet content()}
  {#if thread.diff_hunk}
    <details class="diff-context">
      <summary>Diff context</summary>
      <pre class="diff-full">{#each lines as line}<span
            class:added={line.startsWith('+')}
            class:removed={line.startsWith('-')}>{line || ' '}{'\n'}</span
          >{/each}</pre>
    </details>
    <pre
      class="diff-preview"
      aria-label="Last four lines of diff context">{#each lines.slice(-4) as line}<span
          class:added={line.startsWith('+')}
          class:removed={line.startsWith('-')}>{line || ' '}{'\n'}</span
        >{/each}</pre>
  {/if}
  <div class="replies">
    {#each comments as comment (comment.id)}<div class="reply">
        <PullComment {comment} />
      </div>{/each}
  </div>
  {#if thread.truncated}<p class="helper">More replies are available on GitHub.</p>{/if}
{/snippet}
{#if thread.is_resolved}
  <details class="thread resolved" data-thread={thread.id}>
    <summary
      >Resolved · <span class="mono">{location}</span> · {comments.length}
      {comments.length === 1 ? 'comment' : 'comments'}{#if thread.is_outdated}<span class="badge"
          >Outdated</span
        >{/if}</summary
    >
    <div class="thread-body">{@render content()}</div>
  </details>
{:else}
  <article class="thread" data-thread={thread.id} aria-label={`Thread on ${location}`}>
    <div class="thread-heading">
      <span class="mono">{location}</span>{#if thread.is_outdated}<span class="badge">Outdated</span
        >{/if}<span class="helper">Unresolved</span>
    </div>
    {@render content()}
  </article>
{/if}

<style>
  .thread {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 14px;
    min-width: 0;
  }
  .thread > summary {
    color: var(--muted);
    font-size: 11px;
    cursor: pointer;
    overflow-wrap: anywhere;
  }
  .thread > summary .badge {
    margin-left: 8px;
  }
  .thread-heading {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 12px;
    font-size: 11px;
    overflow-wrap: anywhere;
  }
  .thread-heading .helper {
    margin-left: auto;
  }
  .thread-body {
    margin-top: 14px;
  }
  .diff-context {
    margin-bottom: 8px;
  }
  .diff-context summary {
    font-size: 11px;
    color: var(--muted);
    cursor: pointer;
  }
  pre {
    font-size: 11px;
    line-height: 1.8;
    background: var(--surface);
    border-radius: 4px;
    padding: 10px;
    overflow-x: auto;
    margin: 8px 0 14px;
  }
  .diff-context[open] + .diff-preview {
    display: none;
  }
  .added {
    color: var(--green);
  }
  .removed {
    color: var(--red);
  }
  .reply + .reply {
    border-top: 1px solid var(--border);
    margin-top: 14px;
    padding-top: 14px;
  }
  .replies {
    border-left: 2px solid var(--border);
    padding-left: 12px;
  }
</style>
