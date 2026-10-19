<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import {
    ArrowUpRight,
    Check,
    Copy,
    GitPullRequest,
    FolderGit2,
    ShieldCheck,
    MessageCircle,
  } from '@lucide/svelte';
  import { api, errorMessage, mockMode } from '#lib/api';
  import { ConfigDocument, backends } from '#lib/config.svelte';
  import type { Session, SetupStage, Secret, Role, TestResult } from '#lib/types';
  import Notice from '#lib/components/Notice.svelte';
  import ModelFields from '#lib/components/ModelFields.svelte';
  import SecretDialog from '#lib/components/SecretDialog.svelte';
  import Toggle from '#lib/components/Toggle.svelte';
  import Status from '#lib/components/Status.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  import RerankerFields from '#lib/components/RerankerFields.svelte';

  const stages: SetupStage[] = ['create_app', 'sign_in', 'install', 'configure'];
  const labels = ['Create app', 'Sign in', 'Install', 'Models'];
  const setupLogCommand = 'podman logs revera 2>&1 | grep setup';
  let commandCopied = $state(false);
  let session = $state<Session>();
  let config = $state<ConfigDocument>();
  let name = $state('revera');
  let org = $state('');
  let publicApp = $state('false');
  let publishFixes = $state(false);
  let installUrl = $state('');
  let error = $state('');
  let busy = $state('');
  let message = $state('');
  let secrets = $state<Secret[]>([]);
  let secretOpen = $state(false);
  let secretName = $state('');
  let separateValidator = $state(false);
  let models = $state<string[]>([]);
  let tests = $state<Partial<Record<Role, TestResult>>>({});
  let testedYaml = $state('');
  let apiRerankerInitialized = false;
  const step = $derived(session ? Math.max(0, stages.indexOf(session.setup.stage)) : 0);
  const setupToken = $derived(page.url.searchParams.get('token') || '');
  const needsSetupLink = $derived(
    session?.setup.stage === 'create_app' && !session.setup.authorized && !setupToken,
  );
  const retrievalOn = $derived(
    config?.has(['vera']) ? config.get(['vera', 'enabled'], true) : false,
  );
  const embeddingRequired = $derived(
    retrievalOn && config?.get(['vera', 'backend'], 'local') === 'api',
  );
  const testsCurrent = $derived(!!config && config.yaml === testedYaml);
  const tested = $derived(
    testsCurrent &&
      tests.investigator?.ok &&
      tests.validator?.ok &&
      (!embeddingRequired || tests.embedding?.ok),
  );
  const testRoles: Role[] = ['investigator', 'validator', 'embedding'];
  async function load() {
    try {
      session = await api.session();
      error = '';
      if (session.setup.stage === 'done' && page.url.searchParams.get('stage') !== 'configure') {
        await goto('/');
        return;
      }
      if (session.setup.stage === 'create_app') {
        const parts = new URL(session.setup.public_url).hostname.split('.');
        const host = parts.find((part) => part !== 'revera' && part !== 'www') || 'review';
        name = `revera-${host}`.slice(0, 34);
      }
      if (session.authenticated) {
        installUrl = (await api.overview()).app.install_url;
        if (session.setup.stage === 'configure' && !config) {
          const data = await api.settings();
          config = new ConfigDocument(data.yaml);
          secrets = data.secrets;
          separateValidator = config.has(['models', 'validator']);
          initializeApiReranker();
        }
      }
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  onMount(() => {
    void initialize();
    const timer = setInterval(() => {
      if (session?.setup.stage === 'install') void load();
    }, 4000);
    return () => clearInterval(timer);
  });
  async function initialize() {
    const mockStage = page.url.searchParams.get('stage');
    if (mockMode && mockStage && stages.includes(mockStage as SetupStage)) {
      const { setMockStage } = await import('#lib/mock');
      setMockStage(mockStage as SetupStage, page.url.searchParams.get('authorized') !== '0');
    }
    await load();
  }
  async function checkSetup() {
    busy = 'session';
    try {
      if (session?.authenticated && session.setup.stage === 'install')
        await api.reconcileInstallations();
      await load();
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function copySetupCommand() {
    try {
      await navigator.clipboard.writeText(setupLogCommand);
      commandCopied = true;
    } catch (cause) {
      error = errorMessage(cause);
    }
  }
  async function createApp(event: SubmitEvent) {
    event.preventDefault();
    busy = 'create';
    error = '';
    try {
      const result = await api.manifest(
        setupToken,
        name,
        org.trim(),
        publicApp === 'true',
        publishFixes,
      );
      if (mockMode) {
        const { setMockStage } = await import('#lib/mock');
        setMockStage('sign_in');
        await load();
        return;
      }
      const form = document.createElement('form');
      form.method = 'POST';
      form.action = result.post_url;
      const field = document.createElement('input');
      field.type = 'hidden';
      field.name = 'manifest';
      field.value = JSON.stringify(result.manifest);
      form.append(field);
      document.body.append(form);
      form.submit();
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function mockContinue(next: SetupStage) {
    if (!mockMode) return;
    const { setMockStage } = await import('#lib/mock');
    setMockStage(next);
    await load();
  }
  function setKey(key: string) {
    secretName = key;
    secretOpen = true;
  }
  function setSeparateValidator(enabled: boolean) {
    separateValidator = enabled;
    if (!enabled) config!.remove(['models', 'validator']);
  }
  function initializeApiReranker() {
    if (
      !config ||
      !retrievalOn ||
      config.get(['vera', 'backend'], 'local') !== 'api' ||
      apiRerankerInitialized
    )
      return;
    apiRerankerInitialized = true;
    if (!config.has(['vera', 'reranker'])) config.setRerankerEnabled(true, true);
  }
  function setRetrievalEnabled(enabled: boolean) {
    config!.set(['vera', 'enabled'], enabled);
    initializeApiReranker();
  }
  function setEmbeddingBackend(value: string) {
    config!.set(['vera', 'backend'], value);
    initializeApiReranker();
  }
  async function refreshSecrets() {
    secrets = (await api.settings()).secrets;
    testedYaml = '';
  }
  async function removeSecret(name: string) {
    if (
      !window.confirm(
        `Remove the dashboard key for ${name}? The server environment will be used if available.`,
      )
    )
      return;
    busy = name;
    try {
      await api.deleteSecret(name);
      await refreshSecrets();
      message = 'Dashboard key removed.';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function discover() {
    if (!config) return;
    busy = 'models';
    error = '';
    try {
      await api.saveConfig(config.yaml);
      models = (await api.models('investigator')).models;
      if (!models.length) message = 'No model list is available. Enter the model name manually.';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function test() {
    if (!config) return;
    busy = 'test';
    error = '';
    message = '';
    tests = {};
    const yaml = config.yaml;
    const testEmbedding = embeddingRequired;
    try {
      await api.saveConfig(yaml);
      tests.investigator = await api.test('investigator');
      tests.validator = await api.test('validator');
      if (testEmbedding) tests.embedding = await api.test('embedding');
      testedYaml = yaml;
      if (!tests.investigator.ok || !tests.validator.ok || tests.embedding?.ok === false)
        error =
          tests.investigator.error ||
          tests.validator.error ||
          tests.embedding?.error ||
          'A connection failed. Check the endpoint, model, and key.';
      else message = 'All tested connections are working.';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
  async function finish() {
    busy = 'finish';
    try {
      const current = await api.session();
      if (current.setup.stage !== 'done') {
        error =
          'Setup still needs attention. Test the saved models and check that their keys are set.';
        return;
      }
      await goto('/');
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
</script>

<svelte:head><title>Set up Revera</title></svelte:head>
<div class="setup-shell">
  <header class="setup-header">
    <a class="wordmark" href="/"><span class="brand-mark">R</span>Revera</a>
  </header>
  <main class="setup-content" class:models-step={step === 3}>
    <nav class="setup-progress" aria-label="Setup progress">
      {#each labels as label, index}<div
          class="setup-step"
          class:current={index === step}
          class:finished={index < step}
          aria-current={index === step ? 'step' : undefined}
        >
          <span class="step-number"
            >{#if index < step}<Check size={14} />{:else}{index + 1}{/if}</span
          ><span>{label}</span>
        </div>{/each}
    </nav>
    {#if error}<Notice
        message={error}
        retry={!session ? load : undefined}
      />{/if}{#if message}<Notice {message} kind="success" />{/if}
    {#if !session}<div class="setup-panel"><p class="loading" role="status">Loading setup…</p></div>
    {:else if needsSetupLink}
      <section class="setup-panel">
        <h1>Open your setup link</h1>
        <p>
          When the server starts, it prints a setup link in the server log. The link contains a
          one-time token. Open it in this browser.
        </p>
        <div class="setup-command">
          <pre><code>{setupLogCommand}</code></pre>
          <button class="button small" onclick={copySetupCommand} aria-label="Copy command"
            ><Copy size={13} />{commandCopied ? 'Copied' : 'Copy'}</button
          >
        </div>
        <p class="helper setup-docker-note">With Docker, use <code>docker logs</code> instead.</p>
        <button class="button primary setup-submit" onclick={checkSetup} disabled={!!busy}
          >{busy === 'session' ? 'Checking…' : 'Check again'}</button
        >
      </section>
    {:else if step === 0}<section class="setup-panel">
        <h1>Create your GitHub App</h1>
        <p>
          Revera uses a GitHub App to read code and post reviews. GitHub will ask you to confirm the
          app on the next page.
        </p>
        <form onsubmit={createApp}>
          <div class="field">
            <label for="app-name">App name</label><input
              id="app-name"
              bind:value={name}
              required
              maxlength="34"
            /><small>Choose a unique name. It appears as the author of your reviews.</small>
          </div>
          <div class="field">
            <label for="app-org">Organization <span class="muted">(optional)</span></label><input
              id="app-org"
              bind:value={org}
              placeholder="Your GitHub organization"
            /><small>Leave blank to create the app under your personal account.</small>
          </div>
          <div class="field">
            <label for="app-installability">Who can install this app?</label>
            <SelectField
              id="app-installability"
              bind:value={publicApp}
              options={[
                { value: 'false', label: 'Only this account' },
                { value: 'true', label: 'Any account or organization' },
              ]}
            />
            <small
              >Public installability is not access to this deployment. Each additional owner still
              needs a Revera administrator’s approval. GitHub organization rules also control who
              can install apps.</small
            >
          </div>
          <div class="field">
            <label
              ><input type="checkbox" bind:checked={publishFixes} /> Allow Revera to publish approved
              fixes</label
            >
            <small
              >Requests Contents write instead of read. This does not enable fix execution or
              authorize a write; patch-only fixes need no write permission.</small
            >
          </div>
          <button type="submit" class="button primary setup-submit" disabled={!!busy}
            >{busy ? 'Preparing…' : 'Create GitHub App'}<ArrowUpRight size={15} /></button
          >
        </form>
        <div class="setup-permissions">
          <div>
            <FolderGit2 size={15} />{publishFixes
              ? 'Write repository contents for approved fixes'
              : 'Read repository contents'}
          </div>
          <div><GitPullRequest size={15} />Post pull request reviews and checks</div>
          <div><MessageCircle size={15} />Reply to @mentions in pull request comments</div>
          <div><ShieldCheck size={15} />Only repositories you choose during installation</div>
        </div>
      </section>
      <p class="setup-note">
        The dashboard stays administrator-only, regardless of who can install the app. Your code
        stays between your server and the endpoints you configure.
      </p>
    {:else if step === 1}<section class="setup-panel">
        <h1>Sign in with GitHub</h1>
        <p>
          Your app is ready. Sign in to make your account the first administrator of this server.
        </p>
        {#if mockMode}<button
            class="button primary setup-submit"
            onclick={() => void mockContinue('install')}
            >Sign in with GitHub<ArrowUpRight size={15} /></button
          >{:else}<a class="button primary setup-submit" href="/auth/login"
            >Sign in with GitHub<ArrowUpRight size={15} /></a
          >{/if}
        <p class="helper" style="margin-top: 20px">
          Revera uses your identity for dashboard access. Your personal GitHub token is not stored.
        </p>
      </section>
    {:else if step === 2}<section class="setup-panel">
        <h1>Choose your repositories</h1>
        <p>
          Install your app on a GitHub account or organization. You can select specific repositories
          or allow all current and future repositories.
        </p>
        {#if mockMode}<button
            class="button primary setup-submit"
            onclick={() => void mockContinue('configure')}
            >Install on repositories<ArrowUpRight size={15} /></button
          >{:else if installUrl}<a
            class="button primary setup-submit"
            href={installUrl}
            target="_blank"
            rel="noreferrer">Install on repositories<ArrowUpRight size={15} /></a
          >{/if}
        <div class="polling"><Status status="running" label="Waiting for installation" /></div>
        <button class="button small" disabled={!!busy} onclick={checkSetup}
          >Refresh installation status</button
        >
        <p class="helper" style="margin-top: 24px">
          Keep this page open. It will continue when GitHub confirms the installation. Existing pull
          requests are not automatically reviewed.
        </p>
      </section>
    {:else if config}
      <section class="setup-panel">
        <h1>Connect your review models</h1>
        <p>
          The investigator proposes findings. The validator re-checks them in a fresh context before
          anything is posted.
        </p>
        <ModelFields
          {config}
          role="investigator"
          {secrets}
          {setKey}
          removeKey={removeSecret}
          {models}
          {discover}
          listDisabled={!!busy}
          compact
        />
        <div class="toggle-row" style="margin-top: 16px">
          <div>
            <h3>Use a different validator</h3>
            <p>Otherwise, the same model is called in a fresh context.</p>
          </div>
          <Toggle
            checked={separateValidator}
            label="Use a different validator model"
            onchange={setSeparateValidator}
          />
        </div>
        {#if separateValidator}
          <details>
            <summary>Validator connection</summary>
            <div class="setup-model-section">
              <ModelFields
                {config}
                role="validator"
                {secrets}
                {setKey}
                removeKey={removeSecret}
                compact
              />
            </div>
          </details>
        {/if}
        <details>
          <summary>Repository search <span class="muted">(optional)</span></summary>
          <div class="toggle-row">
            <div>
              <h3>Use Vera retrieval</h3>
              <p>Add semantic repository search. Local embeddings need no key.</p>
            </div>
            <Toggle
              checked={retrievalOn || false}
              label="Enable Vera retrieval"
              onchange={setRetrievalEnabled}
            />
          </div>
          {#if retrievalOn}
            <div class="field">
              <label for="setup-backend">Embedding backend</label>
              <SelectField
                id="setup-backend"
                value={config.get(['vera', 'backend'], 'local')}
                options={backends}
                onchange={setEmbeddingBackend}
              />
            </div>
            {#if config.get(['vera', 'backend'], 'local') === 'api'}
              <div style="margin-top: 20px">
                <ModelFields
                  {config}
                  role="embedding"
                  {secrets}
                  {setKey}
                  removeKey={removeSecret}
                  retrieval
                />
                <RerankerFields {config} {secrets} {setKey} removeKey={removeSecret} compact />
              </div>
            {/if}
          {/if}
        </details>
        <div class="actions" style="margin-top: 16px">
          <button class="button" onclick={test} disabled={!!busy}
            >{busy === 'test' ? 'Testing…' : 'Test connections'}</button
          >
          {#if testsCurrent}
            {#each testRoles as role}
              {#if tests[role]}
                <span title={tests[role]?.error}>
                  <Status
                    status={tests[role]?.ok ? 'complete' : 'failed'}
                    label={`${role}: ${tests[role]?.ok ? 'Connected' : 'Failed'}`}
                  />
                </span>
              {/if}
            {/each}
          {/if}
        </div>
        {#if testsCurrent && tests.embedding?.ok === false}
          <p class="helper">
            Embedding connection failed. Check the endpoint, key, and model. The endpoint must
            support embeddings, and the model must be an embedding model, not a reranker. You can
            turn off Use Vera retrieval under Repository search, then test again to continue without
            semantic search.
          </p>
        {/if}
        <button class="button primary setup-submit" disabled={!!busy || !tested} onclick={finish}
          >Finish setup</button
        >
        <p class="helper" style="margin-top: 12px">
          Testing saves this configuration. You can change all model settings later.
        </p>
        <p class="helper">
          Need to repair an existing configuration? <a class="text-link" href="/settings"
            >Open settings</a
          >.
        </p>
      </section>
      <p class="setup-note">API keys are stored on your server and never shown again.</p>
    {/if}
  </main>
</div>
<SecretDialog bind:open={secretOpen} name={secretName} onsaved={refreshSecrets} />
