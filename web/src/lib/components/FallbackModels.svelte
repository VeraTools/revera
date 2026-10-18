<script lang="ts">
  import { ArrowUp, ArrowDown, X } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import type { ConfigDocument } from '#lib/config.svelte';
  import type { Role, Secret, TestResult } from '#lib/types';
  import ModelFields from './ModelFields.svelte';
  import Notice from './Notice.svelte';
  import Status from './Status.svelte';

  let {
    config,
    role,
    secrets,
    setKey,
    removeKey,
    disabled,
  }: {
    config: ConfigDocument;
    role: Extract<Role, 'investigator' | 'validator'>;
    secrets: Secret[];
    setKey: (name: string) => void;
    removeKey: (name: string) => void;
    disabled: boolean;
  } = $props();
  const routes = $derived(config.get<Record<string, unknown>[]>(['models', role, 'fallback'], []));
  let testing = $state<number>();
  let results = $state<Record<number, TestResult>>({});
  let testedYaml = $state('');
  let error = $state('');

  async function test(index: number) {
    testing = index;
    error = '';
    if (testedYaml !== config.yaml) results = {};
    testedYaml = config.yaml;
    try {
      results[index] = await api.test(role, index);
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      testing = undefined;
    }
  }
</script>

<details class="fallback-models">
  <summary>Fallback models{routes.length ? ` (${routes.length})` : ''}</summary>
  <p class="helper">Used in order when this model fails: rate limits, outages, bad keys.</p>
  {#if error}<Notice message={error} />{/if}
  {#each routes as _, index}
    <section class="fallback-route" aria-label={`${role} fallback ${index + 1}`}>
      <div class="section-heading">
        <h4>Fallback {index + 1}</h4>
        <div class="actions">
          <button
            class="button ghost icon small"
            aria-label={`Move fallback ${index + 1} up`}
            disabled={index === 0 || testing !== undefined}
            onclick={() => config.changeFallback(role, index, -1)}><ArrowUp size={14} /></button
          >
          <button
            class="button ghost icon small"
            aria-label={`Move fallback ${index + 1} down`}
            disabled={index === routes.length - 1 || testing !== undefined}
            onclick={() => config.changeFallback(role, index, 1)}><ArrowDown size={14} /></button
          >
          <button
            class="button ghost icon small"
            aria-label={`Remove fallback ${index + 1}`}
            disabled={testing !== undefined}
            onclick={() => config.changeFallback(role, index)}><X size={14} /></button
          >
        </div>
      </div>
      <ModelFields
        {config}
        {role}
        {secrets}
        {setKey}
        {removeKey}
        routePath={['models', role, 'fallback', index]}
        fieldId={`model-${role}-fallback-${index}`}
      />
      <div class="model-footer">
        <button
          class="button small"
          disabled={disabled || testing !== undefined}
          aria-label={`Test ${role} fallback ${index + 1}`}
          onclick={() => void test(index)}
          >{testing === index ? 'Testing…' : 'Test connection'}</button
        >
        {#if testedYaml === config.yaml && results[index]}
          <Status
            status={results[index].ok ? 'complete' : 'failed'}
            label={results[index].ok
              ? `${results[index].latency_ms} ms`
              : results[index].error || 'Failed'}
          />
        {/if}
      </div>
    </section>
  {/each}
  <button
    class="button small"
    disabled={testing !== undefined}
    onclick={() => config.addFallback(role)}>Add fallback</button
  >
</details>

<style>
  .fallback-models {
    margin-top: 24px;
    border-top: 1px solid var(--border);
    padding-top: 16px;
  }
  summary {
    cursor: pointer;
    font-size: 12px;
  }
  .helper {
    margin: 12px 0 16px;
  }
  .fallback-route {
    padding: 16px 0 24px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 16px;
  }
  h4 {
    font-size: 12px;
    font-weight: 500;
  }
</style>
