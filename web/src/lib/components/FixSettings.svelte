<script lang="ts">
  import { untrack } from 'svelte';
  import type { FixSettings, FixPolicy, Secret, Role, TestResult } from '#lib/types';
  import type { ConfigDocument } from '#lib/config.svelte';
  import ModelFields from './ModelFields.svelte';
  import FallbackModels from './FallbackModels.svelte';
  import Toggle from './Toggle.svelte';
  import SelectField from './SelectField.svelte';
  import Status from './Status.svelte';
  import Notice from './Notice.svelte';
  let {
    data,
    config,
    secrets,
    setKey,
    removeKey,
    disabled,
    test,
    tests,
    valid = $bindable(true),
  }: {
    data: FixSettings;
    config: ConfigDocument;
    secrets: Secret[];
    setKey: (name: string) => void;
    removeKey: (name: string) => void;
    disabled: boolean;
    test: (role: Role) => void;
    tests: Partial<Record<Role, TestResult>>;
    valid?: boolean;
  } = $props();
  const fixes = $derived(data.fixes);
  const triggerOptions = [
    { value: 'manual', label: 'Manual only' },
    { value: 'after_review', label: 'After accepted review' },
  ];
  const deliveryOptions = $derived([
    { value: 'patch', label: 'Patch preview' },
    { value: 'draft_pr', label: 'Draft pull request' },
    { value: 'same_branch', label: 'Commit to PR branch', disabled: !fixes.allow_same_branch },
  ]);
  const advancedToggles = [
    {
      key: 'trusted_skip_confirm',
      title: 'Trusted requesters may skip confirmation',
      hint: 'Maintainers and admins using mentions or Checks; only when persistent execution is on.',
    },
    {
      key: 'author_preview',
      title: 'Allow PR authors to request previews',
      hint: 'Private preview only. Being the author never grants write permission.',
    },
    {
      key: 'allow_workflow_changes',
      title: 'Allow workflow changes',
      hint: 'Also requires Workflows: write on the installation. Off blocks changes to CI files.',
    },
    {
      key: 'external',
      title: 'Allow external fix previews',
      hint: 'For external/private reviews. Patch download only; no upstream publication.',
    },
    {
      key: 'rereview',
      title: 'Re-review published fixes',
      hint: 'Review the published SHA and report resolved and still-open findings.',
    },
  ] as const;
  const limitFields = [
    ['attempts', 'Attempts'],
    ['tool_calls', 'Tool calls'],
    ['seconds', 'Runtime (seconds)'],
    ['model_requests', 'Model requests'],
    ['max_files', 'Changed files'],
    ['max_patch_kb', 'Patch size (KB)'],
    ['concurrent', 'Concurrent fixes'],
    ['per_user_hour', 'Requests per user / hour'],
    ['per_repo_day', 'Requests per repo / day'],
  ] as const;
  const modelRoles = [
    {
      role: 'fixer',
      label: 'Fixer',
      parent: 'investigator',
      description: 'Generates minimal edits in the sandbox.',
    },
    {
      role: 'fix_verifier',
      label: 'Fix verifier',
      parent: 'validator, then investigator',
      description: 'Independently checks the patched tree in a fresh context.',
    },
  ] as const;
  let commandError = $state('');
  let commandText = $state('');
  $effect(() => {
    const policy = data.fixes;
    untrack(() => {
      commandText = policy.test_command.map((arg) => JSON.stringify(arg)).join(' ');
      commandError = '';
      valid = true;
    });
  });
  function command(value: string) {
    commandText = value;
    try {
      const args: string[] = [];
      let arg = '';
      let quote = '';
      let escaped = false;
      let started = false;
      for (const char of value) {
        if (escaped) {
          arg += char;
          escaped = false;
          started = true;
        } else if (char === '\\' && quote !== "'") {
          escaped = true;
          started = true;
        } else if (quote) {
          if (char === quote) quote = '';
          else arg += char;
        } else if (char === '"' || char === "'") {
          quote = char;
          started = true;
        } else if (/\s/.test(char)) {
          if (started) {
            args.push(arg);
            arg = '';
            started = false;
          }
        } else {
          arg += char;
          started = true;
        }
      }
      if (quote || escaped) throw new Error('Close the quote or escape before saving.');
      if (started) args.push(arg);
      if (args.length && !args[0]) throw new Error('The executable cannot be empty.');
      fixes.test_command = args;
      commandError = '';
      valid = true;
    } catch (cause) {
      commandError = cause instanceof Error ? cause.message : 'Invalid command.';
      valid = false;
    }
  }
  function cap(enabled: boolean) {
    fixes.allow_same_branch = enabled;
    if (!enabled && fixes.delivery === 'same_branch') fixes.delivery = 'patch';
  }
  function delivery(value: string) {
    fixes.delivery = value as FixPolicy['delivery'];
    if (value === 'same_branch') fixes.require_approval = true;
  }
