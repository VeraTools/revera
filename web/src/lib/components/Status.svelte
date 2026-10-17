<script lang="ts">
  let { status, label }: { status: string; label?: string } = $props();
  const word = $derived(label ?? status);
</script>

<span class="status" data-status={status}
  ><span class="status-dot"></span>{word.charAt(0).toUpperCase() + word.slice(1)}</span
>
