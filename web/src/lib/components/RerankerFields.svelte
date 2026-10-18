<script lang="ts">
  import { untrack } from 'svelte';
  import type { ConfigDocument } from '#lib/config.svelte';
  import type { Secret } from '#lib/types';
  import ModelFields from './ModelFields.svelte';
  import SelectField from './SelectField.svelte';
  import Toggle from './Toggle.svelte';

  let {
    config,
    secrets,
    setKey,
    removeKey,
    compact = false,
  }: {
    config: ConfigDocument;
    secrets: Secret[];
    setKey: (name: string) => void;
    removeKey: (name: string) => void;
    compact?: boolean;
  } = $props();

  const formats = [
    { value: '', label: 'Detect from endpoint' },
    { value: 'generic', label: 'Generic' },
    { value: 'voyage', label: 'Top-k format' },
  ];
  let previousEmbedding: { base_url: string; api_key_env: string } | undefined;
  const enabled = $derived(config.has(['vera', 'reranker']));
  const sharesConnection = $derived(
    config.get(['vera', 'reranker', 'base_url'], '') ===
      config.get(['vera', 'embedding', 'base_url'], '') &&
      config.get(['vera', 'reranker', 'api_key_env'], '') ===
        config.get(['vera', 'embedding', 'api_key_env'], 'EMBEDDING_API_KEY'),
  );

  // Keep wizard defaults in sync as the embedding connection is filled in.
  // Existing reranker overrides and Settings edits remain independent.
  $effect(() => {
    if (!compact) return;
    const embedding = {
      base_url: config.get(['vera', 'embedding', 'base_url'], ''),
      api_key_env: config.get(['vera', 'embedding', 'api_key_env'], 'EMBEDDING_API_KEY'),
    };
    untrack(() => {
      if (previousEmbedding && config.has(['vera', 'reranker'])) {
        for (const key of ['base_url', 'api_key_env'] as const) {
          if (
            embedding[key] !== previousEmbedding[key] &&
            config.get(['vera', 'reranker', key], '') === previousEmbedding[key]
          ) {
            config.set(['vera', 'reranker', key], embedding[key]);
          }
        }
      }
      previousEmbedding = embedding;
    });
  });

  function setEnabled(value: boolean) {
    config.setRerankerEnabled(value, compact);
  }
</script>

<div class="reranker-fields" class:compact>
  <div class="toggle-row">
    <span class="field-label">Use a reranker</span>
    <Toggle checked={enabled} label="Enable reranker" onchange={setEnabled} />
  </div>
  {#if compact}
    <p class="helper">
      {sharesConnection || !enabled
        ? 'Improves search results using your embedding endpoint and key.'
        : 'Improves search results using the saved reranker connection.'}
    </p>
  {/if}
  {#if enabled}
    <ModelFields
      {config}
      role="reranker"
      {secrets}
      {setKey}
      {removeKey}
      retrieval
      {compact}
      showConnection={!compact}
    />
    {#if !compact}
      <div class="field" style="margin-top: 16px">
        <label for="reranker-protocol">Reranker format</label>
        <SelectField
          id="reranker-protocol"
          value={config.get(['vera', 'reranker', 'protocol'], '')}
          options={formats}
          onchange={(value) => config.setRerankerFormat(value)}
        />
        <small>Reranker options and embedding request limits are in Advanced.</small>
      </div>
    {/if}
  {:else if !compact}
    <p class="helper">Search results are used without reranking.</p>
  {/if}
</div>

<style>
  .toggle-row {
    padding-top: 0;
    margin-bottom: 20px;
  }
  .compact {
    margin-top: 20px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }
  .compact .toggle-row {
    padding: 0;
    margin-bottom: 8px;
    border-bottom: 0;
  }
  .compact .helper {
    margin-bottom: 12px;
  }
</style>
