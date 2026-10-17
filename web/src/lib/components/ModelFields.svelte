<script lang="ts">
  import { List, KeyRound } from '@lucide/svelte';
  import { ConfigDocument, protocols, efforts } from '#lib/config.svelte';
  import type { Secret } from '#lib/types';
  import Status from './Status.svelte';
  import SelectField from './SelectField.svelte';

  let {
    config,
    role,
    secrets,
    setKey,
    discover,
    models = [],
    listDisabled = false,
    retrieval = false,
    compact = false,
  }: {
    config: ConfigDocument;
    role: string;
    secrets: Secret[];
    setKey: (name: string) => void;
    discover?: () => void;
    models?: string[];
    listDisabled?: boolean;
    retrieval?: boolean;
    compact?: boolean;
  } = $props();

  const path = $derived(retrieval ? ['vera', role] : ['models', role]);
  const id = $derived(`model-${role}`);
  const keyName = $derived(
    config.get(
      [...path, 'api_key_env'],
      retrieval ? `${role.toUpperCase()}_API_KEY` : 'REVERA_MODEL_API_KEY',
    ),
  );
  const source = $derived(secrets.find((secret) => secret.name === keyName)?.source || 'missing');
  const protocol = $derived(config.get([...path, 'protocol'], 'openai-chat'));

  function editKey() {
    if (!config.has([...path, 'api_key_env'])) config.set([...path, 'api_key_env'], keyName);
    setKey(keyName);
  }
</script>

<div class="field-grid" class:compact-fields={compact}>
  {#if !retrieval}
    <div class="field" class:full={compact}>
      <label for={`${id}-protocol`}>Protocol</label>
      <SelectField
        id={`${id}-protocol`}
        value={protocol}
        options={protocols}
        onchange={(value) => config.set([...path, 'protocol'], value)}
      />
      {#if compact}<small>Match the API format supported by your endpoint.</small>{/if}
    </div>
    {#if !compact}
      <div class="field">
        <label for={`${id}-reasoning`}>Reasoning effort</label>
        <SelectField
          id={`${id}-reasoning`}
          value={config.effort(path)}
          options={efforts.map((effort) => ({
            value: effort,
            label: effort === 'xhigh' ? 'Extra high' : effort[0].toUpperCase() + effort.slice(1),
          }))}
          onchange={(value) => config.setEffort(path, value)}
        />
      </div>
    {/if}
  {/if}
  {#if protocol === 'scripted' && !retrieval}
    <div class="field full">
      <label for={`${id}-script`}>Transcript path</label>
      <input
        id={`${id}-script`}
        value={config.get([...path, 'script'], '')}
        onchange={(event) => config.set([...path, 'script'], event.currentTarget.value)}
        placeholder="fixtures/transcript.json"
      />
      <small>A file on the server, for offline testing.</small>
    </div>
  {:else}
    <div class="field full">
      <label for={`${id}-endpoint`}>Endpoint</label>
      <input
        id={`${id}-endpoint`}
        type="url"
        value={config.get([...path, 'base_url'], '')}
        onchange={(event) => config.set([...path, 'base_url'], event.currentTarget.value)}
        placeholder="https://api.example.com/v1"
      />
      {#if compact || retrieval}<small>Use HTTPS. Local endpoints can use HTTP.</small>{/if}
    </div>
  {/if}
  <div class="field full">
    <label for={`${id}-name`}>Model</label>
    <div class="inline-field">
      <input
        id={`${id}-name`}
        value={config.get([...path, 'model'], '')}
        onchange={(event) => config.set([...path, 'model'], event.currentTarget.value)}
        placeholder="model-name"
        list={`${id}-options`}
      />
      {#if discover}<button
          class="button icon"
          onclick={discover}
          disabled={listDisabled}
          aria-label={`List ${role} models`}
          title="List models from the saved endpoint"><List size={16} /></button
        >{/if}
    </div>
    <datalist id={`${id}-options`}
      >{#each models as model}<option value={model}></option>{/each}</datalist
    >
  </div>
  {#if !retrieval && !compact}
    <div class="field">
      <label for={`${id}-tokens`}>Max output tokens</label>
      <input
        id={`${id}-tokens`}
        type="number"
        min="1"
        value={config.get([...path, 'max_output_tokens'], 4000)}
        onchange={(event) =>
          config.set([...path, 'max_output_tokens'], event.currentTarget.valueAsNumber)}
      />
    </div>
    <div class="field">
      <label for={`${id}-temperature`}>Temperature</label>
      <input
        id={`${id}-temperature`}
        type="number"
        min="0"
        step="0.1"
        value={config.get([...path, 'temperature'], 0.2)}
        onchange={(event) =>
          config.set([...path, 'temperature'], event.currentTarget.valueAsNumber)}
      />
    </div>
  {/if}
  {#if retrieval || protocol !== 'scripted'}
    <div class="field full">
      {#if compact}
        <span class="field-label">API key</span>
        <code class="key-name" title={keyName}>{keyName}</code>
      {:else}
        <label for={`${id}-env`}>Key environment variable</label>
        <input
          id={`${id}-env`}
          class="mono"
          value={keyName}
          pattern="[A-Za-z_][A-Za-z0-9_]*"
          onchange={(event) => config.set([...path, 'api_key_env'], event.currentTarget.value)}
        />
      {/if}
      <div class="key-status">
        <Status
          status={source === 'missing' ? 'partial' : 'complete'}
          label={source === 'missing' ? 'Key not set' : `Set in ${source}`}
        />
        <button class="button small" onclick={editKey}
          ><KeyRound size={12} />{source === 'missing' ? 'Set key' : 'Replace key'}</button
        >
      </div>
    </div>
  {/if}
</div>
