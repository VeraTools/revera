<script lang="ts">
  import { Tabs } from 'bits-ui';
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { beforeNavigate } from '$app/navigation';
  import { Save, Check } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { ConfigDocument, backends } from '#lib/config.svelte';
  import type { Settings, Secret, ServerSettings, Role, TestResult } from '#lib/types';
  import ModelFields from './ModelFields.svelte';
  import SecretDialog from './SecretDialog.svelte';
  import Toggle from './Toggle.svelte';
  import Status from './Status.svelte';
  import Notice from './Notice.svelte';
  import SelectField from './SelectField.svelte';

  const tabs = ['Models', 'Review', 'Budgets', 'Retrieval', 'Access', 'Advanced'];
  let tab = $state('models'); let config = $state<ConfigDocument>(); let server = $state<ServerSettings>();
  let secrets = $state<Secret[]>([]); let savedYaml = $state(''); let savedServer = $state('');
  let error = $state(''); let message = $state(''); let loading = $state(true); let busy = $state('');
  let secretOpen = $state(false); let secretName = $state('');
  let modelLists = $state<Partial<Record<Role, string[]>>>({}); let tests = $state<Partial<Record<Role, TestResult>>>({});
  const yamlDirty = $derived(config?.yaml !== savedYaml);
  const serverDirty = $derived(JSON.stringify(server) !== savedServer);
  const dirty = $derived(!!config && (yamlDirty || serverDirty));
  const envNames = $derived(config?.referencedEnv() || []);
  const retrievalOn = $derived(config?.has(['vera']) ? config.get(['vera', 'enabled'], true) : false);
  function hydrate(data: Settings) { config = new ConfigDocument(data.yaml); server = data.server; secrets = data.secrets; savedYaml = data.yaml; savedServer = JSON.stringify(data.server); }
  async function load() { try { hydrate(await api.settings()); error = ''; } catch (cause) { error = errorMessage(cause); } finally { loading = false; } }
  onMount(() => { const requested = page.url.searchParams.get('tab'); if (requested && tabs.some((item) => item.toLowerCase() === requested)) tab = requested; void load(); });
  beforeNavigate(({ cancel }) => { if (dirty && !window.confirm('Discard your unsaved settings?')) cancel(); });
  function beforeUnload(event: BeforeUnloadEvent) { if (dirty) { event.preventDefault(); event.returnValue = ''; } }
  async function save() {
    if (!config || !server || config.errors.length) return;
    busy = 'save'; error = ''; message = '';
    let configSaved = false;
    try {
      if (yamlDirty) { await api.saveConfig(config.yaml); savedYaml = config.yaml; tests = {}; configSaved = true; }
      if (serverDirty) { await api.saveServer(server); savedServer = JSON.stringify(server); }
      secrets = (await api.settings()).secrets;
      message = 'Settings saved.';
    } catch (cause) { error = `${errorMessage(cause)}${configSaved && serverDirty ? ' Review configuration was saved; server settings are still unsaved.' : ''}`; }
    finally { busy = ''; }
  }
  function discard() { config?.replace(savedYaml); server = JSON.parse(savedServer) as ServerSettings; error = ''; message = ''; }
  function setKey(name: string) { if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) { error = 'Use a valid environment variable name before setting a key.'; return; } secretName = name; secretOpen = true; }
  async function refreshSecrets() { secrets = (await api.settings()).secrets; tests = {}; message = 'API key saved.'; }
  async function removeSecret(name: string) { if (!window.confirm(`Remove the dashboard key for ${name}? The server environment will be used if available.`)) return; busy = name; try { await api.deleteSecret(name); await refreshSecrets(); message = 'Dashboard key removed.'; } catch (cause) { error = errorMessage(cause); } finally { busy = ''; } }
  async function test(role: Role) { busy = role; error = ''; try { tests[role] = await api.test(role); } catch (cause) { error = errorMessage(cause); } finally { busy = ''; } }
  async function discover(role: Role) { busy = role; error = ''; try { modelLists[role] = (await api.models(role)).models; if (!modelLists[role]?.length) message = 'This endpoint did not return a model list. Enter the model name manually.'; } catch (cause) { error = errorMessage(cause); } finally { busy = ''; } }
  function validate() { if (!config) return; error = config.errors.join('\n'); message = error ? '' : 'YAML syntax is valid. Saving runs the server’s trust and strategy checks.'; }
  const list = (value: string) => value.split(/[\n,]/).map((item) => item.trim()).filter(Boolean);
  const budgetFields = [
    { key: 'agent_max_tool_calls', label: 'Tool calls per agent', value: 25, hint: 'Maximum repository tool calls for each investigator or validator.' },
    { key: 'agent_max_seconds', label: 'Seconds per agent', value: 300, hint: 'Time allowed for each investigator or validator.' },
    { key: 'run_max_requests', label: 'Requests per review', value: 120, hint: 'Total HTTP attempts across the review, including retries.' },
    { key: 'run_max_seconds', label: 'Seconds per review', value: 900, hint: 'Time allowed for the whole review, including validation.' },
    { key: 'retries', label: 'Retries per request', value: 3, hint: 'Retry failed requests, respecting the endpoint’s retry delay.' }
  ];
