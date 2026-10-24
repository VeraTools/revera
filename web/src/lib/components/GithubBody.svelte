<script lang="ts">
  import { renderGithub } from '../markdown';
  let { body, collapsible = false }: { body: string; collapsible?: boolean } = $props();
  let expanded = $state(false);
  const long = $derived(collapsible && (body.split('\n').length > 25 || body.length > 2400));
</script>

<div class="github-body" class:clamped={long && !expanded}>{@html renderGithub(body)}</div>
{#if long}<button
    class="button small ghost disclosure"
    aria-expanded={expanded}
    onclick={() => (expanded = !expanded)}>{expanded ? 'Show less' : 'Show more'}</button
  >{/if}

<style>
  .github-body {
    font-size: 13px;
    line-height: 1.8;
    overflow-wrap: anywhere;
  }
  .clamped {
    max-height: 38em;
    overflow: hidden;
  }
  .github-body :global(p),
  .github-body :global(ul),
  .github-body :global(ol),
  .github-body :global(blockquote),
  .github-body :global(pre),
  .github-body :global(details) {
    margin: 12px 0;
  }
  .github-body :global(> :first-child) {
    margin-top: 0;
  }
  .github-body :global(> :last-child) {
    margin-bottom: 0;
  }
  .github-body :global(h4) {
    font-size: 13px;
    font-weight: 600;
    margin: 18px 0 8px;
  }
  .github-body :global(ul) {
    list-style: disc;
    padding-left: 20px;
  }
  .github-body :global(ol) {
    list-style: decimal;
    padding-left: 20px;
  }
  .github-body :global(li) {
    margin: 3px 0;
  }
  .github-body :global(a) {
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  .github-body :global(code) {
    font-size: 11px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface);
    padding: 1px 4px;
  }
  .github-body :global(pre) {
    overflow-x: auto;
    padding: 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    line-height: 1.7;
  }
  .github-body :global(pre code) {
    border: 0;
    padding: 0;
  }
  .github-body :global(blockquote) {
    border-left: 2px solid var(--strong-border);
    padding-left: 14px;
    color: var(--muted);
  }
  .github-body :global(hr) {
    border: 0;
    border-top: 1px solid var(--border);
    margin: 20px 0;
  }
  .github-body :global(summary) {
    cursor: pointer;
    font-size: 12px;
  }
  .disclosure {
    margin-top: 12px;
  }
</style>
