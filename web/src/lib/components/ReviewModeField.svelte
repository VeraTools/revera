<script lang="ts">
  import { modes } from '#lib/config.svelte';
  import type { ReviewMode } from '#lib/types';
  import SelectField from './SelectField.svelte';

  let {
    id,
    value = $bindable<ReviewMode | ''>(''),
    defaultMode,
  }: { id: string; value?: ReviewMode | ''; defaultMode: ReviewMode } = $props();
  const options = $derived([
    { value: '', label: `Default (${modes.find((mode) => mode.value === defaultMode)?.label})` },
    ...modes,
  ]);
</script>

<div class="field">
  <label for={id}>Review mode</label>
  <SelectField {id} {value} {options} onchange={(next) => (value = next as ReviewMode | '')} />
  <small>Quick uses a smaller budget. Deep allows more time.</small>
</div>
