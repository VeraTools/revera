<script lang="ts">
  import { Select } from 'bits-ui';
  import { Check, ChevronDown } from '@lucide/svelte';

  let {
    id, value = $bindable(''), options, label, disabled = false, onchange
  }: {
    id: string;
    value?: string;
    options: { value: string; label: string; disabled?: boolean }[];
    label?: string;
    disabled?: boolean;
    onchange?: (value: string) => void;
  } = $props();

  // Bits treats an empty value as no selection. Filters still need a named
  // "all" option, so use an internal value without changing API/config values.
  const empty = '__revera_empty_option__';
  const items = $derived(options.map((option) => ({ ...option, value: option.value || empty })));
  const selected = $derived(value || empty);
  const text = $derived(options.find((option) => option.value === value)?.label || value);

  function change(next: string) {
    const result = next === empty ? '' : next;
    if (result === value) return;
    value = result;
    onchange?.(result);
  }
</script>

<Select.Root type="single" value={selected} {items} {disabled} allowDeselect={false} onValueChange={change}>
  <Select.Trigger {id} class="select-trigger" aria-label={label}>
    <span class="select-label">{text}</span><ChevronDown size={14} strokeWidth={1.7} />
  </Select.Trigger>
  <Select.Portal>
    <Select.Content class="select-content" sideOffset={4} align="start" collisionPadding={8} aria-labelledby={id}>
      <Select.Viewport class="select-viewport">
        {#each items as item (item.value)}
          <Select.Item value={item.value} label={item.label} disabled={item.disabled} class="select-item">
            {#snippet children({ selected })}
              <span>{item.label}</span><span class="select-check">{#if selected}<Check size={14} />{/if}</span>
            {/snippet}
          </Select.Item>
        {/each}
      </Select.Viewport>
    </Select.Content>
  </Select.Portal>
</Select.Root>
