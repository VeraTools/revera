<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { api, errorMessage } from '#lib/api';
  import { modes } from '#lib/config.svelte';
  import type { ReviewMode } from '#lib/types';
  import SelectField from './SelectField.svelte';
  import Notice from './Notice.svelte';
  let { onqueued }: { onqueued: (id: string) => void } = $props();
  let open = $state(false);
  let url = $state('');
  let mode = $state<ReviewMode>('standard');
  let busy = $state(false);
  let error = $state('');
  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = '';
    try {
      const result = await api.externalReview(url, mode);
      open = false;
      onqueued(result.id);
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Trigger class="button">Review PR URL</Dialog.Trigger>
  <Dialog.Portal>
    <Dialog.Overlay class="dialog-overlay" />
    <Dialog.Content class="dialog">
      <Dialog.Title class="dialog-title">Review a public PR</Dialog.Title>
      <Dialog.Description class="dialog-description"
        >Review any open public GitHub PR, including forks, without an app installation. Results
        stay private here. No upstream comments, checks, or code writes.</Dialog.Description
      >
      <form onsubmit={submit}>
        <div class="field">
          <label for="external-pr-url">GitHub PR URL</label>
          <input
            id="external-pr-url"
            type="url"
            required
            bind:value={url}
            placeholder="https://github.com/owner/repo/pull/123"
          />
          <small>Only https://github.com/OWNER/REPO/pull/NUMBER is supported.</small>
        </div>
        <div class="field">
          <label for="external-review-mode">External review mode</label>
          <SelectField
            id="external-review-mode"
            value={mode}
            options={modes}
            onchange={(value) => (mode = value as ReviewMode)}
          />
          <small>Quick uses a smaller budget. Deep allows more time and model spend.</small>
        </div>
        {#if error}<Notice message={error} />{/if}
        <div class="actions">
          <Dialog.Close class="button" disabled={busy}>Cancel</Dialog.Close>
          <button type="submit" class="button primary" disabled={busy}
            >{busy ? 'Resolving PR…' : 'Queue private review'}</button
          >
        </div>
      </form>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