</script>

<svelte:window onbeforeunload={beforeUnload} />
{#if error}<Notice message={error} retry={!config ? load : undefined} />{/if}{#if message}<Notice message={message} kind="success" />{/if}
{#if config && server}
  <Tabs.Root bind:value={tab}><Tabs.List class="tabs-list" aria-label="Settings sections">{#each tabs as item}<Tabs.Trigger class="tab-trigger" value={item.toLowerCase()} disabled={config.errors.length > 0 && item !== 'Advanced'}>{item}</Tabs.Trigger>{/each}</Tabs.List>
    <Tabs.Content value="models">
      <div class="section-heading"><div><h2>Review models</h2><p>One model investigates. A fresh context validates every finding.</p></div></div>
      <div class="model-grid">{#each ['investigator', 'validator'] as role}<section class="model-card" aria-label={role}><div class="model-card-header"><h3>{role === 'investigator' ? 'Investigator' : 'Validator'}{#if role === 'validator' && !config.has(['models', 'validator'])}<span class="badge" style="margin-left: 8px">Inherits investigator</span>{/if}</h3><p>{role === 'investigator' ? 'Reads the diff and repository to propose findings.' : 'Re-checks each finding independently before publication.'}</p></div><ModelFields {config} {role} {secrets} {setKey} models={modelLists[role as Role] || []} discover={() => void discover(role as Role)} listDisabled={dirty || !!busy} /><div class="model-footer"><button class="button small" disabled={dirty || !!busy} onclick={() => void test(role as Role)}>{busy === role ? 'Testing…' : 'Test connection'}</button>{#if tests[role as Role]}<Status status={tests[role as Role]!.ok ? 'complete' : 'failed'} label={tests[role as Role]!.ok ? `${tests[role as Role]!.latency_ms} ms` : tests[role as Role]!.error || 'Failed'} />{/if}</div></section>{/each}</div>
      <p class="helper" style="margin-top: 16px">Tests and model lists use saved settings. Save changes first.</p>
      <section class="secrets-section"><div class="section-heading"><div><h2>Referenced keys</h2><p>Keys stay on the server. Only their source is shown here.</p></div></div>{#each envNames as name}<div class="secret-row"><code>{name}</code><Status status={secrets.find((secret) => secret.name === name)?.source === 'missing' || !secrets.some((secret) => secret.name === name) ? 'partial' : 'complete'} label={secrets.find((secret) => secret.name === name)?.source || 'missing'} /><button class="button small" onclick={() => setKey(name)}>Set key</button>{#if secrets.find((secret) => secret.name === name)?.source === 'dashboard'}<button class="button ghost small" disabled={!!busy} onclick={() => void removeSecret(name)}>Remove</button>{/if}</div>{:else}<p class="helper">This configuration does not reference any keys.</p>{/each}</section>
    </Tabs.Content>
    <Tabs.Content value="review"><section class="settings-section"><div class="section-heading"><div><h2>Review behavior</h2><p>Defaults for repositories without their own configuration.</p></div></div><div class="field-grid">
      <div class="field"><label for="review-strategy">Strategy</label><SelectField id="review-strategy" value={config.get(['review', 'strategy'], 'baseline')} options={[{ value: 'baseline', label: 'Baseline' }, { value: 'delegated', label: 'Delegated' }, { value: 'panel', label: 'Panel' }]} onchange={(value) => config!.set(['review', 'strategy'], value)} /><small>Delegated and panel routes are configured in Advanced.</small></div>
      <div class="field"><label for="review-severity">Minimum severity</label><SelectField id="review-severity" value={config.get(['review', 'min_severity'], 'low')} options={[{ value: 'low', label: 'Low' }, { value: 'medium', label: 'Medium' }, { value: 'high', label: 'High' }]} onchange={(value) => config!.set(['review', 'min_severity'], value)} /><small>Drop findings below this severity.</small></div>
      <div class="field"><label for="review-findings">Maximum findings</label><input id="review-findings" type="number" min="1" value={config.get(['review', 'max_findings'], 10)} onchange={(event) => config!.set(['review', 'max_findings'], event.currentTarget.valueAsNumber)} /></div>
      <div class="field"><label for="review-publish">Publish mode</label><SelectField id="review-publish" value={config.get(['review', 'publish'], 'dry-run')} options={[{ value: 'dry-run', label: 'Dry run' }, { value: 'comment', label: 'Comment' }]} onchange={(value) => config!.set(['review', 'publish'], value)} /><small>Server reviews always publish validated findings as comments.</small></div>
    </div><div class="toggle-row" style="margin-top: 24px"><div><h3>Review drafts</h3><p>Automatically review draft pull requests.</p></div><Toggle checked={server.review_drafts} label="Review draft pull requests" onchange={(value) => server!.review_drafts = value} /></div><div class="field" style="margin-top: 24px"><label for="skip-authors">Skipped authors</label><textarea id="skip-authors" rows="3" value={server.skip_authors.join('\n')} onchange={(event) => server!.skip_authors = list(event.currentTarget.value)}></textarea><small>One GitHub login per line. Pull requests from these authors are skipped.</small></div></section></Tabs.Content>
    <Tabs.Content value="budgets"><section class="settings-section"><div class="section-heading"><div><h2>Review limits</h2><p>A limit stops the affected stage and marks the review partial.</p></div></div><div class="field-grid">{#each budgetFields as field}<div class="field"><label for={`budget-${field.key}`}>{field.label}</label><input id={`budget-${field.key}`} type="number" min={field.key === 'retries' ? 0 : 1} value={config.get(['budget', field.key], field.value)} onchange={(event) => config!.set(['budget', field.key], event.currentTarget.valueAsNumber)} /><small>{field.hint}</small></div>{/each}</div></section></Tabs.Content>
    <Tabs.Content value="retrieval">
      <section class="settings-section">
        <div class="section-heading"><div><h2>Repository retrieval</h2><p>Vera adds semantic search alongside the built-in lexical tools.</p></div></div>
        <div class="toggle-row">
          <div><h3>Use Vera</h3><p>Build a reusable search index for each repository.</p></div>
          <Toggle checked={retrievalOn || false} label="Enable Vera retrieval" onchange={(value) => config!.set(['vera', 'enabled'], value)} />
        </div>
        {#if retrievalOn}
          <div class="field" style="max-width: 400px; margin: 24px 0">
            <label for="vera-backend">Embedding backend</label>
            <SelectField id="vera-backend" value={config.get(['vera', 'backend'], 'local')} options={backends} onchange={(value) => config!.set(['vera', 'backend'], value)} />
            <small>Local embeddings do not need an API key.</small>
          </div>
          <div class="model-grid">
            {#if config.get(['vera', 'backend'], 'local') === 'api'}
              <section class="model-card">
                <div class="model-card-header"><h3>Embedding</h3><p>Turns repository content into a searchable index.</p></div>
                <ModelFields {config} role="embedding" {secrets} {setKey} retrieval />
                <div class="model-footer">
                  <button class="button small" disabled={dirty || !!busy} onclick={() => void test('embedding')}>Test connection</button>
                  {#if tests.embedding}<Status status={tests.embedding.ok ? 'complete' : 'failed'} label={tests.embedding.ok ? `${tests.embedding.latency_ms} ms` : tests.embedding.error || 'Failed'} />{/if}
                </div>
              </section>
            {/if}
            <section class="model-card">
              <div class="model-card-header"><h3>Reranker <span class="badge" style="margin-left: 8px">Optional</span></h3><p>Reorders search results for relevance.</p></div>
              <div class="toggle-row" style="padding-top: 0; margin-bottom: 20px">
                <span class="field-label">Use a reranker</span>
                <Toggle checked={config.has(['vera', 'reranker'])} label="Enable reranker" onchange={(value) => value ? config!.set(['vera', 'reranker'], { base_url: 'https://api.example.com/v1', model: 'model-name', api_key_env: 'RERANKER_API_KEY' }) : config!.remove(['vera', 'reranker'])} />
              </div>
              {#if config.has(['vera', 'reranker'])}
                <ModelFields {config} role="reranker" {secrets} {setKey} retrieval />
                <div class="field" style="margin-top: 16px">
                  <label for="reranker-protocol">Reranker format</label>
                  <SelectField id="reranker-protocol" value={config.get(['vera', 'reranker', 'protocol'], '')} options={[{ value: '', label: 'Detect from endpoint' }, { value: 'generic', label: 'Generic' }, { value: 'voyage', label: 'Top-k format' }]} onchange={(value) => value ? config!.set(['vera', 'reranker', 'protocol'], value) : config!.remove(['vera', 'reranker', 'protocol'])} />
                  <small>Reranker options and embedding request limits are in Advanced.</small>
                </div>
              {:else}<p class="helper">Search results are used without reranking.</p>{/if}
            </section>
          </div>
        {/if}
      </section>
    </Tabs.Content>
    <Tabs.Content value="access"><section class="settings-section"><div class="section-heading"><div><h2>Access</h2><p>Who can manage this server, and whose repositories it reviews.</p></div></div><div class="field-grid"><div class="field"><label for="admins">Administrators</label><textarea id="admins" rows="5" value={server.admins.join('\n')} onchange={(event) => server!.admins = list(event.currentTarget.value)}></textarea><small>GitHub logins, one per line. Keep your own login to retain access.</small></div><div class="field"><label for="owners">Allowed owners</label><textarea id="owners" rows="5" value={server.allowed_owners.join('\n')} onchange={(event) => server!.allowed_owners = list(event.currentTarget.value)}></textarea><small>GitHub user or organization logins, one per line. Environment owners are merged in by the server.</small></div></div><div class="toggle-row" style="margin-top: 24px"><div><h3>Enable newly installed repositories</h3><p>Start reviewing future pull requests in new repositories.</p></div><Toggle checked={server.default_enabled} label="Enable new repositories by default" onchange={(value) => server!.default_enabled = value} /></div></section></Tabs.Content>
    <Tabs.Content value="advanced"><div class="section-heading"><div><h2>Configuration file</h2><p>The same document as the form above. Comments are preserved.</p></div><div class="actions"><button class="button small" onclick={validate}><Check size={14} />Validate YAML</button><button class="button small" disabled={!!busy || !dirty || config.errors.length > 0} onclick={save}><Save size={14} />Save</button></div></div><label class="sr-only" for="yaml-editor">Review configuration YAML</label><textarea id="yaml-editor" class="yaml-editor" value={config.yaml} oninput={(event) => config!.replace(event.currentTarget.value)} spellcheck="false" autocapitalize="off" autocomplete="off" wrap="off"></textarea>{#if config.errors.length}<Notice message={config.errors.join('\n')} />{/if}<div class="yaml-toolbar"><span>revera.yaml</span><span>Save validates endpoints, credentials, and the selected strategy.</span></div></Tabs.Content>
  </Tabs.Root>
  {#if dirty}<div class="save-bar" role="status"><span>You have unsaved changes</span><div class="actions"><button class="button small" disabled={!!busy} onclick={discard}>Discard</button><button class="button primary small" disabled={!!busy || config.errors.length > 0} onclick={save}>{busy === 'save' ? 'Saving…' : 'Save changes'}</button></div></div>{/if}
{:else if loading}<div class="loading" role="status">Loading settings…</div>{/if}
<SecretDialog bind:open={secretOpen} name={secretName} onsaved={refreshSecrets} />
