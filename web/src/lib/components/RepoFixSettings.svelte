<script lang="ts">
  import { onMount } from 'svelte';
  import { Dialog } from 'bits-ui';
  import { api, errorMessage } from '#lib/api';
  import type { FixSettings, FixOverride, FixPolicy, FixDelivery } from '#lib/types';
  import SelectField from './SelectField.svelte';
  import Notice from './Notice.svelte';
  let { repo, onclose }: { repo: string; onclose: () => void } = $props();
  let data = $state<FixSettings>();
  let override = $state<FixOverride>({});
  let busy = $state(false);
  let error = $state('');
  let saved = $state(false);
  onMount(() => {
    void api
      .fixSettings()
      .then((result) => {
        data = result;
        override = result.repos[repo] || {};
      })
      .catch((cause) => (error = errorMessage(cause)));
  });
  async function save(reset = false) {
    busy = true;
    error = '';
    try {
      await api.repoFixSettings(repo, reset ? null : override);
      if (reset) override = {};
      saved = true;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog.Root
  open
  onOpenChange={(value) => {
    if (!value) onclose();
  }}
  ><Dialog.Portal
    ><Dialog.Overlay class="dialog-overlay" /><Dialog.Content class="dialog"
      ><Dialog.Title><h2>Repository Fixes</h2></Dialog.Title><Dialog.Description
        class="dialog-description"
        >{repo} · server-owned overrides, independent of repository YAML. Limits and write safety caps
        cannot be raised here.</Dialog.Description
      >
      {#if error}<Notice message={error} />{/if}{#if saved}<Notice
          message="Repository fix settings saved."
          kind="success"
        />{/if}
      {#if data}<div class="field">
          <label for="repo-fix-execution">Fix execution</label><SelectField
            id="repo-fix-execution"
            value={override.execution === null || override.execution === undefined
              ? ''
              : String(override.execution)}
            options={[
              { value: '', label: `Inherit (${data.fixes.execution ? 'enabled' : 'disabled'})` },
              { value: 'true', label: 'Enabled' },
              { value: 'false', label: 'Disabled' },
            ]}
            onchange={(value) => {
              override.execution = value === '' ? null : value === 'true';
              saved = false;
            }}
          />
        </div>
        <div class="field section">
          <label for="repo-fix-trigger">Fix trigger</label><SelectField
            id="repo-fix-trigger"
            value={override.trigger || ''}
            options={[
              {
                value: '',
                label: `Inherit (${data.fixes.trigger === 'manual' ? 'manual only' : 'after accepted review'})`,
              },
              { value: 'manual', label: 'Manual only' },
              { value: 'after_review', label: 'After accepted review' },
            ]}
            onchange={(value) => {
              override.trigger = (value as FixPolicy['trigger']) || null;
              saved = false;
            }}
          />
        </div>
        <div class="field section">
          <label for="repo-fix-delivery">Fix delivery</label><SelectField
            id="repo-fix-delivery"
            value={override.delivery || ''}
            options={[
              { value: '', label: `Inherit (${data.fixes.delivery.replaceAll('_', ' ')})` },
              { value: 'patch', label: 'Patch preview' },
              { value: 'draft_pr', label: 'Draft pull request' },
              {
                value: 'same_branch',
                label: 'Commit to PR branch',
                disabled: !data.fixes.allow_same_branch,
              },
            ]}
            onchange={(value) => {
              override.delivery = (value as FixDelivery) || null;
              saved = false;
            }}
          /><small
            >{data.fixes.allow_same_branch
              ? 'Same-branch publication still needs explicit approval and write permissions.'
              : 'Same-branch delivery is disabled by the server-wide cap.'}</small
          >
        </div>
        <div class="actions">
          <button class="button ghost" disabled={busy} onclick={() => save(true)}
            >Reset to defaults</button
          ><button class="button primary" disabled={busy} onclick={() => save()}
            >Save fix override</button
          >
        </div>{:else if !error}<p class="loading" role="status">Loading fix settings…</p>{/if}
      <div class="actions"><Dialog.Close class="button">Close</Dialog.Close></div>
    </Dialog.Content></Dialog.Portal
  ></Dialog.Root
>
