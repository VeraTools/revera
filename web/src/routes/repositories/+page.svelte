<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { afterNavigate, goto, replaceState } from '$app/navigation';
  import { Dialog, DropdownMenu } from 'bits-ui';
  import {
    Search,
    MoreHorizontal,
    ArrowUpRight,
    LockKeyhole,
    GitPullRequest,
    RefreshCw,
  } from '@lucide/svelte';
  import { api, errorMessage } from '#lib/api';
  import { live } from '#lib/events';
  import { relative } from '#lib/time';
  import { createNotices } from '#lib/notices.svelte';
  import type { Repository, PullRequest, Trigger, ReviewMode } from '#lib/types';
  import { modes, triggers } from '#lib/config.svelte';
  import SelectField from '#lib/components/SelectField.svelte';
  import ReviewModeField from '#lib/components/ReviewModeField.svelte';
  import Status from '#lib/components/Status.svelte';
  import Notice from '#lib/components/Notice.svelte';
  let repos = $state<Repository[]>([]);
  let search = $state('');
  let triggerFilter = $state('');
  let activityFilter = $state('');
  let sort = $state('');
  let selection = $state<string[]>([]);
  let bulkTrigger = $state('');
  let bulkMode = $state('');
  let defaultTrigger = $state<Trigger>('mention');
  let defaultMode = $state<ReviewMode>('standard');
  let reviewMode = $state<ReviewMode | ''>('');
  let selectedMode = $state<ReviewMode>('standard');
  const notice = createNotices();
  let loading = $state(true);
  let busy = $state('');
  let dialog = $state(false);
  let selected = $state('');
  let pulls = $state<PullRequest[]>([]);
  let pullsLoading = $state(false);
  let pullError = $state('');
  let installUrl = $state('');
  let ready = $state(false);
  const timestamp = (date?: string) => (date ? Date.parse(date) || 0 : 0);
  const activity = (repo: Repository) =>
    Math.max(timestamp(repo.pushed_at), timestamp(repo.last_job?.queued_at));
  const activityOptions = [
    { value: '', label: 'Any activity' },
    { value: '7d', label: 'Active in the last 7 days' },
    { value: '30d', label: 'Active in the last 30 days' },
    { value: 'inactive', label: 'No activity in 30 days' },
  ];
  const sortOptions = [
    { value: '', label: 'Sort: Recent activity' },
    { value: 'name', label: 'Sort: Name' },
    { value: 'review', label: 'Sort: Last review' },
    { value: 'prs', label: 'Sort: Open pull requests' },
    { value: 'reviewed', label: 'Sort: Most reviewed' },
  ];
  const filtered = $derived(
    repos
      .filter((repo) => {
        const age = Date.now() - activity(repo);
        return (
          repo.full_name.toLowerCase().includes(search.toLowerCase()) &&
          (!triggerFilter || repo.trigger === triggerFilter) &&
          (!activityFilter ||
            (activityFilter === 'inactive'
              ? age > 30 * 86400000
              : age <= (activityFilter === '7d' ? 7 : 30) * 86400000))
        );
      })
      .sort((a, b) => {
        const difference =
          sort === 'name'
            ? 0
            : sort === 'review'
              ? timestamp(b.last_review?.queued_at) - timestamp(a.last_review?.queued_at)
              : sort === 'prs'
                ? (b.open_prs ?? -1) - (a.open_prs ?? -1)
                : sort === 'reviewed'
                  ? b.review_count - a.review_count
                  : activity(b) - activity(a);
        return difference || a.full_name.localeCompare(b.full_name);
      }),
  );
  // Counts cover the full inventory, so searching doesn't move the segments.
  const segments = $derived([
    { value: '', label: 'All', count: repos.length },
    ...['auto', 'mention', 'off'].map((value) => ({
      value,
      label: value[0].toUpperCase() + value.slice(1),
      count: repos.filter((repo) => repo.trigger === value).length,
    })),
  ]);
  function readQuery() {
    const params = page.url.searchParams;
    search = params.get('q') || '';
    triggerFilter = triggers.some((item) => item.value === params.get('trigger'))
      ? params.get('trigger')!
      : '';
    activityFilter = activityOptions.some((item) => item.value === params.get('active'))
      ? params.get('active')!
      : '';
    sort = sortOptions.some((item) => item.value === params.get('sort')) ? params.get('sort')! : '';
    ready = true;
  }
  afterNavigate(readQuery);
  $effect(() => {
    const values = { q: search, trigger: triggerFilter, active: activityFilter, sort };
    if (!ready) return;
    const url = new URL(window.location.href);
    for (const [key, value] of Object.entries(values)) {
      if (value) url.searchParams.set(key, value);
      else url.searchParams.delete(key);
    }
    if (url.href !== window.location.href) replaceState(url, {});
  });
  $effect(() => {
    const visible = new Set(filtered.map((repo) => repo.full_name));
    const kept = selection.filter((name) => visible.has(name));
    if (kept.length !== selection.length) selection = kept;
  });
  function selectRow(name: string, checked: boolean) {
    selection = checked ? [...selection, name] : selection.filter((item) => item !== name);
  }
  const retryIn = (date: string) =>
    `${Math.max(1, Math.ceil((timestamp(date) - Date.now()) / 60000))}m`;
  const triggerOptions = $derived([
    {
      value: '',
      label: `Default (${triggers.find((item) => item.value === defaultTrigger)?.label})`,
    },
    ...triggers,
  ]);
  const modeOptions = $derived([
    { value: '', label: `Default (${modes.find((item) => item.value === defaultMode)?.label})` },
    ...modes,
  ]);
  async function load() {
    try {
      const [items, defaults] = await Promise.all([api.repos(), api.serverSettings()]);
      repos = items;
      defaultTrigger = defaults.default_trigger;
      defaultMode = defaults.default_mode;
      notice.state.error = '';
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      loading = false;
    }
  }
  onMount(() => {
    readQuery();
    void api
      .overview()
      .then((overview) => (installUrl = overview.app.install_url))
      .catch((cause) => notice.failure(errorMessage(cause)));
  });
  $effect(() => {
    $live.repos;
    if (ready) void load();
  });
  async function update(repo: Repository, field: 'trigger' | 'mode', value: string) {
    busy = repo.full_name;
    try {
      await api.repoSettings(repo.full_name, { [field]: value || null });
      await load();
      notice.success(`${repo.full_name}: ${field} saved.`);
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      busy = '';
    }
  }
  async function index(repo: string) {
    busy = repo;
    try {
      await api.index(repo);
      await load();
      notice.success(`Index queued for ${repo}.`);
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      busy = '';
    }
  }
  async function bulkSettings(field: 'trigger' | 'mode', value: string) {
    if (!value) return;
    busy = 'bulk';
    try {
      const result = await api.bulkRepoSettings([...selection], {
        [field]: value === 'default' ? null : value,
      });
      const label =
        value === 'default'
          ? `Default (${field === 'trigger' ? defaultTrigger : defaultMode})`
          : value[0].toUpperCase() + value.slice(1);
      await load();
      notice.success(
        `${field === 'trigger' ? 'Trigger' : 'Mode'} set to ${label} for ${result.updated} repositories.`,
      );
    } catch (cause) {
      notice.failure(errorMessage(cause));
    } finally {
      busy = '';
      bulkTrigger = '';
      bulkMode = '';
    }
  }
  async function bulkIndex() {
    const selectedRepos = repos.filter((repo) => selection.includes(repo.full_name));
    busy = 'bulk';
    let queued = 0;
    const off = selectedRepos.filter((repo) => repo.trigger === 'off').length;
    const building = selectedRepos.filter(
      (repo) => repo.trigger !== 'off' && repo.index.state === 'building',
    ).length;
    try {
      for (const repo of selectedRepos) {
        if (repo.trigger === 'off' || repo.index.state === 'building') continue;
        await api.index(repo.full_name);
        queued++;
      }
      await load();
      notice.success(
        `Index queued for ${queued} repositories.${off ? ` ${off} skipped (reviews off).` : ''}${building ? ` ${building} skipped (index building).` : ''}`,
      );
    } catch (cause) {
      const failure = `Index queued for ${queued} repositories before an error: ${errorMessage(cause)}`;
      await load();
      notice.failure(failure);
    } finally {
      busy = '';
    }
  }
  async function choosePull(repo: Repository) {
    selected = repo.full_name;
    selectedMode = repo.mode;
    reviewMode = '';
    pulls = [];
    pullError = '';
    dialog = true;
    pullsLoading = true;
    try {
      pulls = await api.pulls(repo.full_name);
    } catch (cause) {
      pullError = errorMessage(cause);
    } finally {
      pullsLoading = false;
    }
  }
  async function review(number: number) {
    busy = selected;
    try {
      await api.review(selected, number, reviewMode || null);
      notice.success(`Review queued for ${selected} #${number}.`);
      dialog = false;
    } catch (cause) {
      pullError = errorMessage(cause);
    } finally {
      busy = '';
    }
  }
