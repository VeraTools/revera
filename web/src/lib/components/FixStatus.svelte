<script lang="ts">
  import Status from './Status.svelte';
  import type { FixState } from '#lib/types';
  let { state }: { state: FixState } = $props();
  const status = $derived(
    ['ready', 'published', 'verified'].includes(state)
      ? 'complete'
      : ['queued', 'generating', 'validating', 'publishing', 'verifying'].includes(state)
        ? 'running'
        : ['failed', 'validation_failed', 'publish_failed'].includes(state)
          ? 'failed'
          : 'partial',
  );
</script>

<Status {status} label={state.replaceAll('_', ' ')} />
