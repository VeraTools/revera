<script lang="ts">
  import { Switch } from 'bits-ui';
  let { checked, label, disabled = false, onchange }: { checked: boolean; label: string; disabled?: boolean; onchange: (value: boolean) => void } = $props();
</script>

<Switch.Root class="switch" {checked} {disabled} aria-label={label} onCheckedChange={onchange}>
  <Switch.Thumb class="switch-thumb" />
</Switch.Root>