</script>

<svelte:head><title>Repositories · Revera</title></svelte:head>
<div class="page-header">
  <div>
    <h1>Repositories</h1>
    <p>Choose what gets reviewed and keep repository indexes up to date.</p>
  </div>
  {#if installUrl}<a href={installUrl} class="button" target="_blank" rel="noreferrer"
      >Manage installation <ArrowUpRight size={14} /></a
    >{/if}
</div>
<div class="filters">
  <div class="search-field">
    <label class="sr-only" for="repo-search">Search repositories</label><Search size={15} /><input
      id="repo-search"
      type="search"
      bind:value={search}
      placeholder="Search repositories…"
    />
  </div>
  <div class="segmented" role="group" aria-label="Filter by trigger">
    {#each segments as segment}<button
        aria-pressed={triggerFilter === segment.value}
        onclick={() => (triggerFilter = segment.value)}
        >{segment.label} <span>{segment.count}</span></button
      >{/each}
  </div>
  <SelectField
    id="repo-activity"
    label="Activity"
    bind:value={activityFilter}
    options={activityOptions}
  />
  <SelectField id="repo-sort" label="Sort repositories" bind:value={sort} options={sortOptions} />
  <span class="result-count">{filtered.length} repositories</span>
</div>
{#if notice.state.error}<Notice
    message={notice.state.error}
    retry={load}
  />{/if}{#if notice.state.message}<Notice message={notice.state.message} kind="success" />{/if}
{#if filtered.length}<div class="table-scroll">
    <table class="repos-table" style="min-width: 940px">
      <thead
        ><tr
          ><th class="selection-cell"
            ><input
              type="checkbox"
              aria-label="Select all visible repositories"
              checked={selection.length === filtered.length}
              indeterminate={selection.length > 0 && selection.length < filtered.length}
              onchange={(event) =>
                (selection = event.currentTarget.checked
                  ? filtered.map((repo) => repo.full_name)
                  : [])}
            /></th
          ><th>Repository</th><th>Trigger</th><th>Mode</th><th>Last review</th><th>Last push</th><th
            >Search index</th
          ><th><span class="sr-only">Actions</span></th></tr
        ></thead
      ><tbody
        >{#each filtered as repo (repo.full_name)}<tr
            class:selected-row={selection.includes(repo.full_name)}
            ><td class="selection-cell"
              ><input
                type="checkbox"
                aria-label={`Select ${repo.full_name}`}
                checked={selection.includes(repo.full_name)}
                onchange={(event) => selectRow(repo.full_name, event.currentTarget.checked)}
              /></td
            ><td
              ><a href={repo.html_url} class="row-title" target="_blank" rel="noreferrer"
                >{repo.full_name}</a
              >
              <div class="row-meta">
                {#if repo.private}<LockKeyhole size={10} />Private{:else}Public{/if}<span>·</span
                >{repo.default_branch}{#if repo.open_prs && repo.open_prs > 0}<span>·</span
                  >{repo.open_prs} open PRs{/if}
                {#if repo.has_override}<span
                    class="badge"
                    title="Uses revera.yaml from the repository base branch">revera.yaml</span
                  >{/if}
              </div></td
            ><td
              ><SelectField
                id={`trigger-${repo.full_name}`}
                label={`Trigger for ${repo.full_name}`}
                value={repo.trigger_override || ''}
                options={triggerOptions}
                disabled={!!busy}
                onchange={(value) => void update(repo, 'trigger', value)}
              /></td
            ><td
              ><SelectField
                id={`mode-${repo.full_name}`}
                label={`Mode for ${repo.full_name}`}
                value={repo.mode_override || ''}
                options={modeOptions}
                disabled={!!busy}
                onchange={(value) => void update(repo, 'mode', value)}
              /></td
            ><td
              >{#if repo.last_review}<a href={`/reviews/${repo.last_review.id}`}
                  ><Status status={repo.last_review.status} />
                  <div class="row-meta">
                    #{repo.last_review.pr} · {relative(
                      repo.last_review.queued_at,
                    )}{#if repo.review_count > 1}{` · ${repo.review_count} reviews`}{/if}
                  </div></a
                >
              {:else}<span class="muted">—</span>{/if}</td
            >
            <td class="muted nowrap"
              >{#if repo.pushed_at}<time
                  datetime={repo.pushed_at}
                  title={new Date(repo.pushed_at).toLocaleString()}>{relative(repo.pushed_at)}</time
                >{:else}—{/if}</td
            >
            <td
              ><Status
                status={repo.index.state}
                label={repo.index.state === 'none' ? 'Not indexed' : repo.index.state}
              />{#if repo.index.updated_at}<div class="row-meta">
                  <time
                    datetime={repo.index.updated_at}
                    title={new Date(repo.index.updated_at).toLocaleString()}
                    >{relative(repo.index.updated_at)}</time
                  >
                </div>{/if}{#if repo.index.retry_at}<div
                  class="row-meta"
                  title={new Date(repo.index.retry_at).toLocaleString()}
                >
                  Retry in {retryIn(repo.index.retry_at)}
                </div>{/if}</td
            ><td
              ><DropdownMenu.Root
                ><DropdownMenu.Trigger
                  class="button ghost icon small"
                  aria-label={`Actions for ${repo.full_name}`}
                  ><MoreHorizontal size={17} /></DropdownMenu.Trigger
                ><DropdownMenu.Portal
                  ><DropdownMenu.Content class="menu" sideOffset={4} align="end"
                    ><DropdownMenu.Item
                      class="menu-item"
                      disabled={!!busy || repo.index.state === 'building' || repo.trigger === 'off'}
                      onSelect={() => void index(repo.full_name)}
                      ><RefreshCw size={14} />Index now</DropdownMenu.Item
                    ><DropdownMenu.Item
                      class="menu-item"
                      disabled={!!busy || repo.trigger === 'off'}
                      onSelect={() => void choosePull(repo)}
                      ><GitPullRequest size={14} />Review a pull request</DropdownMenu.Item
                    ><DropdownMenu.Item
                      class="menu-item"
                      onSelect={() =>
                        void goto(`/reviews?repo=${encodeURIComponent(repo.full_name)}`)}
                      ><ArrowUpRight size={14} />View reviews</DropdownMenu.Item
                    ></DropdownMenu.Content
                  ></DropdownMenu.Portal
                ></DropdownMenu.Root
              ></td
            ></tr
          >{/each}</tbody
      >
    </table>
  </div>
  {#if repos.some((repo) => repo.has_override)}<p class="helper" style="margin-top: 16px">
      Repository overrides are read from the base branch, never from the pull request.
    </p>{/if}
{:else if loading}<div class="loading" role="status">Loading repositories…</div>{:else}<div
    class="empty-state"
  >
    {search || triggerFilter || activityFilter
      ? 'No repositories match your filters.'
      : 'No repositories are installed yet.'}<br
    />{#if search || triggerFilter || activityFilter}<button
        class="button"
        onclick={() => {
          search = '';
          triggerFilter = '';
          activityFilter = '';
        }}>Clear filters</button
      >{:else if installUrl}<a class="button" href={installUrl}>Install on repositories</a>{/if}
  </div>{/if}

{#if selection.length}<div class="save-bar bulk-bar" aria-label="Bulk repository actions">
    <strong>{selection.length} selected</strong>
    <div class="actions">
      <SelectField
        id="bulk-trigger"
        label="Set trigger for selected repositories"
        bind:value={bulkTrigger}
        options={[
          { value: '', label: 'Set trigger…', disabled: true },
          ...triggerOptions.map((item) => ({ ...item, value: item.value || 'default' })),
        ]}
        disabled={!!busy}
        onchange={(value) => void bulkSettings('trigger', value)}
      />
      <SelectField
        id="bulk-mode"
        label="Set mode for selected repositories"
        bind:value={bulkMode}
        options={[
          { value: '', label: 'Set mode…', disabled: true },
          ...modeOptions.map((item) => ({ ...item, value: item.value || 'default' })),
        ]}
        disabled={!!busy}
        onchange={(value) => void bulkSettings('mode', value)}
      />
      <button class="button small" disabled={!!busy} onclick={bulkIndex}>Index now</button>
      <button class="button small" disabled={!!busy} onclick={() => (selection = [])}>Clear</button>
    </div>
  </div>{/if}

<Dialog.Root bind:open={dialog}
  ><Dialog.Portal
    ><Dialog.Overlay class="dialog-overlay" /><Dialog.Content class="dialog"
      ><Dialog.Title class="dialog-title"><h2>Review a pull request</h2></Dialog.Title
      ><Dialog.Description class="dialog-description"
        >Choose an open pull request in {selected}.</Dialog.Description
      ><ReviewModeField
        id="manual-review-mode"
        bind:value={reviewMode}
        defaultMode={selectedMode}
      />
      {#if pullError}<Notice message={pullError} />{/if}{#if pullsLoading}<p class="loading">
          Loading pull requests…
        </p>{:else}{#each pulls as pull}<button
            class="pull-option"
            disabled={busy === selected}
            onclick={() => void review(pull.number)}
            ><span class="row-title">#{pull.number} {pull.title}</span><span class="row-meta"
              >{pull.author}{#if pull.draft}<span class="badge">Draft</span>{/if}<span>·</span
              >{relative(pull.updated_at)}</span
            ><span class="row-meta"
              >{#if pull.last_job}<Status status={pull.last_job.status} /> · {relative(
                  pull.last_job.queued_at,
                )}{:else}Not reviewed{/if}</span
            ></button
          >{:else}<p class="muted">There are no open pull requests.</p>{/each}{/if}
      <div class="actions"><Dialog.Close class="button">Cancel</Dialog.Close></div></Dialog.Content
    ></Dialog.Portal
  ></Dialog.Root
>
