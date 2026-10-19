<script lang="ts">
  let { source }: { source: 'installed' | 'external' } = $props();
</script>

{#if source === 'external'}
  <span class="badge" title="Private to this dashboard. Nothing is posted on GitHub."
    >External · private</span
  >
{/if}
