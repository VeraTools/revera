<script lang="ts">
  import { onMount } from 'svelte';
  import { Dialog } from 'bits-ui';
  import { api, errorMessage } from '#lib/api';
  import type { GuidanceMode, GuidancePreview, RepoGuidanceSettings, Repository } from '#lib/types';
  import {
    guidanceDefaults,
    guidanceGroups,
    guidanceHint,
    guidanceLevels,
    guidanceSize,
    maxNotesCharacters,
    notesCharacters,
  } from '../guidance';
  import SelectField from './SelectField.svelte';
  import Notice from './Notice.svelte';

  let {
    repo,
    onclose,
    onsaved,
  }: { repo: Repository; onclose: () => void; onsaved: (repo: string) => void } = $props();
  let data = $state<GuidancePreview>();
  let global = $state<GuidancePreview['global']>({ guidance: 'off', notes_chars: 0 });
  let maxBytes = $state(16384);
  let level = $state<GuidanceMode | ''>('');
  let notes = $state('');
  let original = $state<{ guidance: GuidanceMode | null; notes: string | null }>({
    guidance: null,
    notes: null,
  });
  let initialized = $state(false);
  let previewLoading = $state(true);
  let previewError = $state('');
  let settingsError = $state('');
  let saveError = $state('');
  let busy = $state(false);
  let showAll = $state(false);
  let openPath = $state('');
  let fileLoading = $state('');
  let fileError = $state('');
  let contents = $state<Record<string, string>>({});
  const effective = $derived(level || global.guidance);
  const groups = $derived(guidanceGroups(data?.files || [], effective));
  const rootTooLarge = $derived(
    groups.some(
      (group) =>
        !group.dir &&
        group.files.some(
          (file) =>
            file.used &&
            file.bytes + new TextEncoder().encode(`\n--- ${file.path} ---\n`).length > maxBytes,
        ),
    ),
  );
  const normalizedNotes = $derived(notes.trim() || null);
  const count = $derived(notesCharacters(notes));
  const tooLong = $derived(count > maxNotesCharacters);
  const dirty = $derived(
    initialized && ((level || null) !== original.guidance || normalizedNotes !== original.notes),
  );
  const options = $derived([
    { value: '', label: `Inherit (${global.guidance})` },
    ...guidanceLevels,
  ]);
  const contentId = (path: string) => `guidance-content-${encodeURIComponent(path)}`;

  function initialize(pref: RepoGuidanceSettings) {
    if (initialized) return;
    original = { guidance: pref.guidance ?? null, notes: pref.notes?.trim() || null };
    level = original.guidance || '';
    notes = original.notes || '';
    initialized = true;
  }
  async function loadSettings() {
    settingsError = '';
    try {
      const result = await api.settings();
      const defaults = guidanceDefaults(result.yaml);
      global = { guidance: defaults.guidance, notes_chars: defaults.notes_chars };
      maxBytes = defaults.max_bytes;
      initialize(result.server.repos[repo.full_name] || {});
    } catch (cause) {
      settingsError = errorMessage(cause);
    }
  }
  async function loadPreview() {
    previewLoading = true;
    previewError = '';
    try {
      data = await api.repoGuidance(repo.full_name);
      global = data.global;
      maxBytes = data.max_bytes;
      initialize(data.repo);
    } catch (cause) {
      previewError = errorMessage(cause);
    } finally {
      previewLoading = false;
    }
  }
  onMount(() => {
    void loadSettings();
    void loadPreview();
  });
  async function readFile(path: string) {
    openPath = path;
    fileError = '';
    if (contents[path] !== undefined) return;
    fileLoading = path;
    try {
      contents[path] = await api.repoGuidanceFile(repo.full_name, path);
    } catch (cause) {
      if (openPath === path) fileError = errorMessage(cause);
    } finally {
      if (fileLoading === path) fileLoading = '';
    }
  }
  function toggleFile(path: string) {
    if (openPath === path) openPath = '';
    else void readFile(path);
  }
  async function save() {
    if (!dirty || tooLong || busy) return;
    const name = repo.full_name;
    const changes: RepoGuidanceSettings = {};
    if ((level || null) !== original.guidance) changes.guidance = level || null;
    if (normalizedNotes !== original.notes) changes.notes = normalizedNotes;
    busy = true;
    saveError = '';
    try {
      await api.repoSettings(name, changes);
      onsaved(name);
      onclose();
    } catch (cause) {
      saveError = errorMessage(cause);
    } finally {
      busy = false;
    }
  }
</script>

