<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { api, errorMessage } from '#lib/api';
  import Notice from './Notice.svelte';
  let {
    open = $bindable(false),
    name,
    onsaved,
  }: { open?: boolean; name: string; onsaved: () => void | Promise<void> } = $props();
  let value = $state('');
  let error = $state('');
  let busy = $state(false);
  $effect(() => {
    if (!open) {
      value = '';
      error = '';
    }
  });
  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!value.trim()) return;
    busy = true;
    try {
      await api.setSecret(name, value);
      value = '';
      await onsaved();
      open = false;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Portal>
    <Dialog.Overlay class="dialog-overlay" />
    <Dialog.Content class="dialog">
      <Dialog.Title><h2>Set API key</h2></Dialog.Title>
      <Dialog.Description class="dialog-description">
        Stored on your server as <code>{name}</code>. The dashboard never displays saved values.
      </Dialog.Description>
      {#if error}<Notice message={error} />{/if}
      <form onsubmit={save}>
        <div class="field">
          <label for="secret-value">API key</label><input
            id="secret-value"
            type="password"
            bind:value
            autocomplete="new-password"
            required
            spellcheck="false"
          />
        </div>
        <div class="actions">
          <Dialog.Close class="button" disabled={busy}>Cancel</Dialog.Close><button
            class="button primary"
            type="submit"
            disabled={busy || !value.trim()}>{busy ? 'Saving…' : 'Set key'}</button
          >
        </div>
      </form>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
