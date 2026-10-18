<script lang="ts">
  import type { ConfigDocument } from '#lib/config.svelte';
  import type { Secret } from '#lib/types';
  import Toggle from './Toggle.svelte';
  import Status from './Status.svelte';

  let {
    config,
    secrets,
    setKey,
    removeKey,
  }: {
    config: ConfigDocument;
    secrets: Secret[];
    setKey: (name: string) => void;
    removeKey: (name: string) => void;
  } = $props();
  const enabled = $derived(config.get(['web_search', 'enabled'], false));
  const keyEnv = $derived(config.get(['web_search', 'api_key_env'], 'EXA_API_KEY'));
  const secret = $derived(secrets.find((key) => key.name === keyEnv));
  const source = $derived(secret?.source || 'missing');
  function changeEnv(value: string) {
    if (value.trim()) config.set(['web_search', 'api_key_env'], value.trim());
    else config.remove(['web_search', 'api_key_env']);
  }
  function addKey() {
    const name = keyEnv || 'EXA_API_KEY';
    config.set(['web_search', 'api_key_env'], name);
    setKey(name);
  }
</script>

<section class="settings-section web-search">
  <div class="section-heading">
    <div>
      <h2>Web search</h2>
      <p>Exa. Works without a key; add one for higher limits.</p>
    </div>
  </div>
  <div class="toggle-row">
    <div>
      <h3>Search the web</h3>
      <p>Look up documentation and read web pages during a review.</p>
    </div>
    <Toggle
      checked={enabled}
      label="Enable web search"
      onchange={(value) => config.set(['web_search', 'enabled'], value)}
    />
  </div>
  {#if enabled}
    <div class="field" style="max-width: 400px; margin: 24px 0">
      <label for="web-search-env"
        >Key environment variable <span class="muted">(optional)</span></label
      >
      <input
        id="web-search-env"
        class="mono"
        value={keyEnv}
        placeholder="EXA_API_KEY"
        onchange={(event) => changeEnv(event.currentTarget.value)}
      />
      <div class="key-status">
        {#if source !== 'missing'}<Status
            status="complete"
            label={`Set in ${source}${source === 'dashboard' && secret?.environment ? ' (overrides environment)' : ''}`}
          />
        {:else}<span class="helper">Using the free endpoint</span>{/if}
        <button class="button small" onclick={addKey}
          >{source === 'missing' ? 'Add key' : 'Replace key'}</button
        >
        {#if source === 'dashboard'}
          <button class="button ghost small" onclick={() => removeKey(keyEnv)}>Remove</button>
        {/if}
      </div>
    </div>
    {#if source !== 'missing'}
      <div class="toggle-row">
        <div>
          <h3>Use the free endpoint first</h3>
          <p>Use your key only if the free endpoint is limited or unavailable.</p>
        </div>
        <Toggle
          checked={config.get(['web_search', 'free_first'], false)}
          label="Use the free endpoint first"
          onchange={(value) => config.set(['web_search', 'free_first'], value)}
        />
      </div>
    {/if}
    <div class="toggle-row">
      <div>
        <h3>Allow on private repositories</h3>
        <p>Search queries from private repositories go to Exa.</p>
      </div>
      <Toggle
        checked={config.get(['web_search', 'allow_private'], false)}
        label="Allow on private repositories"
        onchange={(value) => config.set(['web_search', 'allow_private'], value)}
      />
    </div>
  {/if}
</section>

<style>
  .web-search {
    margin-top: 32px;
    padding-top: 28px;
    border-top: 1px solid var(--border);
  }
</style>