{#snippet fileContent(path: string)}
  {#if openPath === path}
    <div id={contentId(path)} class="file-preview">
      {#if fileLoading === path}<p class="helper" role="status">Loading file…</p>
      {:else if fileError}<Notice message={fileError} retry={() => void readFile(path)} />
      {:else}<div class="file-content" role="region" aria-label={`Contents of ${path}`}>
          <pre>{contents[path]}</pre>
        </div>{/if}
    </div>
  {/if}
{/snippet}

<Dialog.Root
  open
  onOpenChange={(value) => {
    if (!value) onclose();
  }}
>
  <Dialog.Portal>
    <Dialog.Overlay class="dialog-overlay" />
    <Dialog.Content class="dialog guidance-dialog">
      <div class="guidance-heading">
        <Dialog.Title style="font-size: 18px; font-weight: 600">Review guidance</Dialog.Title>
        <Dialog.Description class="dialog-description"
          >{repo.full_name} · project rules and operator review notes.</Dialog.Description
        >
      </div>
      <div class="guidance-body">
        {#if settingsError && !initialized}<Notice
            message={settingsError}
            retry={() => void loadSettings()}
          />{/if}
        <div class="field">
          <label for="repo-guidance-level">Repository guidance</label>
          <SelectField
            id="repo-guidance-level"
            value={level}
            {options}
            disabled={busy || !initialized}
            onchange={(value) => (level = value as GuidanceMode | '')}
          />
          <small>{guidanceHint[effective]}</small>
          {#if effective !== 'off'}<small class="caution"
              >Reviews follow these files from the base commit. Enable only for repositories whose
              default branch you trust.</small
            >{/if}
        </div>
        <div class="field guidance-notes">
          <label for="repo-guidance-notes">Review notes for this repository</label>
          <textarea
            id="repo-guidance-notes"
            rows="3"
            bind:value={notes}
            disabled={busy || !initialized}
            placeholder="What should reviews focus on or ignore here?"
            aria-describedby="repo-guidance-notes-help repo-guidance-notes-count"
            aria-invalid={tooLong}></textarea>
          <div class="notes-meta">
            <small id="repo-guidance-notes-help"
              >Notes apply at any level, including Off, and are added to the global notes.</small
            >
            <small id="repo-guidance-notes-count" class:over-limit={tooLong}
              >{count} / {maxNotesCharacters}</small
            >
          </div>
          {#if global.notes_chars > 0}<small>Global notes: {global.notes_chars} characters</small
            >{/if}
        </div>
        <section class="guidance-preview" aria-label="Guidance preview">
          <h3>
            What a review would read from <code>{data?.default_branch || repo.default_branch}</code>
          </h3>
          <p class="helper">
            Root files come first; nested files share the remaining {guidanceSize(maxBytes)} budget.
          </p>
          <p class="helper">
            Preview uses server defaults; the repository’s base-commit revera.yaml may differ.
          </p>
          {#if previewLoading}<p class="loading" role="status">Loading guidance preview…</p>
          {:else if previewError}<Notice message={previewError} retry={() => void loadPreview()} />
            <p class="helper">Preview is unavailable. You can still edit and save settings.</p>
          {:else if data}
            {#if data.truncated_tree}<Notice
                kind="info"
                message="GitHub returned a truncated tree. Some guidance files or skills may be missing."
              />{/if}
            {#if rootTooLarge}<p class="caution">
                Root guidance will be truncated to {guidanceSize(maxBytes)}.
              </p>{/if}
            {#if !data.files.length}<p class="helper">
                No REVIEW.md, AGENTS.md or CLAUDE.md on {data.default_branch}.
              </p>
            {:else if effective === 'review' && !data.files.some((file) => file.kind === 'review')}<p
                class="helper"
              >
                No REVIEW.md on {data.default_branch}.
              </p>{/if}
            {#each groups as group (group.dir)}
              <div class="guidance-group">
                <h4>{group.dir ? `${group.dir}/` : 'Repository root'}</h4>
                <p class="helper">
                  {group.dir
                    ? `Applies when a PR changes files under ${group.dir}/.`
                    : 'Root files apply to every PR when read at this level.'}
                </p>
                {#each group.files as file (file.path)}
                  <div class="guidance-file" class:unused={!file.used}>
                    <button
                      class="file-button"
                      aria-expanded={openPath === file.path}
                      aria-controls={openPath === file.path ? contentId(file.path) : undefined}
                      onclick={() => toggleFile(file.path)}
                    >
                      <span class="file-name">{file.path.split('/').at(-1)}</span><span
                        class="file-size">{guidanceSize(file.bytes)}</span
                      >
                    </button>
                    <div class="file-reason">
                      {#if file.used}<span class="badge">Eligible</span>{:else}{file.reason}{/if}
                    </div>
                  </div>
                  {@render fileContent(file.path)}
                {/each}
              </div>
            {/each}
            {#if data.files_omitted > 0}<p class="helper">
                {data.files_omitted} more guidance files are not shown.
              </p>{/if}
            {#if effective === 'skills'}
              <div class="guidance-group">
                <h4>Project skills <span class="muted">{data.skills.length}</span></h4>
                <p class="helper">
                  Reviewers open matching skills from the base commit before concluding.
                </p>
                {#each data.skills.slice(0, showAll ? undefined : 6) as skill (skill.name)}
                  <div class="skill-row">
                    <button
                      class="file-button"
                      aria-expanded={openPath === skill.path}
                      aria-controls={openPath === skill.path ? contentId(skill.path) : undefined}
                      onclick={() => toggleFile(skill.path)}
                    >
                      <span class="file-name">{skill.name}</span><span class="file-size"
                        >{guidanceSize(skill.bytes)}</span
                      >
                    </button>
                    <p class="helper">{skill.description || 'No description provided.'}</p>
                  </div>
                  {@render fileContent(skill.path)}
                {:else}<p class="helper">No project skills on {data.default_branch}.</p>{/each}
                {#if data.skills.length > 6}<button
                    class="button small"
                    aria-expanded={showAll}
                    onclick={() => (showAll = !showAll)}
                    >{showAll ? 'Show fewer' : `Show all ${data.skills.length}`}</button
                  >{/if}
                {#if data.skills_omitted}<p class="helper">
                    {data.skills_omitted} additional skills omitted by the index limits.
                  </p>{/if}
              </div>
            {/if}
          {/if}
        </section>
        {#if saveError}<Notice message={saveError} />{/if}
      </div>
      <div class="actions guidance-footer">
        <Dialog.Close class="button" disabled={busy}>Cancel</Dialog.Close>
        <button
          class="button primary"
          disabled={busy || !dirty || tooLong}
          onclick={() => void save()}>{busy ? 'Saving…' : 'Save'}</button
        >
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  :global(.dialog.guidance-dialog) {
    width: min(720px, calc(100vw - 32px));
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: 0;
  }
  .guidance-heading {
    padding: 24px 24px 0;
  }
  .guidance-body {
    padding: 0 24px 24px;
    overflow-y: auto;
    min-height: 0;
  }
  :global(.dialog.guidance-dialog > .guidance-footer) {
    margin: 0;
    padding: 16px 24px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
  }
  .guidance-heading {
    flex-shrink: 0;
  }
  .guidance-notes,
  .guidance-preview {
    margin-top: 24px;
  }
  .notes-meta {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: start;
  }
  .notes-meta small:last-child {
    white-space: nowrap;
  }
  .over-limit {
    color: var(--red);
  }
  .caution {
    color: var(--amber);
    font-size: 12px;
    line-height: 1.6;
  }
  .guidance-group {
    margin-top: 20px;
  }
  h3,
  h4 {
    font-size: 13px;
    font-weight: 600;
  }
  h4 .muted {
    margin-left: 4px;
    font-weight: 400;
  }
  .guidance-group > .helper {
    margin: 4px 0 8px;
  }
  .guidance-file,
  .skill-row {
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .file-button {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    text-align: left;
    gap: 12px;
    width: 100%;
    padding: 2px 0;
    color: var(--text);
  }
  .file-name {
    overflow-wrap: anywhere;
  }
  .file-button:hover .file-name {
    text-decoration: underline;
  }
  .file-size {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
  }
  .file-reason {
    margin-top: 2px;
    color: var(--muted);
    font-size: 11px;
  }
  .unused .file-name {
    color: var(--muted);
  }
  .skill-row .helper {
    margin-top: 4px;
  }
  .guidance-group > .button {
    margin-top: 12px;
  }
  .file-content {
    max-height: 260px;
    overflow: auto;
    padding: 12px;
    margin-top: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .file-content pre {
    margin: 0;
    white-space: inherit;
    overflow-wrap: inherit;
    font-size: inherit;
    line-height: inherit;
  }
  @media (max-width: 480px) {
    .guidance-heading {
      padding: 20px 16px 0;
    }
    .guidance-body {
      padding: 0 16px 20px;
    }
    :global(.dialog.guidance-dialog > .guidance-footer) {
      padding: 12px 16px;
    }
    .notes-meta {
      flex-wrap: wrap;
      gap: 4px;
    }
  }
</style>
