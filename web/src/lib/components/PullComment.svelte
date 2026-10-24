<script lang="ts">
  import type { PullComment } from '../types';
  import { relative } from '../time';
  import GithubBody from './GithubBody.svelte';
  let { comment }: { comment: PullComment } = $props();
</script>

{#snippet content()}
  <div class="comment-meta">
    <strong>{comment.author.login || 'Deleted user'}</strong>{#if comment.author.bot}<span
        class="badge">Bot</span
      >{/if}<a class="muted" href={comment.url} target="_blank" rel="noreferrer"
      ><time datetime={comment.created_at} title={new Date(comment.created_at).toLocaleString()}
        >{relative(comment.created_at)}</time
      ></a
    >
  </div>
  <GithubBody body={comment.body} />
{/snippet}
{#if comment.author.revera}
  <details class="revera-row">
    <summary
      >Revera review · <time datetime={comment.created_at}>{relative(comment.created_at)}</time
      ></summary
    >
    <div class="expanded">{@render content()}</div>
  </details>
{:else}<div class="comment">{@render content()}</div>{/if}

<style>
  .comment-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 11px;
    margin-bottom: 12px;
  }
  .comment-meta strong {
    font-weight: 500;
    font-size: 12px;
  }
  summary {
    font-size: 12px;
    color: var(--muted);
    cursor: pointer;
  }
  .expanded {
    padding-top: 16px;
  }
</style>