</script>

<section class="settings-section" aria-label="Fix settings">
  <div class="section-heading">
    <div>
      <h2>Fixes</h2>
      <p>Separate from reviews. An offer is not permission to generate or publish code.</p>
    </div>
  </div>
  <div class="notice" role="note">
    <span
      ><strong>Effective policy:</strong>
      {fixes.execution
        ? fixes.trigger === 'after_review'
          ? 'Generate after complete reviews with accepted findings.'
          : 'Generate only on request.'
        : 'Execution is off; each preview needs one-time consent.'} Preferred delivery: {fixes.delivery.replaceAll(
        '_',
        ' ',
      )}. {fixes.require_approval || fixes.delivery === 'same_branch'
        ? 'GitHub writes require approval.'
        : fixes.delivery === 'draft_pr'
          ? 'Ready previews publish as draft PRs without approval when that destination is available.'
          : 'Patch previews never write to GitHub.'} Server caps still apply.</span
    >
  </div>
  <div class="toggle-row">
    <div>
      <h3>Offer fixes in installed PR summaries</h3>
      <p>Discovery only. No model work and no code writes.</p>
    </div>
    <Toggle
      label="Offer fixes in installed PR summaries"
      checked={fixes.offer}
      onchange={(value) => (fixes.offer = value)}
    />
  </div>
  <div class="toggle-row">
    <div>
      <h3>Enable fix execution</h3>
      <p>Persistent permission to generate previews. Off still allows explicit one-time consent.</p>
    </div>
    <Toggle
      label="Enable fix execution"
      checked={fixes.execution}
      onchange={(value) => (fixes.execution = value)}
    />
  </div>
  <div class="field-grid section">
    <div class="field">
      <label for="fix-trigger">Fix trigger</label><SelectField
        id="fix-trigger"
        value={fixes.trigger}
        options={triggerOptions}
        onchange={(value) => (fixes.trigger = value as FixPolicy['trigger'])}
      /><small>After-review generation runs only when execution is enabled.</small>
    </div>
    <div class="field">
      <label for="fix-default-delivery">Preferred delivery</label><SelectField
        id="fix-default-delivery"
        value={fixes.delivery}
        options={deliveryOptions}
        onchange={delivery}
      /><small>Actual permissions and fork status decide which destinations are available.</small>
    </div>
  </div>
  <div class="toggle-row">
    <div>
      <h3>Require confirmation before generation</h3>
      <p>Manual requests wait in the dashboard before paid model work.</p>
    </div>
    <Toggle
      label="Require confirmation before generation"
      checked={fixes.confirm}
      onchange={(value) => (fixes.confirm = value)}
    />
  </div>
  <details class="fix-settings-group">
    <summary>Delivery and requester permissions</summary>
    <div class="toggle-row">
      <div>
        <h3>Allow commits to the PR branch</h3>
        <p>
          Server-wide safety cap. Still needs Contents: write, a same-repo branch, and explicit
          approval.
        </p>
      </div>
      <Toggle
        label="Allow commits to the PR branch"
        checked={fixes.allow_same_branch}
        onchange={cap}
      />
    </div>
    <div class="toggle-row">
      <div>
        <h3>Require approval before GitHub writes</h3>
        <p>
          {fixes.delivery === 'same_branch'
            ? 'Always required for same-branch commits.'
            : 'When off, a ready preview is published as a draft PR without approval if that destination is available. Commits to the PR branch always need approval.'}
        </p>
      </div>
      <Toggle
        label="Require approval before GitHub writes"
        checked={fixes.require_approval || fixes.delivery === 'same_branch'}
        disabled={fixes.delivery === 'same_branch'}
        onchange={(value) => (fixes.require_approval = value)}
      />
    </div>
    <div class="field section">
      <label for="fix-requesters">Who may request fixes</label><SelectField
        id="fix-requesters"
        value={fixes.requesters}
        options={[
          { value: 'maintainers', label: 'Maintainers and admins' },
          { value: 'writers', label: 'Writers, maintainers and admins' },
        ]}
        onchange={(value) => (fixes.requesters = value as FixPolicy['requesters'])}
      /><small>Checked using live GitHub repository permissions, not association labels.</small>
    </div>
    {#each advancedToggles as field}<div class="toggle-row">
        <div>
          <h3>{field.title}</h3>
          <p>{field.hint}</p>
        </div>
        <Toggle
          label={field.title}
          checked={fixes[field.key]}
          onchange={(value) => (fixes[field.key] = value)}
        />
      </div>{/each}
    <div class="field section">
      <label for="fix-branch-prefix">Fix branch prefix</label><input
        id="fix-branch-prefix"
        bind:value={fixes.branch_prefix}
      /><small
        >Used for new fix branches. Revera never force-pushes or overwrites an existing branch.</small
      >
    </div>
  </details>
  <details class="fix-settings-group">
    <summary>Sandbox, tests and resource limits</summary>
    <div class="health-line">
      <Status
        status={data.sandbox.available ? 'ready' : 'failed'}
        label={data.sandbox.available ? 'Sandbox available' : 'Sandbox unavailable'}
      /><span class="helper"
        >{data.sandbox.reason ||
          (data.sandbox.proc ? '/proc mount available.' : 'Runs without a /proc mount.')}</span
      >
    </div>
    <p class="helper">
      Generation and verification use isolated workspaces. Tests run without network access or
      credentials. Repository YAML cannot enable fixes or raise these limits.
    </p>
    <div class="field section">
      <label for="fix-test-command">Test command (argv)</label><input
        id="fix-test-command"
        class="mono"
        value={commandText}
        oninput={(event) => command(event.currentTarget.value)}
        placeholder="cargo test --lib"
        aria-invalid={!!commandError}
        aria-describedby="fix-command-hint"
      /><small id="fix-command-hint"
        >Space-separated arguments; quote arguments containing spaces. Runs directly, not through a
        shell. Leave empty for no configured tests.</small
      >{#if commandError}<Notice message={commandError} />{/if}
      <div class="argv" aria-label="Test command arguments">
        {#each fixes.test_command as arg, i}<code
            ><span class="muted">{i}:</span> {JSON.stringify(arg)}</code
          >{:else}<span class="helper"
            >No test command configured. Do not interpret this as tests passed.</span
          >{/each}
      </div>
    </div>
    <div class="field-grid section">
      <div class="field">
        <label for="fix-test-timeout">Test timeout (seconds)</label><input
          id="fix-test-timeout"
          type="number"
          min="1"
          bind:value={fixes.test_timeout_seconds}
        />
      </div>
      {#each limitFields as [key, label]}<div class="field">
          <label for={`fix-limit-${key}`}>{label}</label><input
            id={`fix-limit-${key}`}
            type="number"
            min="1"
            bind:value={fixes.limits[key]}
          />
        </div>{/each}
    </div>
  </details>
  <details class="fix-settings-group">
    <summary>Fix model routes</summary>
    <p class="helper">
      Inherit review routes until overridden. Tests use saved configuration; save changes first.
    </p>
    <div class="model-grid section">
      {#each modelRoles as model}<section class="model-card" aria-label={model.role}>
          <div class="model-card-header">
            <h3>
              {model.label}
              {#if !config.has(['models', model.role])}<span class="badge">Inherit</span>{/if}
            </h3>
            <p>{model.description} Inherits {model.parent} when unset.</p>
            <p class="helper">Effective saved route: {data.models[model.role]}</p>
            <div class="toggle-row">
              <h3>Override route</h3>
              <Toggle
                label={`Override ${model.label.toLowerCase()} model`}
                checked={config.has(['models', model.role])}
                onchange={(value) =>
                  value ? config.overrideRoute(model.role) : config.remove(['models', model.role])}
              />
            </div>
          </div>
          {#if config.has(['models', model.role])}<ModelFields
              {config}
              role={model.role}
              {secrets}
              {setKey}
              {removeKey}
            /><FallbackModels
              {config}
              role={model.role}
              {secrets}
              {setKey}
              {removeKey}
              {disabled}
            />{/if}
          <div class="model-footer">
            <button class="button small" {disabled} onclick={() => test(model.role)}
              >{model.role === 'fixer' ? 'Test fix model' : 'Test fix verifier'}</button
            >{#if tests[model.role]}<Status
                status={tests[model.role]!.ok ? 'complete' : 'failed'}
                label={tests[model.role]!.ok
                  ? `${tests[model.role]!.latency_ms} ms`
                  : tests[model.role]!.error || 'Failed'}
              />{/if}
          </div>
        </section>{/each}
    </div>
  </details>
</section>

<style>
  .fix-settings-group {
    border-bottom: 1px solid var(--border);
    padding: 20px 0;
  }
  summary {
    cursor: pointer;
    font-size: 13px;
    font-weight: 500;
  }
  .fix-settings-group > .helper {
    margin-top: 16px;
  }
  .health-line {
    margin: 20px 0 12px;
  }
  .argv {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .argv code {
    font-size: 11px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 4px 8px;
    overflow-wrap: anywhere;
  }
</style>
