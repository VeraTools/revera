<script lang="ts">
  import { onMount } from 'svelte';
  import { beforeNavigate } from '$app/navigation';
  import { Plus, Save, RotateCcw, Trash2 } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import type { Playbook } from '#lib/types';
  import {
    newSkillTemplate,
    effectiveSkillBytes,
    playbookBytes,
    setSkillEnabled,
    skillMetadata,
  } from '../../lib/playbook';
  import Notice from '#lib/components/Notice.svelte';
  import Toggle from '#lib/components/Toggle.svelte';

  let data = $state<Playbook>();
  let selected = $state('$reviewer');
  let content = $state('');
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let message = $state('');
  let showBuiltin = $state(false);
  const isReviewer = $derived(selected === '$reviewer');
  const isNew = $derived(selected === '$new');
  const skill = $derived(data?.skills.find((item) => item.name === selected));
  const original = $derived(
    isReviewer ? (data?.reviewer.effective ?? '') : (skill?.effective ?? ''),
  );
  const builtin = $derived(isReviewer ? data?.reviewer.builtin : skill?.builtin);
  const edited = $derived(isReviewer ? data?.reviewer.override !== null : skill?.override !== null);
  const dirty = $derived(!!data && (isNew || content !== original));
  const bytes = $derived(playbookBytes(content));
  const limit = $derived(
    isReviewer ? (data?.limits.reviewer_max_bytes ?? 0) : (data?.limits.skill_max_bytes ?? 0),
  );
  const total = $derived.by(() => {
    if (!data) return 0;
    try {
      const before = isReviewer
        ? playbookBytes(original)
        : isNew
          ? 0
          : effectiveSkillBytes(original);
      const after = isReviewer ? bytes : effectiveSkillBytes(content);
      return data.total_bytes - before + after;
    } catch {
      // The server reports invalid front matter when saving an unfinished draft.
      return data.total_bytes;
    }
  });
  const tooLarge = $derived(bytes > limit || total > (data?.limits.total_max_bytes ?? 0));
  const skillLimit = $derived(!!data && isNew && data.skills.length >= data.limits.max_skills);
  const title = $derived(
    isReviewer ? 'Reviewer instructions' : isNew ? 'New skill' : (skill?.name ?? 'Skill'),
  );

  function openItem(name: string, text?: string) {
    selected = name;
    content =
      text ??
      (name === '$reviewer'
        ? data!.reviewer.effective
        : data!.skills.find((item) => item.name === name)!.effective);
    showBuiltin = false;
    error = '';
    message = '';
  }
  function canDiscard() {
    return !dirty || window.confirm('Discard your unsaved playbook changes?');
  }
  function select(name: string) {
    if (busy || name === selected || !canDiscard()) return;
    openItem(name);
  }
  function newSkill() {
    if (!data || busy || data.skills.length >= data.limits.max_skills || !canDiscard()) return;
    let name = 'my-skill';
    let suffix = 2;
    while (data.skills.some((item) => item.name === name)) name = `my-skill-${suffix++}`;
    openItem('$new', newSkillTemplate(name));
  }
  async function load() {
    loading = true;
    error = '';
    try {
      data = await api.playbook();
      openItem('$reviewer');
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      loading = false;
    }
  }
  onMount(() => void load());
  beforeNavigate(({ cancel, willUnload }) => {
    if (!willUnload && !canDiscard()) cancel();
  });
  function beforeUnload(event: BeforeUnloadEvent) {
    if (dirty) {
      event.preventDefault();
      event.returnValue = '';
    }
  }
  function accept(result: Playbook, name: string, notice: string) {
    data = result;
    openItem(name);
    message = notice;
  }
  async function save() {
    if (!data || !dirty || busy || tooLarge || skillLimit) return;
    busy = true;
    error = '';
    message = '';
    try {
      if (isReviewer)
        accept(
          await api.savePlaybookReviewer(content),
          '$reviewer',
          'Reviewer instructions saved.',
        );
      else {
        const name = isNew ? skillMetadata(content).name : selected;
        if (isNew && data.skills.some((item) => item.name === name))
          throw new Error('A skill with this name already exists. Choose a new name.');
        accept(await api.savePlaybookSkill(name, content), name, 'Skill saved.');
      }
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
  async function resetOrDelete() {
    if (busy || !data) return;
    const custom = skill?.custom;
    if (
      custom
        ? !window.confirm(
            `Delete the custom skill "${selected}"?${dirty ? ' Unsaved edits will be discarded.' : ''}`,
          )
        : !window.confirm(
            `Reset "${title}" to built-in? The saved override will be removed.${dirty ? ' Unsaved edits will be discarded.' : ''}`,
          )
    )
      return;
    busy = true;
    error = '';
    message = '';
    try {
      const result = isReviewer
        ? await api.resetPlaybookReviewer()
        : await api.deletePlaybookSkill(selected);
      accept(
        result,
        custom ? '$reviewer' : selected,
        custom ? 'Custom skill deleted.' : 'Reset to built-in.',
      );
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
  async function toggleEnabled(enabled: boolean) {
    if (!skill || busy || dirty) return;
    busy = true;
    error = '';
    message = '';
    try {
      const result = await api.savePlaybookSkill(
        skill.name,
        setSkillEnabled(skill.effective, enabled),
      );
      accept(result, selected, enabled ? 'Skill enabled.' : 'Skill disabled.');
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>Playbook · Revera</title></svelte:head>
<svelte:window onbeforeunload={beforeUnload} />
<div class="page-header">
  <div>
    <h1>Playbook</h1>
    <p>
      Instructions every review follows. Edits apply to all repositories; repository-specific rules
      belong in repository guidance and notes.
    </p>
  </div>
</div>
{#if loading}<p class="loading" role="status">Loading playbook…</p>
{:else if !data}<Notice message={error} retry={load} />
{:else}
  {#if data.error}<Notice message={data.error} />{/if}
  <div class="playbook-layout">
    <aside class="playbook-list" aria-label="Playbook instructions and skills">
      <button
        class="playbook-item"
        class:selected={isReviewer}
        aria-pressed={isReviewer}
        aria-labelledby="reviewer-name"
        aria-describedby="reviewer-badges"
        disabled={busy}
        onclick={() => select('$reviewer')}
      >
        <strong id="reviewer-name">Reviewer instructions</strong>
        <span class="item-description">Method, scope, severity and evidence.</span>
        <span class="badges" id="reviewer-badges"
          ><span class="badge">Built-in</span>{#if data.reviewer.override !== null}<span
              class="badge">Edited</span
            >{/if}</span
        >
      </button>
      <h2 class="list-heading">
        Defect-class skills <span class="muted"
          >{data.skills.length} / {data.limits.max_skills}</span
        >
      </h2>
      <div class="skill-list">
        {#each data.skills as item (item.name)}
          <button
            class="playbook-item"
            class:selected={selected === item.name}
            aria-pressed={selected === item.name}
            aria-labelledby={`skill-name-${item.name}`}
            aria-describedby={`skill-description-${item.name} skill-badges-${item.name}`}
            disabled={busy}
            onclick={() => select(item.name)}
          >
            <strong id={`skill-name-${item.name}`}>{item.name}</strong>
            <span
              class="item-description"
              id={`skill-description-${item.name}`}
              title={item.description}>{item.description}</span
            >
            <span class="badges" id={`skill-badges-${item.name}`}>
              <span class="badge">{item.custom ? 'Custom' : 'Built-in'}</span>
              {#if !item.custom && item.override !== null}<span class="badge">Edited</span>{/if}
              {#if !item.enabled}<span class="badge">Disabled</span>{/if}
            </span>
          </button>
        {/each}
      </div>
      <button
        class="button new-skill"
        disabled={busy || data.skills.length >= data.limits.max_skills}
        onclick={newSkill}><Plus size={15} />New skill</button
      >
      {#if data.skills.length >= data.limits.max_skills}<p class="helper">
          Skill limit reached. Delete a custom skill to add another.
        </p>{/if}
    </aside>
    <section class="playbook-editor" aria-labelledby="editor-title">
      <div class="editor-heading">
        <div>
          <h2 id="editor-title">{title}</h2>
          <p class="helper">
            {isReviewer
              ? 'The method and scope used for every review.'
              : 'Full skill markdown, including name, description and optional enabled front matter.'}
          </p>
        </div>
        {#if dirty}<span class="badge">Unsaved</span>{/if}
      </div>
      {#if skill}<div class="toggle-row">
          <div>
            <h3>{skill.enabled ? 'Enabled' : 'Disabled'}</h3>
            <p>
              {dirty
                ? 'Save or discard your edits before enabling or disabling.'
                : 'Changes take effect immediately for future reviews.'}
            </p>
          </div>
          <Toggle
            checked={skill.enabled}
            label={`Enable ${skill.name}`}
            disabled={busy || dirty}
            onchange={(value) => void toggleEnabled(value)}
          />
        </div>{/if}
      {#if error}<Notice message={error} />{/if}
      {#if message}<Notice {message} kind="success" />{/if}
      <div class="field editor-field">
        <label for="playbook-content">{isReviewer ? 'Reviewer markdown' : 'Skill markdown'}</label>
        <textarea
          id="playbook-content"
          class="mono"
          rows="24"
          bind:value={content}
          disabled={busy}
          spellcheck="false"
          autocapitalize="off"
          autocomplete="off"
          aria-describedby="playbook-bytes playbook-total"
          aria-invalid={tooLarge}></textarea>
        <div class="byte-counts">
          <small id="playbook-bytes" class:over-limit={bytes > limit}>{bytes} / {limit} bytes</small
          >
          <small id="playbook-total" class:over-limit={total > data.limits.total_max_bytes}
            >Total: {total} / {data.limits.total_max_bytes} bytes</small
          >
        </div>
        {#if tooLarge}<p class="over-limit helper" role="status">
            Over size limit. Shorten the text before saving.
          </p>{/if}
        {#if skillLimit}<p class="over-limit helper">
            Too many skills. Delete a custom skill before saving.
          </p>{/if}
      </div>
      <div class="editor-actions actions">
        <button
          class="button primary"
          disabled={busy || !dirty || tooLarge || skillLimit}
          onclick={() => void save()}><Save size={14} />{busy ? 'Saving…' : 'Save'}</button
        >
        {#if dirty}<button
            class="button"
            disabled={busy}
            onclick={() => openItem(isNew ? '$reviewer' : selected)}>Discard</button
          >{/if}
        {#if skill?.custom}<button
            class="button danger"
            disabled={busy}
            onclick={() => void resetOrDelete()}><Trash2 size={14} />Delete</button
          >
        {:else if !isNew && edited}<button
            class="button"
            disabled={busy}
            onclick={() => void resetOrDelete()}><RotateCcw size={14} />Reset to built-in</button
          >{/if}
      </div>
      {#if builtin !== null && builtin !== undefined}<div class="builtin-comparison">
          <div class="toggle-row">
            <h3>Show built-in</h3>
            <Toggle
              checked={showBuiltin}
              label="Show built-in"
              onchange={(value) => (showBuiltin = value)}
            />
          </div>
          {#if showBuiltin}<div class="field">
              <label for="playbook-builtin"
                >Built-in {isReviewer ? 'reviewer instructions' : skill?.name} (read-only)</label
              ><textarea
                id="playbook-builtin"
                class="mono"
                readonly
                rows="18"
                value={builtin}
                spellcheck="false"></textarea>
            </div>{/if}
        </div>{/if}
    </section>
  </div>
{/if}

<style>
  .page-header p {
    max-width: 820px;
    line-height: 1.7;
  }
  .playbook-layout {
    display: grid;
    grid-template-columns: minmax(220px, 280px) minmax(0, 1fr);
    gap: 24px;
    align-items: start;
  }
  .playbook-list,
  .playbook-editor {
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .playbook-list {
    padding: 8px;
    background: var(--bg);
  }
  .playbook-item {
    display: grid;
    gap: 5px;
    width: 100%;
    min-width: 0;
    padding: 12px;
    text-align: left;
    border: 1px solid transparent;
    border-radius: 5px;
  }
  .playbook-item:hover {
    background: var(--hover);
  }
  .playbook-item.selected {
    border-color: var(--strong-border);
    background: var(--surface);
  }
  .playbook-item strong {
    font-size: 12px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .item-description {
    font-size: 11px;
    line-height: 1.6;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .badges {
    display: flex;
    gap: 5px;
    flex-wrap: wrap;
  }
  .list-heading {
    padding: 20px 12px 8px;
    background: var(--bg);
    font-size: 11px;
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .new-skill {
    width: 100%;
    margin-top: 8px;
  }
  .playbook-list > .helper {
    padding: 8px 12px;
  }
  .playbook-editor {
    padding: 24px;
  }
  .editor-heading {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 16px;
  }
  .editor-heading > div {
    min-width: 0;
  }
  .editor-heading h2 {
    overflow-wrap: anywhere;
  }
  .editor-heading .helper {
    margin-top: 6px;
  }
  .editor-heading > .badge {
    flex-shrink: 0;
  }
  .toggle-row {
    margin-bottom: 16px;
    padding-top: 0;
  }
  .toggle-row h3 {
    font-size: 12px;
    font-weight: 500;
  }
  textarea {
    font-size: 12px;
    line-height: 1.7;
    min-height: 320px;
    tab-size: 2;
  }
  .byte-counts {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }
  .over-limit {
    color: var(--red);
  }
  .editor-actions {
    flex-wrap: wrap;
    margin-top: 16px;
  }
  .builtin-comparison {
    margin-top: 24px;
    border-top: 1px solid var(--border);
    padding-top: 20px;
  }
  .builtin-comparison .toggle-row {
    border-bottom: 0;
    margin-bottom: 0;
    padding-bottom: 0;
  }
  .builtin-comparison .field {
    margin-top: 16px;
  }
  @media (max-width: 900px) {
    .playbook-layout {
      grid-template-columns: minmax(0, 1fr);
      gap: 16px;
    }
    .skill-list {
      max-height: 240px;
      overflow-y: auto;
      margin-top: 4px;
      padding: 6px;
      scroll-padding: 6px;
      border: 1px solid var(--border);
      border-radius: 6px;
    }
    .playbook-editor {
      padding: 20px;
    }
  }
  @media (max-width: 480px) {
    .playbook-editor {
      padding: 16px;
    }
    .editor-heading {
      flex-wrap: wrap;
    }
    .editor-actions > .button {
      min-height: 40px;
    }
  }
</style>
