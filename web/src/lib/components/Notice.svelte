<script lang="ts">
  let {
    message,
    kind = 'error',
    retry,
  }: { message: string; kind?: 'error' | 'success' | 'info'; retry?: () => void } = $props();
</script>

<div
  class="notice"
  class:error={kind === 'error'}
  class:success={kind === 'success'}
  role={kind === 'error' ? 'alert' : 'status'}
>
  <span>{message}</span>{#if retry}<button class="button small" onclick={retry}>Try again</button
    >{/if}
</div>
